use anyhow::{Context, Result};
use evdev::EventType as EvdevEventType;
use evdev::{Device, EventStream, InputEvent};
use log::{debug, info, trace};

use std::time::Duration;
use tokio::time::sleep;

use crate::cancellation::GhostwriterCancellation;
use crate::device::{DeviceModel, InputOrientation, VIRTUAL_HEIGHT, VIRTUAL_WIDTH};
use crate::screenshot::Screenshot;
use crate::simulation::{SimulationConfig, TouchSimulator};

/// The active pen tool slot in the RMPP xochitl palette.
/// These correspond to the first two slots in the pen type grid.
/// Verified palette slot coordinates: Ballpoint=(96,119), Fineliner=(150,119).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PenTool {
    Ballpoint,
    Fineliner,
    Unknown,
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

    /// Select fineliner pen with correct tip type, medium size, and black color.
    ///
    /// Robust algorithm that does not rely on knowing the current state:
    /// 1. Open palette (toggle if closed)
    /// 2. Tap ballpoint sidebar icon → guarantees ballpoint is now active
    /// 3. Tap fineliner sidebar icon → selects it (since ballpoint was active, this just selects)
    /// 4. Tap fineliner sidebar icon again → opens its settings (it's now active)
    /// 5. Configure: fineliner tip, medium size, black color
    /// 6. Close palette
    pub async fn select_fineliner(&mut self) -> Result<PenTool> {
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
        let (palette_open, current_tool) = self.read_tool_state().await;
        let previous = if palette_open { PenTool::Unknown } else { current_tool };

        match target {
            PenTool::Fineliner => {
                return self.select_fineliner().await;
            }
            PenTool::Ballpoint => {
                // Open palette, tap pen1 sidebar icon, close palette
                if !palette_open {
                    self.tap(Self::PALETTE_BUTTON).await?;
                    sleep(Duration::from_millis(100)).await;
                }
                self.tap((Self::SIDEBAR_X, Self::SIDEBAR_Y_PEN1)).await?;
                self.tap(Self::PALETTE_BUTTON).await?;
            }
            PenTool::Unknown => {}
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
        for device in [DeviceModel::Remarkable2, DeviceModel::RemarkablePaperPro] {
            // Touch the upper-right corner of the page, in the device's own coordinates
            let (x, y) = Touch::input_to_virtual(Touch::virtual_to_input((750, 20), &device), &device);
            assert!(Touch::is_in_trigger_zone(x, y, TriggerCorner::UpperRight), "{device:?}");
            assert!(!Touch::is_in_trigger_zone(x, y, TriggerCorner::LowerLeft), "{device:?}");
        }
    }
}
