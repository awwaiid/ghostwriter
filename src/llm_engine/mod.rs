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

/// Called with a tool call's arguments while they are still arriving: the text so far
/// of the JSON object they will be, each time there is more of it. Called with None if
/// the response ends without the tool being run after all, so that anything begun on
/// the strength of the partial arguments can be wound up.
pub type PartialCallback = Box<dyn FnMut(Option<&str>) + Send>;

/// The future returned by `LLMEngine::execute`, boxed so engines can be used as `dyn LLMEngine`.
pub type ExecuteFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

pub struct Tool {
    pub name: String,
    /// The tool's name, description and parameter schema, as in `prompts/tool_*.json`
    pub definition: JsonValue,
    pub callback: ToolCallback,
    /// Told of the tool's arguments as they arrive, by an engine that streams its response
    pub partial: Option<PartialCallback>,
}

pub trait LLMEngine: Send {
    fn new(options: &OptionMap) -> Result<Self>
    where
        Self: Sized;
    fn register_tool(&mut self, name: &str, definition: JsonValue, callback: ToolCallback);
    /// Ask to be told of a registered tool's arguments while they are still arriving.
    /// An engine that does not stream its responses never calls `callback`.
    fn watch_tool_arguments(&mut self, _name: &str, _callback: PartialCallback) {}
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

/// Splits a stream of server-sent events into the data of each event.
#[derive(Default)]
struct ServerSentEvents {
    /// What has arrived of an event that is not yet complete
    unfinished: Vec<u8>,
}

impl ServerSentEvents {
    /// Take some more of the stream, and return the data of each event it completes.
    /// The stream can be cut anywhere, including in the middle of a character.
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.unfinished.extend_from_slice(bytes);
        let mut events = Vec::new();
        // An event ends at a blank line
        while let Some((end, gap)) = [&b"\n\n"[..], &b"\r\n\r\n"[..]]
            .iter()
            .filter_map(|blank| {
                self.unfinished
                    .windows(blank.len())
                    .position(|window| window == *blank)
                    .map(|at| (at, blank.len()))
            })
            .min()
        {
            let event: Vec<u8> = self.unfinished.drain(..end + gap).collect();
            let text = String::from_utf8_lossy(&event[..end]);
            let data: Vec<&str> = text
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(|data| data.strip_prefix(' ').unwrap_or(data))
                .collect();
            if !data.is_empty() {
                events.push(data.join("\n"));
            }
        }
        events
    }
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
            partial: None,
        }
    }

    #[test]
    fn server_sent_events_are_put_back_together_however_the_stream_is_cut() {
        let stream = "event: ping\ndata: {\"type\": \"ping\"}\n\nevent: content_block_delta\r\ndata: {\"text\": \"caf\u{e9}\"}\r\n\r\n: a comment\n\ndata: one\ndata: two\n\ndata: unfinished";
        let expected = vec!["{\"type\": \"ping\"}", "{\"text\": \"caf\u{e9}\"}", "one\ntwo"];

        // All at once, a byte at a time, and in pieces of every size between
        for size in 1..=stream.len() {
            let mut events = ServerSentEvents::default();
            let data: Vec<String> = stream.as_bytes().chunks(size).flat_map(|piece| events.push(piece)).collect();
            assert_eq!(data, expected, "in pieces of {size}");
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
