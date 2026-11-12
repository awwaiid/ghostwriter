use super::{call_tool, no_tool_calls, notify, post_json, ExecuteFuture, LLMEngine, ModelExecutionStatus, StatusCallback, Tool, ToolCallback};
use crate::cancellation::GhostwriterCancellation;
use crate::util::{option_or_env, option_or_env_fallback, OptionMap};
use anyhow::{Context, Result};
use log::debug;
use serde_json::json;
use serde_json::Value as json;

pub struct Google {
    model: String,
    base_url: String,
    api_key: String,
    tools: Vec<Tool>,
    content: Vec<json>,
}

impl Google {
    fn tool_definition_json(tool: &Tool) -> json {
        json!({
            "name": tool.definition["name"],
            "description": tool.definition["description"],
            "parameters": tool.definition["parameters"],
        })
    }

    fn request_body(&self) -> json {
        json!({
            "contents": [{
                "role": "user",
                "parts": self.content
            }],
            "tools": [{ "function_declarations": self.tools.iter().map(Self::tool_definition_json).collect::<Vec<_>>() }],
            "tool_config": {
                "function_calling_config": {
                    "mode": "ANY"
                }
            }
        })
    }

    /// The name and arguments of the first function call in a response, if there is one.
    fn first_function_call(response: &json) -> Option<(String, json)> {
        let parts = response["candidates"][0]["content"]["parts"].as_array()?;
        let function_call = parts.iter().find_map(|part| part.get("functionCall"))?;
        Some((function_call["name"].as_str()?.to_string(), function_call["args"].clone()))
    }
}

impl LLMEngine for Google {
    fn new(options: &OptionMap) -> Result<Self> {
        Ok(Self {
            model: options.get("model").context("No model given")?.to_string(),
            base_url: option_or_env_fallback(options, "base_url", "GOOGLE_BASE_URL", "https://generativelanguage.googleapis.com"),
            api_key: option_or_env(options, "api_key", "GOOGLE_API_KEY")?,
            tools: Vec::new(),
            content: Vec::new(),
        })
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
            "text": text,
        }));
    }

    fn add_image_content(&mut self, base64_image: &str) {
        self.content.push(json!({
            "inline_data": {
                "mime_type": "image/png",
                "data": base64_image,
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

            let request = reqwest::Client::new().post(format!("{}/v1beta/models/{}:generateContent?key={}", self.base_url, self.model, self.api_key));
            let response = post_json(request, &body, cancellation).await?;
            debug!("Response: {}", response);

            notify(&mut status_callback, ModelExecutionStatus::ProcessingResponse);

            match Self::first_function_call(&response) {
                Some((function_name, arguments)) => call_tool(&mut self.tools, &function_name, arguments, &mut status_callback),
                None => Err(no_tool_calls(&mut status_callback)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_function_call_skips_text_parts() {
        let response = json!({"candidates": [{"content": {"parts": [
            {"text": "Let me draw that."},
            {"functionCall": {"name": "draw_svg", "args": {"svg": "<svg/>"}}}
        ]}}]});

        let (name, arguments) = Google::first_function_call(&response).unwrap();

        assert_eq!(name, "draw_svg");
        assert_eq!(arguments, json!({"svg": "<svg/>"}));
    }

    #[test]
    fn first_function_call_is_none_without_a_function_call() {
        let response = json!({"candidates": [{"content": {"parts": [{"text": "hello"}]}}]});

        assert!(Google::first_function_call(&response).is_none());
    }
}
