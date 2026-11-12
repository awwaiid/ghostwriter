//! Turn an SVG into pen strokes.
//!
//! The plan is made on the device's own pixel grid, for a pen that behaves like the
//! Paper Pro's thin fineliner (measured on firmware 3.29):
//!
//! - A stroke between two points is a straight line 2 pixels wide with round ends: it
//!   inks every pixel whose centre is within a pixel of it. (Nearly. Seen from the pen
//!   as it travels, the left side of the line starts and stops a pixel earlier than the
//!   right side, so one corner pixel at each end comes out faint.)
//! - Centred on the boundary between two pixel rows or columns the line is crisp.
//!   Anywhere else it is anti-aliased.
//! - A stroke has to move to leave ink; a single point draws nothing.
//! - A stroke with more than two points is smoothed by the device, which rounds its
//!   corners and pulls it off course, so every stroke here is just two points.
//! - The device draws about 2500 strokes a second, however few events each is made of.
//!   Sent faster than it takes them, they are lost a queue-full at a time (see
//!   `Pen::draw_strokes_watched`).
//!
//! So shapes are filled with crisp bars, each inking two rows or two columns of pixels,
//! which reproduces the rendered SVG pixel for pixel. Lines too thin for a bar are drawn
//! along their middle, the way a person would draw them.

use anyhow::Result;

use crate::device::{VIRTUAL_HEIGHT, VIRTUAL_WIDTH};
use crate::pen::{Stroke, FULL_PRESSURE};
use crate::skeleton;
use crate::util::svg_to_alpha_bitmap;

/// How far from its centre line the pen inks, in pixels.
pub const PEN_RADIUS: f32 = 1.0;

/// The shortest run of pixels a bar can cover: its two round ends plus a pixel of travel.
const MIN_BAR: usize = 3;

/// How far a bar runs on into ink that other bars will cover.
const BAR_OVERLAP: usize = 2;

/// The shortest stroke worth sending, in pixels. One that does not move leaves no ink.
const MIN_STROKE: f32 = 1.0;

/// Thin features smaller than this many pixels are left out: they are below what the pen can draw.
const MIN_THIN_FEATURE: usize = 6;

/// A point in device pixels. Pixel (i, j) covers i..i+1 by j..j+1.
type Point = (f32, f32);

/// A straight stroke between two points.
type Segment = (Point, Point);

/// A grid of true/false values, one per device pixel.
#[derive(Clone)]
struct Grid {
    width: usize,
    height: usize,
    cells: Vec<bool>,
}

impl Grid {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            cells: vec![false; width * height],
        }
    }

    fn get(&self, x: usize, y: usize) -> bool {
        self.cells[y * self.width + x]
    }

    /// Like `get`, but false outside the grid.
    fn at(&self, x: isize, y: isize) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height && self.get(x as usize, y as usize)
    }

    fn set(&mut self, x: usize, y: usize, value: bool) {
        self.cells[y * self.width + x] = value;
    }

    /// The same grid with rows and columns swapped.
    fn transposed(&self) -> Grid {
        let mut transposed = Grid::new(self.height, self.width);
        for y in 0..self.height {
            for x in 0..self.width {
                transposed.cells[x * self.height + y] = self.cells[y * self.width + x];
            }
        }
        transposed
    }

    /// Cells that are true here and false in `other`.
    fn minus(&self, other: &Grid) -> Grid {
        let cells = self.cells.iter().zip(&other.cells).map(|(&a, &b)| a && !b).collect();
        Grid { cells, ..*self }
    }

    /// The true cells grown by one cell in every direction.
    fn grown(&self) -> Grid {
        let mut grown = Grid::new(self.width, self.height);
        for y in 0..self.height as isize {
            for x in 0..self.width as isize {
                let near = (-1..=1).any(|dy| (-1..=1).any(|dx| self.at(x + dx, y + dy)));
                grown.set(x as usize, y as usize, near);
            }
        }
        grown
    }

    /// For each cell, the length of the horizontal run of true cells it is in (0 if it is false).
    fn run_lengths(&self) -> Vec<u32> {
        let mut lengths = vec![0; self.cells.len()];
        for y in 0..self.height {
            let mut x = 0;
            while x < self.width {
                if !self.get(x, y) {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < self.width && self.get(x, y) {
                    x += 1;
                }
                lengths[y * self.width + start..y * self.width + x].fill((x - start) as u32);
            }
        }
        lengths
    }

    /// The groups of true cells that touch each other, diagonals included.
    fn components(&self) -> Vec<Vec<(usize, usize)>> {
        let mut seen = vec![false; self.cells.len()];
        let mut components = Vec::new();
        for start_y in 0..self.height {
            for start_x in 0..self.width {
                if !self.get(start_x, start_y) || seen[start_y * self.width + start_x] {
                    continue;
                }
                let mut component = Vec::new();
                let mut queue = vec![(start_x, start_y)];
                seen[start_y * self.width + start_x] = true;
                while let Some((x, y)) = queue.pop() {
                    component.push((x, y));
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            let (nx, ny) = (x as isize + dx, y as isize + dy);
                            if self.at(nx, ny) && !seen[ny as usize * self.width + nx as usize] {
                                seen[ny as usize * self.width + nx as usize] = true;
                                queue.push((nx as usize, ny as usize));
                            }
                        }
                    }
                }
                components.push(component);
            }
        }
        components
    }
}

/// Plan the strokes that draw an SVG on a page of `page_size` device pixels.
/// The strokes are in virtual page coordinates, ready for `Pen::draw_strokes`.
pub fn plan_strokes(svg_data: &str, page_size: (u32, u32)) -> Result<Vec<Stroke>> {
    let (width, height) = (page_size.0 as usize, page_size.1 as usize);
    let coverage = svg_to_alpha_bitmap(svg_data, page_size.0, page_size.1)?;
    let ink = Grid {
        width,
        height,
        cells: coverage.iter().flat_map(|row| row.iter().map(|&alpha| alpha >= 128)).collect(),
    };

    let to_virtual = |(x, y): Point| (x * VIRTUAL_WIDTH as f32 / width as f32, y * VIRTUAL_HEIGHT as f32 / height as f32);
    Ok(plan_ink(&ink)
        .into_iter()
        .map(|(from, to)| Stroke {
            points: vec![to_virtual(from), to_virtual(to)],
            pressure: FULL_PRESSURE,
        })
        .collect())
}

/// Plan the strokes that ink the true cells of a grid, in pixels.
fn plan_ink(ink: &Grid) -> Vec<Segment> {
    let mut strokes = Vec::new();

    // Bars run along whichever direction gives the longer bar at each pixel...
    let across = ink.run_lengths();
    let sideways = ink.transposed();
    let down = sideways.run_lengths();
    let mut prefers_across = Grid::new(ink.width, ink.height);
    for y in 0..ink.height {
        for x in 0..ink.width {
            prefers_across.set(x, y, ink.get(x, y) && across[y * ink.width + x] >= down[x * ink.height + y]);
        }
    }
    let mut covered = Grid::new(ink.width, ink.height);
    strokes.extend(bars(ink, &prefers_across, &mut covered));

    // ...and then the other direction, and the first again, mop up
    let mut covered_sideways = covered.transposed();
    let upright = bars(&sideways, &sideways, &mut covered_sideways);
    strokes.extend(upright.into_iter().map(|((x1, y1), (x2, y2))| ((y1, x1), (y2, x2))));
    covered = covered_sideways.transposed();

    strokes.extend(bars(ink, ink, &mut covered));

    // What is still bare is too thin for a bar either way: lines no wider than the pen,
    // which are drawn along their middle. Slivers along the edge of something already
    // drawn, and specks, are left out.
    let mut thin = Grid::new(ink.width, ink.height);
    for component in ink.minus(&covered).components() {
        let beside_drawn = component
            .iter()
            .filter(|&&(x, y)| {
                [(0, -1), (0, 1), (-1, 0), (1, 0)]
                    .iter()
                    .any(|&(dx, dy)| covered.at(x as isize + dx, y as isize + dy))
            })
            .count();
        if component.len() >= MIN_THIN_FEATURE && beside_drawn * 2 < component.len() {
            for (x, y) in component {
                thin.set(x, y, true);
            }
        }
    }
    strokes.extend(centre_lines(&thin).iter().flat_map(|path| segments(path)));

    strokes
}

/// Cover the cells of `wanted` that are not yet `covered` with horizontal bars.
///
/// A bar is a stroke along the boundary between two rows, inking both. It is only put
/// where both rows are inside `ink`, so it never marks outside the shape, and it stops a
/// pixel short of each end of the run because the pen's round end reaches the rest.
fn bars(ink: &Grid, wanted: &Grid, covered: &mut Grid) -> Vec<Segment> {
    let mut strokes = Vec::new();

    // Going down the page, each boundary is the last chance to ink the row above it,
    // so that is where a bar goes. The row below is inked along with it, and only
    // needs a bar of its own if the row after it cannot share one.
    for boundary in 1..ink.height {
        let (above, below) = (boundary - 1, boundary);
        let both = |x: usize| ink.get(x, above) && ink.get(x, below);
        let last_chance_below = |x: usize| below + 1 >= ink.height || !ink.get(x, below + 1);

        let mut x = 0;
        while x < ink.width {
            if !both(x) {
                x += 1;
                continue;
            }
            let run_start = x;
            while x < ink.width && both(x) {
                x += 1;
            }
            let run_end = x;
            if run_end - run_start < MIN_BAR {
                continue;
            }

            let needs_ink = |x: usize| {
                let above_needs = wanted.get(x, above) && !covered.get(x, above);
                let below_needs = wanted.get(x, below) && !covered.get(x, below);
                above_needs || (below_needs && last_chance_below(x))
            };
            for (start, end) in spans_needing_ink(run_start, run_end, needs_ink) {
                strokes.push(((start as f32 + PEN_RADIUS, boundary as f32), (end as f32 - PEN_RADIUS, boundary as f32)));
                for x in start..end {
                    covered.set(x, above, true);
                    covered.set(x, below, true);
                }
            }
        }
    }

    strokes
}

/// The stretches of a run that still need ink, each widened to at least `MIN_BAR`
/// and merged with its neighbour when they nearly touch.
fn spans_needing_ink(run_start: usize, run_end: usize, needs_ink: impl Fn(usize) -> bool) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for x in (run_start..run_end).filter(|&x| needs_ink(x)) {
        match spans.last_mut() {
            Some(last) if x <= last.1 + MIN_BAR => last.1 = x + 1,
            _ => spans.push((x, x + 1)),
        }
    }

    for span in &mut spans {
        // Where the ink carries on past the span, so does the bar: its round end is
        // not quite black, and would show as a seam against the bars it meets there
        span.0 = span.0.saturating_sub(BAR_OVERLAP).max(run_start);
        span.1 = (span.1 + BAR_OVERLAP).min(run_end);

        while span.1 - span.0 < MIN_BAR {
            if span.0 > run_start {
                span.0 -= 1;
            }
            if span.1 - span.0 < MIN_BAR && span.1 < run_end {
                span.1 += 1;
            }
        }
    }
    spans
}

/// Trace thin shapes along their middle, as paths of pixel points.
fn centre_lines(thin: &Grid) -> Vec<Vec<Point>> {
    // Work on just the part of the page that has anything in it
    let cells = (0..thin.height)
        .flat_map(|y| (0..thin.width).map(move |x| (x, y)))
        .filter(|&(x, y)| thin.get(x, y));
    let Some((left, top, right, bottom)) = cells.fold(None, |bounds: Option<(usize, usize, usize, usize)>, (x, y)| match bounds {
        None => Some((x, y, x, y)),
        Some((left, top, right, bottom)) => Some((left.min(x), top.min(y), right.max(x), bottom.max(y))),
    }) else {
        return Vec::new();
    };

    // Fatten the shapes by a cell first, so that the line left by thinning runs down
    // their middle rather than along one side. Two empty cells all round leave room.
    const MARGIN: usize = 2;
    let fattened = thin.grown();
    let mut rows: Vec<Vec<bool>> = (0..bottom - top + 1 + 2 * MARGIN)
        .map(|y| {
            (0..right - left + 1 + 2 * MARGIN)
                .map(|x| fattened.at((x + left) as isize - MARGIN as isize, (y + top) as isize - MARGIN as isize))
                .collect()
        })
        .collect();
    skeleton::thin_guo_hall(&mut rows);

    skeleton::trace_skeleton(&rows)
        .into_iter()
        .map(|path| {
            // Back to page pixels, at the centre of each cell
            let centres: Vec<Point> = path
                .iter()
                .map(|&(x, y)| (x + left as f32 - MARGIN as f32 + 0.5, y + top as f32 - MARGIN as f32 + 0.5))
                .collect();
            simplify(&skeleton::smooth_path(&centres, 3), 0.5)
        })
        .collect()
}

/// The straight strokes that draw a path.
/// Steps too short to leave ink are dropped; the round ends of their neighbours cover them.
fn segments(path: &[Point]) -> Vec<Segment> {
    let length = |(from, to): &Segment| ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt();

    let drawable: Vec<Segment> = path
        .windows(2)
        .map(|pair| (pair[0], pair[1]))
        .filter(|step| length(step) >= MIN_STROKE)
        .collect();
    if drawable.is_empty() && !path.is_empty() {
        // Too small to draw as it is: the shortest stroke that leaves ink, across its middle
        let (x, y) = path[path.len() / 2];
        return vec![((x - MIN_STROKE / 2.0, y), (x + MIN_STROKE / 2.0, y))];
    }
    drawable
}

/// Drop the points of a path that are within `tolerance` of the line between their neighbours.
fn simplify(path: &[Point], tolerance: f32) -> Vec<Point> {
    if path.len() < 3 {
        return path.to_vec();
    }

    let (first, last) = (path[0], path[path.len() - 1]);
    let (dx, dy) = (last.0 - first.0, last.1 - first.1);
    let length = (dx * dx + dy * dy).sqrt();
    let distance = |point: Point| {
        if length == 0.0 {
            ((point.0 - first.0).powi(2) + (point.1 - first.1).powi(2)).sqrt()
        } else {
            ((point.0 - first.0) * dy - (point.1 - first.1) * dx).abs() / length
        }
    };

    let (furthest, furthest_distance) = path[1..path.len() - 1]
        .iter()
        .enumerate()
        .map(|(index, &point)| (index + 1, distance(point)))
        .fold((0, 0.0), |best, candidate| if candidate.1 > best.1 { candidate } else { best });

    if furthest_distance <= tolerance {
        vec![first, last]
    } else {
        let mut simplified = simplify(&path[..=furthest], tolerance);
        simplified.pop();
        simplified.extend(simplify(&path[furthest..], tolerance));
        simplified
    }
}

/// For each stroke, a page pixel to watch for its ink arriving, in device pixels.
///
/// Where it can be, it is a pixel that no other stroke inks, so that it turning dark
/// means this very stroke was drawn. Failing that it is one that no earlier stroke
/// inks, which still shows the device has got this far. A stroke that only goes over
/// earlier ink has none.
///
/// Only pixels beside the stroke's line, between its two ends, are used. The pixels
/// around the ends are not inked evenly: the device draws the two sides of a stroke a
/// pixel out of step with each other.
pub fn watch_pixels(strokes: &[Stroke], page_size: (u32, u32)) -> Vec<Option<(u32, u32)>> {
    let width = page_size.0 as usize;
    let mut times_inked = vec![0u8; width * page_size.1 as usize];
    for stroke in strokes {
        for_each_inked_pixel(stroke, page_size, |x, y, _, _| {
            let count = &mut times_inked[y * width + x];
            *count = count.saturating_add(1);
        });
    }

    let mut inked_before = vec![false; times_inked.len()];
    strokes
        .iter()
        .map(|stroke| {
            // The best pixel is this stroke's alone, and near its middle
            let mut best = None;
            let mut best_rank = (true, f32::INFINITY);
            for_each_inked_pixel(stroke, page_size, |x, y, from_line, along| {
                if inked_before[y * width + x] || from_line > 0.6 || along <= 0.0 || along >= 1.0 {
                    return;
                }
                let rank = (times_inked[y * width + x] > 1, (along - 0.5).abs());
                if rank < best_rank {
                    best_rank = rank;
                    best = Some((x as u32, y as u32));
                }
            });
            for_each_inked_pixel(stroke, page_size, |x, y, _, _| inked_before[y * width + x] = true);

            best
        })
        .collect()
}

/// The pixels a set of strokes would ink, as rows of true/false, by the pen model above.
/// `strokes` are in virtual page coordinates, as `plan_strokes` returns them.
pub fn simulate(strokes: &[Stroke], page_size: (u32, u32)) -> Vec<Vec<bool>> {
    let mut rows = vec![vec![false; page_size.0 as usize]; page_size.1 as usize];
    for stroke in strokes {
        for_each_inked_pixel(stroke, page_size, |x, y, _, _| rows[y][x] = true);
    }
    rows
}

/// A point in virtual page coordinates, in device pixels.
fn to_pixels((x, y): Point, page_size: (u32, u32)) -> Point {
    (x * page_size.0 as f32 / VIRTUAL_WIDTH as f32, y * page_size.1 as f32 / VIRTUAL_HEIGHT as f32)
}

/// Call `visit` with each pixel a stroke inks, by the pen model above: its x and y, how
/// far its centre is from the stroke's line, and how far along the line it is (0 at the
/// start, 1 at the end, beyond those around the ends). A pixel on two parts of a stroke
/// is visited twice.
fn for_each_inked_pixel(stroke: &Stroke, page_size: (u32, u32), mut visit: impl FnMut(usize, usize, f32, f32)) {
    let (width, height) = (page_size.0 as usize, page_size.1 as usize);
    for pair in stroke.points.windows(2) {
        let (from, to) = (to_pixels(pair[0], page_size), to_pixels(pair[1], page_size));
        if from == to {
            continue;
        }
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let first_x = (from.0.min(to.0) - PEN_RADIUS).floor().max(0.0) as usize;
        let last_x = ((from.0.max(to.0) + PEN_RADIUS).ceil() as usize).min(width.saturating_sub(1));
        let first_y = (from.1.min(to.1) - PEN_RADIUS).floor().max(0.0) as usize;
        let last_y = ((from.1.max(to.1) + PEN_RADIUS).ceil() as usize).min(height.saturating_sub(1));
        for y in first_y..=last_y {
            for x in first_x..=last_x {
                let centre = (x as f32 + 0.5, y as f32 + 0.5);
                let from_line = distance_to_segment(centre, from, to);
                if from_line <= PEN_RADIUS {
                    let along = ((centre.0 - from.0) * dx + (centre.1 - from.1) * dy) / (dx * dx + dy * dy);
                    visit(x, y, from_line, along);
                }
            }
        }
    }
}

fn distance_to_segment(point: Point, from: Point, to: Point) -> f32 {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let along = (((point.0 - from.0) * dx + (point.1 - from.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    let nearest = (from.0 + along * dx, from.1 + along * dy);
    ((point.0 - nearest.0).powi(2) + (point.1 - nearest.1).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid_from(rows: &[&str]) -> Grid {
        let mut grid = Grid::new(rows[0].len(), rows.len());
        for (y, row) in rows.iter().enumerate() {
            for (x, cell) in row.chars().enumerate() {
                grid.set(x, y, cell == '#');
            }
        }
        grid
    }

    /// Ink strokes with the pen model, giving rows of '#' and '.'.
    fn draw(strokes: &[Segment], width: usize, height: usize) -> Vec<String> {
        let mut rows = vec![vec!['.'; width]; height];
        for &(from, to) in strokes {
            for (y, row) in rows.iter_mut().enumerate() {
                for (x, cell) in row.iter_mut().enumerate() {
                    if distance_to_segment((x as f32 + 0.5, y as f32 + 0.5), from, to) <= PEN_RADIUS {
                        *cell = '#';
                    }
                }
            }
        }
        rows.into_iter().map(|row| row.into_iter().collect()).collect()
    }

    #[test]
    fn a_solid_block_is_filled_exactly() {
        let rows = ["..........", ".#######..", ".#######..", ".#######..", ".#######..", ".#######..", ".........."];
        let strokes = plan_ink(&grid_from(&rows));

        assert_eq!(draw(&strokes, 10, 7), rows);
        // Two bars tile four rows; a third picks up the odd row
        assert_eq!(strokes.len(), 3);
    }

    #[test]
    fn a_tall_narrow_block_gets_vertical_bars() {
        let rows = [
            "......", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", "......",
        ];
        let strokes = plan_ink(&grid_from(&rows));

        assert_eq!(draw(&strokes, 6, 14), rows);
        assert_eq!(strokes.len(), 2);
        assert!(strokes.iter().all(|(from, to)| from.0 == to.0), "{strokes:?}");
    }

    #[test]
    fn an_l_shape_gets_bars_both_ways() {
        let rows = [
            "............",
            ".####.......",
            ".####.......",
            ".####.......",
            ".####.......",
            ".####.......",
            ".####.......",
            ".##########.",
            ".##########.",
            ".##########.",
            ".##########.",
            "............",
        ];
        let strokes = plan_ink(&grid_from(&rows));

        assert_eq!(draw(&strokes, 12, 12), rows);
        assert!(strokes.iter().any(|(from, to)| from.0 == to.0), "{strokes:?}");
        assert!(strokes.iter().any(|(from, to)| from.1 == to.1), "{strokes:?}");
    }

    #[test]
    fn a_thin_diagonal_line_is_drawn_along_its_middle() {
        // Two pixels wide, running down to the right
        let mut grid = Grid::new(40, 40);
        for step in 0..30 {
            grid.set(4 + step, 5 + step, true);
            grid.set(5 + step, 5 + step, true);
        }
        let strokes = plan_ink(&grid);

        assert!(strokes.len() <= 3, "{strokes:?}");
        let (start, end) = (strokes[0].0, strokes[strokes.len() - 1].1);
        assert!((start.0 - 5.0).abs() < 2.5 && (start.1 - 5.5).abs() < 2.5, "{strokes:?}");
        assert!((end.0 - 34.0).abs() < 2.5 && (end.1 - 34.5).abs() < 2.5, "{strokes:?}");
    }

    #[test]
    fn specks_are_left_out() {
        let strokes = plan_ink(&grid_from(&["......", "..#...", "......", "......"]));

        assert!(strokes.is_empty());
    }

    #[test]
    fn a_watched_pixel_is_inked_by_its_stroke_and_by_none_before_it() {
        let page_size = (768, 1024);
        let svg = r#"<svg width="768" height="1024" xmlns="http://www.w3.org/2000/svg"><rect x="100" y="100" width="60" height="40" fill="black"/><circle cx="300" cy="300" r="40" fill="black"/></svg>"#;
        let strokes = plan_strokes(svg, page_size).unwrap();

        let watched = watch_pixels(&strokes, page_size);

        assert_eq!(watched.len(), strokes.len());
        let mut its_own = 0;
        for (index, pixel) in watched.iter().enumerate() {
            let Some((x, y)) = *pixel else {
                continue;
            };
            let inks_it = |strokes: &[Stroke]| simulate(strokes, page_size)[y as usize][x as usize];
            assert!(inks_it(&strokes[index..=index]), "stroke {index}");
            assert!(!inks_it(&strokes[..index]), "stroke {index}");
            its_own += !inks_it(&strokes[index + 1..]) as usize;
        }
        // Most strokes can be watched, and most of those by a pixel that no other stroke inks
        let watchable = watched.iter().flatten().count();
        assert!(watchable * 10 >= strokes.len() * 8, "{watchable} of {}", strokes.len());
        assert!(its_own * 10 >= strokes.len() * 7, "{its_own} of {}", strokes.len());
    }

    #[test]
    fn a_stroke_over_earlier_ink_has_nothing_to_watch() {
        let stroke = |points: [Point; 2]| Stroke {
            points: points.to_vec(),
            pressure: FULL_PRESSURE,
        };
        let strokes = [
            stroke([(10.0, 10.0), (30.0, 10.0)]),
            // Along the first stroke, and then on past its end
            stroke([(15.0, 10.0), (25.0, 10.0)]),
            stroke([(20.0, 10.0), (40.0, 10.0)]),
        ];

        let watched = watch_pixels(&strokes, (768, 1024));

        assert!(watched[0].is_some());
        assert_eq!(watched[1], None);
        // Only the part beyond the first stroke is new
        assert!(watched[2].is_some_and(|(x, _)| (31..40).contains(&x)), "{watched:?}");
    }

    #[test]
    fn simplify_keeps_corners_and_drops_points_on_straight_lines() {
        let path = [(0.0, 0.0), (1.0, 0.1), (2.0, 0.0), (3.0, 0.0), (3.0, 1.0), (3.0, 2.0)];

        assert_eq!(simplify(&path, 0.5), vec![(0.0, 0.0), (3.0, 0.0), (3.0, 2.0)]);
    }

    #[test]
    fn an_svg_plans_to_strokes_that_reproduce_it() {
        let svg = r#"<svg width="768" height="1024" xmlns="http://www.w3.org/2000/svg"><rect x="100" y="100" width="200" height="120" fill="black"/><circle cx="500" cy="400" r="60" fill="black"/><line x1="100" y1="600" x2="400" y2="800" stroke="black" stroke-width="4"/></svg>"#;
        let page_size = (768, 1024);

        let strokes = plan_strokes(svg, page_size).unwrap();
        let drawn = simulate(&strokes, page_size);
        let wanted = svg_to_alpha_bitmap(svg, page_size.0, page_size.1).unwrap();

        let mut wrong = 0;
        let mut total = 0;
        for (drawn_row, wanted_row) in drawn.iter().zip(&wanted) {
            for (&is_drawn, &alpha) in drawn_row.iter().zip(wanted_row) {
                total += (alpha >= 128) as usize;
                wrong += (is_drawn != (alpha >= 128)) as usize;
            }
        }
        // Only the odd pixel along a slanted or curved edge differs
        assert!(wrong * 200 < total, "{wrong} of {total} pixels differ");
        assert!(strokes.iter().all(|stroke| stroke.points.len() == 2));
    }
}
