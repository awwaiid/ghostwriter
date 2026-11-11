use super::{call_tool, no_tool_calls, notify, post_json, ExecuteFuture, LLMEngine, ModelExecutionStatus, StatusCallback, Tool, ToolCallback};
use crate::cancellation::GhostwriterCancellation;
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
        })
    }

    fn register_tool(&mut self, name: &str, definition: json, callback: ToolCallback) {
        self.tools.push(Tool {
            name: name.to_string(),
            definition,
            callback,
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
    fn request_body_adds_thinking_and_web_search_when_enabled() {
        let body = engine(&[("thinking", "true"), ("thinking_tokens", "1234"), ("web_search", "true")]).request_body();

        assert_eq!(body["thinking"], json!({"type": "enabled", "budget_tokens": 1234}));
        assert_eq!(body["tools"][0]["name"], "web_search");
    }
}
