//! Strokes that follow the lines of an SVG.
//!
//! A slanting or curving line drawn as bars takes a short bar for every row or two of
//! pixels it crosses. A pen draws it in one go. So the stroked paths of an SVG are
//! followed along their length, side by side as many times as it takes to make up each
//! line's width. Curves are followed in short straight steps, because the device only
//! draws a stroke exactly when it is one straight line.
//!
//! Stretches that run straight across or down the page are left out: bars draw those
//! in as few strokes, and crisply. So is everything that is not a plain line, such as
//! fills and text. The bars are planned afterwards from whatever these strokes do not ink.

use resvg::usvg::{self, tiny_skia_path, Group, LineCap, Node, Transform, Tree};

use crate::stroke_plan::{PenModel, Point, Segment};

/// How far a curve may stray from the straight steps that stand in for it, in pixels.
const FLATNESS: f32 = 0.2;

/// A stretch straight across or down the page that is at least this long, in pixels, is left to the bars.
const LEAVE_TO_BARS: f32 = 20.0;

/// How far a step may lean and still count as straight across or down the page, in pixels.
const LEVEL: f32 = 0.75;

/// How much neighbouring passes along a line overlap, in pixels. The edge of a pen
/// line is not quite black, and two edges side by side would show as a pale seam.
const PASS_OVERLAP: f32 = 0.4;

/// A line no more than this much wider than the pen is drawn with a single pass.
const NEAR_ENOUGH: f32 = 0.5;

/// The shortest step worth a stroke of its own, in pixels.
const SHORTEST_STEP: f32 = 1.0;

/// A path as straight steps between points, in page pixels.
#[derive(Debug, Clone, PartialEq)]
struct Polyline {
    points: Vec<Point>,
    /// Whether the last point joins back up with the first
    closed: bool,
}

/// The strokes that draw the lines of an SVG with `pen`, in page pixels.
pub(crate) fn along_paths(tree: &Tree, page_size: (u32, u32), pen: &PenModel) -> Vec<Segment> {
    let to_page = Transform::from_scale(page_size.0 as f32 / tree.size().width(), page_size.1 as f32 / tree.size().height());
    let mut strokes = Vec::new();
    visit_group(tree.root(), to_page, pen, &mut strokes);
    strokes
}

fn visit_group(group: &Group, to_page: Transform, pen: &PenModel, strokes: &mut Vec<Segment>) {
    // What is clipped, masked, filtered or faint is not simply where its paths are
    if group.clip_path().is_some() || group.mask().is_some() || !group.filters().is_empty() || group.opacity().get() < 0.5 {
        return;
    }
    for node in group.children() {
        match node {
            Node::Group(group) => visit_group(group, to_page, pen, strokes),
            Node::Path(path) => strokes.extend(path_strokes(path, to_page, pen)),
            Node::Text(text) => visit_group(text.flattened(), to_page, pen, strokes),
            Node::Image(_) => {}
        }
    }
}

/// The strokes that draw the line of one path, if it has one.
fn path_strokes(path: &usvg::Path, to_page: Transform, pen: &PenModel) -> Vec<Segment> {
    let Some(stroke) = path.stroke().filter(|stroke| path.is_visible() && stroke.opacity().get() >= 0.5) else {
        return Vec::new();
    };

    // A line keeps an even width only if it is scaled the same in every direction
    let transform = to_page.pre_concat(path.abs_transform());
    let (across, down) = ((transform.sx).hypot(transform.ky), (transform.kx).hypot(transform.sy));
    let skew = transform.sx * transform.kx + transform.ky * transform.sy;
    if (across - down).abs() > 0.02 * across.max(down) || skew.abs() > 0.02 * across * down {
        return Vec::new();
    }
    let scale = (across + down) / 2.0;
    let width = stroke.width().get() * scale;

    let mut polylines = flatten(path.data(), transform);
    if let Some(dashes) = stroke.dasharray() {
        let dashes: Vec<f32> = dashes.iter().map(|dash| dash * scale).collect();
        polylines = polylines
            .iter()
            .flat_map(|polyline| dash(polyline, &dashes, stroke.dashoffset() * scale))
            .collect();
    }

    polylines
        .iter()
        .flat_map(|polyline| line_strokes(polyline, width, stroke.linecap(), pen))
        .collect()
}

/// Turn a path into straight steps, in page pixels.
fn flatten(path: &tiny_skia_path::Path, transform: Transform) -> Vec<Polyline> {
    let to_page = |point: tiny_skia_path::Point| {
        (
            transform.sx * point.x + transform.kx * point.y + transform.tx,
            transform.ky * point.x + transform.sy * point.y + transform.ty,
        )
    };

    let mut polylines: Vec<Polyline> = Vec::new();
    for segment in path.segments() {
        match segment {
            tiny_skia_path::PathSegment::MoveTo(point) => polylines.push(Polyline {
                points: vec![to_page(point)],
                closed: false,
            }),
            tiny_skia_path::PathSegment::Close => {
                if let Some(polyline) = polylines.last_mut() {
                    polyline.closed = true;
                }
            }
            other => {
                let Some(polyline) = polylines.last_mut() else {
                    continue;
                };
                let from = polyline.points[polyline.points.len() - 1];
                match other {
                    tiny_skia_path::PathSegment::LineTo(to) => polyline.points.push(to_page(to)),
                    tiny_skia_path::PathSegment::QuadTo(control, to) => {
                        // The same curve as a cubic
                        let (control, to) = (to_page(control), to_page(to));
                        let towards = |end: Point| (end.0 + (control.0 - end.0) * 2.0 / 3.0, end.1 + (control.1 - end.1) * 2.0 / 3.0);
                        flatten_cubic(from, towards(from), towards(to), to, 12, &mut polyline.points);
                    }
                    tiny_skia_path::PathSegment::CubicTo(control1, control2, to) => {
                        flatten_cubic(from, to_page(control1), to_page(control2), to_page(to), 12, &mut polyline.points);
                    }
                    _ => {}
                }
            }
        }
    }

    for polyline in &mut polylines {
        polyline.points.dedup_by(|a, b| distance(*a, *b) < 0.01);
        // A closed path's last point is its first again; one copy is enough
        if polyline.closed && polyline.points.len() > 1 && distance(polyline.points[0], polyline.points[polyline.points.len() - 1]) < 0.01 {
            polyline.points.pop();
        }
    }
    polylines.retain(|polyline| polyline.points.len() >= 2);
    polylines
}

/// Add the straight steps that stand in for a cubic curve to `points` (which already ends with `from`).
fn flatten_cubic(from: Point, control1: Point, control2: Point, to: Point, depth: u32, points: &mut Vec<Point>) {
    let off_line = |point: Point| {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = dx.hypot(dy);
        if length < 0.001 {
            distance(point, from)
        } else {
            ((point.0 - from.0) * dy - (point.1 - from.1) * dx).abs() / length
        }
    };
    if depth == 0 || off_line(control1).max(off_line(control2)) <= FLATNESS {
        points.push(to);
        return;
    }

    // Split the curve in two at its middle
    let between = |a: Point, b: Point| ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    let (a, b, c) = (between(from, control1), between(control1, control2), between(control2, to));
    let (ab, bc) = (between(a, b), between(b, c));
    let middle = between(ab, bc);
    flatten_cubic(from, a, ab, middle, depth - 1, points);
    flatten_cubic(middle, bc, c, to, depth - 1, points);
}

/// Cut a path into the dashes of a dashed line. `dashes` are lengths in pixels: drawn, skipped, drawn, and so on.
fn dash(polyline: &Polyline, dashes: &[f32], offset: f32) -> Vec<Polyline> {
    let period: f32 = dashes.iter().sum();
    if dashes.is_empty() || period <= 0.0 || dashes.iter().any(|&dash| dash < 0.0) {
        return vec![polyline.clone()];
    }

    let mut points = polyline.points.clone();
    if polyline.closed {
        points.push(points[0]);
    }

    // Where in the pattern the path starts
    let mut index = 0;
    let mut left = dashes[0];
    let mut skip = offset.rem_euclid(period);
    while skip >= left {
        skip -= left;
        index = (index + 1) % dashes.len();
        left = dashes[index];
    }
    left -= skip;

    let mut pieces = Vec::new();
    let mut piece: Vec<Point> = vec![points[0]];
    for pair in points.windows(2) {
        let (mut from, to) = (pair[0], pair[1]);
        loop {
            let length = distance(from, to);
            if length <= left {
                left -= length;
                piece.push(to);
                break;
            }
            let cut = (from.0 + (to.0 - from.0) * left / length, from.1 + (to.1 - from.1) * left / length);
            piece.push(cut);
            // Even places in the pattern are drawn
            if index % 2 == 0 {
                pieces.push(std::mem::take(&mut piece));
            }
            piece = vec![cut];
            from = cut;
            index = (index + 1) % dashes.len();
            left = dashes[index];
        }
    }
    if index % 2 == 0 {
        pieces.push(piece);
    }

    pieces
        .into_iter()
        .map(|mut points| {
            points.dedup_by(|a, b| distance(*a, *b) < 0.01);
            Polyline { points, closed: false }
        })
        .filter(|piece| piece.points.len() >= 2)
        .collect()
}

/// The strokes that draw a line `width` pixels wide along a path.
fn line_strokes(polyline: &Polyline, width: f32, cap: LineCap, pen: &PenModel) -> Vec<Segment> {
    let pen_width = pen.rows as f32;

    // How far from the middle of the line each pass runs, so that the outermost
    // passes reach the line's edges
    let spread = (width - pen_width).max(0.0);
    let passes = if spread <= NEAR_ENOUGH {
        1
    } else {
        (spread / (pen_width - PASS_OVERLAP)).ceil() as usize + 1
    };
    let offsets: Vec<f32> = (0..passes)
        .map(|pass| {
            if passes == 1 {
                0.0
            } else {
                spread * (pass as f32 / (passes - 1) as f32 - 0.5)
            }
        })
        .collect();

    let mut strokes = Vec::new();
    for stretch in stretches_for_the_pen(polyline) {
        for &offset in &offsets {
            let mut points = offset_path(&stretch.points, stretch.closed, offset);
            if stretch.closed {
                points.push(points[0]);
            }

            // The pen's ink carries on past the ends of a stroke, where the line's
            // own end may stop short of that or go further
            let past_end = match cap {
                LineCap::Butt => 0.0,
                LineCap::Square => width / 2.0,
                LineCap::Round => ((width / 2.0).powi(2) - offset.powi(2)).max(0.0).sqrt(),
            } - PenModel::PAST_END;
            if stretch.starts_at_end {
                points.reverse();
                move_end(&mut points, past_end);
                points.reverse();
            }
            if stretch.stops_at_end {
                move_end(&mut points, past_end);
            }

            strokes.extend(steps(&points));
        }
    }
    strokes
}

/// A part of a path that is drawn by following it.
struct Stretch {
    points: Vec<Point>,
    closed: bool,
    /// Whether the stretch starts, and stops, where the path itself does
    starts_at_end: bool,
    stops_at_end: bool,
}

/// The parts of a path to follow with the pen: all of it but the long stretches that
/// run straight across or down the page.
fn stretches_for_the_pen(polyline: &Polyline) -> Vec<Stretch> {
    let mut points = polyline.points.clone();
    if polyline.closed {
        points.push(points[0]);
    }
    let for_bars = |from: Point, to: Point| {
        let (dx, dy) = ((to.0 - from.0).abs(), (to.1 - from.1).abs());
        (dx <= LEVEL || dy <= LEVEL) && dx.max(dy) >= LEAVE_TO_BARS
    };
    let steps: Vec<bool> = points.windows(2).map(|pair| !for_bars(pair[0], pair[1])).collect();

    if steps.iter().all(|&for_pen| for_pen) {
        return vec![Stretch {
            points: polyline.points.clone(),
            closed: polyline.closed,
            starts_at_end: !polyline.closed,
            stops_at_end: !polyline.closed,
        }];
    }

    let mut stretches = Vec::new();
    let mut start = 0;
    while start < steps.len() {
        if !steps[start] {
            start += 1;
            continue;
        }
        let mut stop = start;
        while stop < steps.len() && steps[stop] {
            stop += 1;
        }
        stretches.push(Stretch {
            points: points[start..=stop].to_vec(),
            closed: false,
            starts_at_end: start == 0 && !polyline.closed,
            stops_at_end: stop == steps.len() && !polyline.closed,
        });
        start = stop;
    }
    stretches
}

/// A path moved sideways by `offset` pixels (to its left as it travels, or to its right if negative).
fn offset_path(points: &[Point], closed: bool, offset: f32) -> Vec<Point> {
    if offset == 0.0 {
        return points.to_vec();
    }
    let count = points.len();
    let sideways = |from: Point, to: Point| {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = dx.hypot(dy).max(f32::MIN_POSITIVE);
        (dy / length, -dx / length)
    };
    let moved = |point: Point, direction: (f32, f32), by: f32| (point.0 + direction.0 * by, point.1 + direction.1 * by);

    let mut shifted = Vec::with_capacity(count + 4);
    for index in 0..count {
        let before = if index > 0 {
            Some(index - 1)
        } else if closed {
            Some(count - 1)
        } else {
            None
        };
        let after = if index + 1 < count {
            Some(index + 1)
        } else if closed {
            Some(0)
        } else {
            None
        };
        let point = points[index];
        match (
            before.map(|before| sideways(points[before], point)),
            after.map(|after| sideways(point, points[after])),
        ) {
            (Some(arriving), Some(leaving)) => {
                let turn = arriving.0 * leaving.0 + arriving.1 * leaving.1;
                if turn >= 0.0 {
                    // Where the two shifted steps meet
                    let meet = ((arriving.0 + leaving.0) / (1.0 + turn), (arriving.1 + leaving.1) / (1.0 + turn));
                    shifted.push(moved(point, meet, offset));
                } else {
                    // A sharp corner: they would meet a long way off, so cut it short
                    shifted.push(moved(point, arriving, offset));
                    shifted.push(moved(point, leaving, offset));
                }
            }
            (Some(direction), None) | (None, Some(direction)) => shifted.push(moved(point, direction, offset)),
            (None, None) => shifted.push(point),
        }
    }
    shifted
}

/// Move the end of a path on by `by` pixels in the direction it was going, or back along the path if negative.
fn move_end(points: &mut Vec<Point>, by: f32) {
    let mut back = -by;
    while points.len() >= 2 {
        let (last, before) = (points[points.len() - 1], points[points.len() - 2]);
        let length = distance(before, last);
        if back < length || points.len() == 2 {
            // Never back past the start of the path's first step: leave a stub to draw
            let back = back.min(length - SHORTEST_STEP.min(length / 2.0));
            let end = points.len() - 1;
            points[end] = (last.0 - (last.0 - before.0) * back / length, last.1 - (last.1 - before.1) * back / length);
            return;
        }
        back -= length;
        points.pop();
    }
}

/// The straight strokes that draw a path, step by step. Steps too short to leave ink
/// are joined to the next.
fn steps(points: &[Point]) -> Vec<Segment> {
    let mut strokes = Vec::new();
    let Some(&first) = points.first() else {
        return strokes;
    };
    let mut from = first;
    for (index, &to) in points.iter().enumerate().skip(1) {
        let last = index == points.len() - 1;
        if distance(from, to) >= SHORTEST_STEP || (last && strokes.is_empty() && distance(from, to) > 0.0) {
            strokes.push((from, to));
            from = to;
        }
    }
    strokes
}

fn distance(a: Point, b: Point) -> f32 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::parse_svg;

    const FINELINER: &PenModel = &PenModel::FINELINER_THIN;
    const BALLPOINT: &PenModel = &PenModel::BALLPOINT_MEDIUM;

    fn strokes_for(body: &str, pen: &PenModel) -> Vec<Segment> {
        let svg = format!(r#"<svg width="400" height="400" xmlns="http://www.w3.org/2000/svg">{body}</svg>"#);
        along_paths(&parse_svg(&svg).unwrap(), (400, 400), pen)
    }

    fn length((from, to): &Segment) -> f32 {
        distance(*from, *to)
    }

    #[test]
    fn a_slanting_line_is_one_stroke_for_each_pass_across_its_width() {
        let line = r#"<line x1="50" y1="50" x2="350" y2="250" stroke="black" stroke-width="5"/>"#;

        // 5 wide with a pen 2 wide: three passes, the outer ones along the line's edges
        let strokes = strokes_for(line, FINELINER);
        assert_eq!(strokes.len(), 3, "{strokes:?}");
        // The same line with a pen 3 wide: two
        assert_eq!(strokes_for(line, BALLPOINT).len(), 2);

        // Each runs the length of the line, less the pixel at each end that the pen's ink carries on
        let whole = distance((50.0, 50.0), (350.0, 250.0));
        for stroke in &strokes {
            assert!((length(stroke) - (whole - 2.0)).abs() < 0.01, "{stroke:?}");
        }
        // The outer passes are 1.5 either side of the middle: their edges are the line's edges
        let off_middle = |point: Point| ((point.0 - 50.0) * 200.0 - (point.1 - 50.0) * 300.0) / whole;
        let mut offsets: Vec<f32> = strokes.iter().map(|stroke| off_middle(stroke.0)).collect();
        offsets.sort_by(f32::total_cmp);
        assert!(
            (offsets[0] + 1.5).abs() < 0.01 && offsets[1].abs() < 0.01 && (offsets[2] - 1.5).abs() < 0.01,
            "{offsets:?}"
        );
    }

    #[test]
    fn a_thin_line_is_a_single_pass() {
        let strokes = strokes_for(r#"<line x1="50" y1="50" x2="350" y2="250" stroke="black" stroke-width="2"/>"#, FINELINER);

        assert_eq!(strokes.len(), 1);
    }

    #[test]
    fn lines_straight_across_and_down_are_left_to_the_bars() {
        let strokes = strokes_for(
            r#"<rect x="50" y="50" width="200" height="100" fill="none" stroke="black" stroke-width="4"/>
               <line x1="20" y1="300" x2="380" y2="300" stroke="black" stroke-width="4"/>"#,
            FINELINER,
        );

        assert!(strokes.is_empty(), "{strokes:?}");
    }

    #[test]
    fn fills_and_text_are_left_to_the_bars() {
        let strokes = strokes_for(
            r#"<circle cx="200" cy="200" r="50" fill="black"/><text x="20" y="40" font-size="30">Hello</text>"#,
            FINELINER,
        );

        assert!(strokes.is_empty(), "{strokes:?}");
    }

    #[test]
    fn a_circle_is_followed_round_in_short_steps() {
        let strokes = strokes_for(r#"<circle cx="200" cy="200" r="100" fill="none" stroke="black" stroke-width="2"/>"#, FINELINER);

        // Enough steps to stay within a fifth of a pixel of the circle, and no more than that takes
        assert!((30..=120).contains(&strokes.len()), "{} strokes", strokes.len());
        for (from, to) in &strokes {
            for point in [*from, *to, ((from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0)] {
                let from_centre = distance(point, (200.0, 200.0));
                assert!((from_centre - 100.0).abs() < 0.3, "{point:?} is {from_centre} from the centre");
            }
        }
        // It closes: the last step ends where the first began
        assert!(distance(strokes[0].0, strokes[strokes.len() - 1].1) < 0.01);
    }

    #[test]
    fn a_path_with_a_level_stretch_is_followed_only_where_it_slants() {
        // Up a slope, along the flat, and down again
        let strokes = strokes_for(
            r#"<polyline points="50,300 150,100 300,100 350,300" fill="none" stroke="black" stroke-width="2"/>"#,
            FINELINER,
        );

        assert_eq!(strokes.len(), 2, "{strokes:?}");
        // The slopes run right up to the flat, where the bars take over, and stop a pixel short at the path's own ends
        assert!(distance(strokes[0].1, (150.0, 100.0)) < 0.01, "{strokes:?}");
        assert!(distance(strokes[1].0, (300.0, 100.0)) < 0.01, "{strokes:?}");
        assert!((distance(strokes[0].0, (50.0, 300.0)) - 1.0).abs() < 0.01, "{strokes:?}");
    }

    #[test]
    fn a_dashed_line_is_a_stroke_for_each_dash() {
        let strokes = strokes_for(
            r#"<line x1="50" y1="50" x2="350" y2="350" stroke="black" stroke-width="2" stroke-dasharray="30 20"/>"#,
            FINELINER,
        );

        // The line is about 424 long: nine dashes of 30 with gaps of 20
        assert_eq!(strokes.len(), 9, "{strokes:?}");
        assert!(strokes.iter().take(8).all(|stroke| (length(stroke) - 28.0).abs() < 0.01), "{strokes:?}");
    }

    #[test]
    fn a_corner_is_turned_without_cutting_it() {
        let moved = offset_path(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], false, 2.0);

        // Shifted to the left of travel: up from the first step, right of the second
        assert_eq!(moved, vec![(0.0, -2.0), (12.0, -2.0), (12.0, 10.0)]);
    }

    #[test]
    fn a_squashed_shape_is_left_to_the_bars() {
        // Stretched twice as far across as down, the line would be wider in one direction than the other
        let strokes = strokes_for(
            r#"<g transform="scale(2 1)"><line x1="20" y1="50" x2="150" y2="250" stroke="black" stroke-width="3"/></g>"#,
            FINELINER,
        );

        assert!(strokes.is_empty());
    }
}
