use anyhow::Result;
use clap::Parser;
use dotenv::dotenv;
use log::info;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as TokioMutex, RwLock as TokioRwLock};

use std::time::Duration;
use tokio::time::sleep;

use ghostwriter::{
    cancellation::GhostwriterCancellation,
    config::Config,
    coordinator::{self, CoordinatorChannels, ProgressState},
    device::{DeviceModel, VIRTUAL_HEIGHT, VIRTUAL_WIDTH},
    embedded_assets::load_config,
    keyboard::Keyboard,
    llm_engine::{anthropic::Anthropic, google::Google, openai::OpenAI, LLMEngine},
    pen::{Drawing, Pen},
    simulation::SimulationConfig,
    status::GhostwriterStatus,
    touch::{PenTool, Touch, TriggerCorner},
    util::{setup_uinput, svg_to_bitmap, write_bitmap_to_file, OptionMap},
    web_server::start_web_server,
};

/// Command-line arguments.
///
/// These are layered on top of defaults, ~/.ghostwriter.toml, and GHOSTWRITER_*
/// environment variables (see `Config::load`). Only flags actually passed on the
/// command line should override those, so unset options and `false` switches are
/// skipped during serialization; defaults live in `Config::default()`.
#[derive(Parser, Serialize)]
#[command(author, version)]
#[command(about = "Vision-LLM Agent for the reMarkable")]
#[command(
    long_about = "Ghostwriter is an exploration of how to interact with vision-LLM through the handwritten medium of the reMarkable. It is a pluggable system; you can provide a custom prompt and custom 'tools' that the agent can use."
)]
#[command(after_help = "See https://github.com/awwaiid/ghostwriter for updates!")]
pub struct Args {
    /// Sets the engine to use (openai, anthropic, google);
    /// Sometimes we can guess the engine from the model name
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<String>,

    /// Sets the base URL for the engine API;
    /// Or use environment variable OPENAI_BASE_URL, ANTHROPIC_BASE_URL or GOOGLE_BASE_URL
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_base_url: Option<String>,

    /// Sets the API key for the engine;
    /// Or use environment variable OPENAI_API_KEY, ANTHROPIC_API_KEY or GOOGLE_API_KEY
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_api_key: Option<String>,

    /// Sets the model to use (default: claude-sonnet-4-6)
    #[arg(long, short)]
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,

    /// Sets the prompt to use (default: general.json)
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,

    /// Do not actually submit to the model, for testing
    #[arg(short, long)]
    #[serde(skip_serializing_if = "is_false")]
    no_submit: bool,

    /// Skip running draw_text or draw_svg, for testing
    #[arg(long)]
    #[serde(skip_serializing_if = "is_false")]
    no_draw: bool,

    /// Disable SVG drawing tool
    #[arg(long)]
    #[serde(skip_serializing_if = "is_false")]
    no_svg: bool,

    /// Disable keyboard
    #[arg(long)]
    #[serde(skip_serializing_if = "is_false")]
    no_keyboard: bool,

    /// Disable keyboard progress
    #[arg(long)]
    #[serde(skip_serializing_if = "is_false")]
    no_draw_progress: bool,

    /// Input PNG file for testing
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    input_png: Option<String>,

    /// Output file for testing
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    output_file: Option<String>,

    /// Output file for model parameters
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    model_output_file: Option<String>,

    /// Save screenshot filename
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    save_screenshot: Option<String>,

    /// Save bitmap filename
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    save_bitmap: Option<String>,

    /// Disable looping
    #[arg(long)]
    #[serde(skip_serializing_if = "is_false")]
    no_loop: bool,

    /// Disable waiting for trigger
    #[arg(long)]
    #[serde(skip_serializing_if = "is_false")]
    no_trigger: bool,

    /// Apply segmentation
    #[arg(long)]
    #[serde(skip_serializing_if = "is_false")]
    apply_segmentation: bool,

    /// Enable web search (for Anthropic models)
    #[arg(long)]
    #[serde(skip_serializing_if = "is_false")]
    web_search: bool,

    /// Enable model thinking (for Anthropic models)
    #[arg(long)]
    #[serde(skip_serializing_if = "is_false")]
    thinking: bool,

    /// Set the thinking token budget (for Anthropic models; default: 5000)
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_tokens: Option<u32>,

    /// Set the log level (default: info). Try 'debug' or 'trace'
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    log_level: Option<String>,

    /// Sets which corner the touch trigger listens to (default: UR).
    /// Options: UR, UL, LR, LL, upper-right, upper-left, lower-right, lower-left
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    trigger_corner: Option<String>,

    /// Save current configuration to ~/.ghostwriter.toml and exit
    #[arg(long)]
    #[serde(skip)]
    save_config: bool,

    /// Start web server for configuration UI
    #[arg(long)]
    #[serde(skip)]
    web_server: bool,

    /// Port for web server (default: 8080)
    #[arg(long, default_value = "8080")]
    #[serde(skip)]
    web_port: u16,

    /// Enable test/simulation mode for specific device (rm2, rmpp, rmpure, move)
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    test_mode: Option<String>,

    /// File containing scripted touch events for simulation (JSON format)
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    test_touch_events_file: Option<String>,

    /// Directory containing test screenshots to cycle through
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    test_screenshot_dir: Option<String>,

    /// Auto-trigger delay in seconds for automated testing
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    test_auto_trigger_delay: Option<u32>,

    /// File to log simulated interactions to
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    test_interaction_log: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    let args = Args::parse();

    // Load config before starting the logger so log_level can come from the
    // config file or environment, not just the CLI
    let config = Config::load(&args)?;

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(config.log_level.as_str()))
        .format_timestamp_millis()
        .init();

    // Handle --save-config option (before touching any devices)
    if args.save_config {
        config.save()?;
        println!("Configuration saved to {:?}", Config::config_path()?);
        return Ok(());
    }

    setup_uinput()?;

    ghostwriter(&args, config).await
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
    keyboard.progress_end()?;
    keyboard.key_cmd_body()?;
    keyboard.string_to_keypresses(text)?;
    Ok(())
}

/// Draw an SVG on the page. `drawing` is how to draw it, if that has been worked out already.
fn draw_svg(svg_data: &str, drawing: Option<Drawing>, keyboard: &mut Keyboard, pen: &mut Pen, save_bitmap: Option<&String>, no_draw: bool) -> Result<()> {
    info!("Drawing SVG to the screen.");
    keyboard.progress_end()?;
    let scale = 2u32;
    if let Some(save_bitmap) = save_bitmap {
        let bitmap = svg_to_bitmap(svg_data, VIRTUAL_WIDTH * scale, VIRTUAL_HEIGHT * scale)?;
        write_bitmap_to_file(&bitmap, save_bitmap)?;
    }
    if !no_draw {
        match drawing {
            Some(drawing) => pen.draw(&drawing)?,
            None => pen.draw_svg(svg_data)?,
        }
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
        "openai" => Ok(Box::new(OpenAI::new(engine_options)?)),
        "anthropic" => Ok(Box::new(Anthropic::new(engine_options)?)),
        "google" => Ok(Box::new(Google::new(engine_options)?)),
        _ => Err(anyhow::anyhow!(
            "Unknown engine '{}'. Supported engines: openai, anthropic, google",
            engine_name
        )),
    }
}

async fn ghostwriter(args: &Args, mut config: Config) -> Result<()> {
    // Parse test_mode device model if provided
    if let Some(device_str) = &config.test_mode {
        let device_model = DeviceModel::from_string(device_str)?;
        config.test_device_model = Some(device_model);
        info!("Test mode enabled for device: {}", device_model.name());
    }

    // Create shared state for live config updates
    let shared_config = Arc::new(TokioRwLock::new(config.clone()));
    let shared_status = Arc::new(TokioRwLock::new(GhostwriterStatus::default()));

    // Create Touch component for web API and main loop
    let trigger_corner = TriggerCorner::from_string(&config.trigger_corner)?;
    let shared_touch = if args.web_server || config.is_test_mode() {
        let touch = if config.is_test_mode() {
            let simulation_config = SimulationConfig::from_config(&config);
            Touch::new_simulated(simulation_config, trigger_corner)?
        } else {
            Touch::new(config.no_draw, trigger_corner)?
        };
        Some(Arc::new(TokioRwLock::new(touch)))
    } else {
        None
    };

    // Create cancellation holder to be updated on each restart
    // We use Arc<TokioRwLock> so web server can read current cancellation
    let shared_cancellation = Arc::new(TokioRwLock::new(GhostwriterCancellation::new()));

    // Create config watch channel for communication between web server and main loop
    let (config_watch_tx, config_watch_rx) = tokio::sync::watch::channel(config.clone());
    let shared_config_watch_tx = Arc::new(config_watch_tx);

    // Spawn web server in same tokio runtime if requested
    let web_handle = if args.web_server {
        let config_clone = Arc::clone(&shared_config);
        let status_clone = Arc::clone(&shared_status);
        let touch_clone = shared_touch.as_ref().map(Arc::clone);
        let cancellation_clone = Arc::clone(&shared_cancellation);
        let config_watch_tx_clone = Arc::clone(&shared_config_watch_tx);
        let port = args.web_port;

        Some(tokio::spawn(async move {
            start_web_server(
                port,
                config_clone,
                status_clone,
                touch_clone,
                Some(cancellation_clone),
                Some(config_watch_tx_clone),
            )
            .await
        }))
    } else {
        None
    };

    // Run main ghostwriter logic, restarting on config changes
    // Keep a single receiver across iterations to avoid spurious change notifications
    let mut persistent_config_watch_rx = config_watch_rx.clone();
    let result = loop {
        // Create fresh cancellation for each iteration
        let cancellation = Arc::new(GhostwriterCancellation::new());

        // Update shared cancellation for web server
        if args.web_server {
            let mut shared_cancel = shared_cancellation.write().await;
            *shared_cancel = (*cancellation).clone();
        }

        match run_ghostwriter_loop(
            Arc::clone(&shared_config),
            Arc::clone(&shared_status),
            shared_touch.as_ref().map(Arc::clone),
            cancellation,
            &mut persistent_config_watch_rx,
        )
        .await
        {
            Ok(()) => {
                info!("Ghostwriter loop exited normally, restarting to pick up config changes...");
                continue; // Restart the loop
            }
            Err(e) => {
                break Err(e); // Exit on actual errors
            }
        }
    };

    // Wait for web server task if it exists
    if let Some(handle) = web_handle {
        let _ = handle.await;
    }

    result
}

async fn run_ghostwriter_loop(
    shared_config: Arc<TokioRwLock<Config>>,
    _shared_status: Arc<TokioRwLock<GhostwriterStatus>>,
    shared_touch: Option<Arc<TokioRwLock<Touch>>>,
    cancellation: Arc<GhostwriterCancellation>,
    config_watch_rx: &mut tokio::sync::watch::Receiver<Config>,
) -> Result<()> {
    info!("Starting ghostwriter with new coordinator architecture");

    // Get initial config
    let config = shared_config.read().await.clone();

    // Create coordinator channels
    let channels = CoordinatorChannels::new();

    // Initialize devices
    let trigger_corner = TriggerCorner::from_string(&config.trigger_corner)?;
    let keyboard = shared!(Keyboard::new(
        config.is_test_mode() || config.no_draw || config.no_keyboard,
        config.no_draw_progress,
    )?);

    let pen = shared!(Pen::new(config.is_test_mode() || config.no_draw)?);

    let touch = if let Some(shared_touch) = shared_touch {
        shared_touch
    } else {
        Arc::new(TokioRwLock::new(Touch::new(config.no_draw, trigger_corner)?))
    };

    touch.write().await.tap_middle_bottom().await?;
    lock!(keyboard).progress("Ghostwriter starting...")?;
    sleep(Duration::from_millis(1000)).await;
    lock!(keyboard).progress_end()?;

    // Initialize engine
    let mut engine_options = OptionMap::new();
    engine_options.insert("model".to_string(), config.model.clone());

    let engine_name = determine_engine_name(&config.engine, &config.model)?;
    if let Some(base_url) = &config.engine_base_url {
        engine_options.insert("base_url".to_string(), base_url.clone());
    }
    if let Some(api_key) = &config.engine_api_key {
        engine_options.insert("api_key".to_string(), api_key.clone());
    } else if config.no_submit {
        // Nothing is sent to the model in no-submit mode, so don't insist on an API key
        engine_options.insert("api_key".to_string(), String::new());
    }
    if config.web_search {
        engine_options.insert("web_search".to_string(), "true".to_string());
    }
    if config.thinking {
        engine_options.insert("thinking".to_string(), "true".to_string());
        engine_options.insert("thinking_tokens".to_string(), config.thinking_tokens.to_string());
    }

    let mut engine = create_engine(&engine_name, &engine_options)?;

    // Register tools
    register_tools(&mut engine, Arc::clone(&keyboard), Arc::clone(&pen), Arc::clone(&touch), &config)?;

    let engine = Arc::new(TokioMutex::new(engine));

    // Spawn long-lived tasks
    let trigger_handle = {
        let touch = Arc::clone(&touch);
        let trigger_tx = channels.trigger_tx.clone();
        let cancellation = Arc::clone(&cancellation);
        let no_trigger = config.no_trigger;
        tokio::spawn(async move { coordinator::trigger_task(touch, trigger_tx, cancellation, no_trigger).await })
    };

    let progress_handle = {
        let keyboard = Arc::clone(&keyboard);
        let progress_rx = channels.progress_rx.clone();
        let cancellation = Arc::clone(&cancellation);
        tokio::spawn(async move { coordinator::progress_task(keyboard, progress_rx, cancellation).await })
    };

    // Main loop
    let mut trigger_rx = channels.trigger_rx;
    let progress_tx = channels.progress_tx.clone();

    info!("Main: entering main loop");

    loop {
        // Update progress to waiting for trigger
        let _ = progress_tx.send(ProgressState::WaitingForTrigger);
        info!("Main: waiting for next trigger...");

        tokio::select! {
            Some(_trigger_event) = trigger_rx.recv() => {
                info!("Main: trigger received, starting processing");

                // Create a new execution cycle for this processing run
                cancellation.new_execution_cycle();

                // TODO: also spawn coordinator::cancel_monitor_task here, so a touch can
                // interrupt processing (and stop it again once processing finishes)

                // Spawn processing task
                let processing_handle = {
                    let config_clone = config.clone();
                    let engine_clone = Arc::clone(&engine);
                    let progress_tx_clone = progress_tx.clone();
                    let cancellation_clone = Arc::clone(&cancellation);
                    let touch_clone = Arc::clone(&touch);
                    tokio::spawn(async move {
                        coordinator::processing_task(
                            config_clone,
                            engine_clone,
                            progress_tx_clone,
                            cancellation_clone,
                            touch_clone,
                        ).await
                    })
                };

                let processing_result = processing_handle.await;

                // End this execution cycle
                cancellation.cancel_execution();

                match processing_result {
                    Ok(Ok(_)) => {
                        info!("Processing completed successfully, ready for next trigger");
                    }
                    Ok(Err(e)) => {
                        info!("Processing error: {}, ready for next trigger", e);
                    }
                    Err(e) => {
                        info!("Processing task join error: {}, ready for next trigger", e);
                    }
                }

                // Check no_loop mode
                if config.no_loop {
                    info!("No-loop mode, exiting");
                    std::process::exit(0);
                }

                // Drain any triggers that arrived during processing
                while trigger_rx.try_recv().is_ok() {
                    info!("Ignoring trigger received during processing");
                }
            }

            // Wait for config changes via watch channel (priority 2)
            _ = config_watch_rx.changed() => {
                info!("Config changed via watch channel, restarting loop");
                cancellation.cancel_all(); // Cancel all tokens to ensure clean shutdown
                break; // Exit loop to clean up and restart
            }
        }
    }

    // Clean shutdown - wait for tasks to complete
    info!("Main: shutting down tasks");

    // Cancel any ongoing execution and tasks
    cancellation.cancel_execution();

    // Give tasks a moment to notice cancellation
    sleep(Duration::from_millis(100)).await;

    // Wait for tasks with timeout to prevent hanging
    let shutdown_timeout = Duration::from_secs(2);

    match tokio::time::timeout(shutdown_timeout, trigger_handle).await {
        Ok(Ok(Ok(_))) => info!("Trigger task completed successfully"),
        Ok(Ok(Err(e))) => info!("Trigger task error: {}", e),
        Ok(Err(e)) => info!("Trigger task join error: {}", e),
        Err(_) => {
            info!("Trigger task shutdown timed out - this is expected in no-trigger mode");
        }
    }

    match tokio::time::timeout(shutdown_timeout, progress_handle).await {
        Ok(Ok(Ok(_))) => info!("Progress task completed successfully"),
        Ok(Ok(Err(e))) => info!("Progress task error: {}", e),
        Ok(Err(e)) => info!("Progress task join error: {}", e),
        Err(_) => info!("Progress task shutdown timed out"),
    }

    info!("Main: clean shutdown complete");
    Ok(())
}

// Helper function to register tools with the engine
fn register_tools(
    engine: &mut Box<dyn LLMEngine>,
    keyboard: Arc<Mutex<Keyboard>>,
    pen: Arc<Mutex<Pen>>,
    _touch: Arc<TokioRwLock<Touch>>,
    config: &Config,
) -> Result<()> {
    use serde_json::Value as json;

    // Register draw_text tool
    let output_file = config.output_file.clone();
    let no_draw = config.no_draw;
    let keyboard_clone = Arc::clone(&keyboard);

    let tool_config_draw_text = load_config("tool_draw_text.json")?;
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
                if let Err(e) = draw_text(text, &mut lock!(keyboard_clone)) {
                    log::error!("Failed to draw text: {}", e);
                }
            }
        }),
    );

    // Register draw_svg tool
    if !config.no_svg {
        let output_file = config.output_file.clone();
        let save_bitmap = config.save_bitmap.clone();
        let no_draw = config.no_draw;
        let keyboard_clone = Arc::clone(&keyboard);
        let pen_clone = Arc::clone(&pen);
        let test_mode = config.is_test_mode();

        let tool_config_draw_svg = load_config("tool_draw_svg.json")?;
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

                // Switch to fineliner before drawing, remember original tool for restore.
                // Meanwhile, work out how to draw the SVG.
                // Use a fresh Touch instance to avoid deadlock with trigger_task which
                // holds the shared touch RwLock indefinitely while waiting for user trigger
                let (previous_tool, drawing) = if !no_draw && !test_mode {
                    let device_model = lock!(pen_clone).device_model();
                    std::thread::scope(|scope| {
                        let planning = scope.spawn(|| Drawing::plan(svg_data, device_model));
                        let previous_tool = tokio::task::block_in_place(|| {
                            tokio::runtime::Handle::current().block_on(async { Touch::new(false, TriggerCorner::UpperRight)?.select_fineliner().await })
                        })
                        .unwrap_or(PenTool::Unknown);
                        (previous_tool, planning.join().ok().and_then(Result::ok))
                    })
                } else {
                    (PenTool::Unknown, None)
                };

                let mut keyboard = lock!(keyboard_clone);
                let mut pen = lock!(pen_clone);
                if let Err(e) = draw_svg(svg_data, drawing, &mut keyboard, &mut pen, save_bitmap.as_ref(), no_draw) {
                    log::error!("Failed to draw SVG: {}", e);
                }
                drop(keyboard);
                drop(pen);

                // Restore the original tool after drawing
                if !no_draw && !test_mode && previous_tool != PenTool::Unknown {
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async { Touch::new(false, TriggerCorner::UpperRight)?.restore_tool(previous_tool).await })
                    })
                    .ok();
                }
            }),
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A config file path unique to this test, removed on drop
    struct TempConfig(PathBuf);

    impl TempConfig {
        fn new(name: &str, contents: &str) -> Self {
            let path = std::env::temp_dir().join(format!("ghostwriter-test-{}-{}.toml", std::process::id(), name));
            std::fs::write(&path, contents).unwrap();
            Self(path)
        }
    }

    impl Drop for TempConfig {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    const FILE_CONFIG: &str = r#"
        model = "model-from-file"
        prompt = "prompt-from-file.json"
        thinking_tokens = 1234
        log_level = "debug"
        trigger_corner = "LL"
        engine = "openai"
        no_draw = true
        no_svg = true
    "#;

    #[test]
    fn config_file_wins_over_unset_cli_args() {
        let file = TempConfig::new("file-wins", FILE_CONFIG);
        let args = Args::parse_from(["ghostwriter"]);
        let config = Config::load_from(&args, &file.0).unwrap();

        assert_eq!(config.model, "model-from-file");
        assert_eq!(config.prompt, "prompt-from-file.json");
        assert_eq!(config.thinking_tokens, 1234);
        assert_eq!(config.log_level, "debug");
        assert_eq!(config.trigger_corner, "LL");
        assert_eq!(config.engine.as_deref(), Some("openai"));
        assert!(config.no_draw);
        assert!(config.no_svg);
    }

    #[test]
    fn explicit_cli_args_win_over_config_file() {
        let file = TempConfig::new("cli-wins", FILE_CONFIG);
        let args = Args::parse_from([
            "ghostwriter",
            "--model",
            "model-from-cli",
            "--trigger-corner",
            "UL",
            "--thinking-tokens",
            "42",
            "--no-keyboard",
        ]);
        let config = Config::load_from(&args, &file.0).unwrap();

        assert_eq!(config.model, "model-from-cli");
        assert_eq!(config.trigger_corner, "UL");
        assert_eq!(config.thinking_tokens, 42);
        assert!(config.no_keyboard);
        // Not passed on the CLI, so still from the file
        assert_eq!(config.prompt, "prompt-from-file.json");
        assert!(config.no_draw);
    }

    #[test]
    fn defaults_apply_without_config_file() {
        let args = Args::parse_from(["ghostwriter"]);
        let missing = std::env::temp_dir().join(format!("ghostwriter-test-{}-missing.toml", std::process::id()));
        let config = Config::load_from(&args, &missing).unwrap();

        let defaults = Config::default();
        assert_eq!(config.model, defaults.model);
        assert_eq!(config.prompt, "general.json");
        assert_eq!(config.thinking_tokens, 5000);
        assert_eq!(config.trigger_corner, "UR");
        assert!(!config.no_draw);
    }
}
