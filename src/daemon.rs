//! Daemon — the main orchestration loop.
//!
//! Manages agent lifecycle: spawn, monitor, restart, hot-reload config.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{info, warn};
use zeptoclaw::providers::LLMProvider;

use crate::agent::{
  spawn_agent, AgentHandle, AgentState, AgentStateUpdate, AgentStatus,
};
use crate::config::{self, Config};
use crate::server::{self, DaemonCommand, ManagedAgentRef, SharedState};

/// Run the daemon. This is the main entry point.
pub async fn run(config_path: String, bind: Option<String>) {
  let config = match config::load_config(&config_path) {
    Ok(c) => c,
    Err(e) => {
      eprintln!("Failed to load config: {}", e);
      std::process::exit(1);
    }
  };

  let errors = config::validate_config(&config);
  if !errors.is_empty() {
    for e in &errors {
      eprintln!("Config error: {}", e);
    }
    std::process::exit(1);
  }

  info!(
    agents = config.agents.len(),
    poll_interval_ms = config.daemon.poll_interval_ms,
    "zeptopm daemon starting"
  );

  let (state_tx, mut state_rx) = mpsc::channel::<AgentStateUpdate>(256);

  // Daemon command channel (HTTP handlers -> daemon loop)
  let (daemon_cmd_tx, mut daemon_cmd_rx) = mpsc::channel::<DaemonCommand>(64);

  // Shared state for HTTP server
  let shared_state = server::new_shared_state(daemon_cmd_tx);

  // Track internal daemon state (owns JoinHandles, restart config)
  let mut managed: HashMap<String, InternalAgent> = HashMap::new();

  // Spawn auto_start agents
  for agent_config in &config.agents {
    if !agent_config.auto_start {
      continue;
    }
    match spawn_managed_agent(agent_config, &config, state_tx.clone()) {
      Ok(internal) => {
        info!(agent = %agent_config.name, "agent spawned");
        sync_agent_to_shared(&shared_state, &agent_config.name, &internal).await;
        managed.insert(agent_config.name.clone(), internal);
      }
      Err(e) => {
        warn!(agent = %agent_config.name, error = %e, "failed to spawn agent");
      }
    }
  }

  info!(running = managed.len(), "all auto_start agents spawned");

  // Start HTTP server
  let bind_addr = bind
    .or_else(|| config.daemon.bind.clone())
    .unwrap_or_else(|| "127.0.0.1:9876".into());
  let server_state = shared_state.clone();
  tokio::spawn(async move {
    server::start_server(bind_addr, server_state).await;
  });

  // Config watcher state
  let mut current_config = config.clone();
  let mut last_config_hash = config::config_hash(&config);
  let poll_interval = Duration::from_millis(config.daemon.poll_interval_ms);

  // Main loop
  let mut poll_timer = tokio::time::interval(poll_interval);
  let mut shutdown = false;

  // Setup shutdown signal
  let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
  tokio::spawn(async move {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
      let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
          .expect("register SIGTERM");
      tokio::select! {
        _ = ctrl_c => info!("received SIGINT"),
        _ = sigterm.recv() => info!("received SIGTERM"),
      }
    }
    #[cfg(not(unix))]
    {
      ctrl_c.await.ok();
      info!("received SIGINT");
    }
    let _ = shutdown_tx.send(());
  });

  while !shutdown {
    tokio::select! {
      // Shutdown signal
      _ = &mut shutdown_rx => {
        shutdown = true;
      }

      // Agent state updates
      Some(update) = state_rx.recv() => {
        if let Some(internal) = managed.get_mut(&update.name) {
          internal.state.tokens_used += update.tokens_delta;
          internal.state.status = update.status.clone();
          if update.error.is_some() {
            internal.state.last_error = update.error;
          }

          // Sync to shared state for HTTP handlers
          {
            let mut s = shared_state.write().await;
            if let Some(m) = s.agents.get_mut(&update.name) {
              m.state = internal.state.clone();
            }
          }

          // Handle restart logic for errored agents
          if update.status == AgentStatus::Stopped || update.status == AgentStatus::Error {
            if internal.state.restart_count < internal.max_restarts {
              let backoff = Duration::from_millis(
                internal.restart_backoff_ms * 2u64.pow(internal.state.restart_count),
              );
              info!(
                agent = %update.name,
                restart = internal.state.restart_count + 1,
                backoff_ms = backoff.as_millis() as u64,
                "scheduling restart"
              );
              internal.state.status = AgentStatus::RestartPending;
              internal.restart_at = Some(Instant::now() + backoff);
            }
          }
        }
      }

      // Daemon commands from HTTP handlers (start/restart)
      Some(cmd) = daemon_cmd_rx.recv() => {
        match cmd {
          DaemonCommand::Start { name, reply } => {
            if managed.contains_key(&name) {
              let status = &managed[&name].state.status;
              if *status != AgentStatus::Stopped && *status != AgentStatus::Error {
                let _ = reply.send(Err(format!("agent '{}' is already {}", name, status)));
                continue;
              }
              managed.remove(&name);
              shared_state.write().await.agents.remove(&name);
            }

            match current_config.agents.iter().find(|a| a.name == name) {
              Some(agent_config) => {
                match spawn_managed_agent(agent_config, &current_config, state_tx.clone()) {
                  Ok(internal) => {
                    info!(agent = %name, "agent started via command");
                    sync_agent_to_shared(&shared_state, &name, &internal).await;
                    managed.insert(name.clone(), internal);
                    let _ = reply.send(Ok(format!("agent '{}' started", name)));
                  }
                  Err(e) => {
                    let _ = reply.send(Err(format!("failed to start '{}': {}", name, e)));
                  }
                }
              }
              None => {
                let _ = reply.send(Err(format!("agent '{}' not found in config", name)));
              }
            }
          }
          DaemonCommand::Restart { name, reply } => {
            if let Some(internal) = managed.remove(&name) {
              internal.handle.stop().await;
              shared_state.write().await.agents.remove(&name);
              info!(agent = %name, "stopped agent for restart");
            }

            match current_config.agents.iter().find(|a| a.name == name) {
              Some(agent_config) => {
                match spawn_managed_agent(agent_config, &current_config, state_tx.clone()) {
                  Ok(internal) => {
                    info!(agent = %name, "agent restarted via command");
                    sync_agent_to_shared(&shared_state, &name, &internal).await;
                    managed.insert(name.clone(), internal);
                    let _ = reply.send(Ok(format!("agent '{}' restarted", name)));
                  }
                  Err(e) => {
                    let _ = reply.send(Err(format!("failed to restart '{}': {}", name, e)));
                  }
                }
              }
              None => {
                let _ = reply.send(Err(format!("agent '{}' not found in config", name)));
              }
            }
          }
        }
      }

      // Poll tick — config reload + restart check
      _ = poll_timer.tick() => {
        // Check for config changes
        if let Ok(new_config) = config::load_config(&config_path) {
          let new_hash = config::config_hash(&new_config);
          if new_hash != last_config_hash {
            info!(old_hash = last_config_hash, new_hash = new_hash, "config change detected");
            apply_config_changes(&mut managed, &shared_state, &new_config, state_tx.clone()).await;
            current_config = new_config;
            last_config_hash = new_hash;
          }
        }

        // Check for agents that need restarting
        let now = Instant::now();
        let restart_names: Vec<String> = managed
          .iter()
          .filter(|(_, m)| {
            m.state.status == AgentStatus::RestartPending
              && m.restart_at.map(|t| now >= t).unwrap_or(false)
          })
          .map(|(name, _)| name.clone())
          .collect();

        for name in restart_names {
          if let Some(agent_config) = current_config.agents.iter().find(|a| a.name == name) {
            let restart_count = managed.get(&name).map(|m| m.state.restart_count).unwrap_or(0);
            managed.remove(&name);

            match spawn_managed_agent(agent_config, &current_config, state_tx.clone()) {
              Ok(mut internal) => {
                internal.state.restart_count = restart_count + 1;
                info!(agent = %name, restart = internal.state.restart_count, "agent restarted");
                sync_agent_to_shared(&shared_state, &name, &internal).await;
                managed.insert(name, internal);
              }
              Err(e) => {
                warn!(agent = %name, error = %e, "restart failed");
              }
            }
          }
        }
      }
    }
  }

  // Graceful shutdown
  info!("shutting down...");
  for (name, internal) in &managed {
    internal.handle.stop().await;
    info!(agent = %name, "sent stop to agent");
  }

  tokio::time::sleep(Duration::from_secs(2)).await;
  info!("zeptopm stopped");
}

struct InternalAgent {
  handle: AgentHandle,
  _join: tokio::task::JoinHandle<()>,
  state: AgentState,
  max_restarts: u32,
  restart_backoff_ms: u64,
  restart_at: Option<Instant>,
}

async fn sync_agent_to_shared(shared: &SharedState, name: &str, internal: &InternalAgent) {
  let mut s = shared.write().await;
  s.agents.insert(
    name.to_string(),
    ManagedAgentRef {
      handle: internal.handle.clone(),
      state: internal.state.clone(),
    },
  );
}

/// Create a zeptoclaw LLMProvider from zeptoPM config.
fn create_provider(
  agent_config: &crate::config::AgentConfig,
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

fn spawn_managed_agent(
  agent_config: &crate::config::AgentConfig,
  config: &Config,
  state_tx: mpsc::Sender<AgentStateUpdate>,
) -> Result<InternalAgent, String> {
  let provider = create_provider(agent_config, config)?;

  let (handle, join) = spawn_agent(agent_config.clone(), provider, state_tx);

  Ok(InternalAgent {
    handle,
    _join: join,
    state: AgentState {
      name: agent_config.name.clone(),
      status: AgentStatus::Starting,
      restart_count: 0,
      started_at: Some(Instant::now()),
      last_error: None,
      messages_handled: 0,
      tokens_used: 0,
    },
    max_restarts: agent_config.max_restarts,
    restart_backoff_ms: agent_config.restart_backoff_ms,
    restart_at: None,
  })
}

async fn apply_config_changes(
  managed: &mut HashMap<String, InternalAgent>,
  shared_state: &SharedState,
  new_config: &Config,
  state_tx: mpsc::Sender<AgentStateUpdate>,
) {
  let running_names: std::collections::HashSet<String> =
    managed.keys().cloned().collect();
  let new_names: std::collections::HashSet<String> = new_config
    .agents
    .iter()
    .filter(|a| a.auto_start)
    .map(|a| a.name.clone())
    .collect();

  // Remove agents no longer in config
  let to_remove: Vec<String> = running_names
    .difference(&new_names)
    .cloned()
    .collect();
  for name in &to_remove {
    if let Some(internal) = managed.remove(name) {
      internal.handle.stop().await;
      shared_state.write().await.agents.remove(name);
      info!(agent = %name, "removed agent (config change)");
    }
  }

  // Add new agents
  for agent_config in &new_config.agents {
    if !agent_config.auto_start {
      continue;
    }
    if managed.contains_key(&agent_config.name) {
      continue;
    }
    match spawn_managed_agent(agent_config, new_config, state_tx.clone()) {
      Ok(internal) => {
        info!(agent = %agent_config.name, "added agent (config change)");
        sync_agent_to_shared(shared_state, &agent_config.name, &internal).await;
        managed.insert(agent_config.name.clone(), internal);
      }
      Err(e) => {
        warn!(agent = %agent_config.name, error = %e, "failed to add agent");
      }
    }
  }

  let prev_count = running_names.len();
  if !to_remove.is_empty() || managed.len() != prev_count {
    info!(total = managed.len(), "config reload complete");
  }
}
