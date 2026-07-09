use anyhow::Result;
use image::{GrayImage, Luma};
use imageproc::drawing::draw_line_segment_mut;

use crate::cursive::layout::Word;

/// Render laid-out (and optionally humanized) words to a white-background
/// grayscale PNG by drawing straight line segments between consecutive
/// stroke points. Used for visual/golden verification without hardware.
pub fn render_debug_png(words: &[Word], width: u32, height: u32, path: &str) -> Result<()> {
    let mut img = GrayImage::from_pixel(width, height, Luma([255u8]));
    let black = Luma([0u8]);

    for word in words {
        for stroke in &word.strokes {
            for pair in stroke.windows(2) {
                let (x1, y1) = pair[0];
                let (x2, y2) = pair[1];
                draw_line_segment_mut(&mut img, (x1, y1), (x2, y2), black);
            }
        }
    }

    img.save(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursive::font::CursiveFont;
    use crate::cursive::humanize::{humanize_word, HumanizeConfig};
    use crate::cursive::layout::{layout, LayoutConfig, Placement};

    #[test]
    fn renders_a_golden_sentence_png_without_error() {
        let font = CursiveFont::embedded_allure().unwrap();
        let placement = Placement { x: 40.0, y: 200.0, max_width: 700.0 };
        let words = layout(
            &font,
            "Bonjour, je m'appelle Tom Jedusor. Où étais-tu ?",
            &placement,
            &LayoutConfig::default(),
        );
        let humanized: Vec<_> = words
            .iter()
            .enumerate()
            .map(|(i, w)| humanize_word(w, i as u64, LayoutConfig::default().x_height_px, &HumanizeConfig::default()))
            .collect();

        let out_path = std::env::temp_dir().join("ghostwriter_cursive_golden_test.png");
        let out_path_str = out_path.to_str().unwrap();
        render_debug_png(&humanized, 768, 1024, out_path_str).unwrap();

        assert!(out_path.exists());
        let metadata = std::fs::metadata(&out_path).unwrap();
        assert!(metadata.len() > 0);
        let _ = std::fs::remove_file(&out_path);
    }
}
