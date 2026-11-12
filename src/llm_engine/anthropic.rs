use super::{
    call_tool, no_tool_calls, notify, post_json, ExecuteFuture, LLMEngine, ModelExecutionStatus, PartialCallback, ServerSentEvents, StatusCallback, Tool,
    ToolCallback,
};
use crate::cancellation::{with_cancellation, GhostwriterCancellation};
use crate::util::{option_or_env, option_or_env_fallback, OptionMap};
use anyhow::{Context, Result};
use log::debug;
use serde_json::json;
use serde_json::Value as json;

pub struct Anthropic {
    model: String,
    api_key: String,
    base_url: String,
    tools: Vec<Tool>,
    content: Vec<json>,
    web_search: bool,
    thinking: bool,
    thinking_tokens: u32,
    /// Take the response as a stream, so that a tool can be told of its arguments as they arrive
    stream: bool,
}

/// A tool call as it arrives in a streamed response.
struct StreamedToolCall {
    name: String,
    /// The text so far of the JSON object that its arguments will be
    arguments: String,
    /// Whether more of them has arrived since the tool was last told
    grown: bool,
}

impl Anthropic {
    fn tool_definition_json(tool: &Tool) -> json {
        json!({
            "name": tool.definition["name"],
            "description": tool.definition["description"],
            "input_schema": tool.definition["parameters"],
        })
    }

    fn request_body(&self) -> json {
        let mut tool_definitions = self.tools.iter().map(Self::tool_definition_json).collect::<Vec<_>>();

        if self.web_search {
            tool_definitions.push(json!({
                "type": "web_search_20250305",
                "name": "web_search",
                "max_uses": 5
            }));
        }

        let mut body = json!({
            "model": self.model,
            "max_tokens": 10000,
            "messages": [{
                "role": "user",
                "content": self.content
            }],
            "tools": tool_definitions,
            "tool_choice": {
                "type": "auto",
            }
        });

        if self.thinking {
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": self.thinking_tokens
            });
        }

        body
    }

    /// The request for a streamed response. The tools' arguments are asked for as they
    /// are written rather than held back until each is complete and checked, which
    /// means checking them here once they are.
    fn streaming_request_body(&self) -> json {
        let mut body = self.request_body();
        body["stream"] = json!(true);
        for tool in body["tools"].as_array_mut().into_iter().flatten() {
            // Our own tools have a schema; the ones Anthropic runs do not take the setting
            if tool.get("input_schema").is_some() {
                tool["eager_input_streaming"] = json!(true);
            }
        }
        body
    }

    /// Read a streamed response, telling tools of their arguments as they arrive.
    /// Returns the name and arguments of the first tool the model called, if it called one.
    async fn read_stream(
        &mut self,
        request: reqwest::RequestBuilder,
        body: &json,
        status_callback: &mut Option<StatusCallback>,
    ) -> Result<Option<(String, json)>> {
        let mut response = request.json(body).send().await?;
        if !response.status().is_success() {
            let status = response.status();
            anyhow::bail!("API Error: {}: {}", status, response.text().await?.trim());
        }

        let mut events = ServerSentEvents::default();
        // The tool calls in the response, by the place of each among its content blocks
        let mut calls: Vec<(u64, StreamedToolCall)> = Vec::new();
        let mut stop_reason = String::new();
        let mut finished = false;
        while let Some(piece) = response.chunk().await? {
            for data in events.push(&piece) {
                let Ok(event) = serde_json::from_str::<json>(&data) else {
                    debug!("Skipping an event that is not JSON: {}", data);
                    continue;
                };
                match event["type"].as_str().unwrap_or("") {
                    "content_block_start" if event["content_block"]["type"] == "tool_use" => {
                        if calls.is_empty() {
                            notify(status_callback, ModelExecutionStatus::ProcessingResponse);
                        }
                        let call = StreamedToolCall {
                            name: event["content_block"]["name"].as_str().unwrap_or_default().to_string(),
                            arguments: String::new(),
                            grown: false,
                        };
                        calls.push((event["index"].as_u64().unwrap_or_default(), call));
                    }
                    "content_block_delta" if event["delta"]["type"] == "input_json_delta" => {
                        if let Some((_, call)) = calls.iter_mut().find(|(index, _)| event["index"].as_u64() == Some(*index)) {
                            call.arguments.push_str(event["delta"]["partial_json"].as_str().unwrap_or_default());
                            call.grown = true;
                        }
                    }
                    "message_delta" => {
                        if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                            stop_reason = reason.to_string();
                        }
                    }
                    "message_stop" => finished = true,
                    "error" => anyhow::bail!("API Error: {}", event["error"]),
                    _ => {}
                }
            }

            // Once for each piece of the stream, however many events it held: a tool that
            // takes its time over what it is told finds the next piece all the longer
            if let Some((_, call)) = calls.first_mut().filter(|(_, call)| call.grown) {
                call.grown = false;
                if let Some(partial) = self.tools.iter_mut().find(|tool| tool.name == call.name).and_then(|tool| tool.partial.as_mut()) {
                    partial(Some(&call.arguments));
                }
            }
        }
        debug!("The response stopped for the reason {:?}", stop_reason);
        anyhow::ensure!(finished, "The response was cut off before it was complete");

        let Some((_, call)) = calls.into_iter().next() else {
            return Ok(None);
        };
        // Arguments streamed as they are written have not been checked: they may have
        // been cut short, or not be JSON at all
        anyhow::ensure!(
            stop_reason != "max_tokens" && stop_reason != "refusal",
            "The model's call to {} was cut short ({})",
            call.name,
            stop_reason
        );
        let arguments = if call.arguments.trim().is_empty() { "{}" } else { call.arguments.as_str() };
        let arguments = serde_json::from_str(arguments).with_context(|| format!("The model's call to {} was not valid JSON", call.name))?;
        Ok(Some((call.name, arguments)))
    }

    /// Send the content to the model, take its response as a stream, and run the tool it calls.
    async fn execute_streaming(&mut self, cancellation: &GhostwriterCancellation, status_callback: &mut Option<StatusCallback>) -> Result<()> {
        let body = self.streaming_request_body();
        debug!("Request: {}", body);
        notify(status_callback, ModelExecutionStatus::LlmProcessing);

        let request = reqwest::Client::new()
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", self.api_key.as_str())
            .header("anthropic-version", "2023-06-01");
        let called = with_cancellation(self.read_stream(request, &body, status_callback), cancellation).await;

        match called {
            Ok(Some((name, arguments))) => call_tool(&mut self.tools, &name, arguments, status_callback),
            ended_otherwise => {
                // Whatever a tool began on the strength of its arguments so far, it is not going to be run
                for partial in self.tools.iter_mut().filter_map(|tool| tool.partial.as_mut()) {
                    partial(None);
                }
                ended_otherwise?;
                Err(no_tool_calls(status_callback))
            }
        }
    }
}

impl LLMEngine for Anthropic {
    fn new(options: &OptionMap) -> Result<Self> {
        Ok(Self {
            model: options.get("model").context("No model given")?.to_string(),
            api_key: option_or_env(options, "api_key", "ANTHROPIC_API_KEY")?,
            base_url: option_or_env_fallback(options, "base_url", "ANTHROPIC_BASE_URL", "https://api.anthropic.com"),
            tools: Vec::new(),
            content: Vec::new(),
            web_search: options.get("web_search").is_some_and(|v| v == "true"),
            thinking: options.get("thinking").is_some_and(|v| v == "true"),
            thinking_tokens: options.get("thinking_tokens").and_then(|v| v.parse::<u32>().ok()).unwrap_or(5000),
            stream: options.get("stream").is_some_and(|v| v == "true"),
        })
    }

    fn watch_tool_arguments(&mut self, name: &str, callback: PartialCallback) {
        if let Some(tool) = self.tools.iter_mut().find(|tool| tool.name == name) {
            tool.partial = Some(callback);
        }
    }

    fn register_tool(&mut self, name: &str, definition: json, callback: ToolCallback) {
        self.tools.push(Tool {
            name: name.to_string(),
            definition,
            callback,
            partial: None,
        });
    }

    fn add_text_content(&mut self, text: &str) {
        self.content.push(json!({
            "type": "text",
            "text": text,
        }));
    }

    fn add_image_content(&mut self, base64_image: &str) {
        self.content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": base64_image
            }
        }));
    }

    fn clear_content(&mut self) {
        self.content.clear();
    }

    fn execute<'a>(&'a mut self, cancellation: &'a GhostwriterCancellation, mut status_callback: Option<StatusCallback>) -> ExecuteFuture<'a> {
        Box::pin(async move {
            if self.stream {
                return self.execute_streaming(cancellation, &mut status_callback).await;
            }

            let body = self.request_body();
            debug!("Request: {}", body);

            notify(&mut status_callback, ModelExecutionStatus::LlmProcessing);

            let request = reqwest::Client::new()
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", self.api_key.as_str())
                .header("anthropic-version", "2023-06-01");
            let response = post_json(request, &body, cancellation).await?;
            debug!("Response: {}", response);

            notify(&mut status_callback, ModelExecutionStatus::ProcessingResponse);

            // Run the first tool the model calls
            for content_item in response["content"].as_array().into_iter().flatten() {
                match content_item["type"].as_str().unwrap_or("") {
                    "tool_use" => {
                        let function_name = content_item["name"].as_str().unwrap_or_default();
                        return call_tool(&mut self.tools, function_name, content_item["input"].clone(), &mut status_callback);
                    }
                    "thinking" => debug!("Thinking: {}", content_item["thinking"]),
                    "text" => debug!("Text: {}", content_item["text"]),
                    other => debug!("Unknown content type: {}", other),
                }
            }

            Err(no_tool_calls(&mut status_callback))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(extra_options: &[(&str, &str)]) -> Anthropic {
        let mut options = OptionMap::new();
        options.insert("model".to_string(), "test-model".to_string());
        options.insert("api_key".to_string(), "test-key".to_string());
        for (key, value) in extra_options {
            options.insert(key.to_string(), value.to_string());
        }
        Anthropic::new(&options).unwrap()
    }

    #[test]
    fn request_body_carries_content_and_tools() {
        let mut engine = engine(&[]);
        engine.register_tool(
            "draw_text",
            json!({"name": "draw_text", "description": "Type text", "parameters": {"type": "object"}}),
            Box::new(|_| {}),
        );
        engine.add_image_content("aW1hZ2U=");
        engine.add_text_content("What is this?");

        let body = engine.request_body();

        assert_eq!(body["model"], "test-model");
        assert_eq!(body["messages"][0]["content"][0]["source"]["data"], "aW1hZ2U=");
        assert_eq!(body["messages"][0]["content"][1]["text"], "What is this?");
        assert_eq!(
            body["tools"],
            json!([{"name": "draw_text", "description": "Type text", "input_schema": {"type": "object"}}])
        );
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn a_streaming_request_asks_for_our_tools_arguments_as_they_are_written() {
        let mut engine = engine(&[("stream", "true"), ("web_search", "true")]);
        engine.register_tool(
            "draw_svg",
            json!({"name": "draw_svg", "description": "Draw", "parameters": {"type": "object"}}),
            Box::new(|_| {}),
        );

        let body = engine.streaming_request_body();

        assert_eq!(body["stream"], true);
        assert_eq!(body["tools"][0]["eager_input_streaming"], true);
        // The web search tool is run by Anthropic, and does not take the setting
        assert_eq!(body["tools"][1]["name"], "web_search");
        assert!(body["tools"][1].get("eager_input_streaming").is_none());
        // And an ordinary request is as it was
        assert!(engine.request_body().get("stream").is_none());
        assert!(engine.request_body()["tools"][0].get("eager_input_streaming").is_none());
    }

    #[test]
    fn request_body_adds_thinking_and_web_search_when_enabled() {
        let body = engine(&[("thinking", "true"), ("thinking_tokens", "1234"), ("web_search", "true")]).request_body();

        assert_eq!(body["thinking"], json!({"type": "enabled", "budget_tokens": 1234}));
        assert_eq!(body["tools"][0]["name"], "web_search");
    }
}
