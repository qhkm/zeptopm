//! Daemon — the main orchestration loop.
//!
//! Manages agent lifecycle: spawn, monitor, restart, hot-reload config.

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::agent::{
  spawn_agent, AgentHandle, AgentState, AgentStateUpdate, AgentStatus,
};
use crate::config::{self, Config};
use crate::llm::{self, LlmClient};

/// Run the daemon. This is the main entry point.
pub async fn run(config_path: String, _bind: Option<String>) {
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

  let llm_client = LlmClient::new();
  let (state_tx, mut state_rx) = mpsc::channel::<AgentStateUpdate>(256);

  // Track agent state
  let mut agents: HashMap<String, ManagedAgent> = HashMap::new();

  // Spawn auto_start agents
  for agent_config in &config.agents {
    if !agent_config.auto_start {
      continue;
    }
    match spawn_managed_agent(agent_config, &config, &llm_client, state_tx.clone()) {
      Ok(managed) => {
        info!(agent = %agent_config.name, "agent spawned");
        agents.insert(agent_config.name.clone(), managed);
      }
      Err(e) => {
        warn!(agent = %agent_config.name, error = %e, "failed to spawn agent");
      }
    }
  }

  info!(running = agents.len(), "all auto_start agents spawned");

  // Config watcher state
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
        if let Some(managed) = agents.get_mut(&update.name) {
          managed.state.tokens_used += update.tokens_delta;
          managed.state.status = update.status.clone();
          if update.error.is_some() {
            managed.state.last_error = update.error;
          }

          // Handle restart logic for errored agents
          if update.status == AgentStatus::Stopped || update.status == AgentStatus::Error {
            if managed.state.restart_count < managed.max_restarts {
              let backoff = Duration::from_millis(
                managed.restart_backoff_ms * 2u64.pow(managed.state.restart_count),
              );
              info!(
                agent = %update.name,
                restart = managed.state.restart_count + 1,
                backoff_ms = backoff.as_millis() as u64,
                "scheduling restart"
              );
              managed.state.status = AgentStatus::RestartPending;
              managed.restart_at = Some(Instant::now() + backoff);
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
            apply_config_changes(&mut agents, &new_config, &llm_client, state_tx.clone()).await;
            last_config_hash = new_hash;
          }
        }

        // Check for agents that need restarting
        let now = Instant::now();
        let restart_names: Vec<String> = agents
          .iter()
          .filter(|(_, m)| {
            m.state.status == AgentStatus::RestartPending
              && m.restart_at.map(|t| now >= t).unwrap_or(false)
          })
          .map(|(name, _)| name.clone())
          .collect();

        for name in restart_names {
          if let Some(agent_config) = config.agents.iter().find(|a| a.name == name) {
            let restart_count = agents.get(&name).map(|m| m.state.restart_count).unwrap_or(0);
            agents.remove(&name);

            match spawn_managed_agent(agent_config, &config, &llm_client, state_tx.clone()) {
              Ok(mut managed) => {
                managed.state.restart_count = restart_count + 1;
                info!(agent = %name, restart = managed.state.restart_count, "agent restarted");
                agents.insert(name, managed);
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
  for (name, managed) in &agents {
    managed.handle.stop().await;
    info!(agent = %name, "sent stop to agent");
  }

  // Wait briefly for agents to stop
  tokio::time::sleep(Duration::from_secs(2)).await;

  info!("zeptopm stopped");
}

struct ManagedAgent {
  handle: AgentHandle,
  _join: tokio::task::JoinHandle<()>,
  state: AgentState,
  max_restarts: u32,
  restart_backoff_ms: u64,
  restart_at: Option<Instant>,
}

fn spawn_managed_agent(
  agent_config: &crate::config::AgentConfig,
  config: &Config,
  llm_client: &LlmClient,
  state_tx: mpsc::Sender<AgentStateUpdate>,
) -> Result<ManagedAgent, String> {
  let provider_config = config.providers.get(&agent_config.provider);
  let api_key = provider_config
    .and_then(|p| p.resolve_api_key())
    .ok_or_else(|| format!("no API key for provider '{}'", agent_config.provider))?;
  let base_url = llm::resolve_base_url(provider_config, &agent_config.provider);

  let (handle, join) = spawn_agent(
    agent_config.clone(),
    llm_client.clone(),
    api_key,
    base_url,
    state_tx,
  );

  Ok(ManagedAgent {
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
  agents: &mut HashMap<String, ManagedAgent>,
  new_config: &Config,
  llm_client: &LlmClient,
  state_tx: mpsc::Sender<AgentStateUpdate>,
) {
  let running_names: std::collections::HashSet<String> =
    agents.keys().cloned().collect();
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
    if let Some(managed) = agents.remove(name) {
      managed.handle.stop().await;
      info!(agent = %name, "removed agent (config change)");
    }
  }

  // Add new agents
  for agent_config in &new_config.agents {
    if !agent_config.auto_start {
      continue;
    }
    if agents.contains_key(&agent_config.name) {
      continue;
    }
    match spawn_managed_agent(agent_config, new_config, llm_client, state_tx.clone()) {
      Ok(managed) => {
        info!(agent = %agent_config.name, "added agent (config change)");
        agents.insert(agent_config.name.clone(), managed);
      }
      Err(e) => {
        warn!(agent = %agent_config.name, error = %e, "failed to add agent");
      }
    }
  }

  let prev_count = running_names.len();
  if !to_remove.is_empty() || agents.len() != prev_count {
    info!(total = agents.len(), "config reload complete");
  }
}
