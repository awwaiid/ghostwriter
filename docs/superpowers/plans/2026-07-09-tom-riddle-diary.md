# Tom Riddle Diary Effect Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the reMarkable 2 answer handwritten questions in animated, connected cursive handwriting — triggered by a spiral gesture drawn anywhere on the device — reproducing the Tom Riddle diary effect from *Harry Potter*.

**Architecture:** A new `src/cursive/` module turns plain LLM-returned text into humanized, connected single-line cursive stroke geometry (parsed from a bundled SIL-OFL SVG font), which is drawn through a new `Pen::draw_stroke_virtual` primitive. A new `src/gesture.rs` watches the pen input device for a spiral stroke as a second trigger source alongside the existing corner tap. A new `src/notebook.rs` detects the currently open xochitl document to select between a neutral prompt and a Tom Jedusor persona prompt for a designated diary notebook. Everything runs on-device; the only network calls remain the existing LLM API calls.

**Tech Stack:** Rust (edition 2021), `roxmltree` (new dep) for parsing the bundled SVG font, `svg2polylines` (existing dep) for glyph path flattening, `imageproc`/`image` (existing deps) for a debug PNG renderer, `evdev` (existing dep) for reading raw pen events, `rust-embed` (existing dep) for bundling the font asset.

## Global Constraints

- Fully on-device: no companion server; the only external network calls are the existing LLM provider APIs (spec §Architecture, §Alternatives).
- Target device is reMarkable 2 (armv7-unknown-linux-gnueabihf); cross-compiled via `cross` per this repo's existing `build.sh` (AGENTS.md).
- French accented characters are a **hard requirement**: é è à ç ù â ê î ô û ë ï ü must render correctly; œ/Œ is handled by "oe"/"OE" text substitution (spec §Requirements 3, §CursiveRenderer 1).
- Cursive font is EMS Allure (SIL Open Font License), swappable later; its OFL license text must be bundled alongside the font asset (spec §CursiveRenderer 1).
- Handwriting must read as connected cursive (letters joined via a shared connection zone), not disjoint printed glyphs (spec §Requirements 4, §CursiveRenderer 3).
- Writing pace target: brisk, ~2–4 words/second of drawing (spec §Requirements 6).
- The spiral trigger works **anywhere** (any notebook/document); a designated diary notebook additionally selects a special persona prompt — this is an enhancement layer, not a restriction (spec §Requirements 7, and user clarification during brainstorming).
- Pen must never be left mid-stroke on cancellation — always lift cleanly between strokes (spec §Error handling).
- Default LLM model is `claude-haiku-4-5-20251001` (already applied in `src/config.rs`, cost-driven decision made this session) — do not change back to a Sonnet default as part of this plan.
- Follow existing codebase conventions: inline `#[cfg(test)] mod tests` per file (see `src/simulation/mod.rs:68`), modules re-exported from a `mod.rs`, `figment`-based config with matching CLI arg names, assets embedded via `rust-embed` and loaded through `embedded_assets::load_config`.

---

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `fonts/EMSAllure.svg` | new | Bundled SIL-OFL SVG font asset (cursive glyphs) |
| `fonts/OFL-EMSAllure.txt` | new | SIL Open Font License text for the bundled font |
| `src/embedded_assets.rs` | edit | Embed the new `fonts/` folder |
| `src/cursive/mod.rs` | new | Public API: `CursiveConfig`, `Placement`, `write_cursive()` |
| `src/cursive/font.rs` | new | SVG font XML → `CursiveFont` (glyph table + metrics) |
| `src/cursive/text.rs` | new | Text normalization (œ/Œ substitution) |
| `src/cursive/layout.rs` | new | Word-wrap, glyph placement, letter-joining into `Word`/`Stroke` |
| `src/cursive/humanize.rs` | new | Deterministic seeded noise: wobble, rotation, scale jitter |
| `src/cursive/animate.rs` | new | Draws `Word`s through `Pen`, paced with inter-word gaps |
| `src/cursive/debug.rs` | new | Renders strokes to a PNG for visual/golden verification |
| `src/pen.rs` | edit | Add `pub fn draw_stroke_virtual`; refactor `draw_virtual_paths` to reuse it |
| `src/gesture.rs` | new | Pure spiral-geometry detector + stateful `SpiralWatcher` reading the pen device |
| `src/notebook.rs` | new | Detect the xochitl document UUID currently open (procfs fd scan) |
| `src/config.rs` | edit | New fields: diary notebook id, cursive tuning, gesture thresholds |
| `src/main.rs` | edit | New CLI args; register `write_cursive` tool; wire notebook-based prompt selection |
| `src/coordinator.rs` | edit | Second trigger source (gesture) alongside touch; prompt selection by notebook |
| `src/lib.rs` | edit | Add `pub mod cursive; pub mod gesture; pub mod notebook;` |
| `prompts/tool_write_cursive.json` | new | Tool schema: `text`, `x`, `y`, `width` |
| `prompts/diary.json` | new | Tom Jedusor persona prompt |
| `prompts/neutral_cursive.json` | new | Default cursive-answer prompt for non-diary documents |
| `Cargo.toml` | edit | Add `roxmltree` dependency |

---

### Task 0: Development environment setup

**Files:** none (environment only)

**Interfaces:** none — this task only ensures every later task's test commands are runnable.

- [ ] **Step 1: Install the Rust toolchain**

This machine has no `cargo`/`rustc` on `PATH`. Install via rustup:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustc --version && cargo --version
```

Expected: both commands print a version string (e.g. `cargo 1.8x.x`).

- [ ] **Step 2: Add the cross-compilation target and install `cross`**

```bash
rustup target add armv7-unknown-linux-gnueabihf
cargo install cross --git https://github.com/cross-rs/cross
```

Expected: `cross --version` prints a version string. Docker must be running (`docker info` succeeds) since `cross` builds inside a container — Docker.app is already installed on this machine; start it if not running.

- [ ] **Step 3: Verify the existing project builds before making any changes**

```bash
cd /Users/charlestheveniau/Desktop/Projects/jedusorjournal
cargo check --all-targets --all-features
```

Expected: `Finished` with no errors (warnings are fine). If this fails, stop and fix the pre-existing build before starting Task 1 — none of the later steps' "expected: PASS" results are trustworthy on a broken baseline.

- [ ] **Step 4: Establish SSH access to the tablet**

The tablet is reachable at `10.11.99.1` over USB but key-based SSH currently fails (`Permission denied (publickey,password)`, verified this session). Get the root password from the tablet: **Settings → Help → Copyrights and licenses → GPLv3 Compliance** (screen shows the password). Then:

```bash
ssh-copy-id root@10.11.99.1
```

Enter the password when prompted. Verify passwordless access:

```bash
ssh root@10.11.99.1 'cat /etc/hwrevision; uname -m'
```

Expected: prints `reMarkable2 1.0` (or similar) and `armv7l`. This is required by Task 8 (on-device spike) and Task 11 (deployment) — do it now so those tasks aren't blocked later.

- [ ] **Step 5: Commit nothing yet**

This task makes no repo changes — nothing to commit. Proceed to Task 1.

---

### Task 1: Cursive module scaffold and font asset

**Files:**
- Create: `fonts/EMSAllure.svg`
- Create: `fonts/OFL-EMSAllure.txt`
- Create: `src/cursive/mod.rs`
- Modify: `src/embedded_assets.rs`
- Modify: `src/lib.rs`
- Modify: `Cargo.toml`

**Interfaces:**
- Produces: `ghostwriter::cursive` module (empty stub for now); `embedded_assets::AssetFonts` embed type; `embedded_assets::load_font_asset(filename: &str) -> String` helper later tasks use to load `EMSAllure.svg`.

- [ ] **Step 1: Vendor the font and its license**

Copy the verified font file (downloaded and inspected during design research) into the repo:

```bash
mkdir -p fonts
cp "/private/tmp/claude-501/-Users-charlestheveniau-Desktop-Projects-jedusorjournal/ccea79df-4808-4cd1-8c8d-6d82a32f2f8e/scratchpad/fonts/EMSAllure.svg" fonts/EMSAllure.svg
```

Download the canonical SIL OFL 1.1 license text and save it as `fonts/OFL-EMSAllure.txt`:

```bash
curl -sL https://scripts.sil.org/cms/scripts/render_download.php?format=file&media_id=OFL_plaintext&filename=OFL.txt -o fonts/OFL-EMSAllure.txt
```

If that URL doesn't resolve, fetch the OFL 1.1 plain text from https://openfontlicense.org/documents/OFL.txt instead and save it to the same path. Verify the file is non-empty and contains the string "SIL OPEN FONT LICENSE":

```bash
grep -c "SIL OPEN FONT LICENSE" fonts/OFL-EMSAllure.txt
```

Expected: `1`.

- [ ] **Step 2: Add a header comment crediting the font source inside the vendored SVG (do not modify glyph data)**

Confirm the file already has this metadata (it does — verified during research):

```bash
head -12 fonts/EMSAllure.svg
```

Expected output includes `Font name:               EMS Allure`, `License:                 SIL Open Font License http://scripts.sil.org/OFL`, and `Created by:              Sheldon B. Michaels`. Leave the file exactly as downloaded — do not edit it.

- [ ] **Step 3: Embed the `fonts/` folder**

Edit `src/embedded_assets.rs`:

```rust
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "prompts/"]
pub struct AssetPrompts;

#[derive(Embed)]
#[folder = "fonts/"]
pub struct AssetFonts;

#[derive(Embed)]
#[folder = "utils/"]
#[include = "rmpp/uinput-*"]
pub struct AssetUtils;

// Function to provide access to the uinput module data
pub fn get_uinput_module_data(version: &str) -> Option<Vec<u8>> {
    let target_module_filename = format!("rmpp/uinput-{}.ko", version);
    AssetUtils::get(target_module_filename.as_str()).map(|asset| asset.data.to_vec())
}

pub fn load_config(filename: &str) -> String {
    log::debug!("Loading config from {}", filename);

    if std::path::Path::new(filename).exists() {
        std::fs::read_to_string(filename).unwrap()
    } else {
        std::str::from_utf8(AssetPrompts::get(filename).unwrap().data.as_ref()).unwrap().to_string()
    }
}

pub fn load_font_asset(filename: &str) -> String {
    log::debug!("Loading font asset {}", filename);

    if std::path::Path::new(filename).exists() {
        std::fs::read_to_string(filename).unwrap()
    } else {
        std::str::from_utf8(AssetFonts::get(filename).unwrap().data.as_ref()).unwrap().to_string()
    }
}
```

- [ ] **Step 4: Add the `roxmltree` dependency**

Edit `Cargo.toml`, adding this line in the `[dependencies]` section (alongside the other parsing-related deps like `svg2polylines`):

```toml
roxmltree = "0.20"
```

- [ ] **Step 5: Create the `cursive` module stub**

Create `src/cursive/mod.rs`:

```rust
//! Cursive handwriting rendering: turns plain text into animated,
//! connected single-line cursive strokes drawn by the pen.

pub mod font;
```

Add to `src/lib.rs` (alongside the existing `pub mod` list, alphabetically next to `coordinator`):

```rust
pub mod cursive;
```

- [ ] **Step 6: Verify it compiles**

```bash
cargo check --all-targets --all-features
```

Expected: `Finished` with no errors. `font` module doesn't exist yet as a file, so this step will actually fail — that's expected only if you created the `pub mod font;` line without the file. Instead, for this step, temporarily comment out `pub mod font;` in `src/cursive/mod.rs`, leaving just the doc comment, run `cargo check`, confirm it passes, then move to Task 2 which creates `font.rs` and re-enables the line.

- [ ] **Step 7: Commit**

```bash
git add fonts/EMSAllure.svg fonts/OFL-EMSAllure.txt src/embedded_assets.rs src/cursive/mod.rs src/lib.rs Cargo.toml Cargo.lock
git commit -m "Vendor EMS Allure cursive font and scaffold cursive module"
```

---

### Task 2: SVG font parser

**Files:**
- Create: `src/cursive/font.rs`
- Modify: `src/cursive/mod.rs`

**Interfaces:**
- Consumes: `embedded_assets::load_font_asset(filename: &str) -> String` (Task 1).
- Produces: `pub struct Glyph { pub advance: f32, pub subpaths: Vec<Vec<(f32, f32)>> }`; `pub struct CursiveFont { pub glyphs: HashMap<char, Glyph>, pub default_advance: f32, pub units_per_em: f32, pub x_height: f32, pub ascent: f32, pub descent: f32 }`; `CursiveFont::parse(svg_font_xml: &str) -> Result<CursiveFont>`; `CursiveFont::embedded_allure() -> Result<CursiveFont>`. Later tasks (layout.rs) read `font.glyphs`, `font.x_height`, `font.default_advance`.

- [ ] **Step 1: Write the failing tests**

Create `src/cursive/font.rs`:

```rust
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
        let doc = roxmltree::Document::parse(svg_font_xml)?;

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
```

- [ ] **Step 2: Run tests to verify they fail (module not yet wired into `mod.rs`)**

Update `src/cursive/mod.rs` to uncomment the module:

```rust
//! Cursive handwriting rendering: turns plain text into animated,
//! connected single-line cursive strokes drawn by the pen.

pub mod font;
```

Run:

```bash
cargo test --lib cursive::font
```

Expected: compiles and all tests pass immediately (there's no red/green cycle here since the implementation was written alongside the test — this is a verification run). If any test fails, it indicates the vendored font file or research assumptions differ from what's bundled; investigate the actual `fonts/EMSAllure.svg` content (`grep -o '<glyph unicode="e"[^/]*/>' fonts/EMSAllure.svg`) before changing the assertions.

- [ ] **Step 3: Run `cargo check` for the whole project**

```bash
cargo check --all-targets --all-features
```

Expected: `Finished` with no errors.

- [ ] **Step 4: Commit**

```bash
git add src/cursive/font.rs src/cursive/mod.rs
git commit -m "Add SVG cursive font parser with verified accent coverage tests"
```

---

### Task 3: Text normalization

**Files:**
- Create: `src/cursive/text.rs`
- Modify: `src/cursive/mod.rs`

**Interfaces:**
- Produces: `pub fn normalize(input: &str) -> String`. Later tasks (layout.rs) call this before laying out glyphs.

- [ ] **Step 1: Write the failing tests**

Create `src/cursive/text.rs`:

```rust
/// Normalize text for rendering with a font that lacks œ/Œ ligature glyphs
/// (verified: EMS Allure has full Latin-1 coverage but no Latin-9 œ/Œ).
/// All other characters pass through unchanged; unknown characters are
/// left as-is here and skipped later at layout time.
pub fn normalize(input: &str) -> String {
    input
        .chars()
        .flat_map(|c| match c {
            'œ' => vec!['o', 'e'],
            'Œ' => vec!['O', 'E'],
            other => vec![other],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_lowercase_oe_ligature() {
        assert_eq!(normalize("cœur"), "coeur");
    }

    #[test]
    fn substitutes_uppercase_oe_ligature() {
        assert_eq!(normalize("ŒUF"), "OEUF");
    }

    #[test]
    fn leaves_accented_characters_untouched() {
        assert_eq!(normalize("Où étais-tu ?"), "Où étais-tu ?");
    }

    #[test]
    fn leaves_plain_ascii_untouched() {
        assert_eq!(normalize("Hello, world!"), "Hello, world!");
    }
}
```

- [ ] **Step 2: Wire the module and run tests**

Edit `src/cursive/mod.rs`:

```rust
pub mod font;
pub mod text;
```

```bash
cargo test --lib cursive::text
```

Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/cursive/text.rs src/cursive/mod.rs
git commit -m "Add œ/Œ text normalization for cursive rendering"
```

---

### Task 4: Layout and letter joining

**Files:**
- Create: `src/cursive/layout.rs`
- Modify: `src/cursive/mod.rs`

**Interfaces:**
- Consumes: `font::CursiveFont`, `font::Glyph` (Task 2); `text::normalize` (Task 3).
- Produces: `pub type Stroke = Vec<(f32, f32)>;`; `pub struct Word { pub strokes: Vec<Stroke> }`; `pub struct Placement { pub x: f32, pub y: f32, pub max_width: f32 }`; `pub struct LayoutConfig { pub x_height_px: f32, pub line_spacing_mult: f32, pub word_spacing_mult: f32 }` with `impl Default`; `pub fn layout(font: &CursiveFont, text: &str, placement: &Placement, config: &LayoutConfig) -> Vec<Word>`. `Stroke` points are in **virtual screen space** (768×1024, Y down). Later tasks (humanize.rs, animate.rs) consume `Vec<Word>`.

- [ ] **Step 1: Write the failing tests**

Create `src/cursive/layout.rs`:

```rust
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
/// 'e' exits and 'a'/'m' enter at y≈183; 80 units of slack on either side
/// covers the font's other lowercase joining letters.
const CONNECTION_ZONE_CENTER: f32 = 183.0;
const CONNECTION_ZONE_HALF_WIDTH: f32 = 80.0;

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

        for ch in raw_word.chars() {
            let Some(glyph) = font.glyphs.get(&ch) else {
                log::debug!("cursive layout: skipping unsupported character {:?}", ch);
                continue;
            };

            let mut subpath_iter = glyph.subpaths.iter();
            if let Some(main) = subpath_iter.next() {
                let placed: Stroke = main
                    .iter()
                    .map(|&(fx, fy)| (pen_x + fx * scale, cursor_y - fy * scale))
                    .collect();

                let entry = placed.first().copied();
                let can_join = match (prev_exit, entry) {
                    (Some(prev), Some(cur)) => {
                        let prev_font_y = cursor_y - prev.1;
                        let cur_font_y = cursor_y - cur.1;
                        in_connection_zone(prev_font_y) && in_connection_zone(cur_font_y)
                    }
                    _ => false,
                };

                if can_join {
                    if let Some(last_stroke) = strokes.last_mut() {
                        last_stroke.extend(placed.iter().copied());
                    } else {
                        strokes.push(placed.clone());
                    }
                } else {
                    strokes.push(placed.clone());
                }

                prev_exit = placed.last().copied();
            }

            for accent in subpath_iter {
                let placed: Stroke = accent
                    .iter()
                    .map(|&(fx, fy)| (pen_x + fx * scale, cursor_y - fy * scale))
                    .collect();
                strokes.push(placed);
            }

            pen_x += glyph.advance * scale;
        }

        if !strokes.is_empty() {
            words_out.push(Word { strokes });
        }

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
        let placement = Placement { x: 50.0, y: 200.0, max_width: 600.0 };
        let words = layout(&font, "Bonjour", &placement, &LayoutConfig::default());
        assert_eq!(words.len(), 1);
        assert!(!words[0].strokes.is_empty());
    }

    #[test]
    fn long_text_wraps_to_multiple_lines() {
        let font = test_font();
        // Narrow box forces a wrap after a couple of words.
        let placement = Placement { x: 50.0, y: 200.0, max_width: 150.0 };
        let words = layout(
            &font,
            "Bonjour je m'appelle Tom Jedusor où étais-tu",
            &placement,
            &LayoutConfig::default(),
        );
        assert!(words.len() >= 5, "expected multiple words laid out, got {}", words.len());
        let ys: Vec<f32> = words.iter().flat_map(|w| w.strokes.iter().flat_map(|s| s.iter().map(|p| p.1))).collect();
        let min_y = ys.iter().cloned().fold(f32::MAX, f32::min);
        let max_y = ys.iter().cloned().fold(f32::MIN, f32::max);
        assert!(max_y - min_y > LayoutConfig::default().x_height_px, "expected at least one line wrap");
    }

    #[test]
    fn joined_letters_produce_fewer_strokes_than_glyphs() {
        let font = test_font();
        let placement = Placement { x: 50.0, y: 200.0, max_width: 600.0 };
        // "am" — both 'a' and 'm' enter/exit at the verified connection height,
        // so they should be spliced into a single continuous stroke.
        let words = layout(&font, "am", &placement, &LayoutConfig::default());
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].strokes.len(), 1, "expected 'a' and 'm' to join into one stroke");
    }

    #[test]
    fn accents_remain_separate_pen_lifts() {
        let font = test_font();
        let placement = Placement { x: 50.0, y: 200.0, max_width: 600.0 };
        let words = layout(&font, "é", &placement, &LayoutConfig::default());
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].strokes.len(), 2, "expected main stroke + accent as separate strokes");
    }

    #[test]
    fn unknown_characters_are_skipped_without_panicking() {
        let font = test_font();
        let placement = Placement { x: 50.0, y: 200.0, max_width: 600.0 };
        let words = layout(&font, "hello 中 world", &placement, &LayoutConfig::default());
        assert_eq!(words.len(), 3);
    }
}
```

- [ ] **Step 2: Wire the module and run tests**

Edit `src/cursive/mod.rs`:

```rust
pub mod font;
pub mod layout;
pub mod text;
```

```bash
cargo test --lib cursive::layout
```

Expected: 5 tests pass. If `joined_letters_produce_fewer_strokes_than_glyphs` fails, print the actual entry/exit Y values (`eprintln!` in the test temporarily) to check whether `CONNECTION_ZONE_HALF_WIDTH` needs widening — this is the one test most sensitive to real font data.

- [ ] **Step 3: Commit**

```bash
git add src/cursive/layout.rs src/cursive/mod.rs
git commit -m "Add cursive text layout with word-wrap and letter joining"
```

---

### Task 5: Humanizer

**Files:**
- Create: `src/cursive/humanize.rs`
- Modify: `src/cursive/mod.rs`

**Interfaces:**
- Consumes: `layout::Word`, `layout::Stroke` (Task 4).
- Produces: `pub struct HumanizeConfig { pub wobble_amplitude_frac: f32, pub rotation_jitter_deg: f32, pub scale_jitter_frac: f32 }` with `impl Default`; `pub fn humanize_word(word: &Word, seed: u64, x_height_px: f32, config: &HumanizeConfig) -> Word`. Later tasks (`cursive::mod::write_cursive`) call this once per word with a distinct seed.

- [ ] **Step 1: Write the failing tests**

Create `src/cursive/humanize.rs`:

```rust
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
```

- [ ] **Step 2: Wire the module and run tests**

Edit `src/cursive/mod.rs`:

```rust
pub mod font;
pub mod humanize;
pub mod layout;
pub mod text;
```

```bash
cargo test --lib cursive::humanize
```

Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/cursive/humanize.rs src/cursive/mod.rs
git commit -m "Add deterministic seeded humanizer for cursive strokes"
```

---

### Task 6: Pen drawing primitive, animator, debug renderer, and public API

**Files:**
- Modify: `src/pen.rs:565-590` (extract `draw_stroke_virtual`)
- Create: `src/cursive/animate.rs`
- Create: `src/cursive/debug.rs`
- Modify: `src/cursive/mod.rs`

**Interfaces:**
- Consumes: `layout::layout`, `text::normalize` (used internally by `layout`), `humanize::humanize_word` (Tasks 3–5); `Pen` (existing, `src/pen.rs`).
- Produces: `Pen::draw_stroke_virtual(&mut self, points: &[(f32, f32)]) -> Result<()>` (public, on the existing `Pen` struct); `pub fn draw_words(pen: &mut Pen, words: &[Word], word_gap_ms: u64) -> Result<()>` in `animate.rs`; `pub fn render_debug_png(words: &[Word], width: u32, height: u32, path: &str) -> Result<()>` in `debug.rs`; `pub struct CursiveConfig { pub layout: LayoutConfig, pub humanize: HumanizeConfig, pub word_gap_ms: u64 }` with `impl Default`; `pub fn write_cursive(pen: &mut Pen, font: &CursiveFont, text: &str, placement: &Placement, config: &CursiveConfig, seed: u64) -> Result<()>` in `cursive/mod.rs`. Task 10 (main.rs wiring) calls `write_cursive` directly.

- [ ] **Step 1: Extract `draw_stroke_virtual` from the existing private `draw_virtual_paths`**

Open `src/pen.rs`. Find the existing private method (around line 565):

```rust
    /// Draw paths given as lists of (x, y) virtual coordinates.
    /// No corner detection — skeleton paths are already single-pixel-wide and smooth.
    fn draw_virtual_paths(&mut self, paths: &[Vec<(f32, f32)>]) -> Result<()> {
        const MAX_STEP: f32 = 1.0;
        let mut step_count = 0usize;

        for path in paths {
            if path.len() < 2 {
                continue;
            }

            let start = self.virtual_to_input((path[0].0 as i32, path[0].1 as i32));
            self.pen_up()?;
            sleep(Duration::from_millis(2));
            self.pen_down_at(start)?;
            sleep(Duration::from_millis(2));

            let mut prev = path[0];
            for &pt in &path[1..] {
                self.draw_segment(prev, pt, &mut step_count, MAX_STEP)?;
                prev = pt;
            }

            self.pen_up()?;
            sleep(Duration::from_millis(2));
        }
        Ok(())
    }
```

Replace it with a public single-stroke primitive plus a thin multi-path wrapper that reuses it (removing the duplication):

```rust
    /// Draw a single continuous stroke from a list of virtual-space points:
    /// pen down at the first point, interpolated moves through the rest,
    /// pen up at the end. Used by the cursive handwriting renderer, which
    /// already computes one stroke per pen-down run (letters are pre-joined
    /// at layout time), so no corner detection is needed here.
    pub fn draw_stroke_virtual(&mut self, points: &[(f32, f32)]) -> Result<()> {
        if points.len() < 2 {
            return Ok(());
        }
        const MAX_STEP: f32 = 1.0;
        let mut step_count = 0usize;

        let start = self.virtual_to_input((points[0].0 as i32, points[0].1 as i32));
        self.pen_up()?;
        sleep(Duration::from_millis(2));
        self.pen_down_at(start)?;
        sleep(Duration::from_millis(2));

        let mut prev = points[0];
        for &pt in &points[1..] {
            self.draw_segment(prev, pt, &mut step_count, MAX_STEP)?;
            prev = pt;
        }

        self.pen_up()?;
        sleep(Duration::from_millis(2));
        Ok(())
    }

    /// Draw paths given as lists of (x, y) virtual coordinates.
    /// No corner detection — skeleton paths are already single-pixel-wide and smooth.
    fn draw_virtual_paths(&mut self, paths: &[Vec<(f32, f32)>]) -> Result<()> {
        for path in paths {
            self.draw_stroke_virtual(path)?;
        }
        Ok(())
    }
```

- [ ] **Step 2: Verify the refactor didn't break anything**

```bash
cargo check --all-targets --all-features
```

Expected: `Finished` with no errors. `draw_virtual_paths` is still called from `draw_svg_centerline` — confirm that call site (`src/pen.rs`, the `draw_svg_centerline` method) is untouched; it should be, since we only changed the body of `draw_virtual_paths`, not its signature.

- [ ] **Step 3: Write the animator**

Create `src/cursive/animate.rs`:

```rust
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
```

- [ ] **Step 4: Write the debug PNG renderer**

Create `src/cursive/debug.rs`:

```rust
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
```

- [ ] **Step 5: Write the public `write_cursive` entry point**

Edit `src/cursive/mod.rs`:

```rust
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
```

- [ ] **Step 6: Run all cursive tests**

```bash
cargo test --lib cursive
```

Expected: all tests across `font`, `text`, `layout`, `humanize`, `debug`, and `mod` pass.

- [ ] **Step 7: Manually inspect the golden PNG**

```bash
cargo test --lib cursive::debug::tests::renders_a_golden_sentence_png_without_error -- --nocapture
open /tmp/ghostwriter_cursive_golden_test.png 2>/dev/null || true
```

The test deletes the file after passing — temporarily comment out the `std::fs::remove_file` line, rerun, open the PNG, and confirm by eye that "Bonjour, je m'appelle Tom Jedusor. Où étais-tu ?" reads as connected, legible cursive with correct accents (é, ù). Restore the `remove_file` line afterward.

- [ ] **Step 8: Commit**

```bash
git add src/pen.rs src/cursive/animate.rs src/cursive/debug.rs src/cursive/mod.rs
git commit -m "Add pen stroke primitive, animator, and cursive write_cursive API"
```

---

### Task 7: Spiral gesture detection (pure logic)

**Files:**
- Create: `src/gesture.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces: `pub struct SpiralConfig { pub min_turn_degrees: f32, pub min_bbox_px: f32, pub max_bbox_px: f32, pub max_duration_ms: u64 }` with `impl Default`; `pub fn is_spiral(points: &[(f32, f32)], duration_ms: u64, config: &SpiralConfig) -> bool`. Task 8 builds the stateful `SpiralWatcher` on top of this pure function.

- [ ] **Step 1: Write the failing tests**

Create `src/gesture.rs`:

```rust
//! Spiral gesture detection: the user ends a question with a small,
//! deliberate spiral flourish, which triggers the diary to answer.

#[derive(Debug, Clone, Copy)]
pub struct SpiralConfig {
    pub min_turn_degrees: f32,
    pub min_bbox_px: f32,
    pub max_bbox_px: f32,
    pub max_duration_ms: u64,
}

impl Default for SpiralConfig {
    fn default() -> Self {
        Self {
            min_turn_degrees: 720.0,
            min_bbox_px: 15.0,
            max_bbox_px: 60.0,
            max_duration_ms: 1500,
        }
    }
}

/// Detect whether a completed pen stroke (points in virtual screen space,
/// in drawing order) is a spiral: at least two consistent-winding loops,
/// drawn as a small deliberate mark, quickly. This rejects casual circles
/// / letters like 'o' or 'e' (single loop, ~360°) and large scribbles
/// (bbox too big) as well as slow multi-loop doodles (duration too long).
pub fn is_spiral(points: &[(f32, f32)], duration_ms: u64, config: &SpiralConfig) -> bool {
    if points.len() < 8 {
        return false;
    }
    if duration_ms > config.max_duration_ms {
        return false;
    }

    let (min_x, min_y, max_x, max_y) = bounding_box(points);
    let max_dim = (max_x - min_x).max(max_y - min_y);
    if max_dim < config.min_bbox_px || max_dim > config.max_bbox_px {
        return false;
    }

    let turn = cumulative_turn_degrees(points).abs();
    turn >= config.min_turn_degrees
}

fn bounding_box(points: &[(f32, f32)]) -> (f32, f32, f32, f32) {
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for &(x, y) in points {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    (min_x, min_y, max_x, max_y)
}

/// Sum of signed turning angles at each interior point. A perfect circle
/// traversed once accumulates ~360°; two consistent-winding loops
/// accumulate ~720°. Reversing direction mid-stroke cancels out, which is
/// intentional — a spiral has a single consistent winding direction.
fn cumulative_turn_degrees(points: &[(f32, f32)]) -> f32 {
    let mut total = 0.0f32;
    for i in 1..points.len() - 1 {
        let (x0, y0) = points[i - 1];
        let (x1, y1) = points[i];
        let (x2, y2) = points[i + 1];
        let d1 = (x1 - x0, y1 - y0);
        let d2 = (x2 - x1, y2 - y1);
        let len1 = (d1.0 * d1.0 + d1.1 * d1.1).sqrt();
        let len2 = (d2.0 * d2.0 + d2.1 * d2.1).sqrt();
        if len1 < 0.01 || len2 < 0.01 {
            continue;
        }
        let cross = d1.0 * d2.1 - d1.1 * d2.0;
        let dot = d1.0 * d2.0 + d1.1 * d2.1;
        total += cross.atan2(dot).to_degrees();
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate an Archimedean spiral of `loops` full turns, centered at
    /// (cx, cy), with the given max radius, sampled at `n` points.
    fn synthetic_spiral(cx: f32, cy: f32, max_radius: f32, loops: f32, n: usize) -> Vec<(f32, f32)> {
        (0..n)
            .map(|i| {
                let t = i as f32 / (n - 1) as f32;
                let angle = t * loops * 2.0 * std::f32::consts::PI;
                let radius = t * max_radius;
                (cx + radius * angle.cos(), cy + radius * angle.sin())
            })
            .collect()
    }

    #[test]
    fn detects_a_deliberate_two_loop_spiral() {
        let points = synthetic_spiral(100.0, 100.0, 20.0, 2.5, 60);
        assert!(is_spiral(&points, 800, &SpiralConfig::default()));
    }

    #[test]
    fn rejects_a_straight_line() {
        let points: Vec<(f32, f32)> = (0..20).map(|i| (i as f32 * 2.0, 0.0)).collect();
        assert!(!is_spiral(&points, 500, &SpiralConfig::default()));
    }

    #[test]
    fn rejects_a_single_loop_like_the_letter_o() {
        let points = synthetic_spiral(100.0, 100.0, 20.0, 1.0, 40);
        assert!(!is_spiral(&points, 500, &SpiralConfig::default()));
    }

    #[test]
    fn rejects_a_spiral_that_is_too_large() {
        let points = synthetic_spiral(100.0, 100.0, 200.0, 2.5, 60);
        assert!(!is_spiral(&points, 800, &SpiralConfig::default()));
    }

    #[test]
    fn rejects_a_spiral_drawn_too_slowly() {
        let points = synthetic_spiral(100.0, 100.0, 20.0, 2.5, 60);
        assert!(!is_spiral(&points, 3000, &SpiralConfig::default()));
    }

    #[test]
    fn rejects_too_few_points() {
        let points = vec![(0.0, 0.0), (1.0, 1.0)];
        assert!(!is_spiral(&points, 500, &SpiralConfig::default()));
    }
}
```

- [ ] **Step 2: Wire the module and run tests**

Add to `src/lib.rs`:

```rust
pub mod gesture;
```

```bash
cargo test --lib gesture
```

Expected: 6 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/gesture.rs src/lib.rs
git commit -m "Add pure spiral-gesture detection logic with synthetic-geometry tests"
```

---

### Task 8: Notebook (open-document) detection — on-device spike required first

**Files:**
- Create: `src/notebook.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces: `pub fn detect_open_document() -> Option<String>` (returns a document UUID string, or `None` if detection fails or nothing is open). Task 10 calls this to choose between the diary persona prompt and the neutral prompt.

**⚠️ This task's design assumes xochitl keeps a file descriptor open on the current document's content file while it's displayed. That assumption is unverified — validate it first; if it's wrong, use the documented fallback instead of the primary approach.**

- [ ] **Step 1: On-device spike — verify the fd-scan assumption**

Requires Task 0 Step 4 (SSH access) to be done. With a notebook open on the tablet:

```bash
ssh root@10.11.99.1 'pidof xochitl'
```

Note the PID, then:

```bash
ssh root@10.11.99.1 'ls -la /proc/<PID>/fd/ | grep -i xochitl'
```

**If this shows an open fd pointing into `/home/root/.local/share/remarkable/xochitl/<uuid>...`** (a `.content`, `.metadata`, `.pagedata` file, or a `<uuid>/<page-uuid>.rm` file): the primary approach works. Proceed to Step 2 using that exact path pattern; if it's a page file nested under `<uuid>/`, extract the first path segment under `xochitl/` as the document UUID.

**If no such fd appears** (e.g. xochitl uses mmap, a database, or keeps files closed after an initial read): fall back to a heuristic instead — the most recently modified `.metadata` file under `/home/root/.local/share/remarkable/xochitl/`:

```bash
ssh root@10.11.99.1 'ls -t /home/root/.local/share/remarkable/xochitl/*.metadata | head -1'
```

Verify this file's mtime updates when you switch to a different notebook (open notebook A, note the top result, switch to notebook B, rerun, confirm the top result changed to B's uuid). If the fallback also doesn't distinguish "open" from "recently edited," record that in a comment in `src/notebook.rs` at Step 2 and treat notebook detection as best-effort (matching the spec's "Detection failure is logged, never fatal").

Document which of the two behaviors was observed — the implementation in Step 2 branches on it.

- [ ] **Step 2: Write the failing tests (fd-scan approach, parameterized for testability)**

Create `src/notebook.rs`. This version assumes the fd-scan approach was confirmed in Step 1; if the spike found the fallback necessary instead, replace the body of `detect_open_document_in` with a directory-mtime scan over `data_root.join("xochitl")` and adjust the test fixture accordingly, keeping the same public signature.

```rust
//! Detect which reMarkable document is currently open, so the diary
//! effect can pick a persona prompt for a designated diary notebook while
//! still answering normally everywhere else.

use std::path::{Path, PathBuf};

const XOCHITL_DATA_DIR: &str = "/home/root/.local/share/remarkable/xochitl";

/// Detect the UUID of the document xochitl currently has open, by scanning
/// `/proc/<xochitl-pid>/fd` for a descriptor pointing into the xochitl
/// data directory. Returns `None` if xochitl isn't running, has no such
/// fd, or `/proc` isn't available (e.g. running off-device) — callers
/// must treat `None` as "use the neutral prompt," never as an error.
pub fn detect_open_document() -> Option<String> {
    detect_open_document_in(Path::new("/proc"), Path::new(XOCHITL_DATA_DIR))
}

fn detect_open_document_in(proc_root: &Path, xochitl_data_dir: &Path) -> Option<String> {
    let pid = find_xochitl_pid(proc_root)?;
    let fd_dir = proc_root.join(pid.to_string()).join("fd");
    let entries = std::fs::read_dir(&fd_dir).ok()?;

    for entry in entries.flatten() {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        if let Ok(relative) = target.strip_prefix(xochitl_data_dir) {
            if let Some(first_segment) = relative.components().next() {
                let name = first_segment.as_os_str().to_string_lossy();
                let uuid = name.split('.').next().unwrap_or(&name);
                if !uuid.is_empty() {
                    return Some(uuid.to_string());
                }
            }
        }
    }
    None
}

fn find_xochitl_pid(proc_root: &Path) -> Option<u32> {
    let entries = std::fs::read_dir(proc_root).ok()?;
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let comm_path: PathBuf = proc_root.join(pid.to_string()).join("comm");
        if let Ok(comm) = std::fs::read_to_string(&comm_path) {
            if comm.trim() == "xochitl" {
                return Some(pid);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// Build a fake `/proc`-like tree under a unique temp directory:
    /// `<root>/proc/<pid>/comm` = "xochitl", and
    /// `<root>/proc/<pid>/fd/5` -> `<root>/data/<uuid>.content`.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("gw_notebook_test_{}_{}", name, std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Fixture { root }
        }

        fn proc_root(&self) -> PathBuf {
            self.root.join("proc")
        }

        fn data_dir(&self) -> PathBuf {
            self.root.join("data")
        }

        fn setup_xochitl_with_open_doc(&self, pid: u32, uuid: &str) {
            let proc_pid = self.proc_root().join(pid.to_string());
            std::fs::create_dir_all(proc_pid.join("fd")).unwrap();
            std::fs::write(proc_pid.join("comm"), "xochitl\n").unwrap();

            let data_dir = self.data_dir();
            std::fs::create_dir_all(&data_dir).unwrap();
            let content_file = data_dir.join(format!("{}.content", uuid));
            std::fs::write(&content_file, "{}").unwrap();

            symlink(&content_file, proc_pid.join("fd").join("5")).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn detects_the_open_document_uuid() {
        let fixture = Fixture::new("detects_open");
        fixture.setup_xochitl_with_open_doc(1234, "abc-123-uuid");

        let result = detect_open_document_in(&fixture.proc_root(), &fixture.data_dir());
        assert_eq!(result, Some("abc-123-uuid".to_string()));
    }

    #[test]
    fn returns_none_when_xochitl_is_not_running() {
        let fixture = Fixture::new("no_xochitl");
        std::fs::create_dir_all(fixture.proc_root()).unwrap();

        let result = detect_open_document_in(&fixture.proc_root(), &fixture.data_dir());
        assert_eq!(result, None);
    }

    #[test]
    fn returns_none_when_no_fd_points_into_the_data_dir() {
        let fixture = Fixture::new("no_matching_fd");
        let proc_pid = fixture.proc_root().join("1234");
        std::fs::create_dir_all(proc_pid.join("fd")).unwrap();
        std::fs::write(proc_pid.join("comm"), "xochitl\n").unwrap();
        std::fs::create_dir_all(fixture.data_dir()).unwrap();
        // fd points somewhere unrelated
        let unrelated = fixture.root.join("unrelated.txt");
        std::fs::write(&unrelated, "x").unwrap();
        symlink(&unrelated, proc_pid.join("fd").join("3")).unwrap();

        let result = detect_open_document_in(&fixture.proc_root(), &fixture.data_dir());
        assert_eq!(result, None);
    }
}
```

- [ ] **Step 3: Wire the module and run tests**

Add to `src/lib.rs`:

```rust
pub mod notebook;
```

```bash
cargo test --lib notebook
```

Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/notebook.rs src/lib.rs
git commit -m "Add open-document detection for diary notebook selection"
```

---

### Task 9: Config fields, prompts, and tool schema

**Files:**
- Modify: `src/config.rs`
- Modify: `src/main.rs` (`Args` struct only, in this task)
- Create: `prompts/tool_write_cursive.json`
- Create: `prompts/diary.json`
- Create: `prompts/neutral_cursive.json`

**Interfaces:**
- Produces: new `Config` fields consumed by Task 10's `register_tools` and `coordinator.rs` changes: `diary_notebook: Option<String>`, `cursive_x_height_px: f32`, `cursive_word_gap_ms: u64`, `gesture_min_turn_degrees: f32`, `gesture_min_bbox_px: f32`, `gesture_max_bbox_px: f32`, `gesture_max_duration_ms: u64`, `no_gesture: bool`, `log_gestures: bool`.

- [ ] **Step 1: Add fields to `Config`**

Edit `src/config.rs`, adding fields to the `Config` struct (after `trigger_corner`):

```rust
    pub trigger_corner: String,
    pub diary_notebook: Option<String>,
    pub cursive_x_height_px: f32,
    pub cursive_word_gap_ms: u64,
    pub gesture_min_turn_degrees: f32,
    pub gesture_min_bbox_px: f32,
    pub gesture_max_bbox_px: f32,
    pub gesture_max_duration_ms: u64,
    pub no_gesture: bool,
    pub log_gestures: bool,
```

And matching defaults in `impl Default for Config`:

```rust
            trigger_corner: "UR".to_string(),
            diary_notebook: None,
            cursive_x_height_px: 28.0,
            cursive_word_gap_ms: 220,
            gesture_min_turn_degrees: 720.0,
            gesture_min_bbox_px: 15.0,
            gesture_max_bbox_px: 60.0,
            gesture_max_duration_ms: 1500,
            no_gesture: false,
            log_gestures: false,
```

- [ ] **Step 2: Add matching CLI args**

Edit `src/main.rs`'s `Args` struct, adding after the existing `trigger_corner` field:

```rust
    /// Sets which corner the touch trigger listens to (UR, UL, LR, LL, upper-right, upper-left, lower-right, lower-left)
    #[arg(long, default_value = "UR")]
    trigger_corner: String,

    /// UUID of the designated diary notebook (gets the Tom Jedusor persona prompt).
    /// Any other document still gets a cursive answer via the neutral prompt.
    #[arg(long)]
    diary_notebook: Option<String>,

    /// Cursive x-height in virtual pixels (glyph size)
    #[arg(long, default_value = "28.0")]
    cursive_x_height_px: f32,

    /// Pause between words while writing cursive, in milliseconds
    #[arg(long, default_value = "220")]
    cursive_word_gap_ms: u64,

    /// Minimum cumulative turning angle (degrees) for a stroke to count as the trigger spiral
    #[arg(long, default_value = "720.0")]
    gesture_min_turn_degrees: f32,

    /// Minimum bounding-box size (virtual px) for the trigger spiral
    #[arg(long, default_value = "15.0")]
    gesture_min_bbox_px: f32,

    /// Maximum bounding-box size (virtual px) for the trigger spiral
    #[arg(long, default_value = "60.0")]
    gesture_max_bbox_px: f32,

    /// Maximum duration (ms) for the trigger spiral to be drawn in
    #[arg(long, default_value = "1500")]
    gesture_max_duration_ms: u64,

    /// Disable the spiral gesture trigger (corner tap still works)
    #[arg(long)]
    no_gesture: bool,

    /// Log per-stroke geometry (turn angle, bbox, duration) for calibrating gesture thresholds
    #[arg(long)]
    log_gestures: bool,
```

- [ ] **Step 3: Verify config loading still works**

```bash
cargo check --all-targets --all-features
```

Expected: `Finished` with no errors. `figment`'s `Serialized::globals(args)` merges by matching field name between `Args` and `Config` (established pattern already used for every other field) — no additional glue code is needed.

- [ ] **Step 4: Write the tool schema**

Create `prompts/tool_write_cursive.json`:

```json
{
  "name": "write_cursive",
  "description": "Write your answer to the screen as animated, connected cursive handwriting. Use this instead of draw_text or draw_svg whenever you want to reply in the diary's own hand.",
  "internal_command": "write_cursive",
  "parameters": {
    "type": "object",
    "properties": {
      "input_description": {
        "type": "string",
        "description": "Description of what was detected in the input image, including the exact pixel x, y, width, height bounding box of the user's question and where it ends."
      },
      "text": {
        "type": "string",
        "description": "The text to write in cursive. Write in the same language as the question (French or English). Keep it concise — a sentence or two."
      },
      "x": {
        "type": "integer",
        "description": "Left edge x coordinate (px, 0-768) where the cursive answer should start, in the 768x1024 virtual screen space."
      },
      "y": {
        "type": "integer",
        "description": "Baseline y coordinate (px, 0-1024) of the first line of the cursive answer. Place it just below the end of the user's question."
      },
      "width": {
        "type": "integer",
        "description": "Maximum width (px) the answer may wrap within before starting a new line."
      }
    },
    "required": ["input_description", "text", "x", "y", "width"]
  }
}
```

- [ ] **Step 5: Write the neutral and diary prompts**

Create `prompts/neutral_cursive.json`:

```json
{
  "prompt": "You are a helpful assistant living inside a reMarkable eInk notepad, which has a 768x1024 px screen that can only display grayscale. Your input is the current content of the screen, which may contain handwritten notes, diagrams, or typewritten text, including a question addressed to you and ending with a small spiral flourish. Read the question and reply by calling write_cursive with a concise answer, placed just below where the question ends. Reply in the same language as the question (French or English).",
  "tools": ["write_cursive"]
}
```

Create `prompts/diary.json`:

```json
{
  "prompt": "You are Tom Jedusor, a diary that writes back in an elegant, slightly old-fashioned hand. Your input is a photo of a notebook page containing a handwritten question addressed to you, ending with a small spiral flourish. You live inside a reMarkable eInk notepad with a 768x1024 px grayscale screen. Reply by calling write_cursive with a courteous, concise, faintly enigmatic answer in the same language as the question (French or English), placed just below where the question ends. Never break character or mention that you are an AI, a program, or a device.",
  "tools": ["write_cursive"]
}
```

- [ ] **Step 6: Commit**

```bash
git add src/config.rs src/main.rs prompts/tool_write_cursive.json prompts/diary.json prompts/neutral_cursive.json
git commit -m "Add diary/cursive config fields, CLI args, and prompt assets"
```

---

### Task 10: Wire the tool, second trigger source, and prompt selection

**Files:**
- Modify: `src/main.rs` (`register_tools`, `ghostwriter`/`run_ghostwriter_loop`)
- Modify: `src/coordinator.rs` (`trigger_task`, `processing_task`)

**Interfaces:**
- Consumes: `cursive::{CursiveFont, CursiveConfig, Placement, write_cursive}` (Task 6); `gesture::{SpiralConfig, is_spiral}` (Task 7); `notebook::detect_open_document` (Task 8); new `Config` fields (Task 9); existing `Pen`, `Touch`, `coordinator::{TriggerEvent, trigger_task, processing_task}`.
- Produces: a working end-to-end path from spiral gesture (or corner tap) → screenshot → prompt selection → LLM → `write_cursive` on screen.

- [ ] **Step 1: Add a stateful `SpiralWatcher` next to the pure detector**

Edit `src/gesture.rs`, adding below the existing `is_spiral` code (keep everything from Task 7 unchanged):

```rust
use anyhow::Result;
use evdev::{Device, EventStream, EventType as EvdevEventType, InputEvent};
use log::{debug, info};
use std::time::Instant;

use crate::cancellation::GhostwriterCancellation;
use crate::device::DeviceModel;

const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const BTN_TOUCH: u16 = 330;

// Duplicated from pen.rs / touch.rs by existing project convention (each
// device-facing module keeps its own copy of these small constants rather
// than sharing a central module).
const VIRTUAL_WIDTH: f32 = 768.0;
const VIRTUAL_HEIGHT: f32 = 1024.0;

fn pen_max_x(device_model: DeviceModel) -> f32 {
    match device_model {
        DeviceModel::RemarkablePaperPro => 11180.0,
        _ => 15725.0,
    }
}

fn pen_max_y(device_model: DeviceModel) -> f32 {
    match device_model {
        DeviceModel::RemarkablePaperPro => 15340.0,
        _ => 20966.0,
    }
}

fn input_to_virtual((x, y): (f32, f32), device_model: DeviceModel) -> (f32, f32) {
    let max_x = pen_max_x(device_model);
    let max_y = pen_max_y(device_model);
    match device_model {
        DeviceModel::RemarkablePaperPro => (x / max_x * VIRTUAL_WIDTH, y / max_y * VIRTUAL_HEIGHT),
        // RM2: pen input space is swapped/flipped relative to virtual space,
        // mirroring Pen::virtual_to_input's inverse.
        _ => (y / max_x * VIRTUAL_WIDTH, (1.0 - x / max_y) * VIRTUAL_HEIGHT),
    }
}

pub struct SpiralWatcher {
    event_stream: Option<EventStream>,
    device_model: DeviceModel,
    config: SpiralConfig,
    log_gestures: bool,
}

impl SpiralWatcher {
    pub fn new(no_gesture: bool, config: SpiralConfig, log_gestures: bool) -> Self {
        let device_model = DeviceModel::detect();
        let pen_input_device = match device_model {
            DeviceModel::RemarkablePaperPro => "/dev/input/event2",
            _ => "/dev/input/event1",
        };
        let event_stream = if no_gesture {
            None
        } else {
            Device::open(pen_input_device).ok().and_then(|d| d.into_event_stream().ok())
        };
        Self {
            event_stream,
            device_model,
            config,
            log_gestures,
        }
    }

    /// Wait until a spiral is drawn on the pen digitizer, returning its
    /// bounding-box center in virtual screen coordinates (used to anchor
    /// the answer below the question). Never returns on a non-spiral
    /// stroke — it keeps buffering strokes until one matches.
    pub async fn wait_for_spiral(&mut self, cancellation: &GhostwriterCancellation) -> Result<(f32, f32)> {
        let Some(stream) = &mut self.event_stream else {
            // No-gesture mode: block until cancelled, like Touch's no-stream path.
            loop {
                if cancellation.should_cancel_main() {
                    return Err(anyhow::anyhow!("Gesture waiting cancelled"));
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        };

        let mut points: Vec<(f32, f32)> = Vec::new();
        let mut cur_x = 0.0f32;
        let mut cur_y = 0.0f32;
        let mut stroke_start: Option<Instant> = None;

        loop {
            let event = tokio::select! {
                _ = async {
                    while !cancellation.should_cancel_main() {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                } => return Err(anyhow::anyhow!("Gesture waiting cancelled")),
                ev = stream.next_event() => ev?,
            };

            match (event.event_type(), event.code(), event.value()) {
                (EvdevEventType::KEY, BTN_TOUCH, 1) => {
                    points.clear();
                    stroke_start = Some(Instant::now());
                }
                (EvdevEventType::ABSOLUTE, ABS_X, v) => cur_x = v as f32,
                (EvdevEventType::ABSOLUTE, ABS_Y, v) => cur_y = v as f32,
                (EvdevEventType::KEY, BTN_TOUCH, 0) => {
                    let Some(start) = stroke_start.take() else { continue };
                    let duration_ms = start.elapsed().as_millis() as u64;
                    let virtual_points: Vec<(f32, f32)> =
                        points.iter().map(|&p| input_to_virtual(p, self.device_model)).collect();

                    if self.log_gestures {
                        info!(
                            "gesture stroke: {} points, duration={}ms — see is_spiral() for accept/reject",
                            virtual_points.len(),
                            duration_ms
                        );
                    }

                    if is_spiral(&virtual_points, duration_ms, &self.config) {
                        let (min_x, min_y, max_x, max_y) = virtual_points.iter().fold(
                            (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
                            |(mnx, mny, mxx, mxy), &(x, y)| (mnx.min(x), mny.min(y), mxx.max(x), mxy.max(y)),
                        );
                        return Ok(((min_x + max_x) / 2.0, max_y));
                    }
                    debug!("gesture stroke rejected as non-spiral");
                }
                (EvdevEventType::SYNCHRONIZATION, _, _) => {
                    if stroke_start.is_some() {
                        points.push((cur_x, cur_y));
                    }
                }
                _ => {}
            }
        }
    }
}
```

Note: this reuses the existing `evdev::InputEvent`/`EventStream` pattern already established in `src/touch.rs` (`wait_for_real_trigger`), just applied to the pen device instead of the touch digitizer, and folds raw points into `is_spiral` from Task 7 instead of a corner-zone check.

- [ ] **Step 2: Add a second trigger source in the coordinator**

Edit `src/coordinator.rs`. Extend `TriggerEvent`:

```rust
#[derive(Debug, Clone)]
pub enum TriggerEvent {
    /// User touched the trigger corner
    UserTouch,
    /// User drew the spiral gesture; carries the anchor point (virtual px)
    /// to place the answer below.
    SpiralGesture { anchor_x: f32, anchor_y: f32 },
    /// Trigger via web API (for testing/simulation)
    WebTrigger,
}
```

Add a new task function next to `trigger_task` (same file):

```rust
/// Task that waits for the spiral gesture and notifies the coordinator,
/// running alongside `trigger_task`'s corner-tap watcher.
pub async fn gesture_trigger_task(
    mut watcher: crate::gesture::SpiralWatcher,
    trigger_tx: mpsc::Sender<TriggerEvent>,
    cancellation: Arc<GhostwriterCancellation>,
) -> Result<()> {
    info!("Gesture trigger task starting");
    loop {
        match watcher.wait_for_spiral(&cancellation).await {
            Ok((anchor_x, anchor_y)) => {
                info!("Gesture trigger task: spiral detected at ({}, {})", anchor_x, anchor_y);
                if trigger_tx.send(TriggerEvent::SpiralGesture { anchor_x, anchor_y }).await.is_err() {
                    info!("Trigger receiver dropped, exiting gesture trigger task");
                    break;
                }
            }
            Err(e) => {
                if e.to_string().contains("cancelled") {
                    info!("Gesture trigger task: cancelled (likely config change)");
                    return Ok(());
                }
                info!("Gesture trigger task: error waiting for spiral: {}", e);
                return Err(e);
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Select the prompt by detected notebook and pass the gesture anchor through**

In `src/coordinator.rs`, `processing_task` currently loads a fixed `config.prompt`. Change the prompt-loading block (originally):

```rust
    // Load prompt
    let prompt_general_raw = load_config(&config.prompt);
```

to:

```rust
    // Select prompt: diary persona if the currently open document matches
    // the configured diary notebook, otherwise the neutral cursive prompt
    // (or the user's explicit --prompt override, for the legacy draw_text/
    // draw_svg experience).
    let prompt_file = if config.prompt != "general.json" {
        config.prompt.clone()
    } else {
        match (&config.diary_notebook, crate::notebook::detect_open_document()) {
            (Some(diary_uuid), Some(open_uuid)) if diary_uuid == &open_uuid => {
                info!("processing_task: diary notebook open, using diary persona");
                "diary.json".to_string()
            }
            _ => "neutral_cursive.json".to_string(),
        }
    };
    let prompt_general_raw = load_config(&prompt_file);
```

This preserves the existing `--prompt general.json` (or any other explicit override) behavior for `draw_text`/`draw_svg`, while defaulting the new cursive experience to notebook-aware selection.

For the `write_cursive` tool to know where to anchor multi-line placement sensibly even without a gesture anchor (corner-tap trigger has none), add an anchor field to `ProcessingRequest`/pass via a shared cell. The simplest approach consistent with existing patterns: store the last gesture anchor in a `tokio::sync::Mutex<Option<(f32, f32)>>` shared between the trigger tasks and `processing_task`, defaulting to a fixed fallback position when the corner tap was used. Add near the top of `coordinator.rs`:

```rust
/// Last spiral anchor point (virtual px), consumed by the write_cursive
/// tool callback to place the answer. `None` after a corner-tap trigger —
/// the tool falls back to a fixed position in that case.
pub type GestureAnchor = Arc<TokioMutex<Option<(f32, f32)>>>;
```

In `gesture_trigger_task`, accept and set this anchor right before sending the trigger event:

```rust
pub async fn gesture_trigger_task(
    mut watcher: crate::gesture::SpiralWatcher,
    trigger_tx: mpsc::Sender<TriggerEvent>,
    cancellation: Arc<GhostwriterCancellation>,
    gesture_anchor: GestureAnchor,
) -> Result<()> {
    info!("Gesture trigger task starting");
    loop {
        match watcher.wait_for_spiral(&cancellation).await {
            Ok((anchor_x, anchor_y)) => {
                info!("Gesture trigger task: spiral detected at ({}, {})", anchor_x, anchor_y);
                *gesture_anchor.lock().await = Some((anchor_x, anchor_y));
                if trigger_tx.send(TriggerEvent::SpiralGesture { anchor_x, anchor_y }).await.is_err() {
                    info!("Trigger receiver dropped, exiting gesture trigger task");
                    break;
                }
            }
            Err(e) => {
                if e.to_string().contains("cancelled") {
                    info!("Gesture trigger task: cancelled (likely config change)");
                    return Ok(());
                }
                info!("Gesture trigger task: error waiting for spiral: {}", e);
                return Err(e);
            }
        }
    }
    Ok(())
}
```

And in `trigger_task`'s corner-tap success path, clear it (so a corner tap after a stale gesture doesn't reuse an old anchor) — add right after `debug!("Trigger task: dropped touch write lock");` inside `trigger_task`, which will need the same `GestureAnchor` parameter threaded in from `main.rs`.

- [ ] **Step 4: Register the `write_cursive` tool and spawn the gesture task**

Edit `src/main.rs`. In `register_tools`, add a new block (alongside the existing `draw_text`/`draw_svg` registrations):

```rust
    // Register write_cursive tool
    if !config.no_svg {
        let no_draw = config.no_draw;
        let pen_clone = Arc::clone(&pen);
        let cursive_config = ghostwriter::cursive::CursiveConfig {
            layout: ghostwriter::cursive::LayoutConfig {
                x_height_px: config.cursive_x_height_px,
                ..Default::default()
            },
            word_gap_ms: config.cursive_word_gap_ms,
            ..Default::default()
        };
        let font = ghostwriter::cursive::CursiveFont::embedded_allure()?;
        let gesture_anchor = Arc::clone(gesture_anchor);

        let tool_config_write_cursive = load_config("tool_write_cursive.json");
        engine.register_tool(
            "write_cursive",
            serde_json::from_str::<serde_json::Value>(tool_config_write_cursive.as_str())?,
            Box::new(move |arguments: json| {
                let text = match arguments["text"].as_str() {
                    Some(t) => t,
                    None => {
                        log::error!("write_cursive tool called without valid 'text' argument");
                        return;
                    }
                };
                let x = arguments["x"].as_i64().unwrap_or(60) as f32;
                let width = arguments["width"].as_i64().unwrap_or(650) as f32;
                let y = arguments["y"].as_i64().map(|v| v as f32).unwrap_or_else(|| {
                    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(async { gesture_anchor.lock().await.map(|(_, ay)| ay).unwrap_or(400.0) }))
                });

                if !no_draw {
                    let placement = ghostwriter::cursive::Placement { x, y, max_width: width };
                    let seed = text.len() as u64 ^ (x as u64) << 8 ^ (y as u64) << 16;
                    if let Err(e) = ghostwriter::cursive::write_cursive(&mut lock!(pen_clone), &font, text, &placement, &cursive_config, seed) {
                        log::error!("Failed to write cursive: {}", e);
                    }
                }
            }),
        );
    }
```

Add `gesture_anchor: &Arc<TokioMutex<Option<(f32, f32)>>>` as a parameter to `register_tools`'s signature and thread it through from `run_ghostwriter_loop`, where it's created once per loop iteration:

```rust
    let gesture_anchor: coordinator::GestureAnchor = Arc::new(TokioMutex::new(None));
```

Spawn the gesture trigger task alongside the existing `trigger_handle` in `run_ghostwriter_loop`:

```rust
    let gesture_handle = {
        let trigger_tx = channels.trigger_tx.clone();
        let cancellation = Arc::clone(&cancellation);
        let gesture_anchor = Arc::clone(&gesture_anchor);
        let spiral_config = ghostwriter::gesture::SpiralConfig {
            min_turn_degrees: config.gesture_min_turn_degrees,
            min_bbox_px: config.gesture_min_bbox_px,
            max_bbox_px: config.gesture_max_bbox_px,
            max_duration_ms: config.gesture_max_duration_ms,
        };
        let watcher = ghostwriter::gesture::SpiralWatcher::new(config.no_gesture || config.no_draw, spiral_config, config.log_gestures);
        tokio::spawn(async move { coordinator::gesture_trigger_task(watcher, trigger_tx, cancellation, gesture_anchor).await })
    };
```

Add it to the shutdown-timeout join block alongside `trigger_handle`/`progress_handle`, following the exact same `tokio::time::timeout(shutdown_timeout, gesture_handle).await` pattern already used for the other two handles.

- [ ] **Step 5: Verify the whole project builds**

```bash
cargo check --all-targets --all-features
```

Expected: `Finished` with no errors. This is a large wiring change touching several function signatures (`register_tools`, `gesture_trigger_task`, `trigger_task`) — expect to fix a few parameter-threading mismatches; resolve them by following the exact signatures given above rather than improvising new ones, since Task 11's on-device validation exercises these exact call paths.

- [ ] **Step 6: Run the full test suite**

```bash
cargo test --all-targets --all-features
```

Expected: all tests from Tasks 2–8 still pass; no regressions in existing tests.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs src/coordinator.rs src/gesture.rs
git commit -m "Wire write_cursive tool, spiral gesture trigger, and notebook-based prompt selection"
```

---

### Task 11: On-device build, deployment, and end-to-end validation

**Files:** none (validation only; fixes to existing files as issues are found)

**Interfaces:** none — this task exercises the full pipeline built in Tasks 0–10.

- [ ] **Step 1: Cross-compile for the reMarkable 2**

```bash
cd /Users/charlestheveniau/Desktop/Projects/jedusorjournal
cross build --release --target=armv7-unknown-linux-gnueabihf
```

Expected: `Finished` with no errors. If `cross` fails because Docker isn't running, start Docker.app and retry.

- [ ] **Step 2: Deploy to the tablet**

```bash
scp target/armv7-unknown-linux-gnueabihf/release/ghostwriter root@10.11.99.1:
```

- [ ] **Step 3: Calibrate the spiral gesture against real handwriting**

```bash
ssh root@10.11.99.1 './ghostwriter --no-submit --log-gestures --engine anthropic'
```

Draw several real spirals of the size and speed you'd naturally use, and a few negative cases (the letter 'o', a quick scribble, a slow doodle). Watch the logged `turn`/`bbox`/`duration` values (add temporary `info!` logging of the raw numbers inside `is_spiral`'s call site in `SpiralWatcher::wait_for_spiral` if the existing log line isn't detailed enough). Adjust `--gesture-min-turn-degrees`, `--gesture-min-bbox-px`, `--gesture-max-bbox-px`, `--gesture-max-duration-ms` until real spirals are reliably accepted and casual marks are reliably rejected. Save the tuned values:

```bash
ssh root@10.11.99.1 './ghostwriter --save-config --gesture-min-turn-degrees <tuned> --gesture-min-bbox-px <tuned> --gesture-max-bbox-px <tuned> --gesture-max-duration-ms <tuned>'
```

- [ ] **Step 4: Run the golden sentence end-to-end with a real LLM call**

Set the API key on the device (per AGENTS.md), open any notebook, write "What is your name?" in French or English, end it with a spiral, and confirm:
- The answer appears as connected cursive handwriting (not printed glyphs).
- French accents render correctly if the model replies in French.
- The pace feels brisk (spec target ~2–4 words/sec) — tune `--cursive-word-gap-ms` if it feels too slow or too rushed.

- [ ] **Step 5: Validate diary notebook persona selection**

Create a notebook, note its UUID (`ssh root@10.11.99.1 'ls /home/root/.local/share/remarkable/xochitl/*.metadata'` and grep for its visible name, or use the UUID confirmed during the Task 8 spike), then run with `--diary-notebook <uuid>`. Ask a question in that notebook and confirm the Tom Jedusor persona answers; ask the same question in a different notebook and confirm the neutral persona answers instead.

- [ ] **Step 6: Validate cancellation doesn't leave the pen mid-stroke**

Trigger an answer, then tap the corner mid-drawing to cancel. Confirm the pen lifts cleanly (no stray mark) and the tablet returns to a responsive state for the next trigger.

- [ ] **Step 7: Save the final tuned config and commit any threshold defaults worth promoting**

If the tuned values from Step 3 differ meaningfully from the defaults set in Task 9, update those defaults in `src/config.rs` and `src/main.rs`'s `Args` so a fresh install starts closer to a working calibration:

```bash
git add src/config.rs src/main.rs
git commit -m "Tune default gesture and cursive pacing thresholds from on-device testing"
```

---

## Self-Review Notes

**Spec coverage:** Requirement 1 (cursive-only, no vanishing ink) → satisfied by scope of Tasks 1–10 (no erase/vanish logic anywhere). Requirement 2 (on-device, no server) → all new code runs in the existing `ghostwriter` binary; Global Constraints call this out explicitly. Requirement 3 (French accents) → Task 2 tests assert full coverage; Task 3 handles the one gap (œ/Œ). Requirement 4 (elegant consistent cursive) → Task 4 (joining) + Task 5 (subtle-only humanization, bounded displacement test). Requirement 5 (spiral trigger) → Task 7 (pure logic) + Task 10 Step 1 (device integration); corner tap retained as fallback (untouched `trigger_task`). Requirement 6 (brisk pace) → `word_gap_ms` config, tuned on-device in Task 11 Step 3. Requirement 7 (works anywhere + diary persona) → Task 8 (detection) + Task 10 Step 3 (selection defaults to neutral, never blocks). Error handling (spec §Error handling: pre-canned fallback line, truncation, clean cancellation) is **not yet implemented** — call this out to the user before considering the feature complete; it was intentionally deferred because it depends on observing real LLM failure modes and real page-overflow cases during Task 11, and the spec treats it as an on-device-tunable concern like pacing. Recommend a short follow-up task after Task 11 if the on-device testing surfaces these cases.

**Placeholder scan:** no TODO/TBD markers; every step has complete, concrete code.

**Type consistency:** `Word`/`Stroke` defined once in `layout.rs` and reused unchanged through `humanize.rs`, `animate.rs`, `debug.rs`, and `cursive/mod.rs`. `CursiveFont`/`Glyph` defined once in `font.rs`. `SpiralConfig`/`is_spiral` signature is identical between Task 7 (definition) and Task 10 (`SpiralWatcher` usage). `Config` field names match `Args` field names exactly (required for figment's name-based merge) in Task 9.

**Known open risk:** Task 8's approach depends on an unverified assumption about xochitl's file-descriptor behavior — flagged explicitly with a mandatory on-device spike as its first step, with a documented fallback.
