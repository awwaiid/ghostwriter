//! Cursive handwriting rendering: turns plain text into animated,
//! connected single-line cursive strokes drawn by the pen.

pub mod animate;
pub mod debug;
pub mod font;
pub mod humanize;
pub mod layout;
pub mod text;

use anyhow::Result;

use crate::pen::Pen;
pub use font::CursiveFont;
pub use humanize::HumanizeConfig;
pub use layout::{LayoutConfig, Placement};

#[derive(Debug, Clone)]
pub struct CursiveConfig {
    pub layout: LayoutConfig,
    pub humanize: HumanizeConfig,
    pub word_gap_ms: u64,
}

impl Default for CursiveConfig {
    fn default() -> Self {
        Self {
            layout: LayoutConfig::default(),
            humanize: HumanizeConfig::default(),
            word_gap_ms: 220,
        }
    }
}

/// Lay out, humanize, and draw `text` through `pen` as connected cursive
/// handwriting, anchored at `placement`. `seed` controls the humanizer's
/// per-word noise (vary it per answer so repeated words don't look
/// identical, keep it fixed in tests for reproducibility).
pub fn write_cursive(pen: &mut Pen, font: &CursiveFont, text: &str, placement: &Placement, config: &CursiveConfig, seed: u64) -> Result<()> {
    let words = layout::layout(font, text, placement, &config.layout);
    let humanized: Vec<_> = words
        .iter()
        .enumerate()
        .map(|(i, w)| humanize::humanize_word(w, seed.wrapping_add(i as u64), config.layout.x_height_px, &config.humanize))
        .collect();
    animate::draw_words(pen, &humanized, config.word_gap_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pen::Pen;

    #[test]
    fn write_cursive_runs_headless_without_a_device() {
        let font = CursiveFont::embedded_allure().unwrap();
        let mut pen = Pen::new(true); // no_draw = true, device: None
        let placement = Placement { x: 40.0, y: 200.0, max_width: 700.0 };
        let result = write_cursive(&mut pen, &font, "Bonjour", &placement, &CursiveConfig::default(), 1);
        assert!(result.is_ok());
    }
}
