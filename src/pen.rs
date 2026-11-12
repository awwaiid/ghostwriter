use anyhow::{Context, Result};
use evdev::EventType as EvdevEventType;
use evdev::{Device, InputEvent};
use log::info;
use resvg::usvg;
use std::thread::sleep;
use std::time::Duration;

use crate::device::{DeviceModel, InputOrientation, VIRTUAL_HEIGHT, VIRTUAL_WIDTH};
use crate::util::parse_svg;

pub struct Pen {
    device: Option<Device>,
    device_model: DeviceModel,
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

        Ok(Self { device, device_model })
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
        Pen { device: None, device_model }
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
