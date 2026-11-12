use anyhow::{Context, Result};
use evdev::EventType as EvdevEventType;
use evdev::{Device, EventStream, InputEvent};
use log::{debug, info, trace};

use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::cancellation::GhostwriterCancellation;
use crate::device::{DeviceModel, InputOrientation, VIRTUAL_HEIGHT, VIRTUAL_WIDTH};
use crate::pen::time_since_pen_lifted;
use crate::screenshot::{ScreenProbe, Screenshot};
use crate::simulation::{SimulationConfig, TouchSimulator};

/// The active pen tool slot in the RMPP xochitl palette.
/// These correspond to the first two slots in the pen type grid.
/// Verified palette slot coordinates: Ballpoint=(96,119), Fineliner=(150,119).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PenTool {
    Ballpoint,
    Fineliner,
    /// Another tool, by its place in the palette from the top: 2 is text, 3 the eraser, 4 selection
    Other(u32),
    Unknown,
}

/// How one of the palette's two pens is set up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PenSetup {
    /// Which pen: 0 is the first in the palette, 1 the second
    pub slot: u32,
    /// The kind of pen, by its place among the nine in the pen's settings: 0 is the
    /// ballpoint, 1 the fineliner
    pub kind: u32,
    /// The stroke size: 0 thin, 1 medium, 2 thick
    pub size: u32,
    /// The colour, by its place among the nine in the pen's settings: 0 is black
    pub colour: u32,
}

impl PenSetup {
    /// The second pen as a thin black fineliner.
    pub const FINELINER_THIN: Self = Self {
        slot: 1,
        kind: 1,
        size: 0,
        colour: 0,
    };

    /// The first pen as a medium black ballpoint, which is how the tablet comes.
    pub const BALLPOINT_MEDIUM: Self = Self {
        slot: 0,
        kind: 0,
        size: 1,
        colour: 0,
    };
}

/// What `Touch::select_pen` changed, for `Touch::restore_pen` to put back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PenRestore {
    /// Whether the palette was open
    pub palette_open: bool,
    /// The tool that was selected
    pub tool: PenTool,
    /// What the pen's kind, stroke size and colour were, each only if it was changed.
    /// The size and colour are those of the kind of pen that was drawn with: every
    /// kind keeps its own.
    pub kind: Option<u32>,
    pub size: Option<u32>,
    pub colour: Option<u32>,
}

impl PenRestore {
    /// Nothing was changed, so there is nothing to put back.
    pub const NOTHING: Self = Self {
        palette_open: false,
        tool: PenTool::Unknown,
        kind: None,
        size: None,
        colour: None,
    };
}

#[derive(Debug, Clone, Copy)]
pub enum TriggerCorner {
    UpperRight,
    UpperLeft,
    LowerRight,
    LowerLeft,
}

impl TriggerCorner {
    pub fn from_string(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "ur" | "upper-right" => Ok(TriggerCorner::UpperRight),
            "ul" | "upper-left" => Ok(TriggerCorner::UpperLeft),
            "lr" | "lower-right" => Ok(TriggerCorner::LowerRight),
            "ll" | "lower-left" => Ok(TriggerCorner::LowerLeft),
            _ => Err(anyhow::anyhow!(
                "Invalid trigger corner: {}. Use UR, UL, LR, LL, upper-right, upper-left, lower-right, or lower-left",
                s
            )),
        }
    }
}

// Event codes
const ABS_MT_SLOT: u16 = 47;
const ABS_MT_TOUCH_MAJOR: u16 = 48;
const ABS_MT_TOUCH_MINOR: u16 = 49;
const ABS_MT_ORIENTATION: u16 = 52;
const ABS_MT_POSITION_X: u16 = 53;
const ABS_MT_POSITION_Y: u16 = 54;
const ABS_MT_TRACKING_ID: u16 = 57;
const ABS_MT_PRESSURE: u16 = 58;

#[allow(clippy::large_enum_variant)]
pub enum TouchMode {
    Real {
        input_device: Option<Device>,      // For sending touch events
        event_stream: Option<EventStream>, // For reading touch events
        device_model: DeviceModel,
    },
    Simulated {
        simulator: TouchSimulator,
    },
}

pub struct Touch {
    mode: TouchMode,
    trigger_corner: TriggerCorner,
}

/// What the Paper Pro's tool palette is showing, read off the screen.
/// Positions are in screen pixels, measured on firmware 3.29.
struct Palette {
    screen: ScreenProbe,
}

impl Palette {
    /// The palette marks what is selected by filling in its box, which is this many pixels square
    const BOX: u32 = 112;
    /// The tools run down the left edge from this height: pen 1, pen 2, text, eraser, selection
    const TOOLS_TOP: u32 = 112;
    const TOOLS: u32 = 5;
    /// A pen's settings open beside it, their top level with the pen's own box. In them
    /// are three rows of three kinds of pen, a row of three stroke sizes, and three rows
    /// of three colours. This is the left of each first box, and how far below the top
    /// of the settings the kinds, the sizes and the colours start.
    const SETTINGS_LEFT: u32 = 144;
    const KINDS_DOWN: u32 = 80;
    const SIZES_DOWN: u32 = 530;
    const COLOURS_DOWN: u32 = 708;

    fn new() -> Result<Self> {
        Ok(Self {
            screen: Screenshot::new()?.probe()?,
        })
    }

    /// Whether the box with its corner at (left, top) is filled in.
    fn is_filled(&self, (left, top): (u32, u32)) -> bool {
        // Just inside each corner, clear of the icon or label in the middle
        [(6, 6), (Self::BOX - 6, 6), (6, Self::BOX - 6), (Self::BOX - 6, Self::BOX - 6)]
            .iter()
            .all(|&(x, y)| self.screen.is_dark(left + x, top + y))
    }

    fn tool_box(tool: u32) -> (u32, u32) {
        (0, Self::TOOLS_TOP + tool * Self::BOX)
    }

    /// The box `place` places along a grid three wide, in the settings of the pen in `slot`.
    fn settings_box(slot: u32, down: u32, place: u32) -> (u32, u32) {
        (
            Self::SETTINGS_LEFT + place % 3 * Self::BOX,
            Self::tool_box(slot).1 + down + place / 3 * Self::BOX,
        )
    }

    fn kind_box(slot: u32, kind: u32) -> (u32, u32) {
        Self::settings_box(slot, Self::KINDS_DOWN, kind)
    }

    fn size_box(slot: u32, size: u32) -> (u32, u32) {
        Self::settings_box(slot, Self::SIZES_DOWN, size)
    }

    fn colour_box(slot: u32, colour: u32) -> (u32, u32) {
        Self::settings_box(slot, Self::COLOURS_DOWN, colour)
    }

    /// Which tool is selected, counting from 0 at the top. None while the palette is closed.
    fn selected_tool(&self) -> Option<u32> {
        (0..Self::TOOLS).find(|&tool| self.is_filled(Self::tool_box(tool)))
    }

    /// Whether the settings of the pen in `slot` are showing: a panel with a border down
    /// its right side and white margins inside. Only the top of it is looked at, where
    /// the kinds of pen are: what comes below depends on the kind.
    fn settings_showing(&self, slot: u32) -> bool {
        let top = Self::tool_box(slot).1;
        let dark = |x, down| self.screen.is_dark(x, top + down);
        dark(511, 100) && dark(511, 400) && !dark(128, 276) && !dark(495, 276)
    }

    /// Which of the boxes in one part of a pen's settings is filled in. None unless the settings are showing.
    fn selected(&self, slot: u32, down: u32, places: u32) -> Option<u32> {
        (0..places).find(|&place| self.settings_showing(slot) && self.is_filled(Self::settings_box(slot, down, place)))
    }

    fn selected_kind(&self, slot: u32) -> Option<u32> {
        self.selected(slot, Self::KINDS_DOWN, 9)
    }

    fn selected_size(&self, slot: u32) -> Option<u32> {
        self.selected(slot, Self::SIZES_DOWN, 3)
    }

    fn selected_colour(&self, slot: u32) -> Option<u32> {
        self.selected(slot, Self::COLOURS_DOWN, 9)
    }

    /// The middle of a box, in virtual page coordinates, for tapping it.
    fn middle((left, top): (u32, u32), device_model: &DeviceModel) -> (i32, i32) {
        let (width, height) = device_model.page_size();
        (
            ((left + Self::BOX / 2) * VIRTUAL_WIDTH / width) as i32,
            ((top + Self::BOX / 2) * VIRTUAL_HEIGHT / height) as i32,
        )
    }
}

impl Touch {
    pub fn new(no_touch: bool, trigger_corner: TriggerCorner) -> Result<Self> {
        let device_model = DeviceModel::detect();
        info!("Touch using device model: {}", device_model.name());

        let (input_device, event_stream) = if no_touch {
            (None, None)
        } else {
            let device_path = device_model.touch_device_path();
            let open = || Device::open(device_path).with_context(|| format!("Failed to open touch device {device_path}"));
            let stream = open()?.into_event_stream()?;
            (Some(open()?), Some(stream))
        };

        Ok(Self {
            mode: TouchMode::Real {
                input_device,
                event_stream,
                device_model,
            },
            trigger_corner,
        })
    }

    pub fn new_simulated(simulation_config: SimulationConfig, trigger_corner: TriggerCorner) -> Result<Self> {
        let simulator = TouchSimulator::new(simulation_config, trigger_corner)?;
        info!("Touch using simulation mode");

        Ok(Self {
            mode: TouchMode::Simulated { simulator },
            trigger_corner,
        })
    }

    pub async fn wait_for_trigger(&mut self, cancellation: &GhostwriterCancellation) -> Result<()> {
        debug!("wait_for_trigger: entered, checking mode");
        match &mut self.mode {
            TouchMode::Simulated { simulator } => {
                debug!("wait_for_trigger: using Simulated mode");
                simulator.wait_for_trigger(cancellation).await
            }
            TouchMode::Real {
                event_stream, device_model, ..
            } => {
                debug!("wait_for_trigger: using Real device mode");
                let trigger_corner = self.trigger_corner;
                Self::wait_for_real_trigger(event_stream, device_model, trigger_corner, cancellation).await
            }
        }
    }

    async fn wait_for_real_trigger(
        event_stream: &mut Option<EventStream>,
        device_model: &DeviceModel,
        trigger_corner: TriggerCorner,
        cancellation: &GhostwriterCancellation,
    ) -> Result<()> {
        debug!("wait_for_real_trigger: entered");
        let mut position_x = 0;
        let mut position_y = 0;

        if let Some(events) = event_stream {
            debug!("wait_for_real_trigger: event stream available, entering wait loop");

            loop {
                debug!("wait_for_real_trigger: loop iteration starting");
                tokio::select! {
                    // Check for cancellation (only main token, not execution cycles)
                    _ = async {
                        while !cancellation.should_cancel_main() {
                            sleep(Duration::from_millis(50)).await;
                        }
                    } => {
                        debug!("wait_for_real_trigger: cancellation detected");
                        debug!("Touch waiting cancelled due to shutdown");
                        return Err(anyhow::anyhow!("Touch waiting cancelled"));
                    }

                    // Wait for next event
                    event_result = events.next_event() => {
                        debug!("wait_for_real_trigger: received event");
                        match event_result {
                            Ok(event) => {
                                if event.code() == ABS_MT_POSITION_X {
                                    position_x = event.value();
                                }
                                if event.code() == ABS_MT_POSITION_Y {
                                    position_y = event.value();
                                }
                                if event.code() == ABS_MT_TRACKING_ID && event.value() == -1 {
                                    let (x, y) = Self::input_to_virtual((position_x, position_y), device_model);
                                    debug!("Touch release detected at ({}, {}) normalized ({}, {})", position_x, position_y, x, y);
                                    if Self::is_in_trigger_zone(x, y, trigger_corner) {
                                        debug!("Touch release in target zone!");
                                        debug!("wait_for_real_trigger: returning Ok()");
                                        return Ok(());
                                    } else {
                                        debug!("Touch release NOT in trigger zone, continuing");
                                    }
                                }
                            }
                            Err(e) => {
                                debug!("Error reading touch events: {}", e);
                                return Err(e.into());
                            }
                        }
                    }
                }
            }
        } else {
            debug!("wait_for_real_trigger: no event stream available, entering cancellation wait loop");
            // No event stream available, just wait for cancellation
            loop {
                if cancellation.should_cancel_main() {
                    debug!("wait_for_real_trigger: cancellation detected in no-stream path");
                    debug!("Touch waiting cancelled due to shutdown");
                    return Err(anyhow::anyhow!("Touch waiting cancelled"));
                }
                sleep(Duration::from_millis(50)).await;
            }
        }
    }

    pub async fn touch_start(&mut self, xy: (i32, i32)) -> Result<()> {
        match &mut self.mode {
            TouchMode::Simulated { .. } => {
                debug!("Simulated touch_start at ({}, {})", xy.0, xy.1);
                Ok(())
            }
            TouchMode::Real {
                input_device, device_model, ..
            } => {
                let (x, y) = Self::virtual_to_input(xy, device_model);
                if let Some(device) = input_device {
                    trace!("touch_start at ({}, {})", x, y);
                    device.send_events(&[
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_SLOT, 0),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TRACKING_ID, 1),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_X, x),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_Y, y),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_PRESSURE, 100),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TOUCH_MAJOR, 17),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TOUCH_MINOR, 17),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_ORIENTATION, 4),
                        InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
                    ])?;
                    sleep(Duration::from_millis(1)).await;
                }
                Ok(())
            }
        }
    }

    pub async fn touch_stop(&mut self) -> Result<()> {
        match &mut self.mode {
            TouchMode::Simulated { .. } => {
                debug!("Simulated touch_stop");
                Ok(())
            }
            TouchMode::Real { input_device, .. } => {
                if let Some(device) = input_device {
                    trace!("touch_stop");
                    device.send_events(&[
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_SLOT, 0),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TRACKING_ID, -1),
                        InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
                    ])?;
                    sleep(Duration::from_millis(1)).await;
                }
                Ok(())
            }
        }
    }

    pub async fn goto_xy(&mut self, xy: (i32, i32)) -> Result<()> {
        match &mut self.mode {
            TouchMode::Simulated { .. } => {
                debug!("Simulated goto_xy at ({}, {})", xy.0, xy.1);
                Ok(())
            }
            TouchMode::Real {
                input_device, device_model, ..
            } => {
                let (x, y) = Self::virtual_to_input(xy, device_model);
                if let Some(device) = input_device {
                    device.send_events(&[
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_SLOT, 0),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TRACKING_ID, 1),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_X, x),
                        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_Y, y),
                        InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
                    ])?;
                }
                Ok(())
            }
        }
    }

    pub async fn tap_middle_bottom(&mut self) -> Result<()> {
        self.touch_start((384, 1023)).await?; // middle bottom
        sleep(Duration::from_millis(100)).await;
        self.touch_stop().await?;
        Ok(())
    }

    // ── Tool palette helpers ────────────────────────────────────────────────

    /// Palette toggle button (upper-left circle). Tapping toggles the palette open/closed.
    const PALETTE_BUTTON: (i32, i32) = (35, 35);

    /// Sidebar tool icon y-centers (virtual 768×1024 coords, x≈28).
    /// Verified by screenshot analysis. All icons are at x≈28 when palette is open.
    const SIDEBAR_Y_PEN1: i32 = 80; // Mechanical pencil (pen slot 1)
    const SIDEBAR_Y_PEN2: i32 = 130; // Fineliner (pen slot 2) — used by ghostwriter
    #[allow(dead_code)]
    const SIDEBAR_Y_TEXT: i32 = 187; // Text tool
    #[allow(dead_code)]
    const SIDEBAR_Y_ERASER: i32 = 240;
    const SIDEBAR_X: i32 = 28;

    /// Known sidebar tool y-centers for dynamic scanning.
    #[allow(dead_code)]
    const SIDEBAR_TOOL_YS: &'static [i32] = &[Self::SIDEBAR_Y_PEN1, Self::SIDEBAR_Y_PEN2, Self::SIDEBAR_Y_TEXT, Self::SIDEBAR_Y_ERASER];

    /// Settings panel coordinates for the Fineliner pen (slot 2, y≈130).
    /// NOTE: Tapping a pen-type icon closes the settings panel — skip that tap.
    /// Only configure size and color; these taps keep the settings panel open.
    const SETTINGS_SIZE_THIN: (i32, i32) = (96, 385); // Thin stroke thickness
    #[allow(dead_code)]
    const SETTINGS_SIZE_MEDIUM: (i32, i32) = (150, 385); // Medium stroke thickness
    const SETTINGS_COLOR_BLACK: (i32, i32) = (96, 468); // Black color (row 1, col 1)

    /// Detect whether the palette is currently open by scanning the screenshot.
    ///
    /// When the palette is OPEN, the left ~55px wide strip shows tool icons.
    /// We check whether there's substantial dark content in the sidebar region
    /// (pixel at x=28, y=80 is dark = pen1 icon or selected-background visible).
    /// When palette is CLOSED, only the toggle circle is visible; y=80 is white canvas.
    fn screenshot_palette_open(ss: &Screenshot) -> bool {
        // Check a pixel inside the expected sidebar tool area.
        // Any dark content at this position = palette is open.

        (60u32..110).any(|y| ss.get_pixel(28, y).map(|(r, _, _)| r < 180).unwrap_or(false))
    }

    /// Scan the open palette sidebar and return the y-center of the currently selected tool.
    ///
    /// When the palette is open, the selected tool has a dark (inverted) background
    /// spanning its full ~45px tall icon area. We scan x=5 (just inside the sidebar)
    /// to find the largest contiguous dark band.
    fn screenshot_selected_tool_y(ss: &Screenshot) -> Option<i32> {
        // Scan x=5, y=50..500 for dark pixels; find the longest contiguous run.
        let scan_x = 5u32;
        let mut best_run_start = 0i32;
        let mut best_run_len = 0usize;
        let mut cur_run_start = 0i32;
        let mut cur_run_len = 0usize;

        for y in 50u32..500 {
            let dark = ss.get_pixel(scan_x, y).map(|(r, _, _)| r < 100).unwrap_or(false);
            if dark {
                if cur_run_len == 0 {
                    cur_run_start = y as i32;
                }
                cur_run_len += 1;
            } else {
                if cur_run_len > best_run_len {
                    best_run_len = cur_run_len;
                    best_run_start = cur_run_start;
                }
                cur_run_len = 0;
            }
        }
        if cur_run_len > best_run_len {
            best_run_len = cur_run_len;
            best_run_start = cur_run_start;
        }

        if best_run_len >= 15 {
            Some(best_run_start + best_run_len as i32 / 2)
        } else {
            None
        }
    }

    /// Map a detected sidebar y-center to a PenTool (for the two pen slots we care about).
    fn y_to_pen_tool(y: i32) -> PenTool {
        if (y - Self::SIDEBAR_Y_PEN1).abs() < 25 {
            PenTool::Ballpoint
        } else if (y - Self::SIDEBAR_Y_PEN2).abs() < 25 {
            PenTool::Fineliner
        } else {
            PenTool::Unknown
        }
    }

    /// Take a fresh screenshot and detect palette state + active tool.
    /// Returns (palette_open, tool).
    async fn read_tool_state(&self) -> (bool, PenTool) {
        let mut ss = match Screenshot::new() {
            Ok(s) => s,
            Err(_) => return (false, PenTool::Unknown),
        };
        if ss.take_screenshot().is_err() {
            return (false, PenTool::Unknown);
        }
        let palette_open = Self::screenshot_palette_open(&ss);
        let tool = if palette_open {
            Self::screenshot_selected_tool_y(&ss).map(Self::y_to_pen_tool).unwrap_or(PenTool::Unknown)
        } else {
            PenTool::Unknown
        };
        info!("read_tool_state: palette_open={} → {:?}", palette_open, tool);
        (palette_open, tool)
    }

    /// Tap a point (touch_start + brief hold + touch_stop).
    async fn tap(&mut self, xy: (i32, i32)) -> Result<()> {
        self.touch_start(xy).await?;
        sleep(Duration::from_millis(100)).await;
        self.touch_stop().await?;
        sleep(Duration::from_millis(300)).await;
        Ok(())
    }

    /// Tap, and wait for the screen to show what the tap was for. Taps again if nothing comes of it.
    async fn tap_until(&mut self, xy: (i32, i32), what: &str, done: impl Fn() -> bool) -> Result<()> {
        const HOLD: Duration = Duration::from_millis(40);
        // Long enough that a slow answer is not taken for none: a second tap would undo the
        // first. The palette answers within 20 ms as a rule, but after a long drawing
        // xochitl can take over a second to get round to a tap it has accepted.
        const PATIENCE: Duration = Duration::from_millis(1500);
        // The tablet ignores touches until the pen has been gone this long
        const AFTER_PEN: Duration = Duration::from_millis(300);

        if let Some(wait) = time_since_pen_lifted().and_then(|since| AFTER_PEN.checked_sub(since)) {
            sleep(wait).await;
        }
        for _ in 0..2 {
            self.touch_start(xy).await?;
            sleep(HOLD).await;
            self.touch_stop().await?;

            let tapped_at = Instant::now();
            while tapped_at.elapsed() < PATIENCE {
                if done() {
                    debug!("{} took {:?}", what, tapped_at.elapsed());
                    return Ok(());
                }
                sleep(Duration::from_millis(5)).await;
            }
        }
        anyhow::bail!("Tapping the tool palette at {xy:?} did not {what}")
    }

    /// Open the palette, if it is closed.
    async fn open_palette(&mut self, palette: &Palette) -> Result<()> {
        if palette.selected_tool().is_none() {
            self.tap_until(Self::PALETTE_BUTTON, "open the palette", || palette.selected_tool().is_some())
                .await?;
        }
        Ok(())
    }

    async fn close_palette(&mut self, palette: &Palette) -> Result<()> {
        self.tap_until(Self::PALETTE_BUTTON, "close the palette", || palette.selected_tool().is_none())
            .await
    }

    /// With the palette open, select the pen in `slot` and show its settings.
    async fn open_pen_settings(&mut self, palette: &Palette, slot: u32) -> Result<()> {
        let pen = Palette::middle(Palette::tool_box(slot), &self.device_model());
        if palette.selected_tool() != Some(slot) {
            self.tap_until(pen, "select the pen", || palette.selected_tool() == Some(slot)).await?;
        }
        // Tapping the selected pen opens its settings
        if !palette.settings_showing(slot) {
            self.tap_until(pen, "open the pen's settings", || palette.settings_showing(slot)).await?;
        }
        Ok(())
    }

    /// With a pen's settings showing, choose its kind. Returns the kind it was, if that was another.
    async fn choose_kind(&mut self, palette: &Palette, slot: u32, kind: u32) -> Result<Option<u32>> {
        let before = palette.selected_kind(slot);
        if before == Some(kind) {
            return Ok(None);
        }
        let device_model = self.device_model();
        let kind_box = Palette::middle(Palette::kind_box(slot, kind), &device_model);
        // On some firmware choosing a kind closes the settings, which then need opening again to see it
        self.tap_until(kind_box, "choose the kind of pen", || {
            palette.selected_kind(slot) == Some(kind) || !palette.settings_showing(slot)
        })
        .await?;
        // The settings are laid out afresh for the new kind; give that a moment to finish
        sleep(Duration::from_millis(150)).await;
        if !palette.settings_showing(slot) {
            let pen = Palette::middle(Palette::tool_box(slot), &device_model);
            self.tap_until(pen, "open the pen's settings again", || palette.settings_showing(slot)).await?;
        }
        anyhow::ensure!(palette.selected_kind(slot) == Some(kind), "The palette did not change the kind of pen");
        Ok(before)
    }

    /// With a pen's settings showing, choose its stroke size. Returns the size it was, if that was another.
    async fn choose_size(&mut self, palette: &Palette, slot: u32, size: u32) -> Result<Option<u32>> {
        let before = palette.selected_size(slot);
        if before == Some(size) {
            return Ok(None);
        }
        let size_box = Palette::middle(Palette::size_box(slot, size), &self.device_model());
        self.tap_until(size_box, "choose the stroke size", || palette.selected_size(slot) == Some(size))
            .await?;
        Ok(before)
    }

    /// With a pen's settings showing, choose its colour. Returns the colour it was, if that was another.
    async fn choose_colour(&mut self, palette: &Palette, slot: u32, colour: u32) -> Result<Option<u32>> {
        let before = palette.selected_colour(slot);
        if before == Some(colour) {
            return Ok(None);
        }
        let colour_box = Palette::middle(Palette::colour_box(slot, colour), &self.device_model());
        self.tap_until(colour_box, "choose the colour", || palette.selected_colour(slot) == Some(colour))
            .await?;
        Ok(before)
    }

    /// Select a pen and set it up, reading the palette off the screen after each tap
    /// instead of allowing time for it.
    async fn select_pen_watching(&mut self, palette: &Palette, wanted: PenSetup) -> Result<PenRestore> {
        let palette_open = palette.selected_tool().is_some();
        self.open_palette(palette).await?;
        let tool = match palette.selected_tool() {
            Some(0) => PenTool::Ballpoint,
            Some(1) => PenTool::Fineliner,
            Some(tool) => PenTool::Other(tool),
            None => PenTool::Unknown,
        };
        self.open_pen_settings(palette, wanted.slot).await?;
        let restore = PenRestore {
            palette_open,
            tool,
            kind: self.choose_kind(palette, wanted.slot, wanted.kind).await?,
            size: self.choose_size(palette, wanted.slot, wanted.size).await?,
            colour: self.choose_colour(palette, wanted.slot, wanted.colour).await?,
        };
        self.close_palette(palette).await?;

        info!("Selected the pen {:?}; to put back: {:?}", wanted, restore);
        Ok(restore)
    }

    /// Put back what `select_pen_watching` changed.
    async fn restore_pen_watching(&mut self, palette: &Palette, pen: PenSetup, restore: PenRestore) -> Result<()> {
        let tool = match restore.tool {
            PenTool::Ballpoint => Some(0),
            PenTool::Fineliner => Some(1),
            PenTool::Other(tool) => Some(tool),
            PenTool::Unknown => None,
        };
        let pen_changed = restore.kind.is_some() || restore.size.is_some() || restore.colour.is_some();
        if !pen_changed && tool.is_none_or(|tool| tool == pen.slot) && !restore.palette_open {
            return Ok(());
        }

        self.open_palette(palette).await?;
        if pen_changed {
            self.open_pen_settings(palette, pen.slot).await?;
            // The size and colour belong to the kind of pen that was drawn with, so they go back first
            if let Some(size) = restore.size {
                self.choose_size(palette, pen.slot, size).await?;
            }
            if let Some(colour) = restore.colour {
                self.choose_colour(palette, pen.slot, colour).await?;
            }
            if let Some(kind) = restore.kind {
                self.choose_kind(palette, pen.slot, kind).await?;
            }
        }
        if let Some(tool) = tool.filter(|&tool| palette.selected_tool() != Some(tool)) {
            let middle = Palette::middle(Palette::tool_box(tool), &self.device_model());
            self.tap_until(middle, "select the tool", || palette.selected_tool() == Some(tool)).await?;
        }
        // Drawing needs the palette out of the way; afterwards it is left open if it was
        // open, though without the pen's settings showing
        if restore.palette_open {
            if palette.settings_showing(pen.slot) {
                let pen_box = Palette::middle(Palette::tool_box(pen.slot), &self.device_model());
                self.tap_until(pen_box, "close the pen's settings", || !palette.settings_showing(pen.slot))
                    .await?;
            }
            return Ok(());
        }
        self.close_palette(palette).await
    }

    /// Select a tool by its place in the palette, reading the palette off the screen after each tap.
    async fn select_tool_watching(&mut self, palette: &Palette, tool: u32) -> Result<()> {
        self.open_palette(palette).await?;
        if palette.selected_tool() != Some(tool) {
            let middle = Palette::middle(Palette::tool_box(tool), &self.device_model());
            self.tap_until(middle, "select the tool", || palette.selected_tool() == Some(tool)).await?;
        }
        self.close_palette(palette).await
    }

    /// Whether the tool palette is open, read off the screen. Only on a Paper Pro.
    pub fn palette_is_open() -> Result<bool> {
        Ok(Palette::new()?.selected_tool().is_some())
    }

    /// Tap the button that opens and closes the tool palette, once, without waiting to see what comes of it.
    pub async fn tap_palette_button(&mut self) -> Result<()> {
        self.touch_start(Self::PALETTE_BUTTON).await?;
        sleep(Duration::from_millis(40)).await;
        self.touch_stop().await
    }

    /// Select one of the palette's pens and set it up for drawing. Returns what was
    /// changed, for `restore_pen` to put back afterwards.
    ///
    /// On a Paper Pro the palette is read off the screen, which is quick and sure. On
    /// other devices, and if that fails, the second pen is selected by timed taps, and
    /// only as a thin black fineliner.
    pub async fn select_pen(&mut self, wanted: PenSetup) -> Result<PenRestore> {
        if let Ok(palette) = Palette::new() {
            match self.select_pen_watching(&palette, wanted).await {
                Ok(restore) => return Ok(restore),
                Err(e) => info!("{}; selecting the pen by timed taps instead", e),
            }
        }
        anyhow::ensure!(
            wanted == PenSetup::FINELINER_THIN,
            "Only the thin fineliner can be selected without reading the palette off the screen"
        );
        let tool = self.select_fineliner_by_timed_taps().await?;
        Ok(PenRestore { tool, ..PenRestore::NOTHING })
    }

    /// Change the colour of a pen that `select_pen` has selected, to carry on drawing in
    /// another ink. `restore` is what `select_pen` returned, and is brought up to date
    /// so that the colour the pen had to begin with is the one that gets put back.
    /// Only where the palette can be read off the screen.
    pub async fn change_pen_colour(&mut self, pen: PenSetup, colour: u32, restore: &mut PenRestore) -> Result<()> {
        let palette = Palette::new()?;
        self.open_palette(&palette).await?;
        self.open_pen_settings(&palette, pen.slot).await?;
        let before = self.choose_colour(&palette, pen.slot, colour).await?;
        restore.colour = restore.colour.or(before);
        self.close_palette(&palette).await
    }

    /// Put back the tool, and the pen's settings, that `select_pen` changed.
    pub async fn restore_pen(&mut self, pen: PenSetup, restore: PenRestore) -> Result<()> {
        if let Ok(palette) = Palette::new() {
            match self.restore_pen_watching(&palette, pen, restore).await {
                Ok(()) => return Ok(()),
                Err(e) => info!("{}; restoring the tool by timed taps instead", e),
            }
        }
        self.restore_tool(restore.tool).await
    }

    fn device_model(&self) -> DeviceModel {
        match &self.mode {
            TouchMode::Real { device_model, .. } => *device_model,
            TouchMode::Simulated { .. } => DeviceModel::Unknown,
        }
    }

    /// Select the second pen as a thin black fineliner. Returns the tool that was selected before.
    pub async fn select_fineliner(&mut self) -> Result<PenTool> {
        Ok(self.select_pen(PenSetup::FINELINER_THIN).await?.tool)
    }

    /// Select fineliner pen with thin size and black color, without being able to see the palette.
    ///
    /// Robust algorithm that does not rely on knowing the current state:
    /// 1. Open palette (toggle if closed)
    /// 2. Tap ballpoint sidebar icon → guarantees ballpoint is now active
    /// 3. Tap fineliner sidebar icon → selects it (since ballpoint was active, this just selects)
    /// 4. Tap fineliner sidebar icon again → opens its settings (it's now active)
    /// 5. Configure: thin size, black color
    /// 6. Close palette
    async fn select_fineliner_by_timed_taps(&mut self) -> Result<PenTool> {
        // Read current state so we can return the previous tool
        let (palette_open, previous) = self.read_tool_state().await;

        // Step 1: Open palette if not already open
        if !palette_open {
            self.tap(Self::PALETTE_BUTTON).await?;
            sleep(Duration::from_millis(100)).await; // Extra delay after toggle
        }

        let pen1 = (Self::SIDEBAR_X, Self::SIDEBAR_Y_PEN1);
        let pen2 = (Self::SIDEBAR_X, Self::SIDEBAR_Y_PEN2);

        // Step 2: Tap pen1 — guarantees pen1 is now the active tool
        self.tap(pen1).await?;

        // Step 3: Tap pen2 — selects it (pen1 was active, so this just switches)
        self.tap(pen2).await?;

        // Step 4: Tap pen2 again — opens its settings (pen2 is now active)
        self.tap(pen2).await?;
        sleep(Duration::from_millis(100)).await; // Extra delay for settings panel animation

        // Step 5: Configure thin size (skip tip type — tapping it closes the settings panel)
        self.tap(Self::SETTINGS_SIZE_THIN).await?;

        // Step 6: Configure black color
        self.tap(Self::SETTINGS_COLOR_BLACK).await?;

        // Step 8: Close palette
        self.tap(Self::PALETTE_BUTTON).await?;

        info!("select_fineliner: done, previous={:?}", previous);
        Ok(previous)
    }

    /// Switch to the given pen tool. Returns the previously active tool so caller can restore.
    /// Uses sidebar icons for reliable tool selection.
    pub async fn switch_to_tool(&mut self, target: PenTool) -> Result<PenTool> {
        let place = match target {
            PenTool::Fineliner => return self.select_fineliner().await,
            PenTool::Unknown => return Ok(PenTool::Unknown),
            PenTool::Ballpoint => 0,
            PenTool::Other(place) => place,
        };

        // On a Paper Pro the palette can be read off the screen, which is quicker and surer
        if let Ok(palette) = Palette::new() {
            match self.select_tool_watching(&palette, place).await {
                Ok(()) => return Ok(PenTool::Unknown),
                Err(e) => info!("{}; selecting the tool by timed taps instead", e),
            }
        }

        let (palette_open, current_tool) = self.read_tool_state().await;
        let previous = if palette_open { PenTool::Unknown } else { current_tool };
        if target == PenTool::Ballpoint {
            // Open palette, tap pen1 sidebar icon, close palette
            if !palette_open {
                self.tap(Self::PALETTE_BUTTON).await?;
                sleep(Duration::from_millis(100)).await;
            }
            self.tap((Self::SIDEBAR_X, Self::SIDEBAR_Y_PEN1)).await?;
            self.tap(Self::PALETTE_BUTTON).await?;
        }

        info!("switch_to_tool: {:?} → {:?}", previous, target);
        Ok(previous)
    }

    /// Restore a previously saved tool (e.g. after drawing is done).
    pub async fn restore_tool(&mut self, previous: PenTool) -> Result<()> {
        if previous == PenTool::Unknown || previous == PenTool::Fineliner {
            return Ok(()); // Nothing to restore or already on fineliner
        }
        self.switch_to_tool(previous).await?;
        Ok(())
    }

    fn is_in_trigger_zone(x: i32, y: i32, trigger_corner: TriggerCorner) -> bool {
        const CORNER_SIZE: i32 = 68; // Size of the trigger zone (68x68 pixels)

        match trigger_corner {
            TriggerCorner::UpperRight => x > VIRTUAL_WIDTH as i32 - CORNER_SIZE && y < CORNER_SIZE,
            TriggerCorner::UpperLeft => x < CORNER_SIZE && y < CORNER_SIZE,
            TriggerCorner::LowerRight => x > VIRTUAL_WIDTH as i32 - CORNER_SIZE && y > VIRTUAL_HEIGHT as i32 - CORNER_SIZE,
            TriggerCorner::LowerLeft => x < CORNER_SIZE && y > VIRTUAL_HEIGHT as i32 - CORNER_SIZE,
        }
    }

    /// Convert a virtual page coordinate to the touchscreen's own coordinates.
    fn virtual_to_input((x, y): (i32, i32), device_model: &DeviceModel) -> (i32, i32) {
        let x_normalized = x as f32 / VIRTUAL_WIDTH as f32;
        let mut y_normalized = y as f32 / VIRTUAL_HEIGHT as f32;
        if device_model.input_orientation() == InputOrientation::Rm2 {
            y_normalized = 1.0 - y_normalized;
        }
        let (touch_width, touch_height) = device_model.touch_size();

        ((x_normalized * touch_width as f32) as i32, (y_normalized * touch_height as f32) as i32)
    }

    /// Convert a touchscreen coordinate to the virtual page.
    fn input_to_virtual((x, y): (i32, i32), device_model: &DeviceModel) -> (i32, i32) {
        let (touch_width, touch_height) = device_model.touch_size();
        let x_normalized = x as f32 / touch_width as f32;
        let mut y_normalized = y as f32 / touch_height as f32;
        if device_model.input_orientation() == InputOrientation::Rm2 {
            y_normalized = 1.0 - y_normalized;
        }

        ((x_normalized * VIRTUAL_WIDTH as f32) as i32, (y_normalized * VIRTUAL_HEIGHT as f32) as i32)
    }

    /// Update the trigger corner (called when config changes)
    pub fn set_trigger_corner(&mut self, new_corner: TriggerCorner) {
        self.trigger_corner = new_corner;
        if let TouchMode::Simulated { simulator } = &mut self.mode {
            simulator.set_trigger_corner(new_corner);
        }
    }

    /// Get handle for manual triggering (for web API in simulation mode)
    pub fn get_manual_trigger_handle(&self) -> Option<std::sync::Arc<std::sync::Mutex<Vec<TriggerCorner>>>> {
        match &self.mode {
            TouchMode::Simulated { simulator } => Some(simulator.get_manual_trigger_handle()),
            TouchMode::Real { .. } => None,
        }
    }

    /// Add a manual trigger (for web API in simulation mode)
    pub fn add_manual_trigger(&self, corner: TriggerCorner) {
        if let TouchMode::Simulated { simulator } = &self.mode {
            simulator.add_manual_trigger(corner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_pro_touch_coordinates_scale_directly() {
        let device = DeviceModel::RemarkablePaperPro;
        assert_eq!(Touch::virtual_to_input((384, 1023), &device), (1032, 2830));
        assert_eq!(Touch::input_to_virtual((2065, 0), &device), (768, 0));
    }

    #[test]
    fn rm2_touch_y_axis_is_flipped() {
        let device = DeviceModel::Remarkable2;
        assert_eq!(Touch::virtual_to_input((0, 0), &device), (0, 1872));
        assert_eq!(Touch::virtual_to_input((768, 1024), &device), (1404, 0));
        assert_eq!(Touch::input_to_virtual((1404, 1872), &device), (768, 0));
    }

    #[test]
    fn a_touch_in_the_chosen_corner_is_a_trigger() {
        for device in [DeviceModel::Remarkable2, DeviceModel::RemarkablePaperPro, DeviceModel::RemarkablePaperProMove] {
            // Touch the upper-right corner of the page, in the device's own coordinates
            let (x, y) = Touch::input_to_virtual(Touch::virtual_to_input((750, 20), &device), &device);
            assert!(Touch::is_in_trigger_zone(x, y, TriggerCorner::UpperRight), "{device:?}");
            assert!(!Touch::is_in_trigger_zone(x, y, TriggerCorner::LowerLeft), "{device:?}");
        }
    }
}
