//! Configuration types and TOML parsing for zeptoPM.
//!
//! Config file: `zeptopm.toml`

use serde::Deserialize;
use std::collections::HashMap;

/// Root config structure matching `zeptopm.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
  #[serde(default)]
  pub daemon: DaemonConfig,
  #[serde(default)]
  pub agents: Vec<AgentConfig>,
  #[serde(default)]
  pub providers: HashMap<String, ProviderConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
  #[serde(default = "default_poll_interval")]
  pub poll_interval_ms: u64,
  #[serde(default = "default_log_level")]
  pub log_level: String,
  #[serde(default = "default_log_format")]
  pub log_format: String,
  #[serde(default)]
  pub bind: Option<String>,
}

impl Default for DaemonConfig {
  fn default() -> Self {
    Self {
      poll_interval_ms: default_poll_interval(),
      log_level: default_log_level(),
      log_format: default_log_format(),
      bind: None,
    }
  }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
  pub name: String,
  #[serde(default = "default_provider")]
  pub provider: String,
  pub model: Option<String>,
  pub system_prompt: Option<String>,
  #[serde(default)]
  pub tools: Vec<String>,
  #[serde(default = "default_true")]
  pub auto_start: bool,
  #[serde(default = "default_max_restarts")]
  pub max_restarts: u32,
  #[serde(default = "default_restart_backoff")]
  pub restart_backoff_ms: u64,
  pub max_iterations: Option<usize>,
  pub timeout_ms: Option<u64>,
  #[serde(default)]
  pub budget: Option<BudgetConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BudgetConfig {
  pub token_limit: Option<u64>,
  pub cost_limit_usd: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
  pub api_key: Option<String>,
  pub base_url: Option<String>,
}

impl ProviderConfig {
  /// Resolve the API key, expanding `$ENV_VAR` references.
  pub fn resolve_api_key(&self) -> Option<String> {
    self.api_key.as_ref().and_then(|key| {
      if let Some(env_name) = key.strip_prefix('$') {
        std::env::var(env_name).ok().filter(|v| !v.is_empty())
      } else {
        Some(key.clone())
      }
    })
  }
}

fn default_poll_interval() -> u64 { 5000 }
fn default_log_level() -> String { "info".into() }
fn default_log_format() -> String { "pretty".into() }
fn default_provider() -> String { "openrouter".into() }
fn default_true() -> bool { true }
fn default_max_restarts() -> u32 { 5 }
fn default_restart_backoff() -> u64 { 10_000 }

/// Load config from a TOML file.
pub fn load_config(path: &str) -> Result<Config, ConfigError> {
  let content = std::fs::read_to_string(path)
    .map_err(|e| ConfigError::FileNotFound(path.to_string(), e.to_string()))?;
  let config: Config = toml::from_str(&content)
    .map_err(|e| ConfigError::ParseError(e.to_string()))?;
  Ok(config)
}

/// Validate the config. Returns a list of errors.
pub fn validate_config(config: &Config) -> Vec<String> {
  let mut errors = Vec::new();

  for (i, agent) in config.agents.iter().enumerate() {
    if agent.name.trim().is_empty() {
      errors.push(format!("agents[{}]: name must not be empty", i));
    }
    if agent.provider.trim().is_empty() {
      errors.push(format!("agents[{}] '{}': provider must not be empty", i, agent.name));
    }
    if let Some(max) = agent.max_iterations {
      if max == 0 {
        errors.push(format!("agents[{}] '{}': max_iterations must be > 0", i, agent.name));
      }
    }
  }

  // Check for duplicate agent names
  let mut seen = std::collections::HashSet::new();
  for agent in &config.agents {
    if !seen.insert(&agent.name) {
      errors.push(format!("duplicate agent name: '{}'", agent.name));
    }
  }

  errors
}

/// Compute a simple hash of the config for change detection.
pub fn config_hash(config: &Config) -> u64 {
  use std::collections::hash_map::DefaultHasher;
  use std::hash::{Hash, Hasher};

  let mut hasher = DefaultHasher::new();
  for agent in &config.agents {
    agent.name.hash(&mut hasher);
    agent.provider.hash(&mut hasher);
    agent.model.hash(&mut hasher);
    agent.system_prompt.hash(&mut hasher);
    agent.tools.hash(&mut hasher);
    agent.auto_start.hash(&mut hasher);
    agent.max_restarts.hash(&mut hasher);
    agent.max_iterations.hash(&mut hasher);
    agent.timeout_ms.hash(&mut hasher);
  }
  hasher.finish()
}

#[derive(Debug)]
pub enum ConfigError {
  FileNotFound(String, String),
  ParseError(String),
}

impl std::fmt::Display for ConfigError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      ConfigError::FileNotFound(path, err) => write!(f, "config file '{}' not found: {}", path, err),
      ConfigError::ParseError(err) => write!(f, "config parse error: {}", err),
    }
  }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_parse_minimal_config() {
    let toml = r#"
      [[agents]]
      name = "researcher"
      provider = "openrouter"
      model = "anthropic/claude-sonnet-4"
    "#;
    let config: Config = toml::from_str(toml).unwrap();
    assert_eq!(config.agents.len(), 1);
    assert_eq!(config.agents[0].name, "researcher");
    assert!(config.agents[0].auto_start); // default true
    assert_eq!(config.agents[0].max_restarts, 5); // default
  }

  #[test]
  fn test_parse_full_config() {
    let toml = r#"
      [daemon]
      poll_interval_ms = 10000
      log_level = "debug"

      [[agents]]
      name = "researcher"
      provider = "openrouter"
      model = "anthropic/claude-sonnet-4"
      system_prompt = "You are a research assistant."
      tools = ["web_fetch", "longterm_memory"]
      auto_start = true
      max_restarts = 3

      [agents.budget]
      token_limit = 100000
      cost_limit_usd = 5.0

      [[agents]]
      name = "coder"
      provider = "openrouter"
      model = "anthropic/claude-sonnet-4"
      auto_start = false

      [providers.openrouter]
      api_key = "$OPENROUTER_API_KEY"
      base_url = "https://openrouter.ai/api/v1"
    "#;
    let config: Config = toml::from_str(toml).unwrap();
    assert_eq!(config.daemon.poll_interval_ms, 10000);
    assert_eq!(config.agents.len(), 2);
    assert_eq!(config.agents[0].budget.as_ref().unwrap().token_limit, Some(100000));
    assert!(!config.agents[1].auto_start);
    assert!(config.providers.contains_key("openrouter"));
  }

  #[test]
  fn test_validate_empty_name() {
    let config = Config {
      daemon: DaemonConfig::default(),
      agents: vec![AgentConfig {
        name: "".into(),
        provider: "openrouter".into(),
        model: None,
        system_prompt: None,
        tools: vec![],
        auto_start: true,
        max_restarts: 5,
        restart_backoff_ms: 10_000,
        max_iterations: None,
        timeout_ms: None,
        budget: None,
      }],
      providers: HashMap::new(),
    };
    let errors = validate_config(&config);
    assert!(!errors.is_empty());
    assert!(errors[0].contains("name"));
  }

  #[test]
  fn test_validate_duplicate_names() {
    let agent = AgentConfig {
      name: "same-name".into(),
      provider: "openrouter".into(),
      model: None,
      system_prompt: None,
      tools: vec![],
      auto_start: true,
      max_restarts: 5,
      restart_backoff_ms: 10_000,
      max_iterations: None,
      timeout_ms: None,
      budget: None,
    };
    let config = Config {
      daemon: DaemonConfig::default(),
      agents: vec![agent.clone(), agent],
      providers: HashMap::new(),
    };
    let errors = validate_config(&config);
    assert!(errors.iter().any(|e| e.contains("duplicate")));
  }

  #[test]
  fn test_config_hash_deterministic() {
    let toml = r#"
      [[agents]]
      name = "a"
      provider = "p"
    "#;
    let config: Config = toml::from_str(toml).unwrap();
    let h1 = config_hash(&config);
    let h2 = config_hash(&config);
    assert_eq!(h1, h2);
  }

  #[test]
  fn test_provider_resolve_env_var() {
    let provider = ProviderConfig {
      api_key: Some("$HOME".into()),
      base_url: None,
    };
    // $HOME should resolve to something non-empty
    assert!(provider.resolve_api_key().is_some());
  }

  #[test]
  fn test_provider_resolve_literal() {
    let provider = ProviderConfig {
      api_key: Some("sk-1234".into()),
      base_url: None,
    };
    assert_eq!(provider.resolve_api_key(), Some("sk-1234".into()));
  }
}
