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
use crate::util::{parse_svg, render_tree, svg_to_alpha_bitmap};

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

    /// How far along a stroke `length` pixels long its ink is thin before it reaches its
    /// full width. Only a pen that needs a tail starts thin, and only on strokes longer
    /// than 6 pixels: for about the first quarter of them, up to 45 pixels or so, the
    /// side of the line to the left of the pen's travel is grey rather than black.
    fn thin_start(&self, length: f32) -> usize {
        if self.tail == 0.0 || length <= 6.0 {
            0
        } else {
            ((length * 0.25).ceil() as usize + 2).min(48)
        }
    }

    /// Whether a stroke from `from` to `to` inks the pixel centred on `centre`. If it
    /// does: how far the centre is from the stroke's line, and how far along the stroke
    /// it is, from 0 at the start to 1 at the end (beyond those, past the ends).
    #[cfg(test)]
    fn inks(&self, from: Point, to: Point, centre: Point) -> Option<(f32, f32)> {
        self.reaches(from, to, centre, 0.0)
    }

    /// The same, for a pixel within `margin` pixels of the ones the stroke inks.
    fn reaches(&self, from: Point, to: Point, centre: Point, margin: f32) -> Option<(f32, f32)> {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = dx.hypot(dy);
        if length == 0.0 {
            return None;
        }
        let (px, py) = (centre.0 - from.0, centre.1 - from.1);
        let along = (px * dx + py * dy) / length;
        let across = ((px * dy - py * dx) / length).abs();
        // A hair's breadth of leeway, for centres that sit exactly on the edge
        let (side, end) = (self.half_width() + margin + 0.01, Self::PAST_END + margin + 0.01);
        let reached = across <= side && along >= -end && along <= length + end;
        reached.then_some((across, along / length))
    }

    /// How far beyond the pixels it inks a stroke leaves a faint edge, in pixels. A
    /// stroke straight across or down the page, lined up with the pixels, has none.
    fn soft_edge(from: Point, to: Point) -> f32 {
        if (from.0 - to.0).abs() < 0.01 || (from.1 - to.1).abs() < 0.01 {
            0.0
        } else {
            1.0
        }
    }
}

/// The inks a drawing can be made in: the colours the Paper Pro's pens come in, but for white.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ink {
    Black,
    Gray,
    Blue,
    Red,
    Green,
    Yellow,
    Cyan,
    Magenta,
}

impl Ink {
    /// Every ink, black first: it is the one most of any drawing is in.
    pub const ALL: [Ink; 8] = [Ink::Black, Ink::Gray, Ink::Blue, Ink::Red, Ink::Green, Ink::Yellow, Ink::Cyan, Ink::Magenta];

    /// The ink's place among the nine colours in a pen's settings (white is the third).
    pub fn palette_place(self) -> u32 {
        match self {
            Ink::Black => 0,
            Ink::Gray => 1,
            Ink::Blue => 3,
            Ink::Red => 4,
            Ink::Green => 5,
            Ink::Yellow => 6,
            Ink::Cyan => 7,
            Ink::Magenta => 8,
        }
    }

    fn rgb(self) -> (i32, i32, i32) {
        match self {
            Ink::Black => (0, 0, 0),
            Ink::Gray => (128, 128, 128),
            Ink::Blue => (0, 0, 255),
            Ink::Red => (255, 0, 0),
            Ink::Green => (0, 255, 0),
            Ink::Yellow => (255, 255, 0),
            Ink::Cyan => (0, 255, 255),
            Ink::Magenta => (255, 0, 255),
        }
    }

    /// The ink nearest to a colour, or None if the white of the paper is nearer still:
    /// a white or pale shape is not drawn at all.
    pub fn nearest(red: u8, green: u8, blue: u8) -> Option<Ink> {
        let far_from = |(r, g, b): (i32, i32, i32)| (red as i32 - r).pow(2) + (green as i32 - g).pow(2) + (blue as i32 - b).pow(2);
        let nearest = Ink::ALL.into_iter().min_by_key(|ink| far_from(ink.rgb()))?;
        (far_from(nearest.rgb()) < far_from((255, 255, 255))).then_some(nearest)
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

/// Plan the strokes that draw an SVG with `pen` on a page of `page_size` device pixels,
/// whatever ink each is in. The strokes are in virtual page coordinates, ready for
/// `Pen::draw_strokes`.
pub fn plan_strokes(svg_data: &str, page_size: (u32, u32), pen: &PenModel) -> Result<Vec<Stroke>> {
    Ok(plan_drawing(svg_data, page_size, pen)?.into_iter().flat_map(|(_, strokes)| strokes).collect())
}

/// Plan the strokes that draw an SVG with `pen` on a page of `page_size` device pixels:
/// those for each ink the drawing calls for, black first. Each colour in the SVG is
/// drawn in the ink nearest to it, and what is white or pale is left as bare paper.
pub fn plan_drawing(svg_data: &str, page_size: (u32, u32), pen: &PenModel) -> Result<Vec<(Ink, Vec<Stroke>)>> {
    let (width, height) = (page_size.0 as usize, page_size.1 as usize);
    let started = std::time::Instant::now();
    // An SVG that cannot be read is drawn as a marker saying so, in black, which has no lines to follow
    let parsed = parse_svg(svg_data);
    debug!("Parsed the SVG after {:?}", started.elapsed());
    let (inks, mut lines): (Vec<Option<Ink>>, Vec<(Ink, Segment)>) = match parsed {
        Ok(tree) => {
            let rendered = render_tree(&tree, page_size.0, page_size.1)?;
            debug!("Rendered it after {:?}", started.elapsed());
            let inks = rendered
                .pixels()
                .iter()
                .map(|pixel| {
                    let colour = pixel.demultiply();
                    (colour.alpha() >= 128)
                        .then(|| Ink::nearest(colour.red(), colour.green(), colour.blue()))
                        .flatten()
                })
                .collect();
            (inks, path_strokes::along_paths(&tree, page_size, pen))
        }
        Err(_) => {
            let coverage = svg_to_alpha_bitmap(svg_data, page_size.0, page_size.1)?;
            let inks = coverage.iter().flatten().map(|&alpha| (alpha >= 128).then_some(Ink::Black)).collect();
            (inks, Vec::new())
        }
    };
    debug!("Followed its lines after {:?}", started.elapsed());

    // A line is only followed where it shows: not where something else is drawn over it
    let ink_at = |(x, y): Point| {
        let (x, y) = (x.floor(), y.floor());
        (x >= 0.0 && y >= 0.0 && (x as usize) < width && (y as usize) < height)
            .then(|| inks[y as usize * width + x as usize])
            .flatten()
    };
    lines.retain(|&(ink, (from, to))| ink_at(((from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0)) == Some(ink));

    let to_virtual = |(x, y): Point| (x * VIRTUAL_WIDTH as f32 / width as f32, y * VIRTUAL_HEIGHT as f32 / height as f32);
    let mut layers = Vec::new();
    let called_for: std::collections::HashSet<Ink> = inks.iter().flatten().copied().collect();
    for ink in Ink::ALL.into_iter().filter(|ink| called_for.contains(ink)) {
        let of_this_ink = Grid {
            width,
            height,
            cells: inks.iter().map(|&at| at == Some(ink)).collect(),
        };
        let lines: Vec<Segment> = lines.iter().filter(|line| line.0 == ink).map(|line| line.1).collect();
        let strokes = plan_one_ink(&of_this_ink, lines, page_size, pen);
        if !strokes.is_empty() {
            debug!("{} strokes in {:?}, after {:?}", strokes.len(), ink, started.elapsed());
            let strokes = strokes
                .into_iter()
                .map(|(from, to)| Stroke {
                    points: vec![to_virtual(from), to_virtual(to)],
                    pressure: FULL_PRESSURE,
                })
                .collect();
            layers.push((ink, strokes));
        }
    }
    Ok(layers)
}

/// Plan the strokes for the part of a drawing that is in one ink: `ink` is where that
/// is on the page, and `lines` are strokes already planned along the lines in it.
fn plan_one_ink(ink: &Grid, lines: Vec<Segment>, page_size: (u32, u32), pen: &PenModel) -> Vec<Segment> {
    // The lines first, and then whatever they leave bare
    let mut covered = Grid::new(ink.width, ink.height);
    for &(from, to) in &lines {
        for_each_pixel_near(from, to, page_size, pen, 0.0, |x, y, _, _| covered.set(x, y, true));
    }
    let mut strokes = lines;
    // The rest is planned within the part of the page that has ink on it, which is
    // usually a small part
    if let Some((left, top, width, height)) = ink.bounds() {
        let ink = ink.cropped(left, top, width, height);
        let mut covered = covered.cropped(left, top, width, height);
        drop_slivers(&ink, &mut covered);
        let on_page = |(x, y): Point| (x + left as f32, y + top as f32);
        strokes.extend(plan_ink(&ink, covered, pen).into_iter().map(|(from, to)| (on_page(from), on_page(to))));

        let in_the_crop = |(x, y): Point| (x - left as f32, y - top as f32);
        let order = drawing_order(
            &strokes.iter().map(|&(from, to)| (in_the_crop(from), in_the_crop(to))).collect::<Vec<_>>(),
            &ink,
        );
        strokes = order.into_iter().map(|index| strokes[index]).collect();
    }
    strokes
}

/// The order to draw strokes in, as indexes into `strokes`: one shape at a time, the
/// shapes in reading order, the way a person would go about it. Strokes planned for
/// the whole page at once come out a row of the page at a time, like a printer.
///
/// A shape is a patch of ink that hangs together. Shapes are sorted into lines by how
/// far down the page their middles are, as letters are, and each line is taken from
/// left to right. Within a shape the strokes keep the order they were planned in.
fn drawing_order(strokes: &[Segment], ink: &Grid) -> Vec<usize> {
    // Which shape each cell of ink is part of, counting from 1
    let shapes = ink.components();
    let mut shape_at = vec![0u32; ink.cells.len()];
    for (number, shape) in shapes.iter().enumerate() {
        for &(x, y) in shape {
            shape_at[y * ink.width + x] = number as u32 + 1;
        }
    }
    let shape_near = |(x, y): Point| {
        let (x, y) = (x.floor() as isize, y.floor() as isize);
        // A stroke along the boundary between two rows is beside its ink, not on it
        [(0, 0), (-1, 0), (0, -1), (-1, -1), (1, 0), (0, 1)]
            .iter()
            .filter(|&&(dx, dy)| ink.at(x + dx, y + dy))
            .map(|&(dx, dy)| shape_at[(y + dy) as usize * ink.width + (x + dx) as usize])
            .next()
    };

    // The shape each stroke belongs to: the one its middle is on, or failing that an
    // end, or failing that whatever the stroke before it belonged to
    let mut last = 0;
    let stroke_shapes: Vec<u32> = strokes
        .iter()
        .map(|&(from, to)| {
            let middle = ((from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0);
            last = shape_near(middle).or_else(|| shape_near(from)).or_else(|| shape_near(to)).unwrap_or(last);
            last
        })
        .collect();

    // Where each shape is: its left edge, the middle of its height, and its height
    let places: Vec<(usize, f32, usize)> = shapes
        .iter()
        .map(|shape| {
            let left = shape.iter().map(|cell| cell.0).min().unwrap_or(0);
            let (top, bottom) = shape.iter().fold((usize::MAX, 0), |(top, bottom), cell| (top.min(cell.1), bottom.max(cell.1)));
            (left, (top + bottom) as f32 / 2.0, bottom - top + 1)
        })
        .collect();

    // Shapes whose middles are within most of a typical shape's height of each other
    // down the page are on the same line
    let mut heights: Vec<usize> = places.iter().map(|place| place.2).collect();
    heights.sort_unstable();
    let line_height = heights.get(heights.len() / 2).copied().unwrap_or(1) as f32 * 0.7;
    let mut down_the_page: Vec<usize> = (0..shapes.len()).collect();
    down_the_page.sort_by(|&a, &b| places[a].1.total_cmp(&places[b].1));
    let mut rank = vec![0usize; shapes.len() + 1];
    let mut line: Vec<usize> = Vec::new();
    let mut next_rank = 1;
    let mut finish_line = |line: &mut Vec<usize>, rank: &mut Vec<usize>| {
        line.sort_by_key(|&shape| places[shape].0);
        for shape in line.drain(..) {
            rank[shape + 1] = next_rank;
            next_rank += 1;
        }
    };
    for shape in down_the_page {
        let line_middle = line.iter().map(|&other| places[other].1).sum::<f32>() / line.len().max(1) as f32;
        if !line.is_empty() && places[shape].1 - line_middle > line_height {
            finish_line(&mut line, &mut rank);
        }
        line.push(shape);
    }
    finish_line(&mut line, &mut rank);

    let mut order: Vec<usize> = (0..strokes.len()).collect();
    order.sort_by_key(|&index| rank[stroke_shapes[index] as usize]);
    order
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

    // Bars run along whichever direction gives the longer bar at each pixel,
    let across = ink.run_lengths();
    let sideways = ink.transposed();
    let down = sideways.run_lengths();
    let mut prefers_across = Grid::new(ink.width, ink.height);
    for y in 0..ink.height {
        for x in 0..ink.width {
            prefers_across.set(x, y, ink.get(x, y) && across[y * ink.width + x] >= down[x * ink.height + y]);
        }
    }
    // ...and then the other direction, and the first again, mop up.
    //
    // With some pens a bar's ink is faint along one side near its start. Going across
    // the page that side is the bar's top row; going down it is the right-hand column,
    // which is the last row with rows and columns swapped. What a pass leaves faint is
    // left for the next pass to ink, crossing it the other way, and the passes carry on
    // until nothing is left faint: each has less to do than the one before.
    const MOST_PASSES: usize = 8;
    let (first_row, last_row) = (0, pen.rows - 1);
    let mut faint = Vec::new();
    for pass in 0..MOST_PASSES {
        if pass >= 3 && faint.is_empty() {
            break;
        }
        if pass % 2 == 0 {
            let wanted = if pass == 0 { &prefers_across } else { ink };
            for (x, y) in faint.drain(..) {
                covered.set(x, y, false);
            }
            let (level, left_faint) = bars(ink, wanted, &mut covered, pen, first_row);
            strokes.extend(level);
            faint = left_faint;
        } else {
            let mut covered_sideways = covered.transposed();
            for (x, y) in faint.drain(..) {
                covered_sideways.set(y, x, false);
            }
            let (upright, left_faint) = bars(&sideways, &sideways, &mut covered_sideways, pen, last_row);
            strokes.extend(upright.into_iter().map(|((x1, y1), (x2, y2))| ((y1, x1), (y2, x2))));
            covered = covered_sideways.transposed();
            faint = left_faint.into_iter().map(|(x, y)| (y, x)).collect();
        }
    }

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
///
/// Also returns the cells that a bar inks only faintly, where the pen starts its
/// strokes thin: the first few cells of the bar's `faint_row` (counting from its top
/// row). They are marked as covered like the rest.
fn bars(ink: &Grid, wanted: &Grid, covered: &mut Grid, pen: &PenModel, faint_row: usize) -> (Vec<Segment>, Vec<(usize, usize)>) {
    let mut strokes = Vec::new();
    let mut faint = Vec::new();
    let rows = pen.rows;
    if ink.height < rows {
        return (strokes, faint);
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
                let (from, to) = (start as f32 + PenModel::PAST_END, end as f32 - PenModel::PAST_END);
                strokes.push(((from, middle), (to, middle)));
                for y in top..top + rows {
                    for x in start..end {
                        covered.set(x, y, true);
                    }
                }
                let thin = pen.thin_start(to - from);
                faint.extend((start + 1..(start + 1 + thin).min(end)).map(|x| (x, top + faint_row)));
            }
        }
    }

    (strokes, faint)
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
/// It is a pixel on the middle of the stroke's line, between its two ends, that no
/// earlier stroke touches: not with its ink, and not with the faint edge that a
/// slanting stroke has. So ink of any strength there means that the device has drawn
/// this stroke or a later one, and so has read every stroke up to this one. Where it
/// can be, it is a pixel that no later stroke touches either, and then ink there means
/// this very stroke was drawn. A stroke with no such pixel has nothing to watch.
///
/// The pixels around a stroke's ends are not used. They are not inked evenly: the
/// device draws the two sides of a stroke a pixel out of step with each other.
pub fn watch_pixels(strokes: &[Stroke], page_size: (u32, u32), pen: &PenModel) -> Vec<Option<(u32, u32)>> {
    let width = page_size.0 as usize;
    let ends = |stroke: &Stroke| match (stroke.points.first(), stroke.points.last()) {
        (Some(&first), Some(&last)) => Some((to_pixels(first, page_size), to_pixels(last, page_size))),
        _ => None,
    };

    // How many strokes touch each pixel
    let mut times_touched = vec![0u8; width * page_size.1 as usize];
    for (from, to) in strokes.iter().filter_map(ends) {
        for_each_pixel_near(from, to, page_size, pen, PenModel::soft_edge(from, to), |x, y, _, _| {
            let count = &mut times_touched[y * width + x];
            *count = count.saturating_add(1);
        });
    }

    let mut touched_before = vec![false; times_touched.len()];
    strokes
        .iter()
        .map(|stroke| {
            let (from, to) = ends(stroke)?;
            // The best pixel is this stroke's alone, and halfway along
            let mut best = None;
            let mut best_rank = (true, f32::INFINITY);
            for_each_pixel_near(from, to, page_size, pen, 0.0, |x, y, from_line, along| {
                // Pixels the pen is sure to ink black: those along the middle of its line.
                // A pen that starts its strokes thin (the kind that needs a tail) is only
                // sure of them past the first third.
                let on_the_middle = from_line <= 0.6;
                let past_thin_start = pen.tail == 0.0 || along >= 0.35;
                if touched_before[y * width + x] || !on_the_middle || !past_thin_start || along <= 0.0 || along >= 1.0 {
                    return;
                }
                let rank = (times_touched[y * width + x] > 1, (along - 0.5).abs());
                if rank < best_rank {
                    best_rank = rank;
                    best = Some((x as u32, y as u32));
                }
            });
            for_each_pixel_near(from, to, page_size, pen, PenModel::soft_edge(from, to), |x, y, _, _| {
                touched_before[y * width + x] = true;
            });

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
        for_each_pixel_near(to_pixels(pair[0], page_size), to_pixels(pair[1], page_size), page_size, pen, 0.0, &mut visit);
    }
}

/// The same, for one straight stroke between two points in device pixels, and
/// reaching `margin` pixels further than its ink does.
fn for_each_pixel_near(from: Point, to: Point, page_size: (u32, u32), pen: &PenModel, margin: f32, mut visit: impl FnMut(usize, usize, f32, f32)) {
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
    let beyond = PenModel::PAST_END + pen.half_width() + 2.0 * margin;
    let reach = beyond * (1.0 + slope * slope).sqrt() + 1.0;

    let first_a = (from_a.min(to_a) - beyond).floor().max(0.0) as usize;
    let last_a = ((from_a.max(to_a) + beyond).ceil().max(0.0) as usize).min(size_a.saturating_sub(1) as usize);
    for a in first_a..=last_a {
        // Where the stroke's line is at this column, kept within the stroke's own span
        let along = (a as f32 + 0.5).clamp(from_a.min(to_a), from_a.max(to_a));
        let line_b = from_b + (along - from_a) * slope;
        let first_b = (line_b - reach).floor().max(0.0) as usize;
        let last_b = ((line_b + reach).ceil().max(0.0) as usize).min(size_b.saturating_sub(1) as usize);
        for b in first_b..=last_b {
            let (x, y) = if steep { (b, a) } else { (a, b) };
            if let Some((from_line, along)) = pen.reaches(from, to, (x as f32 + 0.5, y as f32 + 0.5), margin) {
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
    fn a_pen_that_starts_thin_has_its_bars_crossed_near_their_starts() {
        // Forty across and nine down: three bars across it, each thirty-eight long
        let mut ink = Grid::new(44, 13);
        for y in 2..11 {
            for x in 2..42 {
                ink.set(x, y, true);
            }
        }
        let strokes = plan_ink(&ink, Grid::new(44, 13), BALLPOINT);

        let level: Vec<&Segment> = strokes.iter().filter(|(from, to)| from.1 == to.1).collect();
        let upright: Vec<&Segment> = strokes.iter().filter(|(from, to)| from.0 == to.0).collect();
        assert_eq!(level.iter().filter(|(from, to)| to.0 - from.0 == 38.0).count(), 3, "{strokes:?}");
        // Each long bar is faint along its top row for its first ten pixels or so, so
        // bars run down the block there: three pixels wide, side by side
        assert!(upright.len() >= 3, "{strokes:?}");
        for x in 3..12 {
            assert!(
                upright.iter().any(|(from, _)| (from.0 - (x as f32 + 0.5)).abs() <= 1.5),
                "nothing crosses column {x}: {strokes:?}"
            );
        }
        // A fineliner draws evenly from the start, and needs none of that
        let fineliner = plan_ink(&ink, Grid::new(44, 13), FINELINER);
        assert!(fineliner.iter().all(|(from, to)| from.1 == to.1), "{fineliner:?}");
    }

    #[test]
    fn a_wider_pen_fills_a_block_with_fewer_bars() {
        let rows = [
            "..........",
            ".########.",
            ".########.",
            ".########.",
            ".########.",
            ".########.",
            ".########.",
            ".########.",
            "..........",
        ];
        let strokes = plan_ink(&grid_from(&rows), Grid::new(rows[0].len(), rows.len()), BALLPOINT);

        assert_eq!(draw(&strokes, 10, 9, BALLPOINT), rows);
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
    fn each_colour_is_drawn_in_the_nearest_ink_and_white_not_at_all() {
        let svg = r##"<svg width="768" height="1024" xmlns="http://www.w3.org/2000/svg">
            <rect width="768" height="1024" fill="white"/>
            <rect x="100" y="100" width="60" height="40" fill="#222"/>
            <rect x="300" y="100" width="60" height="40" fill="crimson"/>
            <rect x="500" y="100" width="60" height="40" fill="#eee"/>
            <line x1="100" y1="300" x2="400" y2="500" stroke="navy" stroke-width="4"/>
            <line x1="100" y1="600" x2="400" y2="800" stroke="white" stroke-width="4"/>
        </svg>"##;
        let page_size = (768, 1024);

        let layers = plan_drawing(svg, page_size, FINELINER).unwrap();

        let inks: Vec<Ink> = layers.iter().map(|layer| layer.0).collect();
        assert_eq!(inks, vec![Ink::Black, Ink::Blue, Ink::Red]);
        // Each layer inks its own shape and nothing else: the white background and line, and the pale block, are bare
        let inked = |ink: Ink, x: usize, y: usize| {
            let strokes = &layers.iter().find(|layer| layer.0 == ink).unwrap().1;
            simulate(strokes, page_size, FINELINER)[y][x]
        };
        assert!(inked(Ink::Black, 130, 120) && !inked(Ink::Black, 330, 120) && !inked(Ink::Black, 530, 120));
        assert!(inked(Ink::Red, 330, 120) && !inked(Ink::Red, 130, 120));
        assert!(inked(Ink::Blue, 250, 400) && !inked(Ink::Blue, 250, 700));
        assert!(Ink::ALL.iter().all(|&ink| !layers.iter().any(|layer| layer.0 == ink) || !inked(ink, 20, 20)));
    }

    #[test]
    fn a_line_is_not_followed_where_something_is_drawn_over_it() {
        // A slanting line with a white block over its middle
        let svg = r#"<svg width="768" height="1024" xmlns="http://www.w3.org/2000/svg">
            <line x1="100" y1="100" x2="500" y2="400" stroke="black" stroke-width="2"/>
            <rect x="250" y="150" width="100" height="200" fill="white"/>
        </svg>"#;
        let page_size = (768, 1024);

        let strokes = plan_strokes(svg, page_size, FINELINER).unwrap();
        let drawn = simulate(&strokes, page_size, FINELINER);

        assert!(drawn[175][200] && drawn[325][400], "the line is drawn either side of the block");
        assert!(!drawn[250][300], "and not under it");
    }

    #[test]
    fn shapes_are_drawn_one_at_a_time_in_reading_order() {
        // Three blocks on one line and one on the next, the second line starting further left
        let mut ink = Grid::new(60, 40);
        let blocks = [(30, 4), (4, 6), (46, 5), (2, 24)];
        for &(left, top) in &blocks {
            for y in top..top + 8 {
                for x in left..left + 8 {
                    ink.set(x, y, true);
                }
            }
        }
        // Planned a row of the page at a time, the blocks' strokes are mixed together
        let strokes = plan_ink(&ink, Grid::new(60, 40), FINELINER);
        let block_of = |&(from, _): &Segment| {
            blocks
                .iter()
                .position(|&(left, _)| from.0 >= left as f32 && from.0 < left as f32 + 8.0)
                .unwrap()
        };
        let planned: Vec<usize> = strokes.iter().map(block_of).collect();
        assert_ne!(planned, {
            let mut sorted = planned.clone();
            sorted.sort_unstable();
            sorted
        });

        let order = drawing_order(&strokes, &ink);
        let drawn: Vec<usize> = order.iter().map(|&index| block_of(&strokes[index])).collect();

        // The first line from left to right, then the second: four bars each
        let expected: Vec<usize> = [1, 0, 2, 3].iter().flat_map(|&block| [block; 4]).collect();
        assert_eq!(drawn, expected);
        // And each block's own strokes in the order they were planned
        for block in 0..4 {
            let of_block = |indexes: &mut dyn Iterator<Item = usize>| indexes.filter(|&index| block_of(&strokes[index]) == block).collect::<Vec<_>>();
            assert_eq!(of_block(&mut order.iter().copied()), of_block(&mut (0..strokes.len())));
        }
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
    fn a_slanting_stroke_beside_an_earlier_one_has_nothing_to_watch() {
        let stroke = |points: [Point; 2]| Stroke {
            points: points.to_vec(),
            pressure: FULL_PRESSURE,
        };
        // Two passes along a slanting line, a little over a pixel apart, and a third well clear
        let strokes = [
            stroke([(10.0, 10.0), (50.0, 40.0)]),
            stroke([(10.7, 9.1), (50.7, 39.1)]),
            stroke([(14.0, 4.7), (54.0, 34.7)]),
        ];

        let watched = watch_pixels(&strokes, (768, 1024), FINELINER);

        // The second runs through the faint edge of the first, which would pass for its ink
        assert!(watched[0].is_some());
        assert_eq!(watched[1], None);
        assert!(watched[2].is_some());
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
        let wanted = crate::util::svg_to_alpha_bitmap(svg, page_size.0, page_size.1).unwrap();

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
