# Tom Riddle Diary — Design

**Date:** 2026-07-09
**Status:** Approved by Charles (this session)
**Base:** fork of awwaiid/ghostwriter (this repo)

## Goal

Recreate the Tom Riddle diary effect from *Harry Potter and the Chamber of Secrets* on a reMarkable 2 (firmware 3.27.3.0): the user handwrites a question in a notebook and ends it with a small spiral flourish; moments later an answer writes itself onto the page in elegant, connected cursive handwriting, stroke by stroke, at brisk human writing pace.

## Requirements (validated with user)

1. **Scope of effect:** animated cursive answer only. No vanishing/erasing of the user's writing.
2. **Architecture:** fully on-device (rM2, armv7). Only external calls are the existing LLM APIs. No companion server (research showed font-based rendering is cheap; a server adds nothing today — see Alternatives).
3. **Language:** French and English. Accented characters (é è à ç ù â ê î ô û ë ï ü, œ via "oe" substitution) are a **hard requirement**.
4. **Handwriting style:** elegant, consistent, connected cursive — faithful to the movie prop. Not messy/organic-variable; not a mimic of the user's hand.
5. **Trigger:** a deliberately drawn pen gesture — a small spiral — at the end of the question. No pause-detection (too laggy/false-positive-prone), corner-tap retained only as fallback/cancel.
6. **Pace:** brisk handwriting, ~2–4 words/second of drawing.
7. **Availability:** the trigger works **anywhere** (any notebook/document). A designated diary notebook additionally gets a special system prompt (Tom Jedusor persona); other documents get a neutral prompt with the same cursive output.

## Architecture

Current ghostwriter flow: touch corner → screenshot → vision LLM → LLM tool call (`draw_text` via virtual keyboard, or `draw_svg` via simulated pen).

New flow:

```
User writes question, ends with spiral ⌾
      │
[GestureWatcher]  src/gesture.rs — reads pen events (/dev/input/event1),
      │           detects spiral, records its bounding box, sends TriggerEvent
      ▼
[processing_task] coordinator.rs — unchanged skeleton: screenshot → prompt → LLM
      │           NEW: detect currently-open document; if it matches the configured
      │           diary notebook UUID → diary.json persona prompt, else neutral prompt
      ▼
LLM (vision)      replies via NEW tool: write_cursive { text, x, y, width }
      ▼
[CursiveRenderer] src/cursive/ — text → connected single-line cursive polylines
      ▼
[Pen]             pen.rs (existing primitives) — strokes streamed as pen events
                  at brisk pace; e-ink shows them appearing as if written
```

### Key decisions

- **The LLM never draws letterforms.** It returns plain text plus a placement box. Letter shapes are deterministic Rust. (Quality guarantee; also makes rendering testable offline.)
- **Existing `draw_text` / `draw_svg` tools remain** available via the existing prompt config; the diary experience is a new prompt + tool set, not a replacement.
- **Spiral position anchors the answer.** The gesture's bounding box tells us where the question ended; the answer is laid out below it. The LLM's placement box is a hint, clamped by the renderer to sane page bounds.

### New/changed components

| Component | Status | Purpose |
|---|---|---|
| `src/gesture.rs` | new | Spiral detection on pen event stream |
| `src/cursive/font.rs` | new | SVG 1.1 font parser (glyph → strokes) |
| `src/cursive/layout.rs` | new | Word-wrap, glyph placement, letter joining |
| `src/cursive/humanize.rs` | new | Subtle noise: wobble, jitter, spacing variation |
| `src/cursive/animate.rs` | new | Writing-order pacing → pen events |
| `src/notebook.rs` | new | Detect currently open xochitl document UUID |
| `prompts/diary.json` | new | Tom Jedusor persona prompt (French-first) |
| `prompts/neutral_cursive.json` | new | Default prompt for non-diary documents |
| `prompts/tool_write_cursive.json` | new | Tool schema: text, x, y, width |
| `main.rs` | edit | Register `write_cursive` tool |
| `coordinator.rs` | edit | Second trigger source; prompt selection by notebook |
| `config.rs` | edit | diary notebook id, font choice, pace, gesture thresholds |
| `pen.rs` | edit (additive) | One new paced-drawing entry point |

## CursiveRenderer design

1. **Font load (startup, once).** EMS Allure — a SIL-OFL single-line cursive SVG font from the Hershey Text v3 distribution (verified: full Latin-1 accent coverage, 216 glyphs; only œ missing → text preprocessing substitutes "oe"). Embedded via `rust-embed` like existing prompt assets. Parsed with `roxmltree` + `svgtypes`; `svg2polylines` (already a dependency) flattens curves. Note: `usvg`/`resvg` cannot parse SVG fonts (dropped in SVG 2) — manual parse is required and trivial (`<glyph unicode="…" horiz-adv-x="…" d="…"/>`, absolute M/L/C commands only). Font is config-swappable: the smoothed Cutlings variant of Allure, nine sibling EMS script fonts (same verified coverage), or Mistral SingleLine (requires one-time offline glyph extraction) can be dropped in later.
2. **Layout.** Greedy word-wrap within the placement box. Default size ≈28 px x-height in the 768×1024 virtual space; line spacing ≈2.2× x-height. Unknown characters are skipped with a log entry — never a panic.
3. **Letter joining.** EMS Allure glyphs enter/exit strokes at a consistent connection height (y=183/1000 font units, geometrically verified). Where glyph N's exit and glyph N+1's entry both fall in the connection zone, bridge them into one continuous polyline — the pen stays down across the word. Word gaps and accent subpaths remain pen lifts.
4. **Humanizer.** Low-frequency smooth noise, seeded per answer: baseline wobble ~±1.5% of x-height, per-glyph rotation ±1.5°, scale ±2%, slight letter-spacing variation. Amplitudes deliberately small: "living hand", not "drunk robot".
5. **Animator.** Strokes in natural writing order (falls out of layout). Event pacing targets ~2.5 words/sec: rate scaled by stroke length, micro-pauses between words, longer between lines. Gentle pressure variation along strokes for organic line weight. Uses existing `Pen` primitives (`pen_down_at`, `goto_xy`, `pen_up`).

## Trigger design

**Spiral detection.** Background task buffers the current pen stroke from `/dev/input/event1`. A stroke triggers when all hold:
- cumulative signed turning angle ≥ ~720° (two loops, consistent winding),
- bounding box ~15–60 px in virtual space (deliberate mark; rejects 'o', 'e', casual circles),
- drawn within ≤1.5 s.

Guards: detection **suspended while ghostwriter is drawing** (else it would see its own synthetic strokes); post-trigger cooldown prevents double fires. Thresholds live in config; a `--log-gestures` mode prints per-stroke metrics (turn angle, bbox, duration) for calibration against the user's real hand. Corner-tap trigger remains as fallback, and any touch during processing cancels (existing behavior).

**Diary notebook detection.** At trigger time, identify the open document by inspecting which `~/.local/share/remarkable/xochitl/<uuid>` content files the xochitl process holds open (procfs fd scan). Match against `diary_notebook` config → `diary.json` persona; no match or detection failure → neutral prompt. Detection failure is logged, never fatal.

**Persona.** `diary.json`: Tom Jedusor — replies in the language of the question (French-first user), concise, courteous, enigmatic, slightly old-fashioned diction; never breaks fiction; answers placed below the question.

## Error handling

- **LLM unreachable/timeout/API error:** the diary writes a short pre-canned cursive line (e.g. "Je suis fatigué ce soir…") so the fiction never breaks with a typed error; the real error goes to the log.
- **Answer exceeds remaining page space:** shrink font one step; if still too long, truncate at a sentence boundary and append "…".
- **Cancellation mid-answer:** corner tap stops the animator between strokes (pen always lifted cleanly, never mid-stroke).
- **Malformed LLM tool call:** re-prompt once (existing engine behavior), else fall back to pre-canned line.

## Testing

1. **Unit/golden tests (Mac, `cargo test`):** font parser glyph counts and accent presence; layout wrapping; join bridging; golden PNG render of "Bonjour, je m'appelle Tom Jedusor. Où étais-tu ?" for visual verification of accents and joins.
2. **Offline end-to-end:** existing `--input-png … --no-draw --save-bitmap` path exercises trigger→LLM→render without a device.
3. **Gesture calibration:** `--log-gestures` on-device; tune thresholds against real spirals and real French handwriting (accents produce small fast strokes that must not false-trigger).
4. **On-device:** cross-compile armv7 (`./build.sh`), `scp` to tablet, run the golden sentence, tune pace/size by eye.

## Alternatives considered

- **Neural handwriting synthesis (calligrapher.ai / Graves RNN and successors):** rejected — verified alphabet has zero accented characters (missing even Q, X, Z); French support would require retraining on a corpus that doesn't exist off-the-shelf; heavy runtime; inconsistent output conflicts with the "elegant consistent" requirement.
- **Companion server rendering:** rejected for now — font rendering is pure geometry and runs on-device; a server adds deployment + network fragility for zero visual gain. The renderer's clean text-in/polylines-out interface means a server (e.g. for future handwriting mimicry) can be added later without redesign.
- **Commercial single-line fonts (singlelinefonts.com, $7–20):** unnecessary; accent coverage unverified pre-purchase, while the free options are verified.

## Out of scope (possible later phases)

- Vanishing-ink effect on the user's question (eraser simulation / document manipulation).
- Mimicking the user's own handwriting.
- Conversation memory for the diary notebook (persona currently sees only the current page screenshot).
- reMarkable Paper Pro support for the new features (architecture keeps the door open; rM2 is the target device).
