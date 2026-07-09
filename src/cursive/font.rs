use anyhow::Result;
use std::collections::HashMap;

/// One glyph: its advance width and its strokes in font-space coordinates
/// (Y increases upward, baseline at Y=0, per the SVG font convention).
/// `subpaths[0]` is always the main stroke; any remaining subpaths are
/// pen-lift extras (accents, dots) that must never be joined to a
/// neighboring glyph.
#[derive(Debug, Clone)]
pub struct Glyph {
    pub advance: f32,
    pub subpaths: Vec<Vec<(f32, f32)>>,
}

#[derive(Debug, Clone)]
pub struct CursiveFont {
    pub glyphs: HashMap<char, Glyph>,
    pub default_advance: f32,
    pub units_per_em: f32,
    pub x_height: f32,
    pub ascent: f32,
    pub descent: f32,
}

impl CursiveFont {
    /// Parse an SVG 1.1 font document (the format produced by the Evil Mad
    /// Scientist Hershey Text / EMS font distribution: a flat list of
    /// `<glyph unicode="…" horiz-adv-x="…" d="…"/>` elements using only
    /// absolute M/L/C path commands).
    pub fn parse(svg_font_xml: &str) -> Result<Self> {
        // Strip DOCTYPE to work around roxmltree's DTD rejection
        let cleaned_xml = svg_font_xml
            .lines()
            .filter(|line| !line.trim().starts_with("<!DOCTYPE") && !line.trim().starts_with("<!ENTITY"))
            .collect::<Vec<_>>()
            .join("\n");

        let doc = roxmltree::Document::parse(&cleaned_xml)?;

        let font_node = doc
            .descendants()
            .find(|n| n.has_tag_name("font"))
            .ok_or_else(|| anyhow::anyhow!("no <font> element found"))?;
        let default_advance: f32 = font_node
            .attribute("horiz-adv-x")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000.0);

        let face_node = doc
            .descendants()
            .find(|n| n.has_tag_name("font-face"))
            .ok_or_else(|| anyhow::anyhow!("no <font-face> element found"))?;
        let units_per_em: f32 = face_node.attribute("units-per-em").and_then(|s| s.parse().ok()).unwrap_or(1000.0);
        let ascent: f32 = face_node.attribute("ascent").and_then(|s| s.parse().ok()).unwrap_or(800.0);
        let descent: f32 = face_node.attribute("descent").and_then(|s| s.parse().ok()).unwrap_or(-200.0);
        let x_height: f32 = face_node.attribute("x-height").and_then(|s| s.parse().ok()).unwrap_or(300.0);

        let mut glyphs = HashMap::new();
        for node in doc.descendants().filter(|n| n.has_tag_name("glyph")) {
            let Some(unicode_str) = node.attribute("unicode") else {
                continue; // <missing-glyph/> has no unicode attribute
            };
            let Some(unicode) = unicode_str.chars().next() else {
                continue;
            };
            let advance: f32 = node
                .attribute("horiz-adv-x")
                .and_then(|s| s.parse().ok())
                .unwrap_or(default_advance);
            let subpaths = match node.attribute("d") {
                Some(d) if !d.trim().is_empty() => parse_path_to_subpaths(d)?,
                _ => Vec::new(), // e.g. space
            };
            glyphs.insert(unicode, Glyph { advance, subpaths });
        }

        Ok(CursiveFont {
            glyphs,
            default_advance,
            units_per_em,
            x_height,
            ascent,
            descent,
        })
    }

    pub fn embedded_allure() -> Result<Self> {
        let xml = crate::embedded_assets::load_font_asset("EMSAllure.svg");
        Self::parse(&xml)
    }
}

/// Flatten a glyph's `d` path data into one polyline per M-started subpath,
/// reusing the existing svg2polylines flattening logic (handles M/L and,
/// for future fonts, curves) rather than hand-rolling a path parser.
fn parse_path_to_subpaths(d: &str) -> Result<Vec<Vec<(f32, f32)>>> {
    let wrapped = format!(r#"<svg xmlns="http://www.w3.org/2000/svg"><path d="{}"/></svg>"#, d);
    let polylines =
        svg2polylines::parse(&wrapped, 0.5, true).map_err(|e| anyhow::anyhow!("svg2polylines error parsing glyph path: {}", e))?;
    Ok(polylines
        .into_iter()
        .map(|poly| poly.into_iter().map(|p| (p.x as f32, p.y as f32)).collect())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_embedded_allure_font_metrics() {
        let font = CursiveFont::embedded_allure().unwrap();
        assert_eq!(font.units_per_em, 1000.0);
        assert_eq!(font.x_height, 300.0);
        assert_eq!(font.ascent, 800.0);
        assert_eq!(font.descent, -200.0);
    }

    #[test]
    fn parses_full_french_accent_coverage() {
        let font = CursiveFont::embedded_allure().unwrap();
        for ch in "éèàçùâêîôûëïüÉÈÀÇÙÂÊÎÔÛËÏÜ".chars() {
            assert!(font.glyphs.contains_key(&ch), "missing glyph for {:?}", ch);
        }
    }

    #[test]
    fn oe_ligature_is_absent_and_must_be_substituted_upstream() {
        let font = CursiveFont::embedded_allure().unwrap();
        assert!(!font.glyphs.contains_key(&'œ'), "expected œ to be absent from EMS Allure");
    }

    #[test]
    fn plain_letters_have_a_single_main_stroke_subpath() {
        let font = CursiveFont::embedded_allure().unwrap();
        let e = font.glyphs.get(&'e').unwrap();
        assert_eq!(e.subpaths.len(), 1, "'e' should have exactly one subpath (no accent)");
        assert!(e.subpaths[0].len() >= 2);
    }

    #[test]
    fn accented_letters_have_a_separate_accent_subpath() {
        let font = CursiveFont::embedded_allure().unwrap();
        let eacute = font.glyphs.get(&'é').unwrap();
        assert_eq!(eacute.subpaths.len(), 2, "'é' should have a main stroke plus one accent subpath");
    }

    #[test]
    fn letters_join_at_a_consistent_connection_height() {
        // Verified during design research: 'e' exits and 'a'/'m' enter at
        // font-space y ≈ 183, the font's designed joining height.
        let font = CursiveFont::embedded_allure().unwrap();
        let e_exit = *font.glyphs.get(&'e').unwrap().subpaths[0].last().unwrap();
        let a_entry = font.glyphs.get(&'a').unwrap().subpaths[0][0];
        let m_entry = font.glyphs.get(&'m').unwrap().subpaths[0][0];
        assert!((e_exit.1 - 183.0).abs() < 1.0, "e exit y = {}", e_exit.1);
        assert!((a_entry.1 - 183.0).abs() < 1.0, "a entry y = {}", a_entry.1);
        assert!((m_entry.1 - 183.0).abs() < 1.0, "m entry y = {}", m_entry.1);
    }
}
