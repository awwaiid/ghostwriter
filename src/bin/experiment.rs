use anyhow::Result;
use clap::{Parser, Subcommand};
use evdev::{Device, EventType as EvdevEventType, InputEvent};
use ghostwriter::device::{VIRTUAL_HEIGHT, VIRTUAL_WIDTH};
use ghostwriter::pen::Pen;
use ghostwriter::screenshot::Screenshot;
use ghostwriter::touch::{PenTool, Touch, TriggerCorner};
use ghostwriter::util::{svg_to_alpha_bitmap, svg_to_bitmap, svg_to_bitmap_threshold};
use std::thread::sleep as std_sleep;
use std::time::Duration;
use tokio::time::sleep;

// Touch device for RMPP
const TOUCH_DEVICE: &str = "/dev/input/event3";

// RMPP touch input space
const TOUCH_SCREEN_WIDTH: f32 = 2065.0;
const TOUCH_SCREEN_HEIGHT: f32 = 2833.0;

// MT event codes
const ABS_MT_SLOT: u16 = 47;
const ABS_MT_TOUCH_MAJOR: u16 = 48;
const ABS_MT_TOUCH_MINOR: u16 = 49;
const ABS_MT_ORIENTATION: u16 = 52;
const ABS_MT_POSITION_X: u16 = 53;
const ABS_MT_POSITION_Y: u16 = 54;
const ABS_MT_TRACKING_ID: u16 = 57;
const ABS_MT_PRESSURE: u16 = 58;

fn virtual_to_touch(x: i32, y: i32) -> (i32, i32) {
    let tx = (x as f32 / VIRTUAL_WIDTH as f32 * TOUCH_SCREEN_WIDTH) as i32;
    let ty = (y as f32 / VIRTUAL_HEIGHT as f32 * TOUCH_SCREEN_HEIGHT) as i32;
    (tx, ty)
}

#[derive(Parser)]
#[command(name = "experiment")]
#[command(about = "Ghostwriter pen/touch experiment tool for reMarkable Paper Pro")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Capture the current screen to a PNG file
    Screenshot { output_path: String },
    /// Draw a line between two virtual coordinates (768x1024 space)
    DrawLine { x1: i32, y1: i32, x2: i32, y2: i32 },
    /// Draw a dot at a virtual coordinate with a small wiggle
    DrawDot { x: i32, y: i32 },
    /// Draw a rectangle (four lines) in virtual coordinates
    DrawRect { x1: i32, y1: i32, x2: i32, y2: i32 },
    /// Draw a triangle (three lines) in virtual coordinates
    DrawTriangle { x1: i32, y1: i32, x2: i32, y2: i32, x3: i32, y3: i32 },
    /// Draw a circle as a single continuous pen stroke (path tracing, not bitmap)
    DrawCircle { cx: i32, cy: i32, r: i32 },
    /// Render an SVG string via bitmap to pen strokes
    DrawSvg { svg_string: String },
    /// Render an SVG string via skeleton centerline (single-stroke, no fill needed)
    DrawSvgCenterline { svg_string: String },
    /// Draw text at given font size and y position using skeleton centerline rendering
    DrawText { text: String, font_size: u32, y: Option<u32> },
    /// Render an SVG string via path tracing (smooth strokes, handles text via glyph paths)
    DrawSvgPaths { svg_string: String },
    /// Render SVG via path tracing, bypassing usvg (better for geometric shapes, no text)
    DrawSvgPathsRaw { svg_string: String },
    /// Render a PNG file as dark pixels to pen strokes
    DrawPng { png_path: String },
    /// Single-finger tap at a virtual coordinate
    Tap { x: i32, y: i32 },
    /// Two-finger tap at a virtual coordinate (triggers undo in reMarkable)
    TwoFingerTap { x: i32, y: i32 },
    /// Touch swipe gesture between two virtual coordinates
    Swipe { x1: i32, y1: i32, x2: i32, y2: i32 },
    /// Navigate to a new page (swipe left, then tap new-page button)
    NewPage,
    /// Undo last stroke (two-finger tap at center)
    Undo,
    /// Wait N milliseconds
    SleepMs { ms: u64 },
    /// Switch to fineliner tool, detecting current palette state via pixel check
    SelectFineliner,
    /// Switch back to ballpoint tool
    SelectBallpoint,
    /// Read current tool state (for debugging pixel detection)
    ReadToolState,
    /// Render an SVG string via bidirectional scan (alternating L→R and R→L per row)
    DrawSvgBidi { svg_string: String },
    /// Render an SVG string via column-first scan (top→bottom per column)
    DrawSvgCol { svg_string: String },
    /// Render an SVG string using alpha-to-pressure mapping for anti-aliased rendering
    DrawSvgAlphaPressure { svg_string: String },
    /// Render an SVG string with configurable alpha threshold
    DrawSvgThreshold { svg_string: String, threshold: u8 },
    /// Render an SVG string at 3x scale for highest precision
    DrawSvgScale3x { svg_string: String },
    /// Render an SVG string with configurable threshold + bidirectional scan
    DrawSvgThresholdBidi { svg_string: String, threshold: u8 },
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::Screenshot { output_path } => {
            let mut screenshot = Screenshot::new()?;
            screenshot.take_screenshot()?;
            screenshot.save_image(&output_path)?;
            println!("Screenshot saved to {}", output_path);
        }

        Commands::DrawLine { x1, y1, x2, y2 } => {
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_line_screen((x1, y1), (x2, y2))
            })
            .await?;
            println!("Drew line from ({}, {}) to ({}, {})", x1, y1, x2, y2);
        }

        Commands::DrawDot { x, y } => {
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.pen_up()?;
                pen.goto_xy_virtual((x, y))?;
                pen.pen_down()?;
                for n in 0..3 {
                    pen.goto_xy_virtual((x + n, y + n))?;
                }
                pen.goto_xy_virtual((x, y))?;
                pen.pen_up()?;
                std_sleep(Duration::from_millis(5));
                Ok(())
            })
            .await?;
            println!("Drew dot at ({}, {})", x, y);
        }

        Commands::DrawRect { x1, y1, x2, y2 } => {
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_line_screen((x1, y1), (x2, y1))?; // top
                pen.draw_line_screen((x2, y1), (x2, y2))?; // right
                pen.draw_line_screen((x2, y2), (x1, y2))?; // bottom
                pen.draw_line_screen((x1, y2), (x1, y1)) // left
            })
            .await?;
            println!("Drew rect ({}, {}) to ({}, {})", x1, y1, x2, y2);
        }

        Commands::DrawTriangle { x1, y1, x2, y2, x3, y3 } => {
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_line_screen((x1, y1), (x2, y2))?;
                pen.draw_line_screen((x2, y2), (x3, y3))?;
                pen.draw_line_screen((x3, y3), (x1, y1))
            })
            .await?;
            println!("Drew triangle ({},{}) ({},{}) ({},{})", x1, y1, x2, y2, x3, y3);
        }

        Commands::DrawCircle { cx, cy, r } => {
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                let circumference = 2.0 * std::f32::consts::PI * r as f32;
                let steps = (circumference / 3.0).ceil() as usize;
                pen.pen_up()?;
                let x0 = cx + r;
                let y0 = cy;
                pen.pen_down_at(pen.virtual_to_input((x0, y0)))?;
                for i in 0..=steps {
                    let angle = 2.0 * std::f32::consts::PI * i as f32 / steps as f32;
                    let x = (cx as f32 + r as f32 * angle.cos()).round() as i32;
                    let y = (cy as f32 + r as f32 * angle.sin()).round() as i32;
                    pen.goto_xy_virtual((x, y))?;
                }
                pen.pen_up()
            })
            .await?;
            println!("Drew circle at ({}, {}) r={}", cx, cy, r);
        }

        Commands::DrawSvg { svg_string } => {
            // Render at 2x resolution for sub-pixel accuracy; draw_bitmap_scaled maps back
            let scale = 2u32;
            let bitmap = svg_to_bitmap(&svg_string, VIRTUAL_WIDTH * scale, VIRTUAL_HEIGHT * scale)?;
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_bitmap_scaled(&bitmap, scale)
            })
            .await?;
            println!("Drew SVG ({} chars)", svg_string.len());
        }

        Commands::DrawSvgCenterline { svg_string } => {
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_svg_centerline(&svg_string)
            })
            .await?;
            println!("Drew SVG centerline ({} chars)", svg_string.len());
        }

        Commands::DrawText { text, font_size, y } => {
            let y = y.unwrap_or_else(|| 200u32.max(font_size + 150));
            let svg_string = format!(
                r#"<svg width="768" height="1024" xmlns="http://www.w3.org/2000/svg"><text x="80" y="{}" font-family="sans-serif" font-size="{}" fill="black">{}</text></svg>"#,
                y, font_size, text
            );
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_svg_centerline(&svg_string)
            })
            .await?;
            println!("Drew text '{}' at font-size {} (y={})", text, font_size, y);
        }

        Commands::DrawSvgPaths { svg_string } => {
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_svg_paths(&svg_string)
            })
            .await?;
            println!("Drew SVG paths ({} chars)", svg_string.len());
        }

        Commands::DrawSvgPathsRaw { svg_string } => {
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_svg_paths_raw(&svg_string)
            })
            .await?;
            println!("Drew SVG paths raw ({} chars)", svg_string.len());
        }

        Commands::DrawPng { png_path } => {
            let img = image::open(&png_path)?.to_luma8();
            let bitmap: Vec<Vec<bool>> = img.rows().map(|row| row.map(|p| p[0] < 128).collect()).collect();
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_bitmap(&bitmap)
            })
            .await?;
            println!("Drew PNG from {}", png_path);
        }

        Commands::Tap { x, y } => {
            let mut touch = Touch::new(false, TriggerCorner::UpperRight)?;
            touch.touch_start((x, y)).await?;
            sleep(Duration::from_millis(100)).await;
            touch.touch_stop().await?;
            println!("Tapped at ({}, {})", x, y);
        }

        Commands::TwoFingerTap { x, y } => {
            two_finger_tap(x, y).await?;
            println!("Two-finger tapped at ({}, {})", x, y);
        }

        Commands::Swipe { x1, y1, x2, y2 } => {
            let mut touch = Touch::new(false, TriggerCorner::UpperRight)?;
            touch.touch_start((x1, y1)).await?;
            let steps = 20i32;
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let x = (x1 as f32 + (x2 - x1) as f32 * t) as i32;
                let y = (y1 as f32 + (y2 - y1) as f32 * t) as i32;
                touch.goto_xy((x, y)).await?;
                sleep(Duration::from_millis(10)).await;
            }
            touch.touch_stop().await?;
            println!("Swiped from ({}, {}) to ({}, {})", x1, y1, x2, y2);
        }

        Commands::NewPage => {
            let mut touch = Touch::new(false, TriggerCorner::UpperRight)?;
            // Swipe right-to-left to bring up the page navigation UI
            touch.touch_start((700, 512)).await?;
            let steps = 20i32;
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let x = (700.0 - 600.0 * t) as i32;
                touch.goto_xy((x, 512)).await?;
                sleep(Duration::from_millis(10)).await;
            }
            touch.touch_stop().await?;
            sleep(Duration::from_millis(300)).await;
            // Tap the new-page button (dark circle icon at right side ~x=700, y=514)
            touch.touch_start((700, 514)).await?;
            sleep(Duration::from_millis(100)).await;
            touch.touch_stop().await?;
            sleep(Duration::from_millis(300)).await;
            println!("New page command sent");
        }

        Commands::Undo => {
            two_finger_tap(384, 512).await?;
            println!("Undo sent");
        }

        Commands::SleepMs { ms } => {
            sleep(Duration::from_millis(ms)).await;
            println!("Slept {}ms", ms);
        }

        Commands::SelectFineliner => {
            let mut touch = Touch::new(false, TriggerCorner::UpperRight)?;
            let previous = touch.select_fineliner().await?;
            println!("SelectFineliner: switched to fineliner (was {:?})", previous);
        }

        Commands::SelectBallpoint => {
            let mut touch = Touch::new(false, TriggerCorner::UpperRight)?;
            let previous = touch.switch_to_tool(PenTool::Ballpoint).await?;
            println!("SelectBallpoint: switched to ballpoint (was {:?})", previous);
        }

        Commands::ReadToolState => {
            // Take a screenshot and report the detected tool state
            let mut ss = Screenshot::new()?;
            ss.take_screenshot()?;
            let palette_pixel = ss.get_pixel(70, 100);
            let sidebar_pixel = ss.get_pixel(2, 77);
            let palette_open = palette_pixel.map(|(r, _, _)| r < 128).unwrap_or(false);
            let sidebar_dark = sidebar_pixel.map(|(r, _, _)| r < 128).unwrap_or(false);
            let tool = if palette_open {
                "UNKNOWN (palette is open)"
            } else if sidebar_dark {
                "Fineliner"
            } else {
                "Ballpoint"
            };
            println!("Tool state: {} | palette_open={} | sidebar_dark={}", tool, palette_open, sidebar_dark);
            println!("  pixel(70,100)={:?}, pixel(2,77)={:?}", palette_pixel, sidebar_pixel);
        }

        Commands::DrawSvgBidi { svg_string } => {
            let scale = 2u32;
            let bitmap = svg_to_bitmap(&svg_string, VIRTUAL_WIDTH * scale, VIRTUAL_HEIGHT * scale)?;
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_bitmap_bidi(&bitmap, scale)
            })
            .await?;
            println!("Drew SVG bidi ({} chars)", svg_string.len());
        }

        Commands::DrawSvgCol { svg_string } => {
            let scale = 2u32;
            let bitmap = svg_to_bitmap(&svg_string, VIRTUAL_WIDTH * scale, VIRTUAL_HEIGHT * scale)?;
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_bitmap_col(&bitmap, scale)
            })
            .await?;
            println!("Drew SVG col ({} chars)", svg_string.len());
        }

        Commands::DrawSvgAlphaPressure { svg_string } => {
            let scale = 2u32;
            let alpha_bitmap = svg_to_alpha_bitmap(&svg_string, VIRTUAL_WIDTH * scale, VIRTUAL_HEIGHT * scale)?;
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_bitmap_alpha_pressure(&alpha_bitmap, scale)
            })
            .await?;
            println!("Drew SVG alpha-pressure ({} chars)", svg_string.len());
        }

        Commands::DrawSvgThreshold { svg_string, threshold } => {
            let scale = 2u32;
            let bitmap = svg_to_bitmap_threshold(&svg_string, VIRTUAL_WIDTH * scale, VIRTUAL_HEIGHT * scale, threshold)?;
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_bitmap_scaled(&bitmap, scale)
            })
            .await?;
            println!("Drew SVG threshold={} ({} chars)", threshold, svg_string.len());
        }

        Commands::DrawSvgScale3x { svg_string } => {
            let scale = 3u32;
            let bitmap = svg_to_bitmap(&svg_string, VIRTUAL_WIDTH * scale, VIRTUAL_HEIGHT * scale)?;
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_bitmap_scaled(&bitmap, scale)
            })
            .await?;
            println!("Drew SVG scale3x ({} chars)", svg_string.len());
        }

        Commands::DrawSvgThresholdBidi { svg_string, threshold } => {
            let scale = 2u32;
            let bitmap = svg_to_bitmap_threshold(&svg_string, VIRTUAL_WIDTH * scale, VIRTUAL_HEIGHT * scale, threshold)?;
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                pen.draw_bitmap_bidi(&bitmap, scale)
            })
            .await?;
            println!("Drew SVG threshold-bidi threshold={} ({} chars)", threshold, svg_string.len());
        }
    }

    Ok(())
}

/// Switch to fineliner (with correct size/color settings), run drawing closure, then restore.
async fn with_fineliner<F: FnOnce() -> Result<()>>(f: F) -> Result<()> {
    let mut touch = Touch::new(false, TriggerCorner::UpperRight)?;
    let previous = touch.select_fineliner().await?;
    sleep(Duration::from_millis(500)).await; // Wait for palette close animation to finish
    let result = f();
    touch.restore_tool(previous).await?;
    result
}

async fn two_finger_tap(x: i32, y: i32) -> Result<()> {
    let mut device = Device::open(TOUCH_DEVICE)?;
    let (tx, ty) = virtual_to_touch(x, y);
    let (tx2, ty2) = virtual_to_touch(x + 50, y + 50);

    // Press two fingers simultaneously
    device.send_events(&[
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_SLOT, 0),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TRACKING_ID, 1),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_X, tx),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_Y, ty),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_PRESSURE, 100),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TOUCH_MAJOR, 17),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TOUCH_MINOR, 17),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_ORIENTATION, 4),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_SLOT, 1),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TRACKING_ID, 2),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_X, tx2),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_POSITION_Y, ty2),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_PRESSURE, 100),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TOUCH_MAJOR, 17),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TOUCH_MINOR, 17),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_ORIENTATION, 4),
        InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0),
    ])?;

    sleep(Duration::from_millis(100)).await;

    // Release both fingers
    device.send_events(&[
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_SLOT, 0),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TRACKING_ID, -1),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_SLOT, 1),
        InputEvent::new(EvdevEventType::ABSOLUTE.0, ABS_MT_TRACKING_ID, -1),
        InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0),
    ])?;

    Ok(())
}
