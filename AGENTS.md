# AGENTS.md

This file provides guidance to Claude Code (claude.ai/code) and other similar tools when working with code in this repository.

## Project Overview

Ghostwriter is a Vision-LLM agent for the reMarkable tablet that watches handwritten input and responds by drawing or typing back to the screen. It takes screenshots, sends them to LLM APIs (OpenAI, Anthropic, Google), and uses the responses to interact with the device through simulated pen and keyboard input.

## Core Architecture

- **main.rs**: Main application entry point with CLI argument parsing and orchestration
- **coordinator.rs**: The trigger, progress and processing tasks (screenshot → LLM → tool call)
- **config.rs**: Settings layered from defaults, `~/.ghostwriter.toml`, `GHOSTWRITER_*` env vars and CLI flags
- **cancellation.rs**: Tokens for interrupting a run or restarting on a config change
- **LLM Engine Layer** (`src/llm_engine/`): Pluggable backends for different AI providers
  - `openai.rs`: OpenAI API integration (GPT models)
  - `anthropic.rs`: Anthropic API integration (Claude models) 
  - `google.rs`: Google API integration (Gemini models)
  - `mod.rs`: Common LLM engine trait, plus the HTTP request and tool dispatch the engines share
- **Device Interaction** (`src/`):
  - `device.rs`: Device detection and every per-device value (input device paths, pen/touch ranges, framebuffer size, the virtual page size)
  - `screenshot.rs`: Screen capture by reading the framebuffer out of xochitl's memory
  - `pen.rs`: SVG drawing and bitmap rendering to screen 
  - `keyboard.rs`: Virtual keyboard input via uinput
  - `touch.rs`: Touch event detection, simulated taps, and pen tool selection through the palette
- **Image Processing**:
  - `segmenter.rs`: Image segmentation to provide spatial context to LLMs
  - `skeleton.rs`: Thinning and tracing for single-stroke drawing experiments
  - `util.rs`: SVG to bitmap conversion, uinput setup and other utilities
- **Configuration**:
  - `prompts/`: JSON-based prompt templates and tool definitions
  - `embedded_assets.rs`: Bundled configuration files
- **Off-device and experiments**:
  - `simulation/`: Scripted touches and canned screenshots for `--test-mode`
  - `web_server.rs`, `web/`: Optional configuration UI (`--web-server`)
  - `bin/experiment.rs`: Command-line tool for trying pen and touch techniques on a Paper Pro

## Common Development Commands

### Building
```bash
# Local development build
cargo build --release
# or
./build.sh local

# Cross-compile for reMarkable2 (armv7)
cross build --release --target=armv7-unknown-linux-gnueabihf
# or
./build.sh

# Cross-compile for reMarkable Paper Pro, Paper Pro Move and Paper Pure (aarch64)
cross build --release --target=aarch64-unknown-linux-gnu
# or
./build.sh rmpp
```

### Code Quality and Formatting
```bash
# Format code with rustfmt
cargo fmt

# Check formatting without applying changes
cargo fmt -- --check

# Run clippy the way CI does
cargo clippy --all-targets --all-features -- -D warnings

# Run the unit tests
cargo test

# Check code compiles
cargo check --all-targets --all-features
```

### Testing and Evaluation
```bash
# Run evaluation suite across multiple models and configurations
./run_eval.sh

# Test with local input file (no device required)
./target/release/ghostwriter \
  --input-png evaluations/test_case/input.png \
  --output-file tmp/result.out \
  --save-bitmap tmp/result.png \
  --no-draw --no-loop --no-trigger

# Run with specific model and options
./ghostwriter --model gpt-4o-mini --apply-segmentation --thinking
```

### Deployment
```bash
# Deploy to reMarkable (replace IP address)
scp target/armv7-unknown-linux-gnueabihf/release/ghostwriter root@192.168.1.117:
# or for Paper Pro
scp target/aarch64-unknown-linux-gnu/release/ghostwriter root@192.168.1.117:
```

## Key Development Notes

### Cross-compilation Setup Required
- Install `cross`: `cargo install cross --git https://github.com/cross-rs/cross`
- Add targets: `rustup target add armv7-unknown-linux-gnueabihf aarch64-unknown-linux-gnu`
- Docker required for cross-compilation

### Device-Specific Considerations
- reMarkable2 uses armv7 architecture; Paper Pro, Paper Pro Move and Paper Pure use aarch64
- The virtual keyboard needs the uinput kernel module; `setup_uinput` loads it if the firmware doesn't already
- Screen resolutions and input handling differ between devices; add or change a device in `src/device.rs`
- Virtual display size is normalized to 768x1024 pixels
- Screen capture reads the framebuffer out of xochitl's memory, and where it lives moves between firmware versions (the Paper Pro changed at 3.29); `screenshot.rs` documents each layout
- Paper Pro Move support was contributed (PR #19) and has not been tested by the maintainer

### Environment Variables
Set API keys for different LLM providers:
```bash
export OPENAI_API_KEY=your-key-here
export ANTHROPIC_API_KEY=your-key-here
export GOOGLE_API_KEY=your-key-here
```

### Prompt System
- Prompts are defined in `prompts/` directory as JSON files
- Tools (draw_text, draw_svg) are defined separately and referenced by prompts
- Runtime prompt overrides supported by copying files to device
- Default prompt is `general.json`

### Testing and Evaluation Framework
- `evaluations/` contains test scenarios with input images
- `run_eval.sh` runs systematic evaluations across models and configurations
- Results include merged images showing original input + AI output in red
- Supports segmentation analysis for improved spatial awareness

### reMarkable Integration
- Touch trigger in upper-right corner activates the assistant
- Progress indication through keyboard text and screen taps
- Supports both SVG drawing (via pen simulation) and text output (via virtual keyboard)
- Background execution supported with `nohup ./ghostwriter &`
