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
    device::DeviceModel,
    embedded_assets::load_config,
    keyboard::Keyboard,
    llm_engine::{anthropic::Anthropic, google::Google, openai::OpenAI, LLMEngine},
    pen::Pen,
    simulation::SimulationConfig,
    status::GhostwriterStatus,
    touch::{PenTool, Touch, TriggerCorner},
    util::{setup_uinput, svg_to_alpha_bitmap, svg_to_bitmap, write_bitmap_to_file, OptionMap},
    web_server::start_web_server,
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
    #[arg(long, short, default_value = "claude-sonnet-4-6")]
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

    /// Start web server for configuration UI
    #[arg(long)]
    web_server: bool,

    /// Port for web server (default: 8080)
    #[arg(long, default_value = "8080")]
    web_port: u16,

    /// Enable test/simulation mode for specific device (rm2, rmpp)
    #[arg(long)]
    test_mode: Option<String>,

    /// File containing scripted touch events for simulation (JSON format)
    #[arg(long)]
    test_touch_events_file: Option<String>,

    /// Directory containing test screenshots to cycle through
    #[arg(long)]
    test_screenshot_dir: Option<String>,

    /// Auto-trigger delay in seconds for automated testing
    #[arg(long)]
    test_auto_trigger_delay: Option<u32>,

    /// File to log simulated interactions to
    #[arg(long)]
    test_interaction_log: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    let args = Args::parse();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(args.log_level.as_str()))
        .format_timestamp_millis()
        .init();

    setup_uinput()?;

    ghostwriter(&args).await
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
    // Start on a new line: on the Paper Pro the cursor sits at the end of the page's text.
    keyboard.string_to_keypresses("\n")?;
    keyboard.key_cmd_body()?;
    keyboard.string_to_keypresses(text)?;
    Ok(())
}

fn draw_svg(svg_data: &str, keyboard: &mut Keyboard, pen: &mut Pen, save_bitmap: Option<&String>, no_draw: bool) -> Result<()> {
    info!("Drawing SVG to the screen.");
    keyboard.progress_end()?;
    let scale = 2u32;
    if let Some(save_bitmap) = save_bitmap {
        let bitmap = svg_to_bitmap(svg_data, VIRTUAL_WIDTH * scale, VIRTUAL_HEIGHT * scale)?;
        write_bitmap_to_file(&bitmap, save_bitmap)?;
    }
    if !no_draw {
        // Use alpha-to-pressure rendering for best quality: anti-aliased edges via pen pressure
        let alpha_bitmap = svg_to_alpha_bitmap(svg_data, VIRTUAL_WIDTH * scale, VIRTUAL_HEIGHT * scale)?;
        pen.draw_bitmap_alpha_pressure(&alpha_bitmap, scale)?;
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

async fn ghostwriter(args: &Args) -> Result<()> {
    let mut config = Config::load(args)?;

    // Parse test_mode device model if provided
    if let Some(device_str) = &config.test_mode {
        let device_model = DeviceModel::from_string(device_str)?;
        config.test_device_model = Some(device_model);
        info!("Test mode enabled for device: {}", device_model.name());
    }

    // Handle --save-config option
    if args.save_config {
        config.save()?;
        println!("Configuration saved to {:?}", Config::config_path()?);
        return Ok(());
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
            Touch::new(config.no_draw, trigger_corner)
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
    ));

    let pen = shared!(Pen::new(config.is_test_mode() || config.no_draw));

    let touch = if let Some(shared_touch) = shared_touch {
        shared_touch
    } else {
        Arc::new(TokioRwLock::new(Touch::new(config.no_draw, trigger_corner)))
    };

    // Give keyboard time to initialize
    // sleep(Duration::from_millis(1000)).await;
    // Paper Pro: skip the startup message, it would type into the page's text block.
    if ghostwriter::device::DeviceModel::detect() != ghostwriter::device::DeviceModel::RemarkablePaperPro {
        touch.write().await.tap_middle_bottom().await?;
        lock!(keyboard).progress("Ghostwriter starting...")?;
        sleep(Duration::from_millis(1000)).await;
        lock!(keyboard).progress_end()?;
    }

    // Initialize engine
    let mut engine_options = OptionMap::new();
    engine_options.insert("model".to_string(), config.model.clone());

    let engine_name = determine_engine_name(&config.engine, &config.model)?;
    if let Some(base_url) = &config.engine_base_url {
        engine_options.insert("base_url".to_string(), base_url.clone());
    }
    if let Some(api_key) = &config.engine_api_key {
        engine_options.insert("api_key".to_string(), api_key.clone());
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

                // Update progress to indicate we're processing (not waiting for triggers)
                // let _ = progress_tx.send(ProgressState::TakingScreenshot);

                // Create a new execution cycle for this processing run
                cancellation.new_execution_cycle();

                // Spawn cancel monitor to allow user to interrupt
                // let cancel_handle = {
                //     let touch_clone = Arc::clone(&touch);
                //     let cancellation_clone = Arc::clone(&cancellation);
                //     tokio::spawn(async move {
                //         coordinator::cancel_monitor_task(touch_clone, cancellation_clone).await
                //     })
                // };

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

                // Wait for either processing to complete or user to cancel
                // The cancel_monitor will trigger cancellation which processing_task respects
                let processing_result = processing_handle.await;

                // Cancel the cancel monitor (it may still be waiting)
                cancellation.cancel_execution();
                // let _ = tokio::time::timeout(
                //     Duration::from_millis(100),
                //     cancel_handle
                // ).await;

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

                // Switch to fineliner before drawing, remember original tool for restore
                // Use a fresh Touch instance to avoid deadlock with trigger_task which
                // holds the shared touch RwLock indefinitely while waiting for user trigger
                let previous_tool = if !no_draw && !test_mode {
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async { Touch::new(false, TriggerCorner::UpperRight).select_fineliner().await })
                    })
                    .unwrap_or(PenTool::Unknown)
                } else {
                    PenTool::Unknown
                };

                let mut keyboard = lock!(keyboard_clone);
                let mut pen = lock!(pen_clone);
                if let Err(e) = draw_svg(svg_data, &mut keyboard, &mut pen, save_bitmap.as_ref(), no_draw) {
                    log::error!("Failed to draw SVG: {}", e);
                }
                drop(keyboard);
                drop(pen);

                // Restore the original tool after drawing
                if !no_draw && !test_mode && previous_tool != PenTool::Unknown {
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async { Touch::new(false, TriggerCorner::UpperRight).restore_tool(previous_tool).await })
                    })
                    .ok();
                }
            }),
        );
    }

    Ok(())
}
