use crate::cursive::font::CursiveFont;
use crate::cursive::text::normalize;

/// A single continuous pen-down run, in virtual screen coordinates
/// (768×1024, Y increasing downward).
pub type Stroke = Vec<(f32, f32)>;

pub struct Word {
    pub strokes: Vec<Stroke>,
}

#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
    pub max_width: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutConfig {
    pub x_height_px: f32,
    pub line_spacing_mult: f32,
    pub word_spacing_mult: f32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            x_height_px: 28.0,
            line_spacing_mult: 2.2,
            word_spacing_mult: 0.6,
        }
    }
}

/// Font-space Y band (baseline=0, x-height=300) where consecutive glyphs'
/// main strokes are considered joinable. Verified during design research:
/// 'e' exits and 'a'/'m' enter at y≈183. Measured directly from the
/// embedded EMS Allure font: most joining letters (i, t, s, r, b, h, j, k,
/// v, x, m, ...) both enter and exit at y=183. But letters that close a
/// loop before handing off (e.g. 'a', whose main stroke enters at y=183
/// but exits at y=343 after tracing its bowl) exit near the top of the
/// x-height band instead. 80 units of slack was too narrow to bridge that
/// gap (|343-183|=160 exactly); widened to 165 to cover it with a small
/// margin, while still excluding true non-joining extremes (e.g. 'b' exits
/// at y=-9.45, 'w' exits at y=435).
const CONNECTION_ZONE_CENTER: f32 = 183.0;
const CONNECTION_ZONE_HALF_WIDTH: f32 = 165.0;

fn in_connection_zone(y: f32) -> bool {
    (y - CONNECTION_ZONE_CENTER).abs() <= CONNECTION_ZONE_HALF_WIDTH
}

/// Lay out `text` into wrapped lines within `placement.max_width`, placing
/// each glyph left-to-right and converting font-space coordinates
/// (Y-up, baseline 0) into virtual screen-space coordinates (Y-down).
/// Consecutive glyphs whose main-stroke exit/entry both fall in the
/// connection zone are spliced into one continuous stroke so the pen
/// stays down across the join; everything else (word gaps, accents,
/// out-of-zone pairs) becomes a separate stroke (a pen lift).
pub fn layout(font: &CursiveFont, text: &str, placement: &Placement, config: &LayoutConfig) -> Vec<Word> {
    let scale = config.x_height_px / font.x_height;
    let line_height = config.x_height_px * config.line_spacing_mult;
    let word_gap = config.x_height_px * config.word_spacing_mult;

    let normalized = normalize(text);
    let mut words_out = Vec::new();

    let mut cursor_x = placement.x;
    let mut cursor_y = placement.y;

    for raw_word in normalized.split_whitespace() {
        let word_width = measure_word(font, raw_word, scale);
        if cursor_x > placement.x && cursor_x + word_width > placement.x + placement.max_width {
            cursor_x = placement.x;
            cursor_y += line_height;
        }

        let mut strokes: Vec<Stroke> = Vec::new();
        let mut pen_x = cursor_x;
        let mut prev_exit: Option<(f32, f32)> = None;
        let mut prev_main_stroke_index: Option<usize> = None;

        for ch in raw_word.chars() {
            let Some(glyph) = font.glyphs.get(&ch) else {
                log::debug!("cursive layout: skipping unsupported character {:?}", ch);
                continue;
            };

            let mut subpath_iter = glyph.subpaths.iter();
            if let Some(main) = subpath_iter.next() {
                let placed: Stroke = main.iter().map(|&(fx, fy)| (pen_x + fx * scale, cursor_y - fy * scale)).collect();

                let entry = placed.first().copied();
                let can_join = match (prev_exit, entry) {
                    (Some(prev), Some(cur)) => {
                        // Reconstruct original font-space y (baseline=0,
                        // x-height=300) from the scaled screen-space point:
                        // placed.y = cursor_y - fy * scale, so
                        // fy = (cursor_y - placed.y) / scale. Dividing by
                        // `scale` is required here — without it this
                        // recovers fy*scale (a handful of pixel units)
                        // instead of the real font-space y, and the
                        // CONNECTION_ZONE constants (calibrated in
                        // font-space units) would never match.
                        let prev_font_y = (cursor_y - prev.1) / scale;
                        let cur_font_y = (cursor_y - cur.1) / scale;
                        in_connection_zone(prev_font_y) && in_connection_zone(cur_font_y)
                    }
                    _ => false,
                };

                if can_join {
                    if let Some(idx) = prev_main_stroke_index {
                        strokes[idx].extend(placed.iter().copied());
                        prev_main_stroke_index = Some(idx);
                    } else {
                        // Defensive fallback: can_join should only be true
                        // when there was a previous main stroke to join to,
                        // but if that invariant is ever broken, push a new
                        // stroke instead of panicking on an invalid index.
                        strokes.push(placed.clone());
                        prev_main_stroke_index = Some(strokes.len() - 1);
                    }
                } else {
                    strokes.push(placed.clone());
                    prev_main_stroke_index = Some(strokes.len() - 1);
                }

                prev_exit = placed.last().copied();
            } else {
                // No main subpath at all for this glyph: nothing to join
                // from or to, so clear join state to avoid splicing the
                // next glyph onto an unrelated earlier stroke.
                prev_exit = None;
                prev_main_stroke_index = None;
            }

            for accent in subpath_iter {
                let placed: Stroke = accent.iter().map(|&(fx, fy)| (pen_x + fx * scale, cursor_y - fy * scale)).collect();
                strokes.push(placed);
            }

            pen_x += glyph.advance * scale;
        }

        // Always emit a Word per whitespace-split token, even if every
        // character in it was unsupported and produced zero strokes: this
        // keeps output words aligned 1:1 with input tokens (see
        // `unknown_characters_are_skipped_without_panicking`) rather than
        // silently collapsing all-unknown tokens out of the result.
        words_out.push(Word { strokes });

        cursor_x = pen_x + word_gap;
    }

    words_out
}

fn measure_word(font: &CursiveFont, word: &str, scale: f32) -> f32 {
    word.chars()
        .map(|ch| font.glyphs.get(&ch).map(|g| g.advance).unwrap_or(font.default_advance) * scale)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_font() -> CursiveFont {
        CursiveFont::embedded_allure().unwrap()
    }

    #[test]
    fn short_text_fits_on_one_line() {
        let font = test_font();
        let placement = Placement {
            x: 50.0,
            y: 200.0,
            max_width: 600.0,
        };
        let words = layout(&font, "Bonjour", &placement, &LayoutConfig::default());
        assert_eq!(words.len(), 1);
        assert!(!words[0].strokes.is_empty());
    }

    #[test]
    fn long_text_wraps_to_multiple_lines() {
        let font = test_font();
        // Narrow box forces a wrap after a couple of words.
        let placement = Placement {
            x: 50.0,
            y: 200.0,
            max_width: 150.0,
        };
        let words = layout(&font, "Bonjour je m'appelle Tom Jedusor où étais-tu", &placement, &LayoutConfig::default());
        assert!(words.len() >= 5, "expected multiple words laid out, got {}", words.len());
        let ys: Vec<f32> = words.iter().flat_map(|w| w.strokes.iter().flat_map(|s| s.iter().map(|p| p.1))).collect();
        let min_y = ys.iter().cloned().fold(f32::MAX, f32::min);
        let max_y = ys.iter().cloned().fold(f32::MIN, f32::max);
        assert!(max_y - min_y > LayoutConfig::default().x_height_px, "expected at least one line wrap");
    }

    #[test]
    fn joined_letters_produce_fewer_strokes_than_glyphs() {
        let font = test_font();
        let placement = Placement {
            x: 50.0,
            y: 200.0,
            max_width: 600.0,
        };
        // "am" — both 'a' and 'm' enter/exit at the verified connection height,
        // so they should be spliced into a single continuous stroke.
        let words = layout(&font, "am", &placement, &LayoutConfig::default());
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].strokes.len(), 1, "expected 'a' and 'm' to join into one stroke");
    }

    #[test]
    fn accents_remain_separate_pen_lifts() {
        let font = test_font();
        let placement = Placement {
            x: 50.0,
            y: 200.0,
            max_width: 600.0,
        };
        let words = layout(&font, "é", &placement, &LayoutConfig::default());
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].strokes.len(), 2, "expected main stroke + accent as separate strokes");
    }

    #[test]
    fn accented_letter_followed_by_joining_letter_joins_the_main_stroke_not_the_accent() {
        let font = test_font();
        let placement = Placement {
            x: 50.0,
            y: 200.0,
            max_width: 600.0,
        };
        // 'é' exits its main stroke at font-space y=183 (same as plain 'e'), and 't' enters
        // at font-space y=183 too — both are in the connection zone, so 't' should join onto
        // é's MAIN stroke, not its accent mark.
        //
        // Adjustment from the originally suggested test: in the embedded EMS Allure font,
        // 't' itself has two subpaths (a main stroke plus a short separate crossbar stroke,
        // structurally identical to how an accent is stored), so a fully-joined "ét" yields
        // three strokes, not two: é's main stroke extended with t's main stroke, é's accent
        // mark (untouched), and t's own crossbar (untouched). Verified via debug printing of
        // `word.strokes.iter().map(|s| s.len())`, which showed [30, 2, 2] — the joined main
        // stroke growing from é's 17 points to 30 (absorbing t's 13-point main stroke), while
        // both short 2-point strokes (é's accent, t's crossbar) stayed short.
        let eacute_main_len = font.glyphs.get(&'é').unwrap().subpaths[0].len();

        let words = layout(&font, "ét", &placement, &LayoutConfig::default());
        assert_eq!(words.len(), 1);
        let word = &words[0];
        assert_eq!(
            word.strokes.len(),
            3,
            "expected é's main stroke (joined with t's main stroke), é's accent, and t's crossbar; got lens {:?}",
            word.strokes.iter().map(|s| s.len()).collect::<Vec<_>>()
        );

        // The join must have landed on é's MAIN stroke: it should have grown beyond
        // its own point count by absorbing t's main stroke.
        assert!(
            word.strokes[0].len() > eacute_main_len,
            "expected é's main stroke to have absorbed t's main stroke (got {} points, é's main alone has {})",
            word.strokes[0].len(),
            eacute_main_len
        );

        // é's accent stroke (pushed right after its main stroke, before 't' is even
        // processed) must stay short — it must NOT have had t's stroke data spliced
        // onto it. é's accent subpath in the font has only 2 points.
        let eacute_accent_len = word.strokes[1].len();
        assert!(
            eacute_accent_len <= 4,
            "é's accent stroke should remain its short diacritic mark, not have 't' joined onto it (got {} points)",
            eacute_accent_len
        );
    }

    #[test]
    fn unknown_characters_are_skipped_without_panicking() {
        let font = test_font();
        let placement = Placement {
            x: 50.0,
            y: 200.0,
            max_width: 600.0,
        };
        let words = layout(&font, "hello 中 world", &placement, &LayoutConfig::default());
        assert_eq!(words.len(), 3);
    }
}
