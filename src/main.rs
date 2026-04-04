use anyhow::Result;
use base64::prelude::*;
use clap::Parser;
use dotenv::dotenv;
use log::{debug, info};
use serde::Serialize;
use serde_json::Value as json;
use std::sync::{Arc, Mutex};

use std::thread::sleep;
use std::time::Duration;

use ghostwriter::{
    config::Config,
    embedded_assets::load_config,
    keyboard::Keyboard,
    llm_engine::{anthropic::Anthropic, google::Google, openai::OpenAI, LLMEngine},
    pen::Pen,
    screenshot::Screenshot,
    segmenter::analyze_image,
    touch::{Touch, TriggerCorner},
    util::{setup_uinput, svg_to_bitmap, write_bitmap_to_file, OptionMap},
};

// Output dimensions remain the same for both devices
const VIRTUAL_WIDTH: u32 = 768;
const VIRTUAL_HEIGHT: u32 = 1024;

#[derive(Parser, Serialize)]
#[command(author, version)]
#[command(about = "Vision-LLM Agent for the reMarkable2")]
#[command(
    long_about = "Ghostwriter is an exploration of how to interact with vision-LLM through the handwritten medium of the reMarkable2. It is a pluggable system; you can provide a custom prompt and custom 'tools' that the agent can use."
)]
#[command(after_help = "See https://github.com/awwaiid/ghostwriter for updates!")]
pub struct Args {
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
    #[arg(long, short, default_value = "claude-sonnet-4-0")]
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

    /// Disable SVG drawing tool
    #[arg(long)]
    no_svg: bool,

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

    /// Enable web search (for Anthropic models)
    #[arg(long)]
    web_search: bool,

    /// Enable model thinking (for Anthropic models)
    #[arg(long)]
    thinking: bool,

    /// Set the thinking token budget (for Anthropic models)
    #[arg(long, default_value = "5000")]
    thinking_tokens: u32,

    /// Set the log level. Try 'debug' or 'trace'
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Sets which corner the touch trigger listens to (UR, UL, LR, LL, upper-right, upper-left, lower-right, lower-left)
    #[arg(long, default_value = "UR")]
    trigger_corner: String,

    /// Save current configuration to ~/.ghostwriter.toml and exit
    #[arg(long)]
    save_config: bool,
}

fn main() -> Result<()> {
    dotenv().ok();

    let args = Args::parse();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(args.log_level.as_str()))
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

fn draw_text(text: &str, keyboard: &mut Keyboard) -> Result<()> {
    info!("Drawing text to the screen.");
    // keyboard.progress(".")?;
    keyboard.progress_end()?;
    keyboard.key_cmd_body()?;
    keyboard.string_to_keypresses(text)?;
    // keyboard.string_to_keypresses("\n\n")?;
    Ok(())
}

fn draw_svg(svg_data: &str, keyboard: &mut Keyboard, pen: &mut Pen, save_bitmap: Option<&String>, no_draw: bool) -> Result<()> {
    info!("Drawing SVG to the screen.");
    keyboard.progress_end()?;

    // Force the document to scroll by briefly inserting lines, then deleting them.
    keyboard.progress("\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n")?;
    sleep(Duration::from_millis(200));
    keyboard.progress_end()?;

    let bitmap = svg_to_bitmap(svg_data, VIRTUAL_WIDTH, VIRTUAL_HEIGHT)?;
    if let Some(save_bitmap) = save_bitmap {
        write_bitmap_to_file(&bitmap, save_bitmap)?;
    }
    if !no_draw {
        pen.draw_bitmap(&bitmap)?;
    }
    Ok(())
}

fn determine_engine_name(engine_arg: &Option<String>, model: &str) -> Result<String> {
    if let Some(engine) = engine_arg {
        return Ok(engine.clone());
    }

    if model.starts_with("gpt") {
        Ok("openai".to_string())
    } else if model.starts_with("claude") {
        Ok("anthropic".to_string())
    } else if model.starts_with("gemini") {
        Ok("google".to_string())
    } else {
        Err(anyhow::anyhow!(
            "Unable to guess engine from model name '{}'. Please specify --engine (openai, anthropic, or google)",
            model
        ))
    }
}

fn create_engine(engine_name: &str, engine_options: &OptionMap) -> Result<Box<dyn LLMEngine>> {
    match engine_name {
        "openai" => Ok(Box::new(OpenAI::new(engine_options))),
        "anthropic" => Ok(Box::new(Anthropic::new(engine_options))),
        "google" => Ok(Box::new(Google::new(engine_options))),
        _ => Err(anyhow::anyhow!(
            "Unknown engine '{}'. Supported engines: openai, anthropic, google",
            engine_name
        )),
    }
}

fn ghostwriter(args: &Args) -> Result<()> {
    let config = Config::load(args)?;

    // Handle --save-config option
    if args.save_config {
        config.save()?;
        println!("Configuration saved to {:?}", Config::config_path()?);
        return Ok(());
    }

    let trigger_corner = TriggerCorner::from_string(&config.trigger_corner)?;
    let keyboard = shared!(Keyboard::new(config.no_draw || config.no_keyboard, config.no_draw_progress,));
    let pen = shared!(Pen::new(config.no_draw));
    let touch = shared!(Touch::new(config.no_draw, trigger_corner));

    // Give time for the virtual keyboard to be plugged in
    sleep(Duration::from_millis(1000));

    lock!(touch).tap_middle_bottom()?;
    sleep(Duration::from_millis(1000));

    lock!(keyboard).progress("Keyboard loaded...")?;

    let mut engine_options = OptionMap::new();

    let model = config.model.clone();
    engine_options.insert("model".to_string(), model.clone());
    debug!("Model: {}", model);

    let engine_name = determine_engine_name(&config.engine, &model)?;
    debug!("Engine: {}", engine_name);

    if config.engine_base_url.is_some() {
        debug!("Engine base URL: {}", config.engine_base_url.clone().unwrap());
        engine_options.insert("base_url".to_string(), config.engine_base_url.clone().unwrap());
    }
    if config.engine_api_key.is_some() {
        debug!("Using API key from CLI args");
        engine_options.insert("api_key".to_string(), config.engine_api_key.clone().unwrap());
    }

    if config.web_search {
        debug!("Web search tool enabled");
        engine_options.insert("web_search".to_string(), "true".to_string());
    }

    if config.thinking {
        debug!("Thinking enabled with budget: {}", config.thinking_tokens);
        engine_options.insert("thinking".to_string(), "true".to_string());
        engine_options.insert("thinking_tokens".to_string(), config.thinking_tokens.to_string());
    }

    let mut engine = create_engine(&engine_name, &engine_options)?;

    let output_file = config.output_file.clone();
    let no_draw = config.no_draw;
    let keyboard_clone = Arc::clone(&keyboard);

    let tool_config_draw_text = load_config("tool_draw_text.json");

    engine.register_tool(
        "draw_text",
        serde_json::from_str::<serde_json::Value>(tool_config_draw_text.as_str())?,
        Box::new(move |arguments: json| {
            let text = match arguments["text"].as_str() {
                Some(t) => t,
                None => {
                    log::error!("draw_text tool called without valid 'text' argument");
                    return;
                }
            };
            if let Some(output_file) = &output_file {
                if let Err(e) = std::fs::write(output_file, text) {
                    log::error!("Failed to write output file: {}", e);
                }
            }
            if !no_draw {
                // let mut keyboard = lock!(keyboard_clone);
                if let Err(e) = draw_text(text, &mut lock!(keyboard_clone)) {
                    log::error!("Failed to draw text: {}", e);
                }
            }
        }),
    );

    let output_file = config.output_file.clone();
    let save_bitmap = config.save_bitmap.clone();
    let no_draw = config.no_draw;
    let keyboard_clone = Arc::clone(&keyboard);
    let pen_clone = Arc::clone(&pen);

    if !config.no_svg {
        let tool_config_draw_svg = load_config("tool_draw_svg.json");
        engine.register_tool(
            "draw_svg",
            serde_json::from_str::<serde_json::Value>(tool_config_draw_svg.as_str())?,
            Box::new(move |arguments: json| {
                let svg_data = match arguments["svg"].as_str() {
                    Some(svg) => svg,
                    None => {
                        log::error!("draw_svg tool called without valid 'svg' argument");
                        return;
                    }
                };
                if let Some(output_file) = &output_file {
                    if let Err(e) = std::fs::write(output_file, svg_data) {
                        log::error!("Failed to write output file: {}", e);
                    }
                }
                let mut keyboard = lock!(keyboard_clone);
                let mut pen = lock!(pen_clone);
                if let Err(e) = draw_svg(svg_data, &mut keyboard, &mut pen, save_bitmap.as_ref(), no_draw) {
                    log::error!("Failed to draw SVG: {}", e);
                }
            }),
        );
    }

    lock!(keyboard).progress("Tools initialized.")?;
    sleep(Duration::from_millis(1000));
    lock!(keyboard).progress_end()?;
    sleep(Duration::from_millis(1000));

    loop {
        if config.no_trigger {
            debug!("Skipping waiting for trigger");
        } else {
            info!(
                "Waiting for trigger (hand-touch in the {} corner)...",
                match TriggerCorner::from_string(&config.trigger_corner).unwrap() {
                    TriggerCorner::UpperRight => "upper-right",
                    TriggerCorner::UpperLeft => "upper-left",
                    TriggerCorner::LowerRight => "lower-right",
                    TriggerCorner::LowerLeft => "lower-left",
                }
            );
            lock!(touch).wait_for_trigger()?;
        }

        // Sleep a bit to differentiate the touches
        sleep(Duration::from_millis(100));
        lock!(touch).tap_middle_bottom()?;
        // sleep(Duration::from_millis(1000));
        // lock!(keyboard).progress("Taking screenshot...")?;

        info!("Getting screenshot (or loading input image)");
        let base64_image = if let Some(input_png) = &config.input_png {
            BASE64_STANDARD.encode(std::fs::read(input_png)?)
        } else {
            let mut screenshot = Screenshot::new()?;
            screenshot.take_screenshot()?;
            if let Some(save_screenshot) = &config.save_screenshot {
                info!("Saving screenshot to {}", save_screenshot);
                screenshot.save_image(save_screenshot)?;
            }
            screenshot.base64()?
        };

        if config.no_submit {
            info!("Image not submitted to model due to --no-submit flag");
            lock!(keyboard).progress_end()?;
            return Ok(());
        }

        let prompt_general_raw = load_config(&config.prompt);
        let prompt_general_json = serde_json::from_str::<serde_json::Value>(prompt_general_raw.as_str())?;
        let prompt = prompt_general_json["prompt"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Prompt file '{}' missing required 'prompt' field", config.prompt))?;

        let segmentation_description = if config.apply_segmentation {
            info!("Building image segmentation");
            lock!(keyboard).progress("segmenting...")?;
            let input_filename = config
                .input_png
                .clone()
                .or_else(|| config.save_screenshot.clone())
                .ok_or_else(|| anyhow::anyhow!("Segmentation requires either --input-png or --save-screenshot to be specified"))?;
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

        if config.apply_segmentation {
            engine.add_text_content(
               format!("Here are interesting regions based on an automatic segmentation algorithm. Use them to help identify the exact location of interesting features.\n\n{}", segmentation_description).as_str()
            );
        }

        engine.add_text_content(prompt);

        info!("Executing the engine (call out to {}", engine_name);
        lock!(keyboard).progress("thinking...")?;
        if engine.execute().is_err() {
            lock!(keyboard).progress(" model error. ")?;
            // Show the error briefly, then erase progress text so it doesn't remain in the document.
            sleep(Duration::from_millis(800));
            lock!(keyboard).progress_end()?;
        }

        if config.no_loop {
            break Ok(());
        }
    }
}
