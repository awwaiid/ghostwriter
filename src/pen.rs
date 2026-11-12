use anyhow::{Context, Result};
use evdev::EventType as EvdevEventType;
use evdev::{Device, InputEvent};
use log::{debug, info};
use resvg::usvg;
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::device::{DeviceModel, InputOrientation, VIRTUAL_HEIGHT, VIRTUAL_WIDTH};
use crate::screenshot::{ScreenProbe, Screenshot};
use crate::stroke_plan::{plan_drawing, watch_pixels, Ink, PenModel};
use crate::touch::PenSetup;
use crate::util::{parse_svg, svg_to_alpha_bitmap};

/// One continuous pen stroke.
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    /// Where the pen goes, in virtual page coordinates. Fractions are kept: the
    /// digitizer is several times finer than the virtual page.
    pub points: Vec<(f32, f32)>,
    /// Pen pressure. `FULL_PRESSURE` is a firm stroke.
    pub pressure: i32,
}

/// How quickly strokes are sent to the device.
#[derive(Debug, Clone, Copy)]
pub struct StrokeTiming {
    /// Pause after the pen touches down, before it moves.
    pub after_down: Duration,
    /// Pause after the pen lifts, before the next stroke.
    pub after_up: Duration,
    /// Pause after every `points_per_pause` points along a stroke.
    pub pause: Duration,
    pub points_per_pause: usize,
}

impl StrokeTiming {
    /// Under 1000 strokes a second, for when the screen cannot be watched to see how
    /// the device is keeping up. A Paper Pro on firmware 3.29 draws about 2000 a second
    /// and pauses now and then for tens of milliseconds, so this leaves it room: sent
    /// more than it can take, strokes are lost and others run into each other.
    pub fn unwatched() -> Self {
        Self {
            after_up: Duration::from_micros(900),
            ..Self::default()
        }
    }
}

impl Default for StrokeTiming {
    /// About 2000 strokes a second, which is as fast as a Paper Pro on firmware 3.29
    /// draws them when all is well.
    fn default() -> Self {
        Self {
            after_down: Duration::ZERO,
            after_up: Duration::from_micros(300),
            pause: Duration::from_millis(1),
            points_per_pause: 100,
        }
    }
}

/// The pressure of a firm stroke. (The digitizers report up to about 4096.)
pub const FULL_PRESSURE: i32 = 2630;

/// When any `Pen` in this process last lifted off the page.
static LAST_LIFTED: Mutex<Option<Instant>> = Mutex::new(None);

/// How long ago the pen last lifted off the page, if it has drawn at all.
///
/// The tablet ignores touches while a pen is about, and goes on ignoring them for a
/// little under a quarter of a second after it has gone (measured on a Paper Pro,
/// firmware 3.29: a tap 220 ms after the pen is lost, one at 240 ms is taken).
pub fn time_since_pen_lifted() -> Option<Duration> {
    LAST_LIFTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .map(|lifted| lifted.elapsed())
}

/// The pens ghostwriter can draw with: the ones whose strokes have been measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DrawingPen {
    /// The second pen in the palette, as a thin black fineliner
    Fineliner,
    /// The first pen in the palette, as a medium black ballpoint
    Ballpoint,
}

impl DrawingPen {
    pub fn from_name(name: &str) -> Result<Self> {
        match name.to_lowercase().as_str() {
            "fineliner" => Ok(Self::Fineliner),
            "ballpoint" => Ok(Self::Ballpoint),
            other => anyhow::bail!("Unknown pen {other}: use fineliner or ballpoint"),
        }
    }

    /// How the pen's strokes come out, for planning a drawing.
    pub fn model(self) -> PenModel {
        match self {
            Self::Fineliner => PenModel::FINELINER_THIN,
            Self::Ballpoint => PenModel::BALLPOINT_MEDIUM,
        }
    }

    /// How the pen is set up in the palette to draw in `ink`, for selecting it.
    pub fn setup(self, ink: Ink) -> PenSetup {
        let black = match self {
            Self::Fineliner => PenSetup::FINELINER_THIN,
            Self::Ballpoint => PenSetup::BALLPOINT_MEDIUM,
        };
        PenSetup {
            colour: ink.palette_place(),
            ..black
        }
    }
}

/// How an SVG is to be drawn on a device. Working it out takes a few tenths of a
/// second, which can be spent while something else is going on.
pub enum Drawing {
    /// Strokes planned for a pen that has been measured on the Paper Pro: a page that
    /// took half a minute takes a couple of seconds. They are grouped by the ink they
    /// are to be drawn in, black first if there is any.
    Strokes { layers: Vec<(Ink, Vec<Stroke>)>, pen: PenModel },
    /// Row by row, with pen pressure standing in for anti-aliasing. Each row of the
    /// page is `scale` rows of alpha values.
    Rows { alpha: Vec<Vec<u8>>, scale: u32 },
}

impl Drawing {
    /// Work out how to draw an SVG with `pen`, the pen that is selected on the device.
    pub fn plan(svg_data: &str, device_model: DeviceModel, pen: PenModel) -> Result<Self> {
        match device_model {
            DeviceModel::RemarkablePaperPro => {
                let started = Instant::now();
                let layers = plan_drawing(svg_data, device_model.page_size(), &pen)?;
                let strokes: usize = layers.iter().map(|layer| layer.1.len()).sum();
                info!("Planned {} strokes in {} inks in {:?}", strokes, layers.len(), started.elapsed());
                Ok(Self::Strokes { layers, pen })
            }
            _ => {
                let scale = 2;
                let alpha = svg_to_alpha_bitmap(svg_data, VIRTUAL_WIDTH * scale, VIRTUAL_HEIGHT * scale)?;
                Ok(Self::Rows { alpha, scale })
            }
        }
    }
}

pub struct Pen {
    device: Option<Device>,
    device_model: DeviceModel,
    /// How far short of its last point each stroke gets an extra point, in device pixels,
    /// for pens that only draw a stroke evenly when it has one (see `PenModel::tail`)
    tail: f32,
}

impl Pen {
    pub fn new(no_draw: bool) -> Result<Self> {
        let device_model = DeviceModel::detect();
        info!("Pen using device model: {}", device_model.name());

        let device = if no_draw {
            None
        } else {
            let device_path = device_model.pen_device_path();
            Some(Device::open(device_path).with_context(|| format!("Failed to open pen device {device_path}"))?)
        };

        Ok(Self {
            device,
            device_model,
            tail: 0.0,
        })
    }

    pub fn draw_line_screen(&mut self, p1: (i32, i32), p2: (i32, i32)) -> Result<()> {
        self.draw_line(self.virtual_to_input(p1), self.virtual_to_input(p2))
    }

    pub fn draw_line(&mut self, (x1, y1): (i32, i32), (x2, y2): (i32, i32)) -> Result<()> {
        // Divide the line into steps of at most 5 units
        let length = ((x2 as f32 - x1 as f32).powf(2.0) + (y2 as f32 - y1 as f32).powf(2.0)).sqrt();
        let steps = ((length / 5.0).ceil() as i32).max(1);

        self.pen_up()?;
        self.pen_down_at((x1, y1))?;

        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let x = (x1 as f32 + (x2 - x1) as f32 * t).round() as i32;
            let y = (y1 as f32 + (y2 - y1) as f32 * t).round() as i32;
            self.goto_xy((x, y))?;
        }

        self.pen_up()?;

        Ok(())
    }

    pub fn draw_bitmap(&mut self, bitmap: &[Vec<bool>]) -> Result<()> {
        self.draw_bitmap_scaled(bitmap, 1)
    }

    /// Draw a bitmap where bitmap dimensions may differ from the virtual coordinate space.
    /// `scale` = bitmap_size / virtual_size — e.g. scale=2 means a 1536×2048 bitmap
    /// that maps to the 768×1024 virtual space at half-unit precision per pixel.
    /// Input coordinates are computed directly from bitmap pixel position for true sub-pixel accuracy,
    /// bypassing integer rounding through the virtual coordinate space.
    pub fn draw_bitmap_scaled(&mut self, bitmap: &[Vec<bool>], scale: u32) -> Result<()> {
        let mut is_pen_down = false;
        for (y, row) in bitmap.iter().enumerate() {
            for (x, &pixel) in row.iter().enumerate() {
                if pixel {
                    let point = self.bitmap_to_input(x, y, scale);
                    if !is_pen_down {
                        self.pen_down_at(point)?;
                        is_pen_down = true;
                        sleep(Duration::from_millis(1));
                    }
                    self.goto_xy(point)?;
                } else if is_pen_down {
                    self.pen_up()?;
                    is_pen_down = false;
                    sleep(Duration::from_millis(1));
                }
            }
            // Lift at end of every row for clean horizontal-run boundaries
            self.pen_up()?;
            is_pen_down = false;
            sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    pub fn pen_down(&mut self) -> Result<()> {
        if let Some(device) = &mut self.device {
            device.send_events(&[
                InputEvent::new(EvdevEventType::KEY.0, 320, 1),           // BTN_TOOL_PEN
                InputEvent::new(EvdevEventType::KEY.0, 330, 1),           // BTN_TOUCH
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 24, 2630),    // ABS_PRESSURE (max pressure)
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 25, 0),       // ABS_DISTANCE
                InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
            ])?;
        }
        Ok(())
    }

    // Press the pen down at a specific input-space coordinate cleanly:
    // First hover at the target position (BTN_TOOL_PEN=1, no touch), then press.
    // This pre-positions the pen so there's no snap artifact from a prior position.
    pub fn pen_down_at(&mut self, (x, y): (i32, i32)) -> Result<()> {
        if let Some(device) = &mut self.device {
            // Hover far: activate tool at target position from far away (no mark)
            device.send_events(&[
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 0, x),        // ABS_X
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 1, y),        // ABS_Y
                InputEvent::new(EvdevEventType::KEY.0, 320, 1),           // BTN_TOOL_PEN
                InputEvent::new(EvdevEventType::KEY.0, 330, 0),           // BTN_TOUCH (not yet)
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 24, 0),       // ABS_PRESSURE = 0
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 25, 100),     // ABS_DISTANCE far
                InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
            ])?;
            // Press: touch down at same position
            device.send_events(&[
                InputEvent::new(EvdevEventType::KEY.0, 330, 1),           // BTN_TOUCH
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 24, 2630),    // ABS_PRESSURE (max)
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 25, 0),       // ABS_DISTANCE = 0
                InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
            ])?;
        }
        Ok(())
    }

    pub fn pen_up(&mut self) -> Result<()> {
        if let Some(device) = &mut self.device {
            device.send_events(&[
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 24, 0),       // ABS_PRESSURE
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 25, 100),     // ABS_DISTANCE
                InputEvent::new(EvdevEventType::KEY.0, 330, 0),           // BTN_TOUCH
                InputEvent::new(EvdevEventType::KEY.0, 320, 0),           // BTN_TOOL_PEN
                InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
            ])?;
            *LAST_LIFTED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
        }
        Ok(())
    }

    pub fn goto_xy_virtual(&mut self, point: (i32, i32)) -> Result<()> {
        self.goto_xy(self.virtual_to_input(point))
    }

    pub fn goto_xy(&mut self, (x, y): (i32, i32)) -> Result<()> {
        if let Some(device) = &mut self.device {
            device.send_events(&[
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 0, x),        // ABS_X
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 1, y),        // ABS_Y
                InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
            ])?;
        }
        Ok(())
    }

    // Draw a single segment from prev to (px,py), emitting interpolated goto_xy events.
    // Returns updated step_count.
    fn draw_segment(&mut self, prev: (f32, f32), (px, py): (f32, f32), step_count: &mut usize, max_step: f32) -> Result<()> {
        let dist = ((px - prev.0).powi(2) + (py - prev.1).powi(2)).sqrt();
        let steps = ((dist / max_step).ceil() as usize).max(1);
        for s in 1..=steps {
            let t = s as f32 / steps as f32;
            let ix = (prev.0 + (px - prev.0) * t).round() as i32;
            let iy = (prev.1 + (py - prev.1) * t).round() as i32;
            self.goto_xy_virtual((ix, iy))?;
            *step_count += 1;
            if *step_count % 100 == 99 {
                sleep(Duration::from_millis(1));
            }
        }
        Ok(())
    }

    fn draw_polylines(&mut self, polylines: &[svg2polylines::Polyline]) -> Result<()> {
        // Strategy:
        // - Simple polygons (avg segment length > 10 virtual units): split at sharp corners (≥ 25°)
        //   for crisp geometric edges. At each corner, overshoot 3 units in the incoming direction
        //   before lifting to ensure the stroke actually reaches the corner point (prevents gaps).
        // - Complex paths (short avg segment): text glyphs, circles, curves — draw continuously.
        //   avg_segment_len distinguishes these: rect/triangle sides are 100-800 units long;
        //   text glyph segments from svg2polylines at 0.5 tolerance are typically 1-5 units.
        const MAX_STEP: f32 = 1.0; // virtual units per event

        for polyline in polylines {
            if polyline.len() < 2 {
                continue;
            }

            // Compute average segment length to classify path type
            let avg_segment_len: f32 = {
                let mut total = 0.0f32;
                for i in 1..polyline.len() {
                    let dx = (polyline[i].x - polyline[i - 1].x) as f32;
                    let dy = (polyline[i].y - polyline[i - 1].y) as f32;
                    total += (dx * dx + dy * dy).sqrt();
                }
                total / (polyline.len() - 1) as f32
            };

            // Simple polygons have long segments; text/circles have many short segments.
            // Simple geometric shapes (line, triangle, rect) have avg_seg > 100 units.
            // Text glyphs and circles have avg_seg < 20 even at large font sizes.
            // Use 50.0 as threshold to safely distinguish them.
            let corner_threshold_deg: f32 = if avg_segment_len > 50.0 { 25.0 } else { 360.0 };
            info!(
                "polyline: {} pts, avg_seg={:.1}, threshold={}°",
                polyline.len(),
                avg_segment_len,
                corner_threshold_deg as u32
            );

            let mut step_count = 0usize;
            let mut pen_is_down = false;
            let mut prev = (polyline[0].x as f32, polyline[0].y as f32);

            for i in 0..polyline.len() {
                let cur = (polyline[i].x as f32, polyline[i].y as f32);

                if !pen_is_down {
                    let start = self.virtual_to_input((cur.0 as i32, cur.1 as i32));
                    self.pen_up()?;
                    sleep(Duration::from_millis(2));
                    self.pen_down_at(start)?;
                    sleep(Duration::from_millis(2));
                    pen_is_down = true;
                    prev = cur;
                    continue;
                }

                // Check for sharp corner at cur (between prev→cur and cur→next).
                let should_lift = if i + 1 < polyline.len() {
                    let next = (polyline[i + 1].x as f32, polyline[i + 1].y as f32);
                    let d_in = (cur.0 - prev.0, cur.1 - prev.1);
                    let d_out = (next.0 - cur.0, next.1 - cur.1);
                    let len_in = (d_in.0.powi(2) + d_in.1.powi(2)).sqrt();
                    let len_out = (d_out.0.powi(2) + d_out.1.powi(2)).sqrt();
                    if len_in > 0.1 && len_out > 0.1 {
                        let cos_a = (d_in.0 * d_out.0 + d_in.1 * d_out.1) / (len_in * len_out);
                        cos_a.clamp(-1.0, 1.0).acos().to_degrees() > corner_threshold_deg
                    } else {
                        false
                    }
                } else {
                    false
                };

                if should_lift {
                    // Overshoot 2 units past the corner in the incoming direction to ensure
                    // the stroke fully reaches the corner before lifting (prevents gaps).
                    let d_in = (cur.0 - prev.0, cur.1 - prev.1);
                    let len_in = (d_in.0.powi(2) + d_in.1.powi(2)).sqrt();
                    let overshoot = if len_in > 0.1 {
                        let unit = (d_in.0 / len_in, d_in.1 / len_in);
                        (cur.0 + unit.0 * 3.0, cur.1 + unit.1 * 3.0)
                    } else {
                        cur
                    };
                    self.draw_segment(prev, overshoot, &mut step_count, MAX_STEP)?;
                    self.pen_up()?;
                    sleep(Duration::from_millis(2));
                    // Re-press at the exact corner (not the overshoot) for a clean start
                    let start = self.virtual_to_input((cur.0 as i32, cur.1 as i32));
                    self.pen_down_at(start)?;
                    sleep(Duration::from_millis(2));
                } else {
                    self.draw_segment(prev, cur, &mut step_count, MAX_STEP)?;
                }

                prev = cur;
            }

            self.pen_up()?;
            sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    /// Draw a bitmap bidirectionally — alternating L→R and R→L per row.
    /// This cancels the directional bias that causes horizontal leaking.
    pub fn draw_bitmap_bidi(&mut self, bitmap: &[Vec<bool>], scale: u32) -> Result<()> {
        let mut is_pen_down = false;
        for (y, row) in bitmap.iter().enumerate() {
            let cols = row.len();

            // Collect runs in this row
            let mut runs: Vec<(usize, usize)> = Vec::new();
            let mut run_start: Option<usize> = None;
            for (x, &pixel) in row.iter().enumerate() {
                match (pixel, run_start) {
                    (true, None) => run_start = Some(x),
                    (false, Some(s)) => {
                        runs.push((s, x - 1));
                        run_start = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = run_start {
                runs.push((s, cols - 1));
            }

            // Even rows: L→R, odd rows: R→L
            let go_left = (y % 2) == 1;
            let ordered_runs: Vec<(usize, usize)> = if go_left { runs.iter().rev().cloned().collect() } else { runs.clone() };

            for (x_start, x_end) in ordered_runs {
                // For L→R draw start→end; for R→L draw end→start
                let (draw_from, draw_to) = if go_left { (x_end, x_start) } else { (x_start, x_end) };

                if is_pen_down {
                    self.pen_up()?;
                    sleep(Duration::from_millis(1));
                }
                self.pen_down_at(self.bitmap_to_input(draw_from, y, scale))?;
                sleep(Duration::from_millis(1));
                self.goto_xy(self.bitmap_to_input(draw_to, y, scale))?;
                self.pen_up()?;
                is_pen_down = false;
                sleep(Duration::from_millis(1));
            }

            // Lift at end of every row
            if is_pen_down {
                self.pen_up()?;
                is_pen_down = false;
            }
            sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    /// Draw a bitmap column-first — scanning each column top→bottom.
    /// This rotates the directional bias 90° so it appears vertically instead of horizontally.
    pub fn draw_bitmap_col(&mut self, bitmap: &[Vec<bool>], scale: u32) -> Result<()> {
        if bitmap.is_empty() {
            return Ok(());
        }
        let rows = bitmap.len();
        let cols = bitmap[0].len();

        #[allow(clippy::needless_range_loop)]
        for x in 0..cols {
            let mut is_pen_down = false;
            let mut run_start: Option<usize> = None;

            for y in 0..rows {
                let pixel = bitmap[y][x];
                let point = self.bitmap_to_input(x, y, scale);

                match (pixel, run_start) {
                    (true, None) => {
                        run_start = Some(y);
                        self.pen_down_at(point)?;
                        is_pen_down = true;
                        sleep(Duration::from_millis(1));
                        self.goto_xy(point)?;
                    }
                    (true, Some(_)) => {
                        self.goto_xy(point)?;
                    }
                    (false, Some(_)) => {
                        run_start = None;
                        self.pen_up()?;
                        is_pen_down = false;
                        sleep(Duration::from_millis(1));
                    }
                    (false, None) => {}
                }
            }

            if is_pen_down {
                self.pen_up()?;
                sleep(Duration::from_millis(1));
            }
            sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    /// Draw using alpha values as pen pressure for anti-aliased rendering.
    /// Takes a Vec<Vec<u8>> of alpha values (0-255) and draws each pixel
    /// with pressure proportional to its alpha value.
    pub fn draw_bitmap_alpha_pressure(&mut self, alpha_bitmap: &[Vec<u8>], scale: u32) -> Result<()> {
        for (y, row) in alpha_bitmap.iter().enumerate() {
            let mut is_pen_down = false;
            let mut last_pressure: i32 = 0;

            for (x, &alpha) in row.iter().enumerate() {
                if alpha <= 15 {
                    if is_pen_down {
                        self.pen_up()?;
                        is_pen_down = false;
                        sleep(Duration::from_millis(1));
                    }
                    continue;
                }

                let point = self.bitmap_to_input(x, y, scale);
                let pressure = (alpha as f32 / 255.0 * 2630.0).round() as i32;

                if !is_pen_down || pressure != last_pressure {
                    if is_pen_down {
                        self.pen_up()?;
                        sleep(Duration::from_millis(1));
                    }
                    self.goto_xy_with_pressure(point, pressure)?;
                    is_pen_down = true;
                    last_pressure = pressure;
                } else {
                    self.goto_xy(point)?;
                }
            }

            if is_pen_down {
                self.pen_up()?;
            }
            sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    /// Move to position with a specific pressure value (for alpha-pressure rendering).
    fn goto_xy_with_pressure(&mut self, (x, y): (i32, i32), pressure: i32) -> Result<()> {
        if let Some(device) = &mut self.device {
            // Hover to position first
            device.send_events(&[
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 0, x),        // ABS_X
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 1, y),        // ABS_Y
                InputEvent::new(EvdevEventType::KEY.0, 320, 1),           // BTN_TOOL_PEN
                InputEvent::new(EvdevEventType::KEY.0, 330, 0),           // BTN_TOUCH (not yet)
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 24, 0),       // ABS_PRESSURE = 0
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 25, 100),     // ABS_DISTANCE far
                InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
            ])?;
            // Press with given pressure
            device.send_events(&[
                InputEvent::new(EvdevEventType::KEY.0, 330, 1),            // BTN_TOUCH
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 24, pressure), // ABS_PRESSURE
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 25, 0),        // ABS_DISTANCE = 0
                InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0),  // SYN_REPORT
            ])?;
        }
        Ok(())
    }

    /// Draw SVG using path tracing via svg2polylines for smooth continuous strokes.
    /// Text is converted to glyph paths by usvg before tracing.
    pub fn draw_svg_paths(&mut self, svg_data: &str) -> Result<()> {
        // Use usvg to parse and flatten text → paths
        let tree = parse_svg(svg_data)?;
        let write_opt = usvg::WriteOptions::default(); // preserve_text=false → converts text to paths
        let flattened_svg = tree.to_string(&write_opt);

        let polylines = svg2polylines::parse(&flattened_svg, 0.5, true).map_err(|e| anyhow::anyhow!("svg2polylines error: {}", e))?;
        info!("draw_svg_paths: {} polylines", polylines.len());
        self.draw_polylines(&polylines)
    }

    /// Draw paths given as lists of (x, y) virtual coordinates.
    /// No corner detection — skeleton paths are already single-pixel-wide and smooth.
    fn draw_virtual_paths(&mut self, paths: &[Vec<(f32, f32)>]) -> Result<()> {
        const MAX_STEP: f32 = 1.0;
        let mut step_count = 0usize;

        for path in paths {
            if path.len() < 2 {
                continue;
            }

            let start = self.virtual_to_input((path[0].0 as i32, path[0].1 as i32));
            self.pen_up()?;
            sleep(Duration::from_millis(2));
            self.pen_down_at(start)?;
            sleep(Duration::from_millis(2));

            let mut prev = path[0];
            for &pt in &path[1..] {
                self.draw_segment(prev, pt, &mut step_count, MAX_STEP)?;
                prev = pt;
            }

            self.pen_up()?;
            sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    /// Draw SVG by rasterizing to a bitmap, thinning to a 1-pixel skeleton,
    /// then tracing the skeleton to pen strokes.  Produces single-stroke centerlines
    /// for text and shapes without needing to "fill" outlines.
    pub fn draw_svg_centerline(&mut self, svg_data: &str) -> Result<()> {
        use crate::skeleton;
        use crate::util::svg_to_bitmap;

        // Rasterize at 2× for better thinning quality, then scale coords back
        let scale = 2u32;
        let mut bitmap = svg_to_bitmap(svg_data, VIRTUAL_WIDTH * scale, VIRTUAL_HEIGHT * scale)?;
        info!("draw_svg_centerline: rasterized {}×{} bitmap", VIRTUAL_WIDTH * scale, VIRTUAL_HEIGHT * scale);

        skeleton::thin_zhang_suen(&mut bitmap);
        info!("draw_svg_centerline: thinning done");

        let raw_paths = skeleton::trace_skeleton(&bitmap);
        info!("draw_svg_centerline: {} skeleton paths", raw_paths.len());

        // Scale from 2× pixel space back to virtual 768×1024 and smooth out staircase artifacts
        let inv = 1.0 / scale as f32;
        let paths: Vec<Vec<(f32, f32)>> = raw_paths
            .iter()
            .map(|p| {
                let scaled: Vec<(f32, f32)> = p.iter().map(|&(x, y)| (x * inv, y * inv)).collect();
                skeleton::smooth_path(&scaled, 5)
            })
            .collect();

        self.draw_virtual_paths(&paths)
    }

    /// Draw SVG using path tracing, passing SVG directly to svg2polylines without usvg preprocessing.
    /// Better for geometric shapes (lines, rects, circles), but text won't render.
    pub fn draw_svg_paths_raw(&mut self, svg_data: &str) -> Result<()> {
        let polylines = svg2polylines::parse(svg_data, 0.5, true).map_err(|e| anyhow::anyhow!("svg2polylines error: {}", e))?;
        info!("draw_svg_paths_raw: {} polylines", polylines.len());
        self.draw_polylines(&polylines)
    }

    pub fn device_model(&self) -> DeviceModel {
        self.device_model
    }

    /// Draw an SVG on the page with `pen`, the pen that is selected on the device.
    pub fn draw_svg(&mut self, svg_data: &str, pen: PenModel) -> Result<()> {
        self.draw(&Drawing::plan(svg_data, self.device_model, pen)?, false)?;
        Ok(())
    }

    /// Draw what was planned for an SVG, all of it with the pen as it is: whatever ink
    /// each part was meant to be in, it comes out in the colour the pen is set to.
    ///
    /// With `check_pen`, the first stroke is drawn on its own and looked at. If it has
    /// not come out the way the pen the drawing was planned for would draw it, nothing
    /// more is drawn and this returns false: some other tool is selected. (Where the
    /// screen cannot be watched there is no telling, and the whole drawing is drawn.)
    pub fn draw(&mut self, drawing: &Drawing, check_pen: bool) -> Result<bool> {
        match drawing {
            Drawing::Strokes { layers, pen } => {
                let strokes: Vec<Stroke> = layers.iter().flat_map(|(_, strokes)| strokes.iter().cloned()).collect();
                self.draw_planned_strokes(&strokes, pen, check_pen)
            }
            Drawing::Rows { alpha, scale } => self.draw_bitmap_alpha_pressure(alpha, *scale).map(|()| true),
        }
    }

    /// Draw strokes planned for `pen`, the pen that is selected on the device, watching
    /// the screen if it can be watched. `check_pen` is as for `draw`.
    pub fn draw_planned_strokes(&mut self, strokes: &[Stroke], pen: &PenModel, check_pen: bool) -> Result<bool> {
        self.tail = pen.tail;
        let drawn = match Screenshot::new().and_then(|mut screenshot| screenshot.probe()) {
            Ok(screen) => self.draw_strokes_watched(strokes, &screen, pen, check_pen),
            Err(e) => {
                info!("Drawing without watching the screen: {}", e);
                self.draw_strokes(strokes, &StrokeTiming::unwatched()).map(|()| true)
            }
        };
        self.tail = 0.0;
        drawn
    }

    /// Draw one stroke and see whether it came out the way `pen` draws: there at all, and
    /// no wider. An eraser or the text tool leaves nothing; a marker or a highlighter
    /// leaves too much.
    fn first_stroke_is_as_planned(&mut self, stroke: &Stroke, watch: (u32, u32), screen: &ScreenProbe, pen: &PenModel) -> Result<bool> {
        const PATIENCE: Duration = Duration::from_millis(400);

        // Two pixels well clear of either side of the stroke's middle, which this pen leaves alone
        let (width, height) = self.device_model.page_size();
        let to_pixels = |(x, y): (f32, f32)| (x * width as f32 / VIRTUAL_WIDTH as f32, y * height as f32 / VIRTUAL_HEIGHT as f32);
        let (from, to) = (to_pixels(stroke.points[0]), to_pixels(stroke.points[stroke.points.len() - 1]));
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = dx.hypot(dy).max(f32::MIN_POSITIVE);
        let clear = pen.rows as f32 / 2.0 + 2.5;
        let beside: Vec<(u32, u32)> = [clear, -clear]
            .iter()
            .map(|side| ((from.0 + to.0) / 2.0 + dy / length * side, (from.1 + to.1) / 2.0 - dx / length * side))
            .filter(|&(x, y)| x >= 0.0 && y >= 0.0)
            .map(|(x, y)| (x as u32, y as u32))
            .filter(|&(x, y)| !screen.is_inked(x, y))
            .collect();

        self.draw_stroke(stroke, &StrokeTiming::default())?;
        let drawn_at = Instant::now();
        while !screen.is_inked(watch.0, watch.1) {
            if drawn_at.elapsed() > PATIENCE {
                info!("The first stroke left no ink");
                return Ok(false);
            }
            sleep(Duration::from_millis(1));
        }
        // The whole of the stroke is on the screen a moment after its middle
        sleep(Duration::from_millis(5));
        if beside.iter().any(|&(x, y)| screen.is_inked(x, y)) {
            info!("The first stroke came out wider than the pen it was planned for");
            return Ok(false);
        }
        Ok(true)
    }

    /// Draw strokes as fast as the device takes them, watching the screen to see how far
    /// it has got, and draw again any that did not arrive.
    ///
    /// The kernel holds 1024 pen events for xochitl to read, which is about 66 strokes.
    /// xochitl draws some 2500 strokes a second, with pauses of tens of milliseconds. If
    /// the events of a 67th stroke arrive before it has caught up, the kernel throws away
    /// everything it was holding, and those strokes are never drawn. So strokes are only
    /// sent a little way ahead of the last one seen on the screen.
    ///
    /// With `check_pen`, the first stroke is drawn alone and looked at first, and if it
    /// is not what `pen` would have drawn the rest is left undrawn and this returns false.
    pub fn draw_strokes_watched(&mut self, strokes: &[Stroke], screen: &ScreenProbe, pen: &PenModel, check_pen: bool) -> Result<bool> {
        const ROUNDS: usize = 3;
        const STRAGGLERS: Duration = Duration::from_millis(300);

        let started = Instant::now();
        // Ink that is already on the page says nothing about a new stroke
        let watched: Vec<Option<(u32, u32)>> = watch_pixels(strokes, self.device_model.page_size(), pen)
            .into_iter()
            .map(|pixel| pixel.filter(|&(x, y)| !screen.is_inked(x, y)))
            .collect();
        let shows = |index: usize| watched[index].is_some_and(|(x, y)| screen.is_inked(x, y));

        info!(
            "Chose pixels to watch for {} of {} strokes in {:?}",
            watched.iter().flatten().count(),
            strokes.len(),
            started.elapsed()
        );

        let mut to_draw: Vec<usize> = (0..strokes.len()).collect();
        if check_pen {
            // A stroke that can be watched, and is long enough to have a middle
            let long_enough = |stroke: &Stroke| {
                let (first, last) = (stroke.points[0], stroke.points[stroke.points.len() - 1]);
                (first.0 - last.0).hypot(first.1 - last.1) >= 2.0
            };
            let first = (0..strokes.len()).find(|&index| watched[index].is_some() && long_enough(&strokes[index]));
            let Some((first, watch)) = first.and_then(|first| Some((first, watched[first]?))) else {
                info!("No stroke of this drawing would show whether the pen is the one it was planned for");
                return Ok(false);
            };
            if !self.first_stroke_is_as_planned(&strokes[first], watch, screen, pen)? {
                return Ok(false);
            }
            to_draw.retain(|&index| index != first);
        }
        for _ in 0..ROUNDS {
            let started = Instant::now();
            if !self.send_watched(strokes, &to_draw, &watched, &shows)? {
                // The screen is not showing what is drawn, so there is nothing to check
                return Ok(true);
            }
            info!("Drew {} strokes in {:?}", to_draw.len(), started.elapsed());
            to_draw.retain(|&index| watched[index].is_some() && !shows(index));
            // Some strokes show a little after later ones do. Drawn again they would
            // come out heavier, so give the stragglers a moment first.
            let waiting_since = Instant::now();
            while !to_draw.is_empty() && waiting_since.elapsed() < STRAGGLERS {
                sleep(Duration::from_millis(20));
                to_draw.retain(|&index| !shows(index));
            }
            if to_draw.is_empty() {
                break;
            }
            info!("Drawing {} strokes again that did not arrive", to_draw.len());
            for &index in to_draw.iter().take(12) {
                debug!(
                    "  stroke {} of {}: {:?}, watched at {:?}",
                    index,
                    strokes.len(),
                    strokes[index].points,
                    watched[index]
                );
            }
        }
        Ok(true)
    }

    /// Send the strokes listed in `order`, keeping no more than a window of them ahead of
    /// what shows on the screen. Returns false if the screen stopped showing new strokes
    /// part of the way through, after which the rest were sent at a steady pace.
    fn send_watched(&mut self, strokes: &[Stroke], order: &[usize], watched: &[Option<(u32, u32)>], shows: &dyn Fn(usize) -> bool) -> Result<bool> {
        // Strokes sent but not yet seen. At up to 16 events each they fit the kernel's
        // queue with room to spare.
        const WINDOW: usize = 48;
        const POLL: Duration = Duration::from_micros(100);
        // Strokes with nothing to watch for are given time instead, a little more than
        // xochitl takes over a stroke
        const UNWATCHED: Duration = Duration::from_micros(550);
        // The longest pause seen in xochitl was 120 milliseconds
        const STALLED: Duration = Duration::from_millis(500);
        // How many times running a window of strokes may fail to show before giving up on the screen
        const STALLS_ALLOWED: usize = 3;
        const LAST_ONES: Duration = Duration::from_millis(300);

        // order[..seen] have been read by xochitl
        let mut seen = 0;
        let mut stalls = 0;
        let started = Instant::now();
        for sent in 0..order.len() {
            if sent % 500 == 0 {
                debug!("{} strokes sent after {:?}", sent, started.elapsed());
            }
            let mut waiting_since: Option<Instant> = None;
            while sent - seen >= WINDOW {
                // Strokes are drawn in order, so the newest that shows accounts for all before it
                if let Some(newest_showing) = (seen..sent).rev().find(|&at| shows(order[at])) {
                    seen = newest_showing + 1;
                    waiting_since = None;
                    stalls = 0;
                } else if !(seen..sent).any(|at| watched[order[at]].is_some()) {
                    sleep(UNWATCHED);
                    seen += 1;
                } else if waiting_since.get_or_insert_with(Instant::now).elapsed() > STALLED {
                    // Whatever became of those strokes, xochitl has had time to read them
                    stalls += 1;
                    if stalls >= STALLS_ALLOWED {
                        info!("The screen is not showing the strokes being drawn; no longer watching it");
                        let rest: Vec<Stroke> = order[sent..].iter().map(|&index| strokes[index].clone()).collect();
                        self.draw_strokes(&rest, &StrokeTiming::unwatched())?;
                        return Ok(false);
                    }
                    debug!("None of {} strokes showed; carrying on", sent - seen);
                    seen = sent;
                    waiting_since = None;
                } else {
                    sleep(POLL);
                }
            }
            self.draw_stroke(
                &strokes[order[sent]],
                &StrokeTiming {
                    after_up: Duration::ZERO,
                    ..StrokeTiming::default()
                },
            )?;
        }

        // Give the last ones time to arrive
        if let Some(last) = order.iter().rev().find(|&&index| watched[index].is_some()) {
            let waiting_since = Instant::now();
            while !shows(*last) && waiting_since.elapsed() < LAST_ONES {
                sleep(POLL);
            }
        }
        Ok(true)
    }

    /// Draw strokes planned in virtual page coordinates, at a steady pace.
    pub fn draw_strokes(&mut self, strokes: &[Stroke], timing: &StrokeTiming) -> Result<()> {
        for stroke in strokes {
            self.draw_stroke(stroke, timing)?;
        }
        Ok(())
    }

    fn draw_stroke(&mut self, stroke: &Stroke, timing: &StrokeTiming) -> Result<()> {
        let Some(&first) = stroke.points.first() else {
            return Ok(());
        };
        self.goto_xy_with_pressure(self.virtual_point_to_input(first), stroke.pressure)?;
        sleep(timing.after_down);

        for (index, &point) in stroke.points.iter().enumerate().skip(1) {
            if index == stroke.points.len() - 1 {
                if let Some(short_of_end) = self.point_before_end(stroke) {
                    self.goto_xy(self.virtual_point_to_input(short_of_end))?;
                }
            }
            self.goto_xy(self.virtual_point_to_input(point))?;
            if timing.points_per_pause > 0 && index % timing.points_per_pause == 0 {
                sleep(timing.pause);
            }
        }

        self.pen_up()?;
        sleep(timing.after_up);
        Ok(())
    }

    /// The point a little short of the end of a stroke, for a pen that needs one there.
    fn point_before_end(&self, stroke: &Stroke) -> Option<(f32, f32)> {
        if self.tail <= 0.0 || stroke.points.len() < 2 {
            return None;
        }
        let (last, before) = (stroke.points[stroke.points.len() - 1], stroke.points[stroke.points.len() - 2]);
        // Device pixels are not quite square in virtual coordinates, but near enough for so short a step
        let tail = self.tail * VIRTUAL_WIDTH as f32 / self.device_model.page_size().0 as f32;
        let (dx, dy) = (last.0 - before.0, last.1 - before.1);
        let length = dx.hypot(dy);
        (length > tail * 2.0).then(|| (last.0 - dx / length * tail, last.1 - dy / length * tail))
    }

    /// Convert a fractional virtual page coordinate to pen digitizer coordinates.
    fn virtual_point_to_input(&self, (x, y): (f32, f32)) -> (i32, i32) {
        let (x_input, y_input) = self.page_to_input(x / VIRTUAL_WIDTH as f32, y / VIRTUAL_HEIGHT as f32);
        (x_input.round() as i32, y_input.round() as i32)
    }

    /// Convert a position on the page, as fractions of its width and height, to pen digitizer coordinates.
    fn page_to_input(&self, x_fraction: f32, y_fraction: f32) -> (f32, f32) {
        let (pen_width, pen_height) = self.device_model.pen_size();

        // The digitizer's range does not always cover exactly the page
        let (page_width, page_height) = self.device_model.page_size();
        let (area_width, area_height) = self.device_model.pen_area_size();
        let x_fraction = x_fraction * page_width as f32 / area_width as f32;
        let y_fraction = y_fraction * page_height as f32 / area_height as f32;

        match self.device_model.input_orientation() {
            InputOrientation::Portrait => (x_fraction * pen_width as f32, y_fraction * pen_height as f32),
            // The digitizer is rotated: its x axis runs up the page and its y axis across it.
            InputOrientation::Rm2 => ((1.0 - y_fraction) * pen_height as f32, x_fraction * pen_width as f32),
        }
    }

    /// Convert a virtual page coordinate to pen digitizer coordinates.
    pub fn virtual_to_input(&self, (x, y): (i32, i32)) -> (i32, i32) {
        let (x_input, y_input) = self.page_to_input(x as f32 / VIRTUAL_WIDTH as f32, y as f32 / VIRTUAL_HEIGHT as f32);
        (x_input as i32, y_input as i32)
    }

    /// Pen digitizer coordinates of pixel (x, y) in a bitmap that covers the page
    /// at `scale` pixels per virtual unit. Computed straight from the pixel position,
    /// so a scaled bitmap keeps its sub-unit precision.
    fn bitmap_to_input(&self, x: usize, y: usize, scale: u32) -> (i32, i32) {
        let (x_input, y_input) = self.page_to_input(x as f32 / (VIRTUAL_WIDTH * scale) as f32, y as f32 / (VIRTUAL_HEIGHT * scale) as f32);
        (x_input.round() as i32, y_input.round() as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pen(device_model: DeviceModel) -> Pen {
        Pen {
            device: None,
            device_model,
            tail: 0.0,
        }
    }

    #[test]
    fn paper_pro_maps_the_page_straight_onto_the_digitizer() {
        let pen = pen(DeviceModel::RemarkablePaperPro);
        // The page is 1632x2154 pixels, of which the pen covers 1620x2160 from the top-left
        assert_eq!(pen.virtual_to_input((0, 0)), (0, 0));
        assert_eq!(pen.virtual_to_input((384, 512)), (5631, 7648));
        assert_eq!(pen.virtual_to_input((768, 1024)), (11262, 15297));
    }

    #[test]
    fn rm2_digitizer_is_rotated() {
        let pen = pen(DeviceModel::Remarkable2);
        // Top-left of the page is the far end of the digitizer's x axis
        assert_eq!(pen.virtual_to_input((0, 0)), (20966, 0));
        assert_eq!(pen.virtual_to_input((768, 0)), (20966, 15725));
        assert_eq!(pen.virtual_to_input((0, 1024)), (0, 0));
        assert_eq!(pen.virtual_to_input((384, 512)), (10483, 7862));
    }

    #[test]
    fn bitmap_pixels_land_where_their_virtual_coordinates_do() {
        for device_model in [DeviceModel::Remarkable2, DeviceModel::RemarkablePaperPro, DeviceModel::RemarkablePaperProMove] {
            let pen = pen(device_model);
            // Pixel (2x, 2y) of a double-resolution bitmap is virtual point (x, y)
            for point in [(0, 0), (100, 200), (384, 512), (767, 1023)] {
                let (expected_x, expected_y) = pen.virtual_to_input(point);
                let (x, y) = pen.bitmap_to_input(point.0 as usize * 2, point.1 as usize * 2, 2);
                assert!((x - expected_x).abs() <= 1 && (y - expected_y).abs() <= 1, "{device_model:?} {point:?}");
            }
        }
    }
}
