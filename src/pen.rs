use anyhow::Result;
use evdev::EventType as EvdevEventType;
use evdev::{Device, InputEvent};
use log::info;
use resvg::usvg;
use resvg::usvg::{fontdb, Options, Tree};
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use crate::device::DeviceModel;

// Output dimensions remain the same for both devices
const VIRTUAL_WIDTH: u32 = 768;
const VIRTUAL_HEIGHT: u32 = 1024;

pub struct Pen {
    device: Option<Device>,
    device_model: DeviceModel,
}

impl Pen {
    pub fn new(no_draw: bool) -> Self {
        let device_model = DeviceModel::detect();
        info!("Pen using device model: {}", device_model.name());

        let pen_input_device = match device_model {
            DeviceModel::Remarkable2 => "/dev/input/event1",
            DeviceModel::RemarkablePaperPro => "/dev/input/event2",
            DeviceModel::Unknown => "/dev/input/event1", // Default to RM2
        };

        let device = if no_draw { None } else { Some(Device::open(pen_input_device).unwrap()) };

        Self { device, device_model }
    }

    pub fn draw_line_screen(&mut self, p1: (i32, i32), p2: (i32, i32)) -> Result<()> {
        self.draw_line(self.virtual_to_input(p1), self.virtual_to_input(p2))
    }

    pub fn draw_line(&mut self, (x1, y1): (i32, i32), (x2, y2): (i32, i32)) -> Result<()> {
        // trace!("Drawing from ({}, {}) to ({}, {})", x1, y1, x2, y2);

        // We know this is a straight line
        // So figure out the length
        // Then divide it into enough steps to only go 10 units or so
        // Start at x1, y1
        // And then for each step add the right amount to x and y

        let length = ((x2 as f32 - x1 as f32).powf(2.0) + (y2 as f32 - y1 as f32).powf(2.0)).sqrt();
        // 5.0 is the maximum distance between points
        let steps = (length / 5.0).ceil() as i32;

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
        let max_x = self.max_x_value() as f32;
        let max_y = self.max_y_value() as f32;
        let bmp_w = VIRTUAL_WIDTH as f32 * scale as f32;
        let bmp_h = VIRTUAL_HEIGHT as f32 * scale as f32;

        let mut is_pen_down = false;
        for (y, row) in bitmap.iter().enumerate() {
            for (x, &pixel) in row.iter().enumerate() {
                if pixel {
                    let ix = ((x as f32 / bmp_w) * max_x).round() as i32;
                    let iy = ((y as f32 / bmp_h) * max_y).round() as i32;
                    if !is_pen_down {
                        self.pen_down_at((ix, iy))?;
                        is_pen_down = true;
                        sleep(Duration::from_millis(1));
                    }
                    self.goto_xy((ix, iy))?;
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

    // fn draw_dot(device: &mut Device, (x, y): (i32, i32)) -> Result<()> {
    //     // trace!("Drawing at ({}, {})", x, y);
    //     goto_xy(device, (x, y))?;
    //     pen_down(device)?;
    //
    //     // Wiggle a little bit
    //     for n in 0..2 {
    //         goto_xy(device, (x + n, y + n))?;
    //     }
    //
    //     pen_up(device)?;
    //
    //     // sleep for 5ms
    //     thread::sleep(time::Duration::from_millis(1));
    //
    //     Ok(())
    // }

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

    pub fn max_x_value(&self) -> i32 {
        match self.device_model {
            DeviceModel::Remarkable2 => 15725,
            DeviceModel::RemarkablePaperPro => 11180,
            DeviceModel::Unknown => 15725, // Default to RM2
        }
    }

    pub fn max_y_value(&self) -> i32 {
        match self.device_model {
            DeviceModel::Remarkable2 => 20966,
            DeviceModel::RemarkablePaperPro => 15340,
            DeviceModel::Unknown => 20966, // Default to RM2
        }
    }

    pub fn virtual_to_input_pub(&self, point: (i32, i32)) -> (i32, i32) {
        self.virtual_to_input(point)
    }

    // Draw a single segment from prev to (px,py), emitting interpolated goto_xy events.
    // Returns updated step_count.
    fn draw_segment(
        &mut self,
        prev: (f32, f32),
        (px, py): (f32, f32),
        step_count: &mut usize,
        max_step: f32,
    ) -> Result<()> {
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
        let max_x = self.max_x_value() as f32;
        let max_y = self.max_y_value() as f32;
        let bmp_w = VIRTUAL_WIDTH as f32 * scale as f32;
        let bmp_h = VIRTUAL_HEIGHT as f32 * scale as f32;

        let mut is_pen_down = false;
        for (y, row) in bitmap.iter().enumerate() {
            let iy = ((y as f32 / bmp_h) * max_y).round() as i32;
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
            let ordered_runs: Vec<(usize, usize)> = if go_left {
                runs.iter().rev().cloned().collect()
            } else {
                runs.clone()
            };

            for (x_start, x_end) in ordered_runs {
                // For L→R draw start→end; for R→L draw end→start
                let (draw_from, draw_to) = if go_left {
                    (x_end, x_start)
                } else {
                    (x_start, x_end)
                };

                let ix_from = ((draw_from as f32 / bmp_w) * max_x).round() as i32;
                let ix_to = ((draw_to as f32 / bmp_w) * max_x).round() as i32;

                if is_pen_down {
                    self.pen_up()?;
                    is_pen_down = false;
                    sleep(Duration::from_millis(1));
                }
                self.pen_down_at((ix_from, iy))?;
                is_pen_down = true;
                sleep(Duration::from_millis(1));
                self.goto_xy((ix_to, iy))?;
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
        let max_x = self.max_x_value() as f32;
        let max_y = self.max_y_value() as f32;
        let bmp_w = VIRTUAL_WIDTH as f32 * scale as f32;
        let bmp_h = VIRTUAL_HEIGHT as f32 * scale as f32;

        if bitmap.is_empty() {
            return Ok(());
        }
        let rows = bitmap.len();
        let cols = bitmap[0].len();

        for x in 0..cols {
            let ix = ((x as f32 / bmp_w) * max_x).round() as i32;

            let mut is_pen_down = false;
            let mut run_start: Option<usize> = None;

            for y in 0..rows {
                let pixel = bitmap[y][x];
                let iy = ((y as f32 / bmp_h) * max_y).round() as i32;

                match (pixel, run_start) {
                    (true, None) => {
                        run_start = Some(y);
                        self.pen_down_at((ix, iy))?;
                        is_pen_down = true;
                        sleep(Duration::from_millis(1));
                        self.goto_xy((ix, iy))?;
                    }
                    (true, Some(_)) => {
                        self.goto_xy((ix, iy))?;
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
        let max_x = self.max_x_value() as f32;
        let max_y = self.max_y_value() as f32;
        let bmp_w = VIRTUAL_WIDTH as f32 * scale as f32;
        let bmp_h = VIRTUAL_HEIGHT as f32 * scale as f32;

        for (y, row) in alpha_bitmap.iter().enumerate() {
            let iy = ((y as f32 / bmp_h) * max_y).round() as i32;

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

                let ix = ((x as f32 / bmp_w) * max_x).round() as i32;
                let pressure = (alpha as f32 / 255.0 * 2630.0).round() as i32;

                if !is_pen_down || pressure != last_pressure {
                    if is_pen_down {
                        self.pen_up()?;
                        sleep(Duration::from_millis(1));
                    }
                    self.goto_xy_with_pressure((ix, iy), pressure)?;
                    is_pen_down = true;
                    last_pressure = pressure;
                } else {
                    self.goto_xy((ix, iy))?;
                }
            }

            if is_pen_down {
                self.pen_up()?;
                is_pen_down = false;
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
                InputEvent::new(EvdevEventType::KEY.0, 330, 1),           // BTN_TOUCH
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 24, pressure), // ABS_PRESSURE
                InputEvent::new(EvdevEventType::ABSOLUTE.0, 25, 0),       // ABS_DISTANCE = 0
                InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0), // SYN_REPORT
            ])?;
        }
        Ok(())
    }

    /// Draw SVG using path tracing via svg2polylines for smooth continuous strokes.
    /// Text is converted to glyph paths by usvg before tracing.
    pub fn draw_svg_paths(&mut self, svg_data: &str) -> Result<()> {
        // Use usvg to parse and flatten text → paths
        let mut opt = Options::default();
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        opt.fontdb = Arc::new(db);
        let tree = Tree::from_str(svg_data, &opt)?;
        let write_opt = usvg::WriteOptions::default(); // preserve_text=false → converts text to paths
        let flattened_svg = tree.to_string(&write_opt);

        let polylines = svg2polylines::parse(&flattened_svg, 0.5, true)
            .map_err(|e| anyhow::anyhow!("svg2polylines error: {}", e))?;
        info!("draw_svg_paths: {} polylines", polylines.len());
        self.draw_polylines(&polylines)
    }

    /// Draw a single continuous stroke from a list of virtual-space points:
    /// pen down at the first point, interpolated moves through the rest,
    /// pen up at the end. Used by the cursive handwriting renderer, which
    /// already computes one stroke per pen-down run (letters are pre-joined
    /// at layout time), so no corner detection is needed here.
    pub fn draw_stroke_virtual(&mut self, points: &[(f32, f32)]) -> Result<()> {
        if points.len() < 2 {
            return Ok(());
        }
        const MAX_STEP: f32 = 1.0;
        let mut step_count = 0usize;

        let start = self.virtual_to_input((points[0].0 as i32, points[0].1 as i32));
        self.pen_up()?;
        sleep(Duration::from_millis(2));
        self.pen_down_at(start)?;
        sleep(Duration::from_millis(2));

        let mut prev = points[0];
        for &pt in &points[1..] {
            self.draw_segment(prev, pt, &mut step_count, MAX_STEP)?;
            prev = pt;
        }

        self.pen_up()?;
        sleep(Duration::from_millis(2));
        Ok(())
    }

    /// Draw paths given as lists of (x, y) virtual coordinates.
    /// No corner detection — skeleton paths are already single-pixel-wide and smooth.
    fn draw_virtual_paths(&mut self, paths: &[Vec<(f32, f32)>]) -> Result<()> {
        for path in paths {
            self.draw_stroke_virtual(path)?;
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
        info!(
            "draw_svg_centerline: rasterized {}×{} bitmap",
            VIRTUAL_WIDTH * scale,
            VIRTUAL_HEIGHT * scale
        );

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
        let polylines = svg2polylines::parse(svg_data, 0.5, true)
            .map_err(|e| anyhow::anyhow!("svg2polylines error: {}", e))?;
        info!("draw_svg_paths_raw: {} polylines", polylines.len());
        self.draw_polylines(&polylines)
    }

    fn virtual_to_input(&self, (x, y): (i32, i32)) -> (i32, i32) {
        // Swap and normalize the coordinates
        let x_normalized = x as f32 / VIRTUAL_WIDTH as f32;
        let y_normalized = y as f32 / VIRTUAL_HEIGHT as f32;

        match self.device_model {
            DeviceModel::RemarkablePaperPro => {
                let x_input = (x_normalized * self.max_x_value() as f32) as i32;
                let y_input = (y_normalized * self.max_y_value() as f32) as i32;
                (x_input, y_input)
            }
            _ => {
                let x_input = ((1.0 - y_normalized) * self.max_y_value() as f32) as i32;
                let y_input = (x_normalized * self.max_x_value() as f32) as i32;
                (x_input, y_input)
            }
        }
    }
}
