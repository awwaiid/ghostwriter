//! Turn an SVG into pen strokes.
//!
//! The plan is made on the device's own pixel grid, for a pen that has been measured on
//! the device (see `PenModel`). What the Paper Pro's pens have in common, on firmware 3.29:
//!
//! - A stroke between two points is a straight line with round ends, of a width that
//!   depends on the pen. It inks every pixel whose centre is near enough to it.
//! - Lined up with the pixel grid the line is crisp. Anywhere else it is anti-aliased.
//! - A stroke has to move to leave ink; a single point draws nothing.
//! - A stroke with more points is smoothed by the device, which rounds its corners and
//!   pulls it off course, so every stroke here is one straight line.
//! - The device draws about 2500 strokes a second, however few events each is made of.
//!   Sent faster than it takes them, they are lost a queue-full at a time (see
//!   `Pen::draw_strokes_watched`).
//!
//! So the lines of the SVG that slant or curve are followed along their length, as a
//! pen would (see `path_strokes`). Everything else is filled with crisp bars, each
//! inking a few whole rows or columns of pixels, which reproduces the rendered SVG
//! pixel for pixel. What is too thin for a bar is drawn along its middle.

use anyhow::Result;
use log::debug;

use crate::device::{VIRTUAL_HEIGHT, VIRTUAL_WIDTH};
use crate::path_strokes;
use crate::pen::{Stroke, FULL_PRESSURE};
use crate::skeleton;
use crate::util::{parse_svg, svg_to_alpha_bitmap, tree_to_alpha_bitmap};

/// What planning needs to know about a pen: how its strokes come out on the device.
///
/// A stroke inks the pixels whose centres are within half the pen's width of its line,
/// and it carries on a pixel past each end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PenModel {
    /// How many rows of pixels a stroke along them inks solidly, when it is lined up
    /// with them: along the boundary between two rows, or along the middle of three
    pub rows: usize,
    /// How far short of its end a stroke needs an extra point, for the device to draw it
    /// evenly from end to end. Zero for a pen that needs no such thing.
    pub tail: f32,
}

impl PenModel {
    /// The fineliner at its thin setting: a line 2 pixels wide whatever the pressure.
    ///
    /// Seen from the pen as it travels, the left side of the line starts and stops a
    /// pixel earlier than the right side, so one corner pixel at each end comes out faint.
    pub const FINELINER_THIN: Self = Self { rows: 2, tail: 0.0 };

    /// The ballpoint at its medium setting, pressed firmly: a line 3 pixels wide.
    ///
    /// A two-point stroke starts thin and widens along its whole length. With a third
    /// point just before the last, the device draws it at its full width for all but the
    /// first fifth or so of one side. Pressed lightly (under about 600 of 4096) the line
    /// is narrower and uneven. More points make it wider, and the device stops short of
    /// the last of them.
    pub const BALLPOINT_MEDIUM: Self = Self { rows: 3, tail: 0.3 };

    /// How far a stroke's ink carries on past each of its ends, in pixels.
    pub(crate) const PAST_END: f32 = 1.0;

    /// The shortest run of pixels a bar can cover: a pixel of travel, and what is inked past each end.
    const SHORTEST_BAR: usize = 3;

    /// How far to either side of a stroke's line a pixel's centre can be and still be inked.
    fn half_width(&self) -> f32 {
        self.rows as f32 / 2.0
    }

    /// Whether a stroke from `from` to `to` inks the pixel centred on `centre`. If it
    /// does: how far the centre is from the stroke's line, and how far along the stroke
    /// it is, from 0 at the start to 1 at the end (beyond those, past the ends).
    fn inks(&self, from: Point, to: Point, centre: Point) -> Option<(f32, f32)> {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = dx.hypot(dy);
        if length == 0.0 {
            return None;
        }
        let (px, py) = (centre.0 - from.0, centre.1 - from.1);
        let along = (px * dx + py * dy) / length;
        let across = ((px * dy - py * dx) / length).abs();
        // A hair's breadth of leeway, for centres that sit exactly on the edge
        let inked = across <= self.half_width() + 0.01 && along >= -Self::PAST_END - 0.01 && along <= length + Self::PAST_END + 0.01;
        inked.then_some((across, along / length))
    }
}

/// How far a bar runs on into ink that other bars will cover.
const BAR_OVERLAP: usize = 2;

/// The shortest stroke worth sending, in pixels. One that does not move leaves no ink.
const MIN_STROKE: f32 = 1.0;

/// Thin features smaller than this many pixels are left out: they are below what the pen can draw.
const MIN_THIN_FEATURE: usize = 6;

/// A point in device pixels. Pixel (i, j) covers i..i+1 by j..j+1.
pub(crate) type Point = (f32, f32);

/// A straight stroke between two points.
pub(crate) type Segment = (Point, Point);

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
        // Most of a page is blank, so only the true cells are worth a visit
        for (index, _) in self.cells.iter().enumerate().filter(|(_, &cell)| cell) {
            let (x, y) = (index % self.width, index / self.width);
            transposed.cells[x * self.height + y] = true;
        }
        transposed
    }

    /// The part of the grid `width` by `height` cells from (left, top).
    fn cropped(&self, left: usize, top: usize, width: usize, height: usize) -> Grid {
        let cells = (top..top + height)
            .flat_map(|y| self.cells[y * self.width + left..y * self.width + left + width].iter().copied())
            .collect();
        Grid { width, height, cells }
    }

    /// The first and last true cell of each row, for rows that have any.
    fn row_spans(&self) -> Vec<Option<(usize, usize)>> {
        self.cells
            .chunks(self.width.max(1))
            .map(|row| {
                let first = row.iter().position(|&cell| cell)?;
                let last = row.iter().rposition(|&cell| cell)?;
                Some((first, last))
            })
            .collect()
    }

    /// The smallest box holding every true cell, as (left, top, width, height).
    fn bounds(&self) -> Option<(usize, usize, usize, usize)> {
        let spans = self.row_spans();
        let top = spans.iter().position(Option::is_some)?;
        let bottom = spans.iter().rposition(Option::is_some)?;
        let left = spans.iter().flatten().map(|span| span.0).min()?;
        let right = spans.iter().flatten().map(|span| span.1).max()?;
        Some((left, top, right - left + 1, bottom - top + 1))
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

/// Plan the strokes that draw an SVG with `pen` on a page of `page_size` device pixels.
/// The strokes are in virtual page coordinates, ready for `Pen::draw_strokes`.
pub fn plan_strokes(svg_data: &str, page_size: (u32, u32), pen: &PenModel) -> Result<Vec<Stroke>> {
    let (width, height) = (page_size.0 as usize, page_size.1 as usize);
    let started = std::time::Instant::now();
    // An SVG that cannot be read is drawn as a marker saying so, which has no lines to follow
    let parsed = parse_svg(svg_data);
    debug!("Parsed the SVG after {:?}", started.elapsed());
    let (coverage, lines) = match parsed {
        Ok(tree) => {
            let coverage = tree_to_alpha_bitmap(&tree, page_size.0, page_size.1)?;
            debug!("Rendered it after {:?}", started.elapsed());
            (coverage, path_strokes::along_paths(&tree, page_size, pen))
        }
        Err(_) => (svg_to_alpha_bitmap(svg_data, page_size.0, page_size.1)?, Vec::new()),
    };
    let ink = Grid {
        width,
        height,
        cells: coverage.iter().flat_map(|row| row.iter().map(|&alpha| alpha >= 128)).collect(),
    };
    debug!("Followed its lines after {:?}", started.elapsed());

    // The lines first, and then whatever they leave bare
    let mut covered = Grid::new(width, height);
    for &(from, to) in &lines {
        for_each_inked_pixel_of(from, to, page_size, pen, |x, y, _, _| covered.set(x, y, true));
    }
    let mut strokes = lines;
    let along_lines = strokes.len();
    // The rest is planned within the part of the page that has ink on it, which is
    // usually a small part
    if let Some((left, top, width, height)) = ink.bounds() {
        let ink = ink.cropped(left, top, width, height);
        let mut covered = covered.cropped(left, top, width, height);
        drop_slivers(&ink, &mut covered);
        debug!("Marked what they ink after {:?}", started.elapsed());
        let on_page = |(x, y): Point| (x + left as f32, y + top as f32);
        strokes.extend(plan_ink(&ink, covered, pen).into_iter().map(|(from, to)| (on_page(from), on_page(to))));
    }
    debug!(
        "{} strokes along lines and {} for the rest, after {:?}",
        along_lines,
        strokes.len() - along_lines,
        started.elapsed()
    );

    let to_virtual = |(x, y): Point| (x * VIRTUAL_WIDTH as f32 / width as f32, y * VIRTUAL_HEIGHT as f32 / height as f32);
    Ok(strokes
        .into_iter()
        .map(|(from, to)| Stroke {
            points: vec![to_virtual(from), to_virtual(to)],
            pressure: FULL_PRESSURE,
        })
        .collect())
}

/// Count as covered the bare ink that is only a pixel thick along the edge of what is
/// covered already. The pen's line and the rendered line rarely agree to the pixel
/// about where a slanting edge is, and those slivers are not worth a stroke each.
fn drop_slivers(ink: &Grid, covered: &mut Grid) {
    let bare = |x: isize, y: isize| ink.at(x, y) && !covered.at(x, y);
    let mut slivers = Vec::new();
    for (index, _) in ink.cells.iter().enumerate().filter(|&(index, &cell)| cell && !covered.cells[index]) {
        let (x, y) = ((index % ink.width) as isize, (index / ink.width) as isize);
        let beside_covered = (-1..=1).any(|dy| (-1..=1).any(|dx| covered.at(x + dx, y + dy)));
        // Part of some two-by-two block of bare ink, or not
        let has_substance = (-1..=0).any(|dy| (-1..=0).any(|dx| (0..=1).all(|by| (0..=1).all(|bx| bare(x + dx + bx, y + dy + by)))));
        if beside_covered && !has_substance {
            slivers.push((x as usize, y as usize));
        }
    }
    for (x, y) in slivers {
        covered.set(x, y, true);
    }
}

/// Plan the strokes that ink the true cells of a grid that are not `covered` already, in pixels.
fn plan_ink(ink: &Grid, mut covered: Grid, pen: &PenModel) -> Vec<Segment> {
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
    strokes.extend(bars(ink, &prefers_across, &mut covered, pen));

    // ...and then the other direction, and the first again, mop up
    let mut covered_sideways = covered.transposed();
    let upright = bars(&sideways, &sideways, &mut covered_sideways, pen);
    strokes.extend(upright.into_iter().map(|((x1, y1), (x2, y2))| ((y1, x1), (y2, x2))));
    covered = covered_sideways.transposed();

    strokes.extend(bars(ink, ink, &mut covered, pen));

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
/// A bar is a stroke lined up with the rows of pixels, inking as many of them as the pen
/// is wide. It is only put where all those rows are inside `ink`, so it never marks
/// outside the shape, and it stops a pixel short of each end of the run because the
/// pen's ink carries on that far.
fn bars(ink: &Grid, wanted: &Grid, covered: &mut Grid, pen: &PenModel) -> Vec<Segment> {
    let mut strokes = Vec::new();
    let rows = pen.rows;
    if ink.height < rows {
        return strokes;
    }

    // Going down the page, a bar whose top row is `top` is the last chance to ink that
    // row, so that is where a bar goes. The rows below are inked along with it, and
    // only need this bar for themselves where no bar can start lower down.
    let spans = ink.row_spans();
    for top in 0..=ink.height - rows {
        // Only as far across as all of these rows have ink
        let Some((first, last)) = spans[top..top + rows]
            .iter()
            .copied()
            .reduce(|a, b| Some((a?.0.max(b?.0), a?.1.min(b?.1))))
            .flatten()
        else {
            continue;
        };
        let all_rows = |x: usize| (top..top + rows).all(|y| ink.get(x, y));
        let no_lower_bar = |x: usize| top + rows >= ink.height || !ink.get(x, top + rows);

        let mut x = first;
        while x <= last {
            if !all_rows(x) {
                x += 1;
                continue;
            }
            let run_start = x;
            while x <= last && all_rows(x) {
                x += 1;
            }
            let run_end = x;
            if run_end - run_start < PenModel::SHORTEST_BAR {
                continue;
            }

            let needs_ink = |x: usize| {
                let needs = |y: usize| wanted.get(x, y) && !covered.get(x, y);
                needs(top) || (no_lower_bar(x) && (top + 1..top + rows).any(needs))
            };
            for (start, end) in spans_needing_ink(run_start, run_end, needs_ink) {
                let middle = top as f32 + pen.half_width();
                strokes.push(((start as f32 + PenModel::PAST_END, middle), (end as f32 - PenModel::PAST_END, middle)));
                for y in top..top + rows {
                    for x in start..end {
                        covered.set(x, y, true);
                    }
                }
            }
        }
    }

    strokes
}

/// The stretches of a run that still need ink, each widened to at least the shortest
/// bar and merged with its neighbour when they nearly touch.
fn spans_needing_ink(run_start: usize, run_end: usize, needs_ink: impl Fn(usize) -> bool) -> Vec<(usize, usize)> {
    let shortest = PenModel::SHORTEST_BAR;
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for x in (run_start..run_end).filter(|&x| needs_ink(x)) {
        match spans.last_mut() {
            Some(last) if x <= last.1 + shortest => last.1 = x + 1,
            _ => spans.push((x, x + 1)),
        }
    }

    for span in &mut spans {
        // Where the ink carries on past the span, so does the bar: its round end is
        // not quite black, and would show as a seam against the bars it meets there
        span.0 = span.0.saturating_sub(BAR_OVERLAP).max(run_start);
        span.1 = (span.1 + BAR_OVERLAP).min(run_end);

        while span.1 - span.0 < shortest {
            if span.0 > run_start {
                span.0 -= 1;
            }
            if span.1 - span.0 < shortest && span.1 < run_end {
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
pub fn watch_pixels(strokes: &[Stroke], page_size: (u32, u32), pen: &PenModel) -> Vec<Option<(u32, u32)>> {
    let width = page_size.0 as usize;
    let mut times_inked = vec![0u8; width * page_size.1 as usize];
    for stroke in strokes {
        for_each_inked_pixel(stroke, page_size, pen, |x, y, _, _| {
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
            for_each_inked_pixel(stroke, page_size, pen, |x, y, from_line, along| {
                if inked_before[y * width + x] || from_line > pen.half_width() - 0.4 || along <= 0.0 || along >= 1.0 {
                    return;
                }
                let rank = (times_inked[y * width + x] > 1, (along - 0.5).abs());
                if rank < best_rank {
                    best_rank = rank;
                    best = Some((x as u32, y as u32));
                }
            });
            for_each_inked_pixel(stroke, page_size, pen, |x, y, _, _| inked_before[y * width + x] = true);

            best
        })
        .collect()
}

/// The pixels a set of strokes would ink with `pen`, as rows of true/false.
/// `strokes` are in virtual page coordinates, as `plan_strokes` returns them.
pub fn simulate(strokes: &[Stroke], page_size: (u32, u32), pen: &PenModel) -> Vec<Vec<bool>> {
    let mut rows = vec![vec![false; page_size.0 as usize]; page_size.1 as usize];
    for stroke in strokes {
        for_each_inked_pixel(stroke, page_size, pen, |x, y, _, _| rows[y][x] = true);
    }
    rows
}

/// A point in virtual page coordinates, in device pixels.
fn to_pixels((x, y): Point, page_size: (u32, u32)) -> Point {
    (x * page_size.0 as f32 / VIRTUAL_WIDTH as f32, y * page_size.1 as f32 / VIRTUAL_HEIGHT as f32)
}

/// Call `visit` with each pixel a stroke inks with `pen`: its x and y, how far its centre
/// is from the stroke's line, and how far along the line it is (0 at the start, 1 at the
/// end, beyond those past the ends). A pixel on two parts of a stroke is visited twice.
fn for_each_inked_pixel(stroke: &Stroke, page_size: (u32, u32), pen: &PenModel, mut visit: impl FnMut(usize, usize, f32, f32)) {
    for pair in stroke.points.windows(2) {
        for_each_inked_pixel_of(to_pixels(pair[0], page_size), to_pixels(pair[1], page_size), page_size, pen, &mut visit);
    }
}

/// The same, for one straight stroke between two points in device pixels.
fn for_each_inked_pixel_of(from: Point, to: Point, page_size: (u32, u32), pen: &PenModel, mut visit: impl FnMut(usize, usize, f32, f32)) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    // Go along the stroke the long way, a column of pixels at a time (or a row, for a
    // steep stroke), looking only at the few pixels in each that are near its line
    let steep = dy.abs() > dx.abs();
    let (from_a, from_b, to_a, to_b) = if steep { (from.1, from.0, to.1, to.0) } else { (from.0, from.1, to.0, to.1) };
    let (size_a, size_b) = if steep { (page_size.1, page_size.0) } else { (page_size.0, page_size.1) };
    let slope = (to_b - from_b) / (to_a - from_a);
    let reach = (pen.half_width() + PenModel::PAST_END) * (1.0 + slope * slope).sqrt() + 1.0;

    let first_a = (from_a.min(to_a) - PenModel::PAST_END - pen.half_width()).floor().max(0.0) as usize;
    let last_a = ((from_a.max(to_a) + PenModel::PAST_END + pen.half_width()).ceil().max(0.0) as usize).min(size_a.saturating_sub(1) as usize);
    for a in first_a..=last_a {
        // Where the stroke's line is at this column, kept within the stroke's own span
        let along = (a as f32 + 0.5).clamp(from_a.min(to_a), from_a.max(to_a));
        let line_b = from_b + (along - from_a) * slope;
        let first_b = (line_b - reach).floor().max(0.0) as usize;
        let last_b = ((line_b + reach).ceil().max(0.0) as usize).min(size_b.saturating_sub(1) as usize);
        for b in first_b..=last_b {
            let (x, y) = if steep { (b, a) } else { (a, b) };
            if let Some((from_line, along)) = pen.inks(from, to, (x as f32 + 0.5, y as f32 + 0.5)) {
                visit(x, y, from_line, along);
            }
        }
    }
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

    const FINELINER: &PenModel = &PenModel::FINELINER_THIN;
    const BALLPOINT: &PenModel = &PenModel::BALLPOINT_MEDIUM;

    /// Ink strokes with a pen, giving rows of '#' and '.'.
    fn draw(strokes: &[Segment], width: usize, height: usize, pen: &PenModel) -> Vec<String> {
        let mut rows = vec![vec!['.'; width]; height];
        for &(from, to) in strokes {
            for (y, row) in rows.iter_mut().enumerate() {
                for (x, cell) in row.iter_mut().enumerate() {
                    if pen.inks(from, to, (x as f32 + 0.5, y as f32 + 0.5)).is_some() {
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
        let strokes = plan_ink(&grid_from(&rows), Grid::new(rows[0].len(), rows.len()), FINELINER);

        assert_eq!(draw(&strokes, 10, 7, FINELINER), rows);
        // Two bars tile four rows; a third picks up the odd row
        assert_eq!(strokes.len(), 3);
    }

    #[test]
    fn a_wider_pen_fills_a_block_with_fewer_bars() {
        let rows = [
            "............",
            ".##########.",
            ".##########.",
            ".##########.",
            ".##########.",
            ".##########.",
            ".##########.",
            ".##########.",
            "............",
        ];
        let strokes = plan_ink(&grid_from(&rows), Grid::new(rows[0].len(), rows.len()), BALLPOINT);

        assert_eq!(draw(&strokes, 12, 9, BALLPOINT), rows);
        // Two bars tile six rows; a third picks up the odd row
        assert_eq!(strokes.len(), 3);
        // Each runs along the middle of a row of pixels
        assert!(strokes.iter().all(|(from, to)| from.1 == to.1 && from.1.fract() == 0.5), "{strokes:?}");
    }

    #[test]
    fn a_tall_narrow_block_gets_vertical_bars() {
        let rows = [
            "......", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", ".####.", "......",
        ];
        let strokes = plan_ink(&grid_from(&rows), Grid::new(rows[0].len(), rows.len()), FINELINER);

        assert_eq!(draw(&strokes, 6, 14, FINELINER), rows);
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
        let strokes = plan_ink(&grid_from(&rows), Grid::new(rows[0].len(), rows.len()), FINELINER);

        assert_eq!(draw(&strokes, 12, 12, FINELINER), rows);
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
        let strokes = plan_ink(&grid, Grid::new(40, 40), FINELINER);

        assert!(strokes.len() <= 3, "{strokes:?}");
        let (start, end) = (strokes[0].0, strokes[strokes.len() - 1].1);
        assert!((start.0 - 5.0).abs() < 2.5 && (start.1 - 5.5).abs() < 2.5, "{strokes:?}");
        assert!((end.0 - 34.0).abs() < 2.5 && (end.1 - 34.5).abs() < 2.5, "{strokes:?}");
    }

    #[test]
    fn specks_are_left_out() {
        let strokes = plan_ink(&grid_from(&["......", "..#...", "......", "......"]), Grid::new(6, 4), FINELINER);

        assert!(strokes.is_empty());
    }

    #[test]
    fn a_watched_pixel_is_inked_by_its_stroke_and_by_none_before_it() {
        let page_size = (768, 1024);
        let svg = r#"<svg width="768" height="1024" xmlns="http://www.w3.org/2000/svg"><rect x="100" y="100" width="60" height="40" fill="black"/><circle cx="300" cy="300" r="40" fill="black"/></svg>"#;
        let strokes = plan_strokes(svg, page_size, FINELINER).unwrap();

        let watched = watch_pixels(&strokes, page_size, FINELINER);

        assert_eq!(watched.len(), strokes.len());
        let mut its_own = 0;
        for (index, pixel) in watched.iter().enumerate() {
            let Some((x, y)) = *pixel else {
                continue;
            };
            let inks_it = |strokes: &[Stroke]| simulate(strokes, page_size, FINELINER)[y as usize][x as usize];
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

        let watched = watch_pixels(&strokes, (768, 1024), FINELINER);

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

        let strokes = plan_strokes(svg, page_size, FINELINER).unwrap();
        let drawn = simulate(&strokes, page_size, FINELINER);
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
