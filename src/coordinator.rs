use anyhow::Result;
use base64::prelude::*;
use log::{debug, info};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch, Mutex as TokioMutex};
use tokio::time::{sleep, Duration};

use crate::cancellation::GhostwriterCancellation;
use crate::config::Config;
use crate::device::DeviceModel;
use crate::embedded_assets::load_config;
use crate::keyboard::Keyboard;
use crate::llm_engine::{LLMEngine, ModelExecutionStatus};
use crate::screenshot::Screenshot;
use crate::segmenter::ImageAnalyzer;
use crate::simulation::SimulationConfig;
use crate::touch::{Touch, TriggerCorner};

/// Events that can trigger AI processing
#[derive(Debug, Clone)]
pub enum TriggerEvent {
    /// User touched the trigger corner
    UserTouch,
    /// Trigger via web API (for testing/simulation)
    WebTrigger,
}

/// Progress states during AI processing
/// Uses ModelExecutionStatus for LLM operations, plus additional states for the full workflow
#[derive(Debug, Clone, PartialEq)]
pub enum ProgressState {
    /// No processing happening
    Idle,
    /// Waiting for user trigger
    WaitingForTrigger,
    /// Taking screenshot
    TakingScreenshot,
    /// LLM execution state
    LlmState(ModelExecutionStatus),
    /// Processing completed successfully
    Done,
}

/// Message from coordinator to processing task
#[derive(Debug)]
pub struct ProcessingRequest {
    /// The trigger event that started this
    pub trigger: TriggerEvent,
}

/// Communication channels for the coordinator
pub struct CoordinatorChannels {
    /// Send trigger events to coordinator
    pub trigger_tx: mpsc::Sender<TriggerEvent>,
    /// Receive trigger events in coordinator
    pub trigger_rx: mpsc::Receiver<TriggerEvent>,

    /// Broadcast progress state updates
    pub progress_tx: watch::Sender<ProgressState>,
    /// Receive progress state updates
    pub progress_rx: watch::Receiver<ProgressState>,
}

impl CoordinatorChannels {
    pub fn new() -> Self {
        let (trigger_tx, trigger_rx) = mpsc::channel(10);
        let (progress_tx, progress_rx) = watch::channel(ProgressState::Idle);

        Self {
            trigger_tx,
            trigger_rx,
            progress_tx,
            progress_rx,
        }
    }
}

impl Default for CoordinatorChannels {
    fn default() -> Self {
        Self::new()
    }
}

/// Task that waits for triggers and notifies the coordinator
pub async fn trigger_task(
    touch: Arc<tokio::sync::RwLock<Touch>>,
    trigger_tx: mpsc::Sender<TriggerEvent>,
    cancellation: Arc<GhostwriterCancellation>,
    no_trigger: bool,
) -> Result<()> {
    info!("Trigger task starting");

    loop {
        debug!("Trigger loop looping");

        if no_trigger {
            debug!("No-trigger mode: auto-triggering");
            if trigger_tx.send(TriggerEvent::UserTouch).await.is_err() {
                info!("Trigger receiver dropped, exiting trigger task");
                break;
            }
            // In no-trigger mode, wait a bit before next auto-trigger or check for cancellation
            tokio::select! {
                _ = sleep(Duration::from_millis(100)) => {
                    if cancellation.should_cancel_main() {
                        info!("Trigger task: cancelled in no-trigger mode");
                        break;
                    }
                }
                _ = async {
                    while !cancellation.should_cancel_main() {
                        sleep(Duration::from_millis(10)).await;
                    }
                } => {
                    info!("Trigger task: cancelled in no-trigger mode");
                    break;
                }
            }
            continue;
        }

        info!("Trigger task: waiting for touch trigger...");

        debug!("Trigger task: about to acquire touch write lock");
        let mut touch_guard = touch.write().await;
        debug!("Trigger task: acquired touch write lock, calling wait_for_trigger");

        match touch_guard.wait_for_trigger(&cancellation).await {
            Ok(()) => {
                debug!("Trigger task: wait_for_trigger returned Ok, touch detected");
                info!("Trigger task: touch detected");

                // Drop the lock before sending the event so processing_task can acquire it
                drop(touch_guard);
                debug!("Trigger task: dropped touch write lock");

                if trigger_tx.send(TriggerEvent::UserTouch).await.is_err() {
                    info!("Trigger receiver dropped, exiting trigger task");
                    break;
                }
                debug!("Trigger task: sent trigger event, continuing loop");

                // Give processing_task a moment to acquire the lock before we loop back
                sleep(Duration::from_millis(50)).await;
            }
            Err(e) => {
                debug!("Trigger task: wait_for_trigger returned Err: {}", e);
                if e.to_string().contains("cancelled") {
                    info!("Trigger task: cancelled (likely config change)");
                    return Ok(()); // Clean exit for restart
                } else {
                    info!("Trigger task: error waiting for trigger: {}", e);
                    return Err(e);
                }
            }
        }
    }

    debug!("Escaped from trigger task loop");

    Ok(())
}

/// Task that monitors for cancel touch during processing
pub async fn cancel_monitor_task(touch: Arc<tokio::sync::RwLock<Touch>>, cancellation: Arc<GhostwriterCancellation>) -> Result<()> {
    info!("Cancel monitor task: starting");

    // Wait for any touch to cancel
    match touch.write().await.wait_for_trigger(&cancellation).await {
        Ok(()) => {
            info!("Cancel monitor task: touch detected, cancelling processing");
            cancellation.cancel_execution();
            Ok(())
        }
        Err(e) => {
            if e.to_string().contains("cancelled") {
                info!("Cancel monitor task: processing completed before touch");
                Ok(())
            } else {
                info!("Cancel monitor task: error: {}", e);
                Err(e)
            }
        }
    }
}

/// Task that displays progress updates on the keyboard
pub async fn progress_task(
    keyboard: Arc<Mutex<Keyboard>>,
    mut progress_rx: watch::Receiver<ProgressState>,
    cancellation: Arc<GhostwriterCancellation>,
) -> Result<()> {
    info!("Progress task starting");

    let mut current_state = ProgressState::Idle;
    let cancel_token = cancellation.execution_token();

    loop {
        tokio::select! {
            // Check for cancellation
            _ = cancel_token.cancelled() => {
                info!("Progress task cancelled");
                // Clear any progress display
                if let Ok(mut kb) = keyboard.lock() {
                    let _ = kb.progress_end();
                }
                return Ok(());
            }

            // Watch for progress updates
            result = progress_rx.changed() => {
                if result.is_err() {
                    info!("Progress sender dropped, exiting progress task");
                    break;
                }

                let new_state = progress_rx.borrow().clone();
                if new_state != current_state {
                    current_state = new_state.clone();

                    match &current_state {
                        ProgressState::Idle => {
                            info!("Progress: Idle");
                            if let Ok(mut kb) = keyboard.lock() {
                                let _ = kb.progress_end();
                            }
                        }
                        ProgressState::WaitingForTrigger => {
                            info!("Progress: Waiting for trigger");
                        }
                        ProgressState::TakingScreenshot => {
                            info!("Progress: Taking screenshot...");
                        }
                        ProgressState::LlmState(ModelExecutionStatus::BuildingContext) => {
                            info!("Progress: Building context...");
                            if let Ok(mut kb) = keyboard.lock() {
                                let _ = kb.progress("Thinking");
                            }
                        }
                        ProgressState::LlmState(ModelExecutionStatus::LlmProcessing) => {
                            info!("Progress: Thinking...");
                        }
                        ProgressState::LlmState(ModelExecutionStatus::ProcessingResponse) => {
                            info!("Progress: Processing response...");
                        }
                        ProgressState::LlmState(ModelExecutionStatus::CallingTools) => {
                            info!("Progress: Executing tools...");
                            if let Ok(mut kb) = keyboard.lock() {
                                let _ = kb.progress_end();
                            }
                        }
                        ProgressState::LlmState(ModelExecutionStatus::Done) => {
                            debug!("Progress: LLM Done");
                        }
                        ProgressState::LlmState(ModelExecutionStatus::Error(msg)) => {
                            debug!("Progress: Error - {}", msg);
                        }
                        ProgressState::Done => {
                            debug!("Progress: Done");
                        }
                    }
                }
            }

            // Add dots for thinking state
            _ = sleep(Duration::from_millis(500)) => {
                if matches!(current_state, ProgressState::LlmState(ModelExecutionStatus::LlmProcessing)) {
                    if let Ok(mut kb) = keyboard.lock() {
                        let _ = kb.progress(".");
                    }
                }
            }
        }
    }

    Ok(())
}

/// Task that processes a trigger: screenshot → LLM → tool execution
pub async fn processing_task(
    config: Config,
    engine: Arc<TokioMutex<Box<dyn LLMEngine>>>,
    progress_tx: watch::Sender<ProgressState>,
    cancellation: Arc<GhostwriterCancellation>,
    touch: Arc<tokio::sync::RwLock<Touch>>,
) -> Result<()> {
    info!("Processing task: starting");

    // Update progress: taking screenshot
    info!("Setting ProgressState::TakingScreenshot");
    let _ = progress_tx.send(ProgressState::TakingScreenshot);
    tokio::time::sleep(Duration::from_millis(10)).await; // Give progress_task time

    // Take screenshot
    let screenshot_path = config.save_screenshot.clone();
    let base64_image = if let Some(input_png) = &config.input_png {
        BASE64_STANDARD.encode(std::fs::read(input_png)?)
    } else {
        let mut screenshot = if config.is_test_mode() {
            let simulation_config = SimulationConfig::from_config(&config);
            Screenshot::new_simulated(simulation_config)?
        } else {
            Screenshot::new()?
        };
        screenshot.take_screenshot()?;
        if let Some(save_screenshot) = &config.save_screenshot {
            info!("Saving screenshot to {}", save_screenshot);
            screenshot.save_image(save_screenshot)?;
        }
        screenshot.base64()?
    };

    if config.no_submit {
        info!("Skipping LLM submission (no_submit mode)");
        let _ = progress_tx.send(ProgressState::Done);
        return Ok(());
    }

    // Position the cursor for text input (before showing "Thinking").
    // Paper Pro: open the page's text block through the Text tool, closed again below.
    let paper_pro = !config.no_draw && !config.is_test_mode() && DeviceModel::detect() == DeviceModel::RemarkablePaperPro;
    let mut text_edit_palette: Option<bool> = None;
    // A fresh Touch: the trigger task takes the shared one again while we wait for the LLM.
    if paper_pro {
        match Touch::new(false, TriggerCorner::UpperRight).open_text_edit().await {
            Ok(palette_was_open) => text_edit_palette = Some(palette_was_open),
            Err(e) => info!("Failed to open text editing: {}", e),
        }
    } else if let Err(e) = touch.write().await.tap_middle_bottom().await {
        info!("Failed to tap middle bottom: {}", e);
    }

    // Update progress: building context
    let _ = progress_tx.send(ProgressState::LlmState(ModelExecutionStatus::BuildingContext));
    tokio::time::sleep(Duration::from_millis(10)).await; // Give progress_task time

    // Apply segmentation if requested
    let segmentation_description = if config.apply_segmentation {
        let image_path = config
            .input_png
            .as_ref()
            .or(screenshot_path.as_ref())
            .ok_or_else(|| anyhow::anyhow!("Segmentation requires either input_png or save_screenshot"))?;

        info!("Applying segmentation to {}", image_path);
        let analyzer = ImageAnalyzer::new(0.001, 10); // min_region_size=0.1%, max_regions=10
        match analyzer.analyze_image(image_path) {
            Ok(result) => {
                let description = analyzer.generate_description(&result);
                info!("Segmentation found {} regions", result.regions.len());
                Some(description)
            }
            Err(e) => {
                info!("Segmentation failed: {}, continuing without it", e);
                None
            }
        }
    } else {
        None
    };

    // Load prompt
    let prompt_general_raw = load_config(&config.prompt);
    let prompt_general_json = serde_json::from_str::<serde_json::Value>(prompt_general_raw.as_str())?;
    let mut prompt = prompt_general_json["prompt"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Prompt file '{}' missing required 'prompt' field", config.prompt))?
        .to_string();

    // Add segmentation to prompt if available
    if let Some(seg_desc) = segmentation_description {
        prompt.push_str("\n\nImage Analysis:\n");
        prompt.push_str(&seg_desc);
    }

    // Prepare engine
    let mut engine_guard = engine.lock().await;
    engine_guard.clear_content();
    engine_guard.add_image_content(&base64_image);
    engine_guard.add_text_content(&prompt);

    // Create status callback that wraps model execution status in LlmState
    let progress_tx_clone = progress_tx.clone();
    let status_callback = Some(Box::new(move |status: ModelExecutionStatus| {
        let _ = progress_tx_clone.send(ProgressState::LlmState(status));
    }) as Box<dyn FnMut(ModelExecutionStatus) + Send>);

    // Execute LLM with proper error handling
    info!("Processing task: calling LLM");
    let execution_result = engine_guard.execute(&cancellation, status_callback).await;

    if let Some(palette_was_open) = text_edit_palette {
        sleep(Duration::from_millis(300)).await;
        if let Err(e) = Touch::new(false, TriggerCorner::UpperRight).close_text_edit(palette_was_open).await {
            info!("Failed to close text editing: {}", e);
        }
    }

    // Write model output if configured
    if let Some(model_output_file) = &config.model_output_file {
        info!("Would write model output to {}", model_output_file);
        // Note: The actual model output would need to be captured from the engine
        // This is a placeholder - the LLMEngine trait would need to expose the raw response
    }

    // Handle execution result
    match execution_result {
        Ok(_) => {
            let _ = progress_tx.send(ProgressState::Done);
            info!("Processing task: completed successfully");
            Ok(())
        }
        Err(e) => {
            let error_msg = e.to_string();
            info!("Processing task: LLM error: {}", error_msg);

            // Only send error state if not already cancelled
            if !error_msg.contains("cancelled") && !error_msg.contains("canceled") {
                let _ = progress_tx.send(ProgressState::LlmState(ModelExecutionStatus::Error(error_msg.clone())));
                // Keep error visible for a moment
                sleep(Duration::from_secs(2)).await;
            }

            // Return to idle state
            let _ = progress_tx.send(ProgressState::Idle);
            Err(e)
        }
    }
}
