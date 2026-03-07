//! Provider creation — shared between daemon (validation) and worker (instantiation).

use std::sync::Arc;
use zeptoclaw::providers::LLMProvider;

use crate::config::{AgentConfig, Config};

/// Create a zeptoclaw LLMProvider from zeptoPM config.
pub fn create_provider(
  agent_config: &AgentConfig,
  config: &Config,
) -> Result<Arc<dyn LLMProvider>, String> {
  let provider_config = config.providers.get(&agent_config.provider);
  let api_key = provider_config
    .and_then(|p| p.resolve_api_key())
    .ok_or_else(|| format!("no API key for provider '{}'", agent_config.provider))?;

  let provider_name = &agent_config.provider;
  let base_url = provider_config.and_then(|p| p.base_url.clone());

  let provider: Arc<dyn LLMProvider> = match provider_name.as_str() {
    "anthropic" | "claude" => Arc::new(zeptoclaw::ClaudeProvider::new(&api_key)),
    "openai" => match base_url {
      Some(url) => Arc::new(zeptoclaw::OpenAIProvider::with_base_url(&api_key, &url)),
      None => Arc::new(zeptoclaw::OpenAIProvider::new(&api_key)),
    },
    // OpenRouter, Groq, Together, etc. are OpenAI-compatible
    _ => {
      let url = base_url.unwrap_or_else(|| match provider_name.as_str() {
        "openrouter" => "https://openrouter.ai/api/v1".into(),
        "groq" => "https://api.groq.com/openai/v1".into(),
        "together" => "https://api.together.xyz/v1".into(),
        _ => format!("https://{}.api.example.com/v1", provider_name),
      });
      Arc::new(zeptoclaw::OpenAIProvider::with_base_url(&api_key, &url))
    }
  };

  Ok(provider)
}
