use super::{call_tool, no_tool_calls, notify, post_json, ExecuteFuture, LLMEngine, ModelExecutionStatus, StatusCallback, Tool, ToolCallback};
use crate::cancellation::GhostwriterCancellation;
use crate::util::{option_or_env, option_or_env_fallback, OptionMap};
use anyhow::{Context, Result};
use log::debug;
use serde_json::json;
use serde_json::Value as json;

pub struct OpenAI {
    model: String,
    base_url: String,
    api_key: String,
    tools: Vec<Tool>,
    content: Vec<json>,
}

impl OpenAI {
    fn tool_definition_json(tool: &Tool) -> json {
        json!({
            "type": "function",
            "function": {
                "name": tool.definition["name"],
                "description": tool.definition["description"],
                "parameters": tool.definition["parameters"],
            }
        })
    }

    fn request_body(&self) -> json {
        json!({
            "model": self.model,
            "messages": [{
                "role": "user",
                "content": self.content
            }],
            "tools": self.tools.iter().map(Self::tool_definition_json).collect::<Vec<_>>(),
            "tool_choice": "required",
            "parallel_tool_calls": false
        })
    }

    /// The name and arguments of the first tool call in a response, if there is one.
    fn first_tool_call(response: &json) -> Result<Option<(String, json)>> {
        let function = &response["choices"][0]["message"]["tool_calls"][0]["function"];
        let Some(name) = function["name"].as_str() else {
            return Ok(None);
        };
        let arguments = function["arguments"].as_str().context("Tool call has no arguments")?;
        let arguments = serde_json::from_str(arguments).context("Tool call arguments are not valid JSON")?;
        Ok(Some((name.to_string(), arguments)))
    }
}

impl LLMEngine for OpenAI {
    fn new(options: &OptionMap) -> Result<Self> {
        Ok(Self {
            model: options.get("model").context("No model given")?.to_string(),
            base_url: option_or_env_fallback(options, "base_url", "OPENAI_BASE_URL", "https://api.openai.com"),
            api_key: option_or_env(options, "api_key", "OPENAI_API_KEY")?,
            tools: Vec::new(),
            content: Vec::new(),
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
            "type": "image_url",
            "image_url": {
                "url": format!("data:image/png;base64,{}", base64_image)
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
                .post(format!("{}/v1/chat/completions", self.base_url))
                .header("Authorization", format!("Bearer {}", self.api_key));
            let response = post_json(request, &body, cancellation).await?;
            debug!("Response: {}", response);

            notify(&mut status_callback, ModelExecutionStatus::ProcessingResponse);

            match Self::first_tool_call(&response)? {
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
    fn first_tool_call_parses_the_json_encoded_arguments() {
        let response = json!({"choices": [{"message": {"tool_calls": [
            {"type": "function", "function": {"name": "draw_text", "arguments": "{\"text\": \"4\"}"}}
        ]}}]});

        let (name, arguments) = OpenAI::first_tool_call(&response).unwrap().unwrap();

        assert_eq!(name, "draw_text");
        assert_eq!(arguments, json!({"text": "4"}));
    }

    #[test]
    fn first_tool_call_is_none_for_a_plain_message() {
        let response = json!({"choices": [{"message": {"content": "hello"}}]});

        assert!(OpenAI::first_tool_call(&response).unwrap().is_none());
    }

    #[test]
    fn first_tool_call_rejects_malformed_arguments() {
        let response = json!({"choices": [{"message": {"tool_calls": [
            {"function": {"name": "draw_text", "arguments": "{not json"}}
        ]}}]});

        assert!(OpenAI::first_tool_call(&response).is_err());
    }
}
