pub mod anthropic;
pub mod google;
pub mod openai;

use crate::cancellation::{with_cancellation, GhostwriterCancellation};
use crate::util::OptionMap;
use anyhow::Result;
use serde_json::Value as JsonValue;
use std::future::Future;
use std::pin::Pin;

#[derive(Debug, Clone, PartialEq)]
pub enum ModelExecutionStatus {
    BuildingContext,
    LlmProcessing,
    ProcessingResponse,
    CallingTools,
    Done,
    Error(String),
}

pub type ToolCallback = Box<dyn FnMut(JsonValue) + Send>;
pub type StatusCallback = Box<dyn FnMut(ModelExecutionStatus) + Send>;

/// The future returned by `LLMEngine::execute`, boxed so engines can be used as `dyn LLMEngine`.
pub type ExecuteFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

pub struct Tool {
    pub name: String,
    /// The tool's name, description and parameter schema, as in `prompts/tool_*.json`
    pub definition: JsonValue,
    pub callback: ToolCallback,
}

pub trait LLMEngine: Send {
    fn new(options: &OptionMap) -> Result<Self>
    where
        Self: Sized;
    fn register_tool(&mut self, name: &str, definition: JsonValue, callback: ToolCallback);
    fn add_text_content(&mut self, text: &str);
    fn add_image_content(&mut self, base64_image: &str);
    fn clear_content(&mut self);
    /// Send the content to the model and run the tool it calls in response.
    fn execute<'a>(&'a mut self, cancellation: &'a GhostwriterCancellation, status_callback: Option<StatusCallback>) -> ExecuteFuture<'a>;
}

fn notify(status_callback: &mut Option<StatusCallback>, status: ModelExecutionStatus) {
    if let Some(callback) = status_callback {
        callback(status);
    }
}

/// POST a JSON body and parse the JSON response, giving up early if the run is cancelled.
async fn post_json(request: reqwest::RequestBuilder, body: &JsonValue, cancellation: &GhostwriterCancellation) -> Result<JsonValue> {
    let request_future = async {
        let response = request.json(body).send().await?;
        let status = response.status();
        let body_text = response.text().await?;
        if !status.is_success() {
            anyhow::bail!("API Error: {}: {}", status, body_text.trim());
        }
        Ok(serde_json::from_str(&body_text)?)
    };

    with_cancellation(request_future, cancellation).await
}

/// Run the registered tool `name` with the arguments the model gave for it.
fn call_tool(tools: &mut [Tool], name: &str, arguments: JsonValue, status_callback: &mut Option<StatusCallback>) -> Result<()> {
    notify(status_callback, ModelExecutionStatus::CallingTools);

    match tools.iter_mut().find(|tool| tool.name == name) {
        Some(tool) => {
            (tool.callback)(arguments);
            notify(status_callback, ModelExecutionStatus::Done);
            Ok(())
        }
        None => {
            notify(status_callback, ModelExecutionStatus::Error("No tool registered".to_string()));
            Err(anyhow::anyhow!("No tool registered with name {}", name))
        }
    }
}

/// The error for a response in which the model did not call any tool.
fn no_tool_calls(status_callback: &mut Option<StatusCallback>) -> anyhow::Error {
    let message = "No tool calls found in response";
    notify(status_callback, ModelExecutionStatus::Error(message.to_string()));
    anyhow::anyhow!(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn recording_tool(name: &str, calls: &Arc<Mutex<Vec<JsonValue>>>) -> Tool {
        let calls = Arc::clone(calls);
        Tool {
            name: name.to_string(),
            definition: json!({}),
            callback: Box::new(move |arguments| calls.lock().unwrap().push(arguments)),
        }
    }

    #[test]
    fn call_tool_runs_the_named_tool_and_reports_status() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut tools = vec![recording_tool("draw_text", &calls)];

        let statuses = Arc::new(Mutex::new(Vec::new()));
        let statuses_for_callback = Arc::clone(&statuses);
        let mut status_callback: Option<StatusCallback> = Some(Box::new(move |status| statuses_for_callback.lock().unwrap().push(status)));

        call_tool(&mut tools, "draw_text", json!({"text": "hi"}), &mut status_callback).unwrap();

        assert_eq!(*calls.lock().unwrap(), vec![json!({"text": "hi"})]);
        assert_eq!(*statuses.lock().unwrap(), vec![ModelExecutionStatus::CallingTools, ModelExecutionStatus::Done]);
    }

    #[test]
    fn call_tool_rejects_an_unknown_tool() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut tools = vec![recording_tool("draw_text", &calls)];

        let error = call_tool(&mut tools, "draw_svg", json!({}), &mut None).unwrap_err();

        assert!(error.to_string().contains("draw_svg"));
        assert!(calls.lock().unwrap().is_empty());
    }
}
