//! LLM client — direct HTTP calls to OpenRouter-compatible APIs.
//!
//! No crossbeam bridge, no sync scheduler. Just reqwest + tokio.

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::config::ProviderConfig;

/// A lightweight LLM client for chat completions.
#[derive(Clone)]
pub struct LlmClient {
  http: Client,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
  model: String,
  messages: Vec<ChatMessage>,
  #[serde(skip_serializing_if = "Option::is_none")]
  max_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
  pub role: String,
  pub content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
  choices: Vec<ChatChoice>,
  usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
  message: ChatMessage,
}

#[derive(Debug, Deserialize)]
pub struct Usage {
  pub prompt_tokens: u64,
  pub completion_tokens: u64,
  pub total_tokens: u64,
}

/// Result of an LLM call.
#[derive(Debug)]
pub struct LlmResponse {
  pub content: String,
  pub usage: Option<Usage>,
}

impl LlmClient {
  pub fn new() -> Self {
    Self {
      http: Client::new(),
    }
  }

  /// Send a chat completion request.
  pub async fn chat(
    &self,
    base_url: &str,
    api_key: &str,
    model: &str,
    system_prompt: Option<&str>,
    user_message: &str,
  ) -> Result<LlmResponse, LlmError> {
    let mut messages = Vec::new();

    if let Some(sys) = system_prompt {
      messages.push(ChatMessage {
        role: "system".into(),
        content: sys.into(),
      });
    }

    messages.push(ChatMessage {
      role: "user".into(),
      content: user_message.into(),
    });

    let request = ChatRequest {
      model: model.into(),
      messages,
      max_tokens: None,
    };

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let response = self
      .http
      .post(&url)
      .header("Authorization", format!("Bearer {}", api_key))
      .header("Content-Type", "application/json")
      .json(&request)
      .send()
      .await
      .map_err(|e| LlmError::Network(e.to_string()))?;

    if !response.status().is_success() {
      let status = response.status();
      let body = response.text().await.unwrap_or_default();
      return Err(LlmError::Api(format!("{}: {}", status, body)));
    }

    let chat_response: ChatResponse = response
      .json()
      .await
      .map_err(|e| LlmError::Parse(e.to_string()))?;

    let content = chat_response
      .choices
      .first()
      .map(|c| c.message.content.clone())
      .unwrap_or_default();

    Ok(LlmResponse {
      content,
      usage: chat_response.usage,
    })
  }
}

/// Resolve base URL for a provider.
pub fn resolve_base_url(provider_config: Option<&ProviderConfig>, provider_name: &str) -> String {
  if let Some(config) = provider_config {
    if let Some(ref url) = config.base_url {
      return url.clone();
    }
  }
  // Default base URLs by provider name
  match provider_name {
    "openrouter" => "https://openrouter.ai/api/v1".into(),
    "openai" => "https://api.openai.com/v1".into(),
    "anthropic" => "https://api.anthropic.com/v1".into(),
    _ => format!("https://{}.api.example.com/v1", provider_name),
  }
}

#[derive(Debug)]
pub enum LlmError {
  Network(String),
  Api(String),
  Parse(String),
  NoApiKey(String),
}

impl std::fmt::Display for LlmError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      LlmError::Network(e) => write!(f, "network error: {}", e),
      LlmError::Api(e) => write!(f, "API error: {}", e),
      LlmError::Parse(e) => write!(f, "parse error: {}", e),
      LlmError::NoApiKey(provider) => write!(f, "no API key for provider '{}'", provider),
    }
  }
}

impl std::error::Error for LlmError {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_resolve_base_url_default_openrouter() {
    let url = resolve_base_url(None, "openrouter");
    assert_eq!(url, "https://openrouter.ai/api/v1");
  }

  #[test]
  fn test_resolve_base_url_custom() {
    let config = ProviderConfig {
      api_key: None,
      base_url: Some("https://custom.api.com/v1".into()),
    };
    let url = resolve_base_url(Some(&config), "openrouter");
    assert_eq!(url, "https://custom.api.com/v1");
  }

  #[test]
  fn test_llm_client_creates() {
    let _client = LlmClient::new();
  }
}
