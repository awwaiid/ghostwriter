use anyhow::Result;
use std::time::Duration;
use tokio::time::sleep as tokio_sleep;

use crate::cursive::layout::Word;
use crate::pen::Pen;

/// Draw a sequence of already-humanized words through the pen, pausing
/// briefly between words so the result reads as brisk, natural writing
/// rather than a single unbroken scrawl (spec: ~2-4 words/second).
pub fn draw_words(pen: &mut Pen, words: &[Word], word_gap_ms: u64) -> Result<()> {
    for (i, word) in words.iter().enumerate() {
        for stroke in &word.strokes {
            pen.draw_stroke_virtual(stroke)?;
        }
        if i + 1 < words.len() {
            std::thread::sleep(Duration::from_millis(word_gap_ms));
        }
    }
    Ok(())
}

/// Async variant for call sites already inside a tokio task that don't want
/// to block the executor thread during the inter-word pause.
pub async fn draw_words_async(pen: &mut Pen, words: &[Word], word_gap_ms: u64) -> Result<()> {
    for (i, word) in words.iter().enumerate() {
        for stroke in &word.strokes {
            pen.draw_stroke_virtual(stroke)?;
        }
        if i + 1 < words.len() {
            tokio_sleep(Duration::from_millis(word_gap_ms)).await;
        }
    }
    Ok(())
}
