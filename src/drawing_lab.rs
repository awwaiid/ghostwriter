//! Tools for measuring how well a drawing came out, used by the experiment binary.
//!
//! The loop is: plan strokes for an SVG, play them on the device, capture the screen at
//! full resolution, then score the capture against a rendering of the same SVG.

use anyhow::{Context, Result};
use image::{GrayImage, Rgb, RgbImage};

use crate::device::{VIRTUAL_HEIGHT, VIRTUAL_WIDTH};
use crate::pen::Stroke;
use crate::util::svg_to_alpha_bitmap;

/// Write strokes as text: one stroke per line, its pressure followed by its x,y points.
pub fn write_strokes(strokes: &[Stroke]) -> String {
    let mut text = String::new();
    for stroke in strokes {
        text.push_str(&stroke.pressure.to_string());
        for (x, y) in &stroke.points {
            text.push_str(&format!(" {x:.2},{y:.2}"));
        }
        text.push('\n');
    }
    text
}

/// Read strokes written by `write_strokes`. Blank lines and lines starting with # are skipped.
pub fn read_strokes(text: &str) -> Result<Vec<Stroke>> {
    let mut strokes = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let mut fields = line.split_whitespace();
        let pressure = fields.next().context("Empty stroke")?.parse().context("Invalid pressure")?;
        let points = fields
            .map(|point| {
                let (x, y) = point.split_once(',').with_context(|| format!("Invalid point '{point}'"))?;
                Ok((x.parse()?, y.parse()?))
            })
            .collect::<Result<Vec<(f32, f32)>>>()?;
        strokes.push(Stroke { points, pressure });
    }
    Ok(strokes)
}

/// How closely the ink on a screen capture matches the SVG that was drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    /// Share of the drawn ink that is within the tolerance of where ink belongs.
    pub precision: f32,
    /// Share of the wanted ink that has drawn ink within the tolerance.
    pub recall: f32,
    /// Overlap of drawn and wanted ink, pixel for pixel, as a share of their union.
    pub overlap: f32,
    /// Drawn ink area divided by wanted ink area: above 1 is too heavy, below is too light.
    pub ink_ratio: f32,
    /// The shift of the drawn ink, in capture pixels, that would give the best overlap.
    pub best_shift: (i32, i32),
    /// The overlap at that shift.
    pub best_shift_overlap: f32,
}

impl Score {
    /// Precision and recall combined: 1.0 is a perfect drawing.
    pub fn f1(&self) -> f32 {
        if self.precision + self.recall == 0.0 {
            0.0
        } else {
            2.0 * self.precision * self.recall / (self.precision + self.recall)
        }
    }
}

/// A grid of true/false values the size of a screen capture.
pub struct Mask {
    pub width: u32,
    pub height: u32,
    cells: Vec<bool>,
}

impl Mask {
    fn get(&self, x: i64, y: i64) -> bool {
        x >= 0 && y >= 0 && x < self.width as i64 && y < self.height as i64 && self.cells[(y * self.width as i64 + x) as usize]
    }

    fn count(&self) -> usize {
        self.cells.iter().filter(|&&cell| cell).count()
    }

    /// For every cell, the distance (in cells, diagonals counting 1) to the nearest true cell.
    fn distances(&self) -> Vec<u32> {
        let (width, height) = (self.width as usize, self.height as usize);
        let far = (width + height) as u32;
        let mut distance: Vec<u32> = self.cells.iter().map(|&cell| if cell { 0 } else { far }).collect();

        for y in 0..height {
            for x in 0..width {
                let mut best = distance[y * width + x];
                if x > 0 {
                    best = best.min(distance[y * width + x - 1] + 1);
                }
                if y > 0 {
                    best = best.min(distance[(y - 1) * width + x] + 1);
                    if x > 0 {
                        best = best.min(distance[(y - 1) * width + x - 1] + 1);
                    }
                    if x + 1 < width {
                        best = best.min(distance[(y - 1) * width + x + 1] + 1);
                    }
                }
                distance[y * width + x] = best;
            }
        }
        for y in (0..height).rev() {
            for x in (0..width).rev() {
                let mut best = distance[y * width + x];
                if x + 1 < width {
                    best = best.min(distance[y * width + x + 1] + 1);
                }
                if y + 1 < height {
                    best = best.min(distance[(y + 1) * width + x] + 1);
                    if x + 1 < width {
                        best = best.min(distance[(y + 1) * width + x + 1] + 1);
                    }
                    if x > 0 {
                        best = best.min(distance[(y + 1) * width + x - 1] + 1);
                    }
                }
                distance[y * width + x] = best;
            }
        }
        distance
    }
}

/// Whether a capture pixel is part of the page being scored. The palette toggle in the
/// top-left corner and the scroll bar down the right edge are not.
fn on_page(x: u32, y: u32, width: u32, height: u32) -> bool {
    let virtual_x = x as f32 * VIRTUAL_WIDTH as f32 / width as f32;
    let virtual_y = y as f32 * VIRTUAL_HEIGHT as f32 / height as f32;
    !(virtual_x < 60.0 && virtual_y < 60.0) && virtual_x < 722.0
}

/// The dark pixels of a screen capture, or of an image from `render_ink`.
pub fn drawn_ink(capture: &GrayImage) -> Mask {
    let (width, height) = capture.dimensions();
    let cells = capture
        .enumerate_pixels()
        .map(|(x, y, pixel)| pixel[0] < 128 && on_page(x, y, width, height))
        .collect();
    Mask { width, height, cells }
}

/// Where an SVG puts ink, at the size of a screen capture.
pub fn wanted_ink(svg_data: &str, width: u32, height: u32) -> Result<Mask> {
    let coverage = svg_to_alpha_bitmap(svg_data, width, height)?;
    let cells = coverage
        .iter()
        .enumerate()
        .flat_map(|(y, row)| {
            row.iter()
                .enumerate()
                .map(move |(x, &alpha)| alpha >= 128 && on_page(x as u32, y as u32, width, height))
        })
        .collect();
    Ok(Mask { width, height, cells })
}

/// An SVG rendered the way `wanted_ink` sees it: black where it puts ink, white elsewhere.
/// Rendering on the device and scoring elsewhere keeps the device's fonts in the comparison.
pub fn render_ink(svg_data: &str, width: u32, height: u32) -> Result<GrayImage> {
    let coverage = svg_to_alpha_bitmap(svg_data, width, height)?;
    Ok(GrayImage::from_fn(width, height, |x, y| {
        image::Luma([if coverage[y as usize][x as usize] >= 128 { 0 } else { 255 }])
    }))
}

fn overlap_at_shift(drawn: &Mask, wanted: &Mask, shift: (i32, i32)) -> f32 {
    let mut both = 0usize;
    for y in 0..drawn.height as i64 {
        for x in 0..drawn.width as i64 {
            if drawn.cells[(y * drawn.width as i64 + x) as usize] && wanted.get(x + shift.0 as i64, y + shift.1 as i64) {
                both += 1;
            }
        }
    }
    let either = drawn.count() + wanted.count() - both;
    if either == 0 {
        1.0
    } else {
        both as f32 / either as f32
    }
}

/// Score a screen capture against the ink that was wanted on it.
/// `tolerance` is how far, in capture pixels, ink may be from where it belongs.
pub fn score(wanted: &Mask, capture: &GrayImage, tolerance: u32) -> Score {
    let drawn = drawn_ink(capture);

    let distance_to_wanted = wanted.distances();
    let distance_to_drawn = drawn.distances();
    let drawn_near_wanted = drawn
        .cells
        .iter()
        .zip(&distance_to_wanted)
        .filter(|(&ink, &distance)| ink && distance <= tolerance)
        .count();
    let wanted_near_drawn = wanted
        .cells
        .iter()
        .zip(&distance_to_drawn)
        .filter(|(&ink, &distance)| ink && distance <= tolerance)
        .count();

    let (drawn_count, wanted_count) = (drawn.count().max(1), wanted.count().max(1));

    // Look for a consistent offset between the drawing and what was wanted
    let mut best_shift = (0, 0);
    let mut best_shift_overlap = overlap_at_shift(&drawn, wanted, (0, 0));
    let overlap = best_shift_overlap;
    for dy in -8..=8 {
        for dx in -8..=8 {
            let shifted = overlap_at_shift(&drawn, wanted, (dx, dy));
            if shifted > best_shift_overlap {
                best_shift_overlap = shifted;
                // Moving the drawing by -shift lines it up, so report where it sits now
                best_shift = (-dx, -dy);
            }
        }
    }

    Score {
        precision: drawn_near_wanted as f32 / drawn_count as f32,
        recall: wanted_near_drawn as f32 / wanted_count as f32,
        overlap,
        ink_ratio: drawn.count() as f32 / wanted_count as f32,
        best_shift,
        best_shift_overlap,
    }
}

/// A picture of how a capture differs from what was wanted: black where both have ink,
/// red where only the drawing does (stray ink), blue where only the wanted ink is (missing ink).
pub fn difference_image(wanted: &Mask, capture: &GrayImage) -> RgbImage {
    let drawn = drawn_ink(capture);

    let mut image = RgbImage::from_pixel(capture.width(), capture.height(), Rgb([255, 255, 255]));
    for (index, (&is_drawn, &is_wanted)) in drawn.cells.iter().zip(&wanted.cells).enumerate() {
        let colour = match (is_drawn, is_wanted) {
            (true, true) => Rgb([0, 0, 0]),
            (true, false) => Rgb([230, 30, 30]),
            (false, true) => Rgb([30, 80, 230]),
            (false, false) => continue,
        };
        image.put_pixel(index as u32 % capture.width(), index as u32 / capture.width(), colour);
    }
    image
}

/// Plan an SVG as stipples: the page is divided into cells the size of the smallest mark
/// the pen can make (3 pixels by 2), and each cell that is mostly ink gets one.
/// This is for comparing against `stroke_plan::plan_strokes`, which merges such marks into bars.
pub fn plan_stipple(svg_data: &str, page_size: (u32, u32)) -> Result<Vec<Stroke>> {
    const CELL: (usize, usize) = (3, 2);
    let coverage = svg_to_alpha_bitmap(svg_data, page_size.0, page_size.1)?;
    let to_virtual = |x: f32, y: f32| (x * VIRTUAL_WIDTH as f32 / page_size.0 as f32, y * VIRTUAL_HEIGHT as f32 / page_size.1 as f32);

    let mut strokes = Vec::new();
    for top in (0..page_size.1 as usize - CELL.1 + 1).step_by(CELL.1) {
        for left in (0..page_size.0 as usize - CELL.0 + 1).step_by(CELL.0) {
            let inked = (0..CELL.1)
                .flat_map(|dy| (0..CELL.0).map(move |dx| (dx, dy)))
                .filter(|&(dx, dy)| coverage[top + dy][left + dx] >= 128)
                .count();
            if inked * 2 > CELL.0 * CELL.1 {
                // A one pixel stroke along the middle of the cell inks all of it
                let middle = (top + 1) as f32;
                strokes.push(Stroke {
                    points: vec![to_virtual(left as f32 + 1.0, middle), to_virtual(left as f32 + 2.0, middle)],
                    pressure: crate::pen::FULL_PRESSURE,
                });
            }
        }
    }
    Ok(strokes)
}

/// The (start, length) of each run of dark values.
pub fn dark_runs(values: impl Iterator<Item = u8>) -> Vec<(u32, u32)> {
    let mut runs = Vec::new();
    let mut start = None;
    let mut length = 0;
    for (index, value) in values.enumerate() {
        length = index as u32 + 1;
        match (value < 128, start) {
            (true, None) => start = Some(index as u32),
            (false, Some(first)) => {
                runs.push((first, index as u32 - first));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(first) = start {
        runs.push((first, length - first));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Luma;

    const SQUARE: &str =
        r#"<svg width="768" height="1024" xmlns="http://www.w3.org/2000/svg"><rect x="200" y="300" width="100" height="100" fill="black"/></svg>"#;

    /// A white capture the size of the virtual page, with a black square on it.
    fn capture_with_square(left: u32, top: u32) -> GrayImage {
        let mut capture = GrayImage::from_pixel(768, 1024, Luma([255]));
        for y in top..top + 100 {
            for x in left..left + 100 {
                capture.put_pixel(x, y, Luma([0]));
            }
        }
        capture
    }

    #[test]
    fn strokes_survive_a_round_trip_through_text() {
        let strokes = vec![
            Stroke {
                points: vec![(1.5, 2.25), (100.0, 200.0)],
                pressure: 2630,
            },
            Stroke {
                points: vec![(7.0, 8.0)],
                pressure: 100,
            },
        ];

        assert_eq!(read_strokes(&write_strokes(&strokes)).unwrap(), strokes);
    }

    #[test]
    fn a_drawing_in_the_right_place_scores_perfectly() {
        let score = score(&wanted_ink(SQUARE, 768, 1024).unwrap(), &capture_with_square(200, 300), 2);

        assert!(score.f1() > 0.999, "{score:?}");
        assert!(score.overlap > 0.99, "{score:?}");
        assert_eq!(score.best_shift, (0, 0));
    }

    #[test]
    fn a_shifted_drawing_loses_overlap_and_reports_its_offset() {
        let score = score(&wanted_ink(SQUARE, 768, 1024).unwrap(), &capture_with_square(205, 297), 2);

        assert!(score.overlap < 0.95, "{score:?}");
        assert_eq!(score.best_shift, (5, -3));
        assert!(score.best_shift_overlap > 0.99, "{score:?}");
    }

    #[test]
    fn dark_runs_reports_each_stretch() {
        assert_eq!(dark_runs([255, 0, 0, 255, 10, 255].into_iter()), vec![(1, 2), (4, 1)]);
        assert_eq!(dark_runs([0, 0].into_iter()), vec![(0, 2)]);
    }
}
