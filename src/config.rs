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
    #[serde(default)]
    pub sessions_dir: Option<String>,
    #[serde(default = "default_max_revisions")]
    pub max_revisions: u32,
    /// Isolation backend: "none" (default, bare child process) or "capsule" (ZeptoKernel).
    #[serde(default = "default_isolation")]
    pub isolation: String,
    /// Optional path to the zk-init binary used for namespace capsules.
    /// When omitted, ZeptoKernel resolves a default sibling `zk-init` path.
    #[serde(default)]
    pub worker_binary: Option<String>,
    /// Path to the ZeptoClaw worker binary spawned inside the capsule.
    #[serde(default)]
    pub zeptoclaw_binary: Option<String>,
    /// Auto-delete completed/failed runs older than N days (0 = disabled).
    #[serde(default)]
    pub run_ttl_days: u32,
    /// Security profile: "dev", "standard" (default), or "hardened".
    #[serde(default)]
    pub security: Option<String>,
    /// Override: whether cgroup setup failure is fatal.
    #[serde(default)]
    pub cgroup_required: Option<bool>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: default_poll_interval(),
            log_level: default_log_level(),
            log_format: default_log_format(),
            bind: None,
            sessions_dir: None,
            max_revisions: default_max_revisions(),
            isolation: default_isolation(),
            worker_binary: None,
            zeptoclaw_binary: None,
            run_ttl_days: 0,
            security: None,
            cgroup_required: None,
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
    #[serde(default)]
    pub gateway: Option<GatewayConfig>,
    #[serde(default = "default_true")]
    pub session_persist: bool,
    #[serde(default)]
    pub max_history: Option<usize>,
    /// Memory limit for capsule jobs (MiB). None = unlimited.
    #[serde(default)]
    pub memory_mib: Option<u64>,
    /// Max process count inside capsule. None = unlimited.
    #[serde(default)]
    pub max_pids: Option<u32>,
    /// Wall clock timeout for capsule jobs (seconds). None = use `DEFAULT_CAPSULE_TIMEOUT_SEC`.
    #[serde(default)]
    pub timeout_sec: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    #[serde(default)]
    pub enabled: bool,
    pub api_key: Option<String>,
    pub rate_limit: Option<u32>,
}

impl GatewayConfig {
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

fn default_poll_interval() -> u64 {
    5000
}
fn default_log_level() -> String {
    "info".into()
}
fn default_log_format() -> String {
    "pretty".into()
}
fn default_provider() -> String {
    "openrouter".into()
}
fn default_true() -> bool {
    true
}
fn default_max_restarts() -> u32 {
    5
}
fn default_restart_backoff() -> u64 {
    10_000
}
fn default_max_revisions() -> u32 {
    3
}
fn default_isolation() -> String {
    "none".into()
}

/// Default wall-clock timeout for capsule jobs when `AgentConfig.timeout_sec` is None.
/// Applied by `job_to_spec` in `capsule.rs`.
pub const DEFAULT_CAPSULE_TIMEOUT_SEC: u64 = 300;

/// Load config from a TOML file.
pub fn load_config(path: &str) -> Result<Config, ConfigError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::FileNotFound(path.to_string(), e.to_string()))?;
    let config: Config =
        toml::from_str(&content).map_err(|e| ConfigError::ParseError(e.to_string()))?;
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
            errors.push(format!(
                "agents[{}] '{}': provider must not be empty",
                i, agent.name
            ));
        }
        if let Some(max) = agent.max_iterations {
            if max == 0 {
                errors.push(format!(
                    "agents[{}] '{}': max_iterations must be > 0",
                    i, agent.name
                ));
            }
        }
        // Warn if agent references a provider not defined in [providers.*]
        if !config.providers.contains_key(&agent.provider) && !config.providers.is_empty() {
            errors.push(format!(
                "agents[{}] '{}': provider '{}' not defined in [providers.*]",
                i, agent.name, agent.provider
            ));
        }
    }

    // Check for duplicate agent names
    let mut seen = std::collections::HashSet::new();
    for agent in &config.agents {
        if !seen.insert(&agent.name) {
            errors.push(format!("duplicate agent name: '{}'", agent.name));
        }
    }

    // Validate isolation config
    match config.daemon.isolation.as_str() {
        "none" | "capsule" | "process" | "namespace" => {}
        other => {
            errors.push(format!(
        "daemon.isolation: unknown value '{}' (expected \"none\", \"process\", \"namespace\", or \"capsule\")",
        other
      ));
        }
    }

    errors
}

/// Compute a simple hash of the config for change detection.
///
/// Covers agent identity/process fields plus daemon settings that affect orchestration
/// dispatch or scheduling during runtime. Capsule resource limits (`memory_mib`,
/// `max_pids`, `timeout_sec`) are intentionally excluded — they are re-read per-job at
/// dispatch time and do not require a restart.
pub fn config_hash(config: &Config) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    config.daemon.isolation.hash(&mut hasher);
    config.daemon.worker_binary.hash(&mut hasher);
    config.daemon.zeptoclaw_binary.hash(&mut hasher);
    config.daemon.run_ttl_days.hash(&mut hasher);
    config.daemon.security.hash(&mut hasher);
    config.daemon.cgroup_required.hash(&mut hasher);
    config.daemon.max_revisions.hash(&mut hasher);
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
            ConfigError::FileNotFound(path, err) => {
                write!(f, "config file '{}' not found: {}", path, err)
            }
            ConfigError::ParseError(err) => write!(f, "config parse error: {}", err),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Resolve the sessions directory path.
/// Uses `daemon.sessions_dir` if set, otherwise `~/.zeptopm/sessions`.
pub fn resolve_sessions_dir(config: &Config) -> std::path::PathBuf {
    if let Some(ref dir) = config.daemon.sessions_dir {
        let expanded = if dir.starts_with('~') {
            dirs::home_dir()
                .map(|h| h.join(dir.trim_start_matches("~/")))
                .unwrap_or_else(|| std::path::PathBuf::from(dir))
        } else {
            std::path::PathBuf::from(dir)
        };
        expanded
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".zeptopm")
            .join("sessions")
    }
}

/// Get the session file path for a given agent.
pub fn session_file(config: &Config, agent_name: &str) -> std::path::PathBuf {
    resolve_sessions_dir(config).join(format!("{}.json", agent_name))
}

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
        assert_eq!(
            config.agents[0].budget.as_ref().unwrap().token_limit,
            Some(100000)
        );
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
                gateway: None,
                session_persist: true,
                max_history: None,
                memory_mib: None,
                max_pids: None,
                timeout_sec: None,
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
            gateway: None,
            session_persist: true,
            max_history: None,
            memory_mib: None,
            max_pids: None,
            timeout_sec: None,
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
    fn test_config_hash_changes_when_daemon_isolation_changes() {
        let mut config = Config {
            daemon: DaemonConfig::default(),
            agents: vec![],
            providers: HashMap::new(),
        };
        let h1 = config_hash(&config);
        config.daemon.isolation = "process".into();
        let h2 = config_hash(&config);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_validate_unknown_provider_ref() {
        let config = Config {
            daemon: DaemonConfig::default(),
            agents: vec![AgentConfig {
                name: "a".into(),
                provider: "nonexistent".into(),
                model: None,
                system_prompt: None,
                tools: vec![],
                auto_start: true,
                max_restarts: 5,
                restart_backoff_ms: 10_000,
                max_iterations: None,
                timeout_ms: None,
                budget: None,
                gateway: None,
                session_persist: true,
                max_history: None,
                memory_mib: None,
                max_pids: None,
                timeout_sec: None,
            }],
            providers: {
                let mut m = HashMap::new();
                m.insert(
                    "openrouter".into(),
                    ProviderConfig {
                        api_key: None,
                        base_url: None,
                    },
                );
                m
            },
        };
        let errors = validate_config(&config);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("not defined in [providers.*]"))
        );
    }

    #[test]
    fn test_validate_capsule_does_not_need_worker_binary() {
        let toml = r#"
      [daemon]
      isolation = "capsule"

      [[agents]]
      name = "a"
      provider = "p"
    "#;
        let config: Config = toml::from_str(toml).unwrap();
        let errors = validate_config(&config);
        assert!(!errors.iter().any(|e| e.contains("worker_binary")));
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

    #[test]
    fn test_daemon_config_zeptoclaw_binary() {
        let toml_str = r#"
[daemon]
isolation = "process"
worker_binary = "/usr/bin/zk-init"
zeptoclaw_binary = "/usr/bin/zeptoclaw"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.daemon.zeptoclaw_binary.as_deref(),
            Some("/usr/bin/zeptoclaw")
        );
    }

    #[test]
    fn test_daemon_config_zeptoclaw_binary_optional() {
        // Parse-only test: verifies zeptoclaw_binary defaults to None when omitted.
        let toml_str = r#"
[daemon]
isolation = "process"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.daemon.zeptoclaw_binary.is_none());
    }

    #[test]
    fn test_agent_config_resource_limits() {
        let toml_str = r#"
[[agents]]
name = "researcher"
memory_mib = 512
max_pids = 64
timeout_sec = 600
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let agent = &config.agents[0];
        assert_eq!(agent.memory_mib, Some(512));
        assert_eq!(agent.max_pids, Some(64));
        assert_eq!(agent.timeout_sec, Some(600));
    }

    #[test]
    fn test_validation_accepts_process_isolation() {
        let toml_str = r#"
[daemon]
isolation = "process"
worker_binary = "/usr/bin/zk-init"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let errors = validate_config(&config);
        assert!(errors.is_empty(), "errors: {:?}", errors);
    }

    #[test]
    fn test_validation_accepts_namespace_isolation() {
        let toml_str = r#"
[daemon]
isolation = "namespace"
worker_binary = "/usr/bin/zk-init"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let errors = validate_config(&config);
        assert!(errors.is_empty(), "errors: {:?}", errors);
    }

    #[test]
    fn test_validation_namespace_allows_default_init_binary() {
        let toml_str = r#"
[daemon]
isolation = "namespace"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let errors = validate_config(&config);
        assert!(!errors.iter().any(|e| e.contains("worker_binary")));
    }

    #[test]
    fn test_validation_rejects_unknown_isolation() {
        let toml_str = r#"
[daemon]
isolation = "firecracker"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let errors = validate_config(&config);
        assert!(
            errors.iter().any(|e| e.contains("isolation")),
            "expected isolation error, got: {:?}",
            errors
        );
    }
}
