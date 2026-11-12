use anyhow::Result;
use clap::{Parser, Subcommand};
use evdev::{Device, EventType as EvdevEventType, InputEvent};
use ghostwriter::device::{DeviceModel, FullSpeed, VIRTUAL_HEIGHT, VIRTUAL_WIDTH};
use ghostwriter::drawing_lab;
use ghostwriter::pen::{Drawing, DrawingPen, Pen, StrokeTiming};
use ghostwriter::screenshot::Screenshot;
use ghostwriter::stroke_plan::{plan_strokes, simulate, PenModel};
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
    /// Replay a stroke file on the device and report how long it took.
    /// Each line is one stroke: its pressure, then its x,y points in virtual coordinates
    Play {
        strokes_path: String,
        /// Microseconds to wait after the pen touches down
        #[arg(long, default_value_t = 1000)]
        after_down_us: u64,
        /// Microseconds to wait after the pen lifts
        #[arg(long, default_value_t = 1000)]
        after_up_us: u64,
        /// Microseconds to wait after every --points-per-pause points
        #[arg(long, default_value_t = 1000)]
        pause_us: u64,
        #[arg(long, default_value_t = 100)]
        points_per_pause: usize,
        /// Select the fineliner first, and restore the previous tool afterwards
        #[arg(long)]
        select_tool: bool,
    },
    /// Capture the screen at the device's full resolution
    ScreenshotFull { output_path: String },
    /// Print the axis ranges the pen and touch devices report
    AbsInfo,
    /// Plan the strokes for an SVG file, write them as a stroke file, and report how
    /// closely they should reproduce the SVG (runs anywhere)
    Plan {
        svg_path: String,
        strokes_path: String,
        /// The device's page size in pixels
        #[arg(long, default_value = "1632x2154")]
        page: String,
        /// Plan one small mark per patch of ink instead of merging them into bars
        #[arg(long)]
        stipple: bool,
        /// The pen to plan for: fineliner (thin) or ballpoint (medium)
        #[arg(long, default_value = "fineliner")]
        pen: String,
        /// Plan it this many more times first, pausing before each, and report how long each took
        #[arg(long, default_value_t = 0)]
        repeat: usize,
        /// How long to pause before each repeat, and what to do meanwhile: wait or spin
        #[arg(long, default_value_t = 3000)]
        pause_ms: u64,
        #[arg(long)]
        spin: bool,
    },
    /// Draw an SVG file the way ghostwriter does: plan its strokes and draw them, watching the screen for any that do not arrive
    DrawPlanned {
        svg_path: String,
        /// The pen to plan for: fineliner (thin) or ballpoint (medium)
        #[arg(long, default_value = "fineliner")]
        pen: String,
        /// Draw with whatever pen is selected, instead of selecting the fineliner first
        #[arg(long)]
        no_select: bool,
    },
    /// Draw the first strokes planned for an SVG file at a steady pace, and report how
    /// many of their watched pixels show on the screen as time passes
    WatchCheck {
        svg_path: String,
        #[arg(long, default_value_t = 48)]
        strokes: usize,
        /// Select the fineliner first, as ghostwriter does
        #[arg(long)]
        select_tool: bool,
    },
    /// Draw bars of each length from 1 to 12 pixels, across and down, and print the grey
    /// levels the screen shows around each, to see exactly what a short stroke inks
    BarLengths {
        /// Where to start down the page, in device pixels
        #[arg(long, default_value_t = 300)]
        top: u32,
        /// A fraction of a pixel to shift every bar by, in tenths
        #[arg(long, default_value_t = 0)]
        shift_tenths: u32,
        /// Add a third point this far beyond the end of each bar, in tenths of a pixel
        #[arg(long, default_value_t = 0)]
        tail_tenths: u32,
        /// Shift every bar along its own length by this instead of by --shift-tenths
        #[arg(long)]
        along_tenths: Option<u32>,
        /// Put the third point at the end of the bar and the second one short of it,
        /// instead of the second at the end and the third beyond
        #[arg(long)]
        tail_inside: bool,
    },
    /// Draw a bar at each of several pen pressures with whatever pen is selected, and
    /// print the grey levels across each bar and along its ends
    PenProfile {
        /// Pen pressures to try, separated by commas
        #[arg(long, default_value = "200,600,1000,1500,2000,2630,3200,4000")]
        pressures: String,
        /// How long each bar is, in device pixels
        #[arg(long, default_value_t = 60)]
        length: u32,
        /// Where to start down the page, in device pixels
        #[arg(long, default_value_t = 300)]
        top: u32,
        /// A fraction of a pixel to shift every bar down by, in tenths
        #[arg(long, default_value_t = 0)]
        shift_tenths: u32,
        /// Draw the bars down the page instead of across it
        #[arg(long)]
        down: bool,
        /// Milliseconds to hold the pen at each end of a bar
        #[arg(long, default_value_t = 0)]
        dwell_ms: u64,
        /// Extra points along the bar, as distances from its start in device pixels, separated by commas
        #[arg(long, default_value = "")]
        via: String,
    },
    /// Draw a short stroke, wait, tap the palette's button once, and report whether and
    /// when the palette opened: how soon after the pen the tablet takes a touch
    TapAfterPen {
        #[arg(long, default_value_t = 0)]
        delay_ms: u64,
        /// How many strokes to draw first
        #[arg(long, default_value_t = 1)]
        strokes: usize,
    },
    /// Render where an SVG file puts ink, at the device's page size, to compare a capture with.
    /// Run on the device this uses the device's own fonts
    Render { svg_path: String, output_path: String },
    /// Score a full-resolution capture against what was drawn: an SVG file, or an image from `render` (runs anywhere)
    Score {
        reference_path: String,
        capture_path: String,
        /// How far ink may be from where it belongs, in capture pixels
        #[arg(long, default_value_t = 2)]
        tolerance: u32,
        /// Write a picture of the differences: red is stray ink, blue is missing ink
        #[arg(long)]
        diff: Option<String>,
    },
    /// List the dark runs along one column or row of an image (runs anywhere)
    Profile {
        image_path: String,
        #[arg(long)]
        col: Option<u32>,
        #[arg(long)]
        row: Option<u32>,
    },
    /// Draw a grid of short bars in bursts, waiting after each burst for its last bar to
    /// show on the screen, and report how long the waits were and which bars never arrived
    StrokeFlow {
        /// Strokes written back to back before waiting
        #[arg(long, default_value_t = 16)]
        batch: usize,
        #[arg(long, default_value_t = 20)]
        batches: usize,
        /// Which events make up a stroke: full, hover, hover-distance, merged-down, merged-lift or lean
        #[arg(long, default_value = "full")]
        style: String,
        /// Microseconds between strokes within a burst
        #[arg(long, default_value_t = 0)]
        gap_us: u64,
        /// Instead of waiting for the last bar to show, wait this many microseconds after each burst
        #[arg(long)]
        blind_wait_us: Option<u64>,
        /// Where the grid starts down the page, in device pixels
        #[arg(long, default_value_t = 300)]
        top: u32,
        /// Draw the bars in a scattered order instead of along each row in turn
        #[arg(long)]
        scatter: bool,
    },
    /// Draw short bars while a second reader of the pen device does not read, then report
    /// what that reader was sent: events per stroke, and whether the kernel dropped any
    PenEvents {
        #[arg(long, default_value_t = 4)]
        strokes: usize,
        #[arg(long, default_value = "full")]
        style: String,
    },
    /// Save an enlarged crop of an image (runs anywhere)
    Crop {
        image_path: String,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        scale: u32,
        output_path: String,
    },
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
            let mut elapsed = Duration::ZERO;
            with_fineliner(|| {
                let mut pen = Pen::new(false)?;
                let started = std::time::Instant::now();
                pen.draw_bitmap_alpha_pressure(&alpha_bitmap, scale)?;
                elapsed = started.elapsed();
                Ok(())
            })
            .await?;
            println!("Drew SVG alpha-pressure ({} chars) elapsed_ms={}", svg_string.len(), elapsed.as_millis());
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

        Commands::Play {
            strokes_path,
            after_down_us,
            after_up_us,
            pause_us,
            points_per_pause,
            select_tool,
        } => {
            let strokes = drawing_lab::read_strokes(&std::fs::read_to_string(&strokes_path)?)?;
            let timing = StrokeTiming {
                after_down: Duration::from_micros(after_down_us),
                after_up: Duration::from_micros(after_up_us),
                pause: Duration::from_micros(pause_us),
                points_per_pause,
            };
            let mut elapsed = Duration::ZERO;
            let mut draw = || {
                let mut pen = Pen::new(false)?;
                let started = std::time::Instant::now();
                pen.draw_strokes(&strokes, &timing)?;
                elapsed = started.elapsed();
                Ok(())
            };
            if select_tool {
                with_fineliner(draw).await?;
            } else {
                draw()?;
            }
            let points: usize = strokes.iter().map(|stroke| stroke.points.len()).sum();
            println!("strokes={} points={} elapsed_ms={}", strokes.len(), points, elapsed.as_millis());
        }

        Commands::ScreenshotFull { output_path } => {
            let image = Screenshot::new()?.capture_full_resolution()?;
            image.to_luma8().save(&output_path)?;
            println!("Full-resolution screenshot saved to {}", output_path);
        }

        Commands::AbsInfo => {
            let device_model = DeviceModel::detect();
            for (name, path) in [("pen", device_model.pen_device_path()), ("touch", device_model.touch_device_path())] {
                println!("{} ({})", name, path);
                for (axis, info) in Device::open(path)?.get_absinfo()? {
                    println!("  {:?}: {}..{} resolution {}", axis, info.minimum(), info.maximum(), info.resolution());
                }
            }
        }

        Commands::Plan {
            svg_path,
            strokes_path,
            page,
            stipple,
            pen,
            repeat,
            pause_ms,
            spin,
        } => {
            let pen = DrawingPen::from_name(&pen)?.model();
            let (width, height) = page.split_once('x').ok_or_else(|| anyhow::anyhow!("--page looks like 1632x2154"))?;
            let page_size = (width.parse()?, height.parse()?);
            let svg = std::fs::read_to_string(&svg_path)?;

            for _ in 0..repeat {
                let pause = Duration::from_millis(pause_ms);
                if spin {
                    // Wait, but keep a processor busy for the last tenth of a second
                    std_sleep(pause.saturating_sub(Duration::from_millis(100)));
                    let spinning = std::time::Instant::now();
                    while spinning.elapsed() < Duration::from_millis(100).min(pause) {
                        std::hint::spin_loop();
                    }
                } else {
                    std_sleep(pause);
                }
                let started = std::time::Instant::now();
                let strokes = plan_strokes(&svg, page_size, &pen)?;
                println!("planned {} strokes in {} ms", strokes.len(), started.elapsed().as_millis());
            }

            let started = std::time::Instant::now();
            let strokes = if stipple {
                drawing_lab::plan_stipple(&svg, page_size)?
            } else {
                plan_strokes(&svg, page_size, &pen)?
            };
            let planning = started.elapsed();
            std::fs::write(&strokes_path, drawing_lab::write_strokes(&strokes))?;

            // How the plan compares with the SVG, by the pen model
            let drawn = simulate(&strokes, page_size, &pen);
            let wanted = svg_to_alpha_bitmap(&svg, page_size.0, page_size.1)?;
            let (mut ink, mut missing, mut stray) = (0, 0, 0);
            for (drawn_row, wanted_row) in drawn.iter().zip(&wanted) {
                for (&is_drawn, &alpha) in drawn_row.iter().zip(wanted_row) {
                    let is_wanted = alpha >= 128;
                    ink += is_wanted as usize;
                    missing += (is_wanted && !is_drawn) as usize;
                    stray += (is_drawn && !is_wanted) as usize;
                }
            }
            let points: usize = strokes.iter().map(|stroke| stroke.points.len()).sum();
            println!(
                "strokes={} points={} planning_ms={} ink_px={} missing_px={} stray_px={}",
                strokes.len(),
                points,
                planning.as_millis(),
                ink,
                missing,
                stray
            );
        }

        Commands::DrawPlanned { svg_path, pen, no_select } => {
            let _full_speed = FullSpeed::hold();
            let svg = std::fs::read_to_string(&svg_path)?;
            let pen = DrawingPen::from_name(&pen)?;
            let started = std::time::Instant::now();
            if no_select {
                let drawing = Drawing::plan(&svg, DeviceModel::detect(), pen.model())?;
                let as_planned = Pen::new(false)?.draw(&drawing, true)?;
                println!("elapsed_ms={} pen_as_planned={}", started.elapsed().as_millis(), as_planned);
            } else {
                let mut touch = Touch::new(false, TriggerCorner::UpperRight)?;
                let restore = touch.select_pen(pen.setup()).await?;
                let selected = started.elapsed();
                Pen::new(false)?.draw_svg(&svg, pen.model())?;
                let drawn = started.elapsed();
                touch.restore_pen(pen.setup(), restore).await?;
                println!(
                    "elapsed_ms={} select_ms={} restore_ms={}",
                    (drawn - selected).as_millis(),
                    selected.as_millis(),
                    (started.elapsed() - drawn).as_millis()
                );
            }
        }

        Commands::WatchCheck {
            svg_path,
            strokes: count,
            select_tool,
        } => {
            let page_size = DeviceModel::detect().page_size();
            let mut strokes = plan_strokes(&std::fs::read_to_string(&svg_path)?, page_size, &PenModel::FINELINER_THIN)?;
            strokes.truncate(count);
            let watched = ghostwriter::stroke_plan::watch_pixels(&strokes, page_size, &PenModel::FINELINER_THIN);
            for (stroke, pixel) in strokes.iter().zip(&watched).take(5) {
                let to_pixels = |(x, y): (f32, f32)| (x * page_size.0 as f32 / VIRTUAL_WIDTH as f32, y * page_size.1 as f32 / VIRTUAL_HEIGHT as f32);
                println!(
                    "stroke {:?} -> {:?} watching {:?}",
                    to_pixels(stroke.points[0]),
                    to_pixels(stroke.points[1]),
                    pixel
                );
            }

            let mut touch = Touch::new(false, TriggerCorner::UpperRight)?;
            if select_tool {
                touch.select_fineliner().await?;
            }
            let screen = Screenshot::new()?.probe()?;
            let showing = || watched.iter().flatten().filter(|&&(x, y)| screen.is_dark(x, y)).count();
            println!("before drawing: {} of {} watched pixels dark", showing(), watched.iter().flatten().count());

            Pen::new(false)?.draw_strokes(&strokes, &StrokeTiming::default())?;
            let drawn_at = std::time::Instant::now();
            for wait_ms in [0, 5, 10, 20, 50, 100, 200, 400, 800, 1500] {
                while drawn_at.elapsed() < Duration::from_millis(wait_ms) {
                    std_sleep(Duration::from_micros(200));
                }
                println!("{}ms after drawing: {} showing", drawn_at.elapsed().as_millis(), showing());
            }
            // Where the ink went, around the first stroke's watched pixel
            if let Some((x, y)) = watched.iter().flatten().next().copied() {
                for row in y.saturating_sub(4)..y + 5 {
                    let cells: String = (x.saturating_sub(12)..x + 13)
                        .map(|column| if screen.is_dark(column, row) { '#' } else { '.' })
                        .collect();
                    println!("{:5} {}", row, cells);
                }
            }
        }

        Commands::BarLengths {
            top,
            shift_tenths,
            tail_tenths,
            along_tenths,
            tail_inside,
        } => {
            let tail = tail_tenths as f32 / 10.0;
            let along = along_tenths.unwrap_or(shift_tenths) as f32 / 10.0;
            let device_model = DeviceModel::detect();
            let (page_width, page_height) = device_model.page_size();
            let shift = shift_tenths as f32 / 10.0;
            let to_virtual = |x: f32, y: f32| (x * VIRTUAL_WIDTH as f32 / page_width as f32, y * VIRTUAL_HEIGHT as f32 / page_height as f32);
            // Across: bar `length` starts at x = 200 on its own line. Down: at y = top + 300.
            let across = |length: u32| ((200, top + length * 12), (200 + length, top + length * 12));
            let down = |length: u32| ((400 + length * 12, top), (400 + length * 12, top + length));
            let mut strokes = Vec::new();
            for length in 1..=12 {
                for ((x1, y1), (x2, y2)) in [across(length), down(length)] {
                    // Shifted by `along` in the direction the bar runs, and by `shift` across it
                    let (shift_x, shift_y) = if y1 == y2 { (along, shift) } else { (shift, along) };
                    let (x1, y1, x2, y2) = (x1 as f32 + shift_x, y1 as f32 + shift_y, x2 as f32 + shift_x, y2 as f32 + shift_y);
                    let mut points = vec![to_virtual(x1, y1), to_virtual(x2, y2)];
                    if tail > 0.0 {
                        let (dx, dy) = if y1 == y2 { (tail, 0.0) } else { (0.0, tail) };
                        if tail_inside {
                            points.insert(1, to_virtual(x2 - dx, y2 - dy));
                        } else {
                            points.push(to_virtual(x2 + dx, y2 + dy));
                        }
                    }
                    strokes.push(ghostwriter::pen::Stroke {
                        points,
                        pressure: ghostwriter::pen::FULL_PRESSURE,
                    });
                }
            }
            Pen::new(false)?.draw_strokes(
                &strokes,
                &StrokeTiming {
                    after_up: Duration::from_millis(2),
                    ..StrokeTiming::default()
                },
            )?;
            std_sleep(Duration::from_millis(800));

            let capture = Screenshot::new()?.capture_full_resolution()?.to_luma8();
            let level = |x: u32, y: u32| match capture.get_pixel(x, y)[0] {
                0..=63 => '#',
                64..=127 => 'x',
                128..=191 => '-',
                192..=239 => '.',
                _ => ' ',
            };
            println!("across: bar from x=200 to 200+length, on the boundary above the marked row; columns 196..216");
            for length in 1..=12 {
                let ((_, y), _) = across(length);
                for row in y - 2..y + 2 {
                    let cells: String = (196..216).map(|x| level(x, row)).collect();
                    println!("len {:2} row {:+} |{}|", length, row as i32 - y as i32, cells);
                }
            }
            println!("down: bar from y=top to top+length, on the boundary left of column +0; rows top-4..top+16");
            for row in top - 4..top + 16 {
                let cells: String = (1..=12)
                    .map(|length| {
                        let ((x, _), _) = down(length);
                        let cells: String = (x - 2..x + 2).map(|x| level(x, row)).collect();
                        format!("|{}| ", cells)
                    })
                    .collect();
                println!("row {:+3} {}", row as i32 - top as i32, cells);
            }
        }

        Commands::PenProfile {
            pressures,
            length,
            top,
            shift_tenths,
            down,
            dwell_ms,
            via,
        } => {
            let device_model = DeviceModel::detect();
            let (page_width, page_height) = device_model.page_size();
            let shift = shift_tenths as f32 / 10.0;
            let via: Vec<f32> = via
                .split(',')
                .filter(|distance| !distance.trim().is_empty())
                .map(|distance| distance.trim().parse())
                .collect::<Result<_, _>>()?;
            let pressures: Vec<i32> = pressures.split(',').map(|pressure| pressure.trim().parse()).collect::<Result<_, _>>()?;
            // Bar number `index` runs from (along, across) for `length`, where "along" is x for bars across the page
            let start = |index: usize| (300u32, top + index as u32 * 30);
            let to_virtual = |along: f32, across: f32| {
                let (x, y) = if down { (across, along) } else { (along, across) };
                (x * VIRTUAL_WIDTH as f32 / page_width as f32, y * VIRTUAL_HEIGHT as f32 / page_height as f32)
            };
            let mut pen = Pen::new(false)?;
            for (index, &pressure) in pressures.iter().enumerate() {
                let (along, across) = start(index);
                let mut points = vec![to_virtual(along as f32, across as f32 + shift)];
                points.extend(via.iter().map(|distance| to_virtual(along as f32 + distance, across as f32 + shift)));
                points.push(to_virtual((along + length) as f32, across as f32 + shift));
                let stroke = ghostwriter::pen::Stroke { points, pressure };
                pen.draw_strokes(
                    &[stroke],
                    &StrokeTiming {
                        after_down: Duration::from_millis(dwell_ms),
                        after_up: Duration::from_millis(3),
                        ..StrokeTiming::default()
                    },
                )?;
            }
            std_sleep(Duration::from_millis(900));

            let capture = Screenshot::new()?.capture_full_resolution()?.to_luma8();
            let level = |along: u32, across: u32| {
                let (x, y) = if down { (across, along) } else { (along, across) };
                capture.get_pixel(x, y)[0]
            };
            for (index, &pressure) in pressures.iter().enumerate() {
                let (along, across) = start(index);
                println!(
                    "pressure {pressure}: bar from {along} to {} along, centred on {}{}",
                    along + length,
                    across,
                    if shift > 0.0 { format!("+{shift}") } else { String::new() }
                );
                for (name, at) in [
                    ("quarter", along + length / 4),
                    ("middle", along + length / 2),
                    ("three-quarter", along + length * 3 / 4),
                ] {
                    let levels: Vec<String> = (across - 6..across + 6).map(|across| format!("{:3}", level(at, across))).collect();
                    println!("  across at {name:13} (rows -6..+5): {}", levels.join(" "));
                }
                for (name, across) in [("row -1", across - 1), ("row +0", across)] {
                    let before: Vec<String> = (along - 5..along + 4).map(|along| format!("{:3}", level(along, across))).collect();
                    let after: Vec<String> = (along + length - 4..along + length + 5)
                        .map(|along| format!("{:3}", level(along, across)))
                        .collect();
                    println!("  along {name}: start-5..+3: {}   end-4..+4: {}", before.join(" "), after.join(" "));
                }
            }
        }

        Commands::TapAfterPen { delay_ms, strokes } => {
            let mut touch = Touch::new(false, TriggerCorner::UpperRight)?;
            anyhow::ensure!(!Touch::palette_is_open()?, "Close the palette first");
            let short_strokes: Vec<ghostwriter::pen::Stroke> = (0..strokes)
                .map(|index| ghostwriter::pen::Stroke {
                    points: vec![(300.0 + (index % 40) as f32 * 4.0, 900.0), (302.0 + (index % 40) as f32 * 4.0, 900.0)],
                    pressure: ghostwriter::pen::FULL_PRESSURE,
                })
                .collect();
            Pen::new(false)?.draw_strokes(&short_strokes, &StrokeTiming::default())?;
            sleep(Duration::from_millis(delay_ms)).await;

            touch.tap_palette_button().await?;
            let tapped_at = std::time::Instant::now();
            let mut opened = None;
            while tapped_at.elapsed() < Duration::from_millis(700) && opened.is_none() {
                if Touch::palette_is_open()? {
                    opened = Some(tapped_at.elapsed());
                }
                sleep(Duration::from_millis(2)).await;
            }
            println!(
                "tap {} ms after {} strokes: opened={:?}",
                delay_ms,
                strokes,
                opened.map(|after| after.as_millis())
            );

            // Leave the palette closed
            sleep(Duration::from_millis(600)).await;
            for _ in 0..3 {
                if !Touch::palette_is_open()? {
                    break;
                }
                touch.tap_palette_button().await?;
                sleep(Duration::from_millis(700)).await;
            }
        }

        Commands::Render { svg_path, output_path } => {
            let (width, height) = DeviceModel::detect().page_size();
            drawing_lab::render_ink(&std::fs::read_to_string(&svg_path)?, width, height)?.save(&output_path)?;
            println!("Rendered {} at {}x{}", output_path, width, height);
        }

        Commands::Score {
            reference_path,
            capture_path,
            tolerance,
            diff,
        } => {
            let capture = image::open(&capture_path)?.to_luma8();
            let wanted = if reference_path.ends_with(".svg") {
                drawing_lab::wanted_ink(&std::fs::read_to_string(&reference_path)?, capture.width(), capture.height())?
            } else {
                drawing_lab::drawn_ink(&image::open(&reference_path)?.to_luma8())
            };
            let score = drawing_lab::score(&wanted, &capture, tolerance);
            println!(
                "f1={:.3} precision={:.3} recall={:.3} overlap={:.3} ink_ratio={:.2} shift=({},{}) overlap_at_shift={:.3}",
                score.f1(),
                score.precision,
                score.recall,
                score.overlap,
                score.ink_ratio,
                score.best_shift.0,
                score.best_shift.1,
                score.best_shift_overlap
            );
            if let Some(diff_path) = diff {
                drawing_lab::difference_image(&wanted, &capture).save(&diff_path)?;
            }
        }

        Commands::Profile { image_path, col, row } => {
            let image = image::open(&image_path)?.to_luma8();
            let runs = match (col, row) {
                (Some(col), None) => drawing_lab::dark_runs((0..image.height()).map(|y| image.get_pixel(col, y)[0])),
                (None, Some(row)) => drawing_lab::dark_runs((0..image.width()).map(|x| image.get_pixel(x, row)[0])),
                _ => anyhow::bail!("Give exactly one of --col or --row"),
            };
            for (start, length) in runs {
                println!("{} +{}", start, length);
            }
        }

        Commands::Crop {
            image_path,
            x,
            y,
            width,
            height,
            scale,
            output_path,
        } => {
            let crop = image::open(&image_path)?.crop_imm(x, y, width, height);
            crop.resize_exact(width * scale, height * scale, image::imageops::FilterType::Nearest)
                .save(&output_path)?;
            println!("Saved {}", output_path);
        }

        Commands::StrokeFlow {
            batch,
            batches,
            style,
            gap_us,
            blind_wait_us,
            top,
            scatter,
        } => {
            let device_model = DeviceModel::detect();
            let probe = Screenshot::new()?.probe()?;
            let mut pen = Device::open(device_model.pen_device_path())?;

            // Bars 10 pixels long, each centred on the boundary between two pixel rows
            const COLUMNS: usize = 87;
            let total = batch * batches;
            let bar = |index: usize| {
                // A step that shares no factor with the total visits every bar once
                let index = if scatter {
                    let step = (total / 2..total).find(|&step| gcd(step, total) == 1).unwrap_or(1);
                    index * step % total
                } else {
                    index
                };
                let x = 100 + (index % COLUMNS) as u32 * 16;
                let y = top + (index / COLUMNS) as u32 * 6;
                ((x, y), (x + 10, y))
            };
            let shows = |index: usize| {
                let ((x, y), _) = bar(index);
                probe.is_dark(x + 5, y - 1) && probe.is_dark(x + 5, y)
            };
            // Inked from end to end, not just in the middle
            let complete = |index: usize| {
                let ((x, y), _) = bar(index);
                probe.is_dark(x + 1, y) && probe.is_dark(x + 9, y)
            };
            let stays_in_range = stroke_events(&style, (0, 0), (0, 0))?.1;

            let mut waits = Vec::new();
            let mut timeouts = 0;
            if stays_in_range {
                pen.send_events(&pen_in_range(true))?;
            }
            let started = std::time::Instant::now();
            for batch_index in 0..batches {
                for index in batch_index * batch..(batch_index + 1) * batch {
                    let (from, to) = bar(index);
                    let (from, to) = (pixel_to_pen(&device_model, from), pixel_to_pen(&device_model, to));
                    for packet in stroke_events(&style, from, to)?.0 {
                        pen.send_events(&packet)?;
                    }
                    if gap_us > 0 {
                        std_sleep(Duration::from_micros(gap_us));
                    }
                }
                match blind_wait_us {
                    Some(wait) => std_sleep(Duration::from_micros(wait)),
                    None => {
                        let waiting = std::time::Instant::now();
                        let last = (batch_index + 1) * batch - 1;
                        while !shows(last) && waiting.elapsed() < Duration::from_millis(300) {
                            std_sleep(Duration::from_micros(100));
                        }
                        if shows(last) {
                            waits.push(waiting.elapsed().as_micros() as u64);
                        } else {
                            timeouts += 1;
                        }
                    }
                }
            }
            let elapsed = started.elapsed();
            if stays_in_range {
                pen.send_events(&pen_in_range(false))?;
            }

            let missing_at = |delay_ms: u64| {
                std_sleep(Duration::from_millis(delay_ms));
                (0..total).filter(|&index| !shows(index)).collect::<Vec<_>>()
            };
            let missing_soon = missing_at(0).len();
            let missing = missing_at(600);
            let short = (0..total).filter(|&index| shows(index) && !complete(index)).count();
            waits.sort_unstable();
            let percentile = |p: usize| waits.get((waits.len() * p / 100).min(waits.len().saturating_sub(1))).copied().unwrap_or(0);
            println!(
                "style={} strokes={} elapsed_ms={} per_second={:.0} wait_us(min/median/p90/max)={}/{}/{}/{} timeouts={} missing_at_once={} missing={} short={}",
                style,
                total,
                elapsed.as_millis(),
                total as f64 / elapsed.as_secs_f64(),
                percentile(0),
                percentile(50),
                percentile(90),
                percentile(100),
                timeouts,
                missing_soon,
                missing.len(),
                short
            );
            // Runs of consecutive missing bars
            let mut runs: Vec<(usize, usize)> = Vec::new();
            for index in missing {
                match runs.last_mut() {
                    Some((start, length)) if *start + *length == index => *length += 1,
                    _ => runs.push((index, 1)),
                }
            }
            if !runs.is_empty() {
                println!("missing runs (start+length): {:?}", runs);
            }
        }

        Commands::PenEvents { strokes, style } => {
            use std::io::Read;
            use std::os::unix::fs::OpenOptionsExt;
            const O_NONBLOCK: i32 = 0o4000;
            const EVENT_SIZE: usize = 2 * std::mem::size_of::<usize>() + 8;

            let device_model = DeviceModel::detect();
            let mut reader = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(O_NONBLOCK)
                .open(device_model.pen_device_path())?;
            let mut pen = Device::open(device_model.pen_device_path())?;

            // Short bars across the top of the page
            let mut written = 0;
            let stays_in_range = stroke_events(&style, (0, 0), (0, 0))?.1;
            if stays_in_range {
                pen.send_events(&pen_in_range(true))?;
            }
            for index in 0..strokes {
                let from = (100 + (index % 87) as u32 * 16, 200 + (index / 87) as u32 * 6);
                let (from, to) = (pixel_to_pen(&device_model, from), pixel_to_pen(&device_model, (from.0 + 10, from.1)));
                for packet in stroke_events(&style, from, to)?.0 {
                    written += packet.len();
                    pen.send_events(&packet)?;
                }
            }
            if stays_in_range {
                pen.send_events(&pen_in_range(false))?;
            }

            let mut bytes = Vec::new();
            let mut chunk = [0u8; 24 * 64];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => bytes.extend_from_slice(&chunk[..count]),
                    Err(_) => break,
                }
            }
            let events: Vec<(u16, u16)> = bytes
                .chunks_exact(EVENT_SIZE)
                .map(|event| {
                    let fields = &event[EVENT_SIZE - 8..];
                    (u16::from_ne_bytes([fields[0], fields[1]]), u16::from_ne_bytes([fields[2], fields[3]]))
                })
                .collect();
            let dropped = events.iter().filter(|&&(kind, code)| kind == 0 && code == 3).count();
            let reports = events.iter().filter(|&&(kind, code)| kind == 0 && code == 0).count();
            println!(
                "strokes={} events_written={} events_read={} per_stroke={:.1} reports={} syn_dropped={}",
                strokes,
                written,
                events.len(),
                events.len() as f64 / strokes as f64,
                reports,
                dropped
            );
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

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Where a page pixel is on the pen digitizer.
fn pixel_to_pen(device_model: &DeviceModel, (x, y): (u32, u32)) -> (i32, i32) {
    let (pen_width, pen_height) = device_model.pen_size();
    let (area_width, area_height) = device_model.pen_area_size();
    (
        (x as f32 / area_width as f32 * pen_width as f32).round() as i32,
        (y as f32 / area_height as f32 * pen_height as f32).round() as i32,
    )
}

fn absolute(code: u16, value: i32) -> InputEvent {
    InputEvent::new(EvdevEventType::ABSOLUTE.0, code, value)
}

fn key(code: u16, value: i32) -> InputEvent {
    InputEvent::new(EvdevEventType::KEY.0, code, value)
}

fn report() -> InputEvent {
    InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0)
}

const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_PRESSURE: u16 = 24;
const ABS_DISTANCE: u16 = 25;
const BTN_TOOL_PEN: u16 = 320;
const BTN_TOUCH: u16 = 330;

/// The packets of events for a two-point stroke in one of several styles, and whether
/// the style needs the pen to be in range already (`pen_in_range`).
fn stroke_events(style: &str, from: (i32, i32), to: (i32, i32)) -> Result<(Vec<Vec<InputEvent>>, bool)> {
    let position = |(x, y): (i32, i32)| [absolute(ABS_X, x), absolute(ABS_Y, y)];
    let packet = |events: &[InputEvent]| events.iter().copied().chain([report()]).collect::<Vec<_>>();
    let [from_x, from_y] = position(from);
    let [to_x, to_y] = position(to);
    let pressed = absolute(ABS_PRESSURE, 2630);
    let released = absolute(ABS_PRESSURE, 0);
    let (near, far) = (absolute(ABS_DISTANCE, 0), absolute(ABS_DISTANCE, 100));
    let (tool_on, tool_off) = (key(BTN_TOOL_PEN, 1), key(BTN_TOOL_PEN, 0));
    let (touch_on, touch_off) = (key(BTN_TOUCH, 1), key(BTN_TOUCH, 0));

    Ok(match style {
        // The way `Pen::draw_strokes` sends it: hover, press, move, lift away
        "full" => (
            vec![
                packet(&[from_x, from_y, tool_on, touch_off, released, far]),
                packet(&[touch_on, pressed, near]),
                packet(&[to_x, to_y]),
                packet(&[released, far, touch_off, tool_off]),
            ],
            false,
        ),
        // The pen stays in range: move, press, move, lift
        "hover" => (
            vec![
                packet(&[from_x, from_y]),
                packet(&[touch_on, pressed]),
                packet(&[to_x, to_y]),
                packet(&[touch_off, released]),
            ],
            true,
        ),
        // The same, and it also rises off the surface between strokes
        "hover-distance" => (
            vec![
                packet(&[from_x, from_y]),
                packet(&[touch_on, pressed, near]),
                packet(&[to_x, to_y]),
                packet(&[touch_off, released, far]),
            ],
            true,
        ),
        // Arrive and press at once
        "merged-down" => (
            vec![
                packet(&[from_x, from_y, tool_on, touch_on, pressed, near]),
                packet(&[to_x, to_y]),
                packet(&[released, far, touch_off, tool_off]),
            ],
            false,
        ),
        // Move and lift at once
        "merged-lift" => (
            vec![
                packet(&[from_x, from_y, tool_on, touch_off, released, far]),
                packet(&[touch_on, pressed, near]),
                packet(&[to_x, to_y, released, far, touch_off, tool_off]),
            ],
            false,
        ),
        // The fewest events: in range throughout, press on arrival, move, lift
        "lean" => (
            vec![
                packet(&[from_x, from_y, touch_on, pressed]),
                packet(&[to_x, to_y]),
                packet(&[touch_off, released]),
            ],
            true,
        ),
        other => anyhow::bail!("Unknown stroke style {other}"),
    })
}

/// Bring the pen into range of the screen without touching it, or take it away.
fn pen_in_range(in_range: bool) -> Vec<InputEvent> {
    vec![
        key(BTN_TOOL_PEN, in_range as i32),
        absolute(ABS_DISTANCE, if in_range { 0 } else { 100 }),
        report(),
    ]
}

/// Switch to fineliner (with correct size/color settings), run drawing closure, then restore.
/// Prints how long the drawing itself took.
async fn with_fineliner<F: FnOnce() -> Result<()>>(f: F) -> Result<()> {
    let mut touch = Touch::new(false, TriggerCorner::UpperRight)?;
    let previous = touch.select_fineliner().await?;
    let started = std::time::Instant::now();
    let result = f();
    println!("elapsed_ms={}", started.elapsed().as_millis());
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
