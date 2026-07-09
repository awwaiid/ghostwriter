use crate::cursive::layout::{Stroke, Word};

#[derive(Debug, Clone, Copy)]
pub struct HumanizeConfig {
    pub wobble_amplitude_frac: f32,
    pub rotation_jitter_deg: f32,
    pub scale_jitter_frac: f32,
}

impl Default for HumanizeConfig {
    fn default() -> Self {
        Self {
            wobble_amplitude_frac: 0.015,
            rotation_jitter_deg: 1.5,
            scale_jitter_frac: 0.02,
        }
    }
}

/// Deterministic xorshift PRNG so the same seed always produces the same
/// "organic" variation — required for reproducible tests and for the
/// humanizer to be tunable without hardware in the loop.
struct Xorshift64(u64);

impl Xorshift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9E3779B97F4A7C15 } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform float in [-1.0, 1.0].
    fn next_signed_unit(&mut self) -> f32 {
        let bits = (self.next_u64() >> 40) as u32; // 24 bits of entropy
        (bits as f32 / 0x00FF_FFFF as f32) * 2.0 - 1.0
    }
}

/// Apply subtle per-word noise so the cursive doesn't look mechanically
/// identical every time: a small rigid rotation, a small uniform scale,
/// and low-frequency baseline wobble along each stroke. Amplitudes are
/// deliberately tiny (spec: "living hand", not "drunk robot").
pub fn humanize_word(word: &Word, seed: u64, x_height_px: f32, config: &HumanizeConfig) -> Word {
    let mut rng = Xorshift64::new(seed);

    let rotation_rad = (config.rotation_jitter_deg * rng.next_signed_unit()).to_radians();
    let scale = 1.0 + config.scale_jitter_frac * rng.next_signed_unit();
    let wobble_amplitude = config.wobble_amplitude_frac * x_height_px;
    let wobble_phase = rng.next_signed_unit() * std::f32::consts::PI;

    // Pivot the rotation/scale around the word's own centroid so the word
    // doesn't drift away from its laid-out position.
    let (cx, cy) = centroid(word);

    let strokes: Vec<Stroke> = word
        .strokes
        .iter()
        .map(|stroke| {
            stroke
                .iter()
                .enumerate()
                .map(|(i, &(x, y))| {
                    let dx = x - cx;
                    let dy = y - cy;
                    let rx = dx * rotation_rad.cos() - dy * rotation_rad.sin();
                    let ry = dx * rotation_rad.sin() + dy * rotation_rad.cos();
                    let scaled_x = cx + rx * scale;
                    let scaled_y = cy + ry * scale;
                    let wobble = wobble_amplitude * (i as f32 * 0.6 + wobble_phase).sin();
                    (scaled_x, scaled_y + wobble)
                })
                .collect()
        })
        .collect();

    Word { strokes }
}

fn centroid(word: &Word) -> (f32, f32) {
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut count = 0.0;
    for stroke in &word.strokes {
        for &(x, y) in stroke {
            sum_x += x;
            sum_y += y;
            count += 1.0;
        }
    }
    if count == 0.0 {
        (0.0, 0.0)
    } else {
        (sum_x / count, sum_y / count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursive::layout::Word;

    fn sample_word() -> Word {
        Word {
            strokes: vec![vec![(0.0, 0.0), (10.0, 0.0), (20.0, 0.0), (30.0, 0.0)]],
        }
    }

    #[test]
    fn same_seed_produces_identical_output() {
        let word = sample_word();
        let a = humanize_word(&word, 42, 28.0, &HumanizeConfig::default());
        let b = humanize_word(&word, 42, 28.0, &HumanizeConfig::default());
        assert_eq!(a.strokes, b.strokes);
    }

    #[test]
    fn different_seeds_produce_different_output() {
        let word = sample_word();
        let a = humanize_word(&word, 1, 28.0, &HumanizeConfig::default());
        let b = humanize_word(&word, 2, 28.0, &HumanizeConfig::default());
        assert_ne!(a.strokes, b.strokes);
    }

    #[test]
    fn displacement_stays_within_a_small_bound() {
        let word = sample_word();
        let config = HumanizeConfig::default();
        let x_height = 28.0;
        let humanized = humanize_word(&word, 7, x_height, &config);
        let original = &word.strokes[0];
        let result = &humanized.strokes[0];
        // Generous bound: rotation + scale + wobble combined shouldn't move
        // any point by more than ~15% of x-height for this short stroke.
        let max_expected_displacement = x_height * 0.15;
        for (orig, moved) in original.iter().zip(result.iter()) {
            let dx = orig.0 - moved.0;
            let dy = orig.1 - moved.1;
            let dist = (dx * dx + dy * dy).sqrt();
            assert!(dist <= max_expected_displacement, "displacement {} exceeds bound {}", dist, max_expected_displacement);
        }
    }

    #[test]
    fn word_with_no_points_does_not_panic() {
        let empty = Word { strokes: vec![] };
        let result = humanize_word(&empty, 1, 28.0, &HumanizeConfig::default());
        assert!(result.strokes.is_empty());
    }
}
