use anyhow::Result;
use base64::prelude::*;
use clap::Parser;
use dotenv::dotenv;
use env_logger;
use log::{debug, info};
use rust_embed::Embed;
use serde_json::Value as json;
use std::sync::{Arc, Mutex};

use std::io::Write;
use std::thread::sleep;
use std::time::Duration;

use ghostwriter::{
    keyboard::Keyboard,
    llm_engine::{anthropic::Anthropic, google::Google, openai::OpenAI, LLMEngine},
    pen::Pen,
    screenshot::Screenshot,
    segmenter::analyze_image,
    touch::Touch,
    util::{svg_to_bitmap, write_bitmap_to_file, OptionMap},
};

// Output dimensions remain the same for both devices
const VIRTUAL_WIDTH: u32 = 768;
const VIRTUAL_HEIGHT: u32 = 1024;

#[derive(Embed)]
#[folder = "prompts/"]
struct AssetPrompts;

#[derive(Embed)]
#[folder = "utils/"]
#[include = "rmpp/uinput-3.17.ko"]
struct AssetUtils;

#[derive(Parser)]
#[command(author, version)]
#[command(about = "Vision-LLM Agent for the reMarkable2")]
#[command(
    long_about = "Ghostwriter is an exploration of how to interact with vision-LLM through the handwritten medium of the reMarkable2. It is a pluggable system; you can provide a custom prompt and custom 'tools' that the agent can use."
)]
#[command(after_help = "See https://github.com/awwaiid/ghostwriter for updates!")]
struct Args {
    /// Sets the engine to use (openai, anthropic);
    /// Sometimes we can guess the engine from the model name
    #[arg(long)]
    engine: Option<String>,

    /// Sets the base URL for the engine API;
    /// Or use environment variable OPENAI_BASE_URL or ANTHROPIC_BASE_URL
    #[arg(long)]
    engine_base_url: Option<String>,

    /// Sets the API key for the engine;
    /// Or use environment variable OPENAI_API_KEY or ANTHROPIC_API_KEY
    #[arg(long)]
    engine_api_key: Option<String>,

    /// Sets the model to use
    #[arg(long, short, default_value = "claude-3-5-sonnet-latest")]
    model: String,

    /// Sets the prompt to use
    #[arg(long, default_value = "general.json")]
    prompt: String,

    /// Do not actually submit to the model, for testing
    #[arg(short, long)]
    no_submit: bool,

    /// Skip running draw_text or draw_svg, for testing
    #[arg(long)]
    no_draw: bool,

    /// Disable keyboard
    #[arg(long)]
    no_keyboard: bool,

    /// Disable keyboard progress
    #[arg(long)]
    no_draw_progress: bool,

    /// Input PNG file for testing
    #[arg(long)]
    input_png: Option<String>,

    /// Output file for testing
    #[arg(long)]
    output_file: Option<String>,

    /// Output file for model parameters
    #[arg(long)]
    model_output_file: Option<String>,

    /// Save screenshot filename
    #[arg(long)]
    save_screenshot: Option<String>,

    /// Save bitmap filename
    #[arg(long)]
    save_bitmap: Option<String>,

    /// Disable looping
    #[arg(long)]
    no_loop: bool,

    /// Disable waiting for trigger
    #[arg(long)]
    no_trigger: bool,

    /// Apply segmentation
    #[arg(long)]
    apply_segmentation: bool,

    /// Set the log level. Try 'debug' or 'trace'
    #[arg(long, default_value = "info")]
    log_level: String,
}

fn main() -> Result<()> {
    dotenv().ok();
    let args = Args::parse();

    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(args.log_level.as_str()),
    )
    .format_timestamp_millis()
    .init();

    setup_uinput()?;

    ghostwriter(&args)
}

macro_rules! shared {
    ($x:expr) => {
        Arc::new(Mutex::new($x))
    };
}

macro_rules! lock {
    ($x:expr) => {
        $x.lock().unwrap()
    };
}

fn setup_uinput() -> Result<()> {
    debug!("Checking for uinput module");
    // Check if uinput module is loaded by looking at the lsmod output
    let output = std::process::Command::new("lsmod")
        .output()
        .expect("Failed to execute lsmod");
    let output_str = std::str::from_utf8(&output.stdout).unwrap();
    if !output_str.contains("uinput") {
        info!("uinput module not found, installing bundled version");
        let uinput_module_asset = AssetUtils::get("rmpp/uinput-3.17.ko").unwrap();
        let raw_uinput_module_data = uinput_module_asset.data.as_ref();
        let mut uinput_module_file = std::fs::File::create("/tmp/uinput.ko")?;
        uinput_module_file.write_all(raw_uinput_module_data)?;
        uinput_module_file.flush()?;
        drop(uinput_module_file);
        let output = std::process::Command::new("insmod")
            .arg("/tmp/uinput.ko")
            .output()?;
        let output_str = std::str::from_utf8(&output.stderr).unwrap();
        info!("insmod output: {}", output_str);
    }

    Ok(())
}

fn draw_text(text: &str, keyboard: &mut Keyboard) -> Result<()> {
    info!("Drawing text to the screen.");
    // keyboard.progress(".")?;
    keyboard.progress_end()?;
    keyboard.key_cmd_body()?;
    keyboard.string_to_keypresses(text)?;
    // keyboard.string_to_keypresses("\n\n")?;
    Ok(())
}

fn draw_svg(
    svg_data: &str,
    keyboard: &mut Keyboard,
    pen: &mut Pen,
    save_bitmap: Option<&String>,
    no_draw: bool,
) -> Result<()> {
    info!("Drawing SVG to the screen.");
    keyboard.progress_end()?;
    if let Some(save_bitmap) = save_bitmap {
        let bitmap = svg_to_bitmap(svg_data, VIRTUAL_WIDTH, VIRTUAL_HEIGHT)?;
        write_bitmap_to_file(&bitmap, save_bitmap)?;
    }
    if !no_draw {
        // pen.draw_bitmap(&bitmap)?;
        pen.draw_svg(svg_data)?;
    }
    Ok(())
}

fn load_config(filename: &str) -> String {
    debug!("Loading config from {}", filename);

    if std::path::Path::new(filename).exists() {
        std::fs::read_to_string(filename).unwrap()
    } else {
        std::str::from_utf8(AssetPrompts::get(filename).unwrap().data.as_ref())
            .unwrap()
            .to_string()
    }
}

fn ghostwriter(args: &Args) -> Result<()> {
    let keyboard = shared!(Keyboard::new(
        args.no_draw || args.no_keyboard,
        args.no_draw_progress,
    ));
    let pen = shared!(Pen::new(args.no_draw));
    let touch = shared!(Touch::new(args.no_draw));

    // Give time for the virtual keyboard to be plugged in
    sleep(Duration::from_millis(1000));

    lock!(touch).tap_middle_bottom()?;
    sleep(Duration::from_millis(1000));

    lock!(keyboard).progress("Keyboard loaded...")?;

    let mut engine_options = OptionMap::new();

    let model = args.model.clone();
    engine_options.insert("model".to_string(), model.clone());

    let engine_name = if let Some(engine) = args.engine.clone() {
        engine.to_string()
    } else {
        if model.starts_with("gpt") {
            "openai".to_string()
        } else if model.starts_with("claude") {
            "anthropic".to_string()
        } else if model.starts_with("gemini") {
            "google".to_string()
        } else {
            panic!("Unable to guess engine from model name {}", model)
        }
    };

    if args.engine_base_url.is_some() {
        engine_options.insert(
            "base_url".to_string(),
            args.engine_base_url.clone().unwrap(),
        );
    }
    if args.engine_api_key.is_some() {
        engine_options.insert("api_key".to_string(), args.engine_api_key.clone().unwrap());
    }

    let mut engine: Box<dyn LLMEngine> = match engine_name.as_str() {
        "openai" => Box::new(OpenAI::new(&engine_options)),
        "anthropic" => Box::new(Anthropic::new(&engine_options)),
        "google" => Box::new(Google::new(&engine_options)),
        _ => panic!("Unknown engine {}", engine_name),
    };

    let output_file = args.output_file.clone();
    let no_draw = args.no_draw;
    let keyboard_clone = Arc::clone(&keyboard);

    let tool_config_draw_text = load_config("tool_draw_text.json");

    engine.register_tool(
        "draw_text",
        serde_json::from_str::<serde_json::Value>(tool_config_draw_text.as_str())?,
        Box::new(move |arguments: json| {
            let text = arguments["text"].as_str().unwrap();
            if let Some(output_file) = &output_file {
                std::fs::write(output_file, text).unwrap();
            }
            if !no_draw {
                // let mut keyboard = lock!(keyboard_clone);
                draw_text(text, &mut lock!(keyboard_clone)).unwrap();
            }
        }),
    );

    let output_file = args.output_file.clone();
    let save_bitmap = args.save_bitmap.clone();
    let no_draw = args.no_draw;
    let keyboard_clone = Arc::clone(&keyboard);
    let pen_clone = Arc::clone(&pen);

    let tool_config_draw_svg = load_config("tool_draw_svg.json");
    engine.register_tool(
        "draw_svg",
        serde_json::from_str::<serde_json::Value>(tool_config_draw_svg.as_str())?,
        Box::new(move |arguments: json| {
            let svg_data = arguments["svg"].as_str().unwrap();
            if let Some(output_file) = &output_file {
                std::fs::write(output_file, svg_data).unwrap();
            }
            let mut keyboard = lock!(keyboard_clone);
            let mut pen = lock!(pen_clone);
            draw_svg(
                svg_data,
                &mut keyboard,
                &mut pen,
                save_bitmap.as_ref(),
                no_draw,
            )
            .unwrap();
        }),
    );

    lock!(keyboard).progress("Tools initialized.")?;
    sleep(Duration::from_millis(1000));
    lock!(keyboard).progress_end()?;
    sleep(Duration::from_millis(1000));

    loop {
        if args.no_trigger {
            debug!("Skipping waiting for trigger");
        } else {
            info!("Waiting for trigger (hand-touch in the upper-right corner)...");
            lock!(touch).wait_for_trigger()?;
        }

        // Sleep a bit to differentiate the touches
        sleep(Duration::from_millis(100));
        lock!(touch).tap_middle_bottom()?;
        // sleep(Duration::from_millis(1000));
        // lock!(keyboard).progress("Taking screenshot...")?;

        info!("Getting screenshot (or loading input image)");
        let base64_image = if let Some(input_png) = &args.input_png {
            BASE64_STANDARD.encode(std::fs::read(input_png)?)
        } else {
            let mut screenshot = Screenshot::new()?;
            screenshot.take_screenshot()?;
            if let Some(save_screenshot) = &args.save_screenshot {
                info!("Saving screenshot to {}", save_screenshot);
                screenshot.save_image(save_screenshot)?;
            }
            screenshot.base64()?
        };

        if args.no_submit {
            info!("Image not submitted to model due to --no-submit flag");
            lock!(keyboard).progress_end()?;
            return Ok(());
        }

        let prompt_general_raw = load_config(&args.prompt);
        let prompt_general_json =
            serde_json::from_str::<serde_json::Value>(prompt_general_raw.as_str())?;
        let prompt = prompt_general_json["prompt"].as_str().unwrap();

        let segmentation_description = if args.apply_segmentation {
            info!("Building image segmentation");
            lock!(keyboard).progress("segmenting...")?;
            let input_filename = args
                .input_png
                .clone()
                .unwrap_or(args.save_screenshot.clone().unwrap());
            match analyze_image(input_filename.as_str()) {
                Ok(description) => description,
                Err(e) => format!("Error analyzing image: {}", e),
            }
        } else {
            String::new()
        };
        debug!("Segmentation description: {}", segmentation_description);

        engine.clear_content();
        engine.add_image_content(&base64_image);

        if args.apply_segmentation {
            engine.add_text_content(
               format!("Here are interesting regions based on an automatic segmentation algorithm. Use them to help identify the exact location of interesting features.\n\n{}", segmentation_description).as_str()
            );
        }

        engine.add_text_content(prompt);

        info!("Executing the engine (call out to {}", engine_name);
        lock!(keyboard).progress("thinking...")?;
        if engine.execute().is_err() {
            lock!(keyboard).progress(" model error. ")?;
        }

        if args.no_loop {
            break Ok(());
        }
    }
}
