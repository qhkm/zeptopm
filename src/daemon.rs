//! Daemon — the main orchestration loop.
//!
//! Manages agent lifecycle: spawn, monitor, restart, hot-reload config.

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::agent::{
  push_log, spawn_agent, AgentHandle, AgentState, AgentStateUpdate, AgentStatus,
};
use crate::config::{self, Config};
use crate::orchestrator::engine::OrchestratorEngine;
use crate::server::{self, DaemonCommand, ManagedAgentRef, ResolvedGatewayConfig, SharedState};

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

  // Initialize orchestrator
  let mut orchestrator = OrchestratorEngine::new(4);
  let (orch_event_tx, mut orch_event_rx) = mpsc::channel::<serde_json::Value>(256);

  // Spawn auto_start agents
  for agent_config in &config.agents {
    if !agent_config.auto_start {
      continue;
    }
    match spawn_managed_agent(agent_config, &config_path, &config, state_tx.clone()) {
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
          if update.pid.is_some() {
            internal.state.pid = update.pid;
          }
          if let Some(log_entry) = update.log {
            push_log(&mut internal.state.logs, log_entry);
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
                match spawn_managed_agent(agent_config, &config_path, &current_config, state_tx.clone()) {
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
                match spawn_managed_agent(agent_config, &config_path, &current_config, state_tx.clone()) {
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
          DaemonCommand::SubmitRun { task, reply } => {
            let run_id = orchestrator.submit_run(task);
            info!(run_id = %run_id, "new orchestrated run submitted");

            // Spawn ready jobs from the new run
            while let Some(job) = orchestrator.next_job() {
              info!(job_id = %job.job_id, role = %job.role, "spawning job worker");
              orchestrator.mark_running(&job.job_id);
              spawn_job_worker(&job, &config_path, &current_config, state_tx.clone(), orch_event_tx.clone(), &mut managed, &shared_state, &orchestrator.store).await;
            }

            let _ = reply.send(Ok(run_id));
          }
          DaemonCommand::GetRunStatus { run_id, reply } => {
            let result = get_run_status_data(&orchestrator, &run_id);
            let _ = reply.send(result);
          }
          DaemonCommand::ListRuns { reply } => {
            let runs = get_runs_list_data(&orchestrator);
            let _ = reply.send(Ok(runs));
          }
        }
      }

      // Orchestrator events from job workers
      Some(event) = orch_event_rx.recv() => {
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match event_type {
          "heartbeat" => {
            let job_id = event.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
            orchestrator.record_heartbeat(job_id);
          }
          "progress" => {
            let job_id = event.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
            orchestrator.record_heartbeat(job_id);
            let phase = event.get("phase").and_then(|v| v.as_str()).unwrap_or("");
            let message = event.get("message").and_then(|v| v.as_str()).unwrap_or("");
            info!(job_id = %job_id, phase = %phase, message = %message, "job progress");
          }
          "job_completed" => {
            let job_id = event.get("job_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let artifacts: Vec<String> = event.get("output_artifact_ids")
              .and_then(|v| v.as_array())
              .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
              .unwrap_or_default();

            // Check if completed job is a planner — if so, materialize the plan
            let is_planner = orchestrator.store.get_job(&job_id)
              .map(|j| j.role == "planner")
              .unwrap_or(false);
            let run_id = orchestrator.store.get_job(&job_id)
              .map(|j| j.run_id.clone())
              .unwrap_or_default();

            orchestrator.mark_completed(&job_id, artifacts.clone());
            info!(job_id = %job_id, "job completed");

            if is_planner {
              // Read planner output and try to parse as ExecutionPlan
              if let Some(artifact) = artifacts.first().and_then(|aid| orchestrator.store.get_artifact(aid)) {
                let plan_text = std::fs::read_to_string(&artifact.path).unwrap_or_default();
                // Try to extract JSON from the response (may be wrapped in markdown)
                let json_str = extract_json(&plan_text);
                match serde_json::from_str::<crate::orchestrator::types::ExecutionPlan>(&json_str) {
                  Ok(plan) => {
                    info!(run_id = %run_id, jobs = plan.jobs.len(), "planner produced execution plan");
                    let new_job_ids = crate::orchestrator::planner::materialize_plan(
                      &mut orchestrator.store, &run_id, &job_id, &plan
                    );
                    // Promote ready jobs to the queue
                    for jid in &new_job_ids {
                      if let Some(j) = orchestrator.store.get_job(jid) {
                        if j.status == crate::orchestrator::types::JobStatus::Ready {
                          orchestrator.ready_queue.push_back(jid.clone());
                        }
                      }
                    }
                  }
                  Err(e) => {
                    warn!(run_id = %run_id, error = %e, "failed to parse planner output as ExecutionPlan");
                  }
                }
              }
            }

            // Spawn newly ready jobs
            while let Some(job) = orchestrator.next_job() {
              info!(job_id = %job.job_id, role = %job.role, "spawning job worker");
              orchestrator.mark_running(&job.job_id);
              spawn_job_worker(&job, &config_path, &current_config, state_tx.clone(), orch_event_tx.clone(), &mut managed, &shared_state, &orchestrator.store).await;
            }
          }
          "job_failed" => {
            let job_id = event.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
            let error = event.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
            orchestrator.mark_failed(job_id, error.to_string());
            warn!(job_id = %job_id, error = %error, "job failed");

            // Spawn retry if re-queued
            while let Some(job) = orchestrator.next_job() {
              info!(job_id = %job.job_id, role = %job.role, "spawning job worker (retry)");
              orchestrator.mark_running(&job.job_id);
              spawn_job_worker(&job, &config_path, &current_config, state_tx.clone(), orch_event_tx.clone(), &mut managed, &shared_state, &orchestrator.store).await;
            }
          }
          "artifact_produced" => {
            let job_id = event.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
            let run_id = orchestrator.store.get_job(job_id)
              .map(|j| j.run_id.clone())
              .unwrap_or_default();
            let artifact = crate::orchestrator::types::Artifact {
              artifact_id: event.get("artifact_id").and_then(|v| v.as_str()).unwrap_or("").into(),
              run_id,
              job_id: job_id.into(),
              kind: event.get("kind").and_then(|v| v.as_str()).unwrap_or("").into(),
              path: std::path::PathBuf::from(event.get("path").and_then(|v| v.as_str()).unwrap_or("")),
              summary: event.get("summary").and_then(|v| v.as_str()).unwrap_or("").into(),
              created_at: std::time::SystemTime::now(),
            };
            orchestrator.store.create_artifact(artifact);
          }
          _ => {}
        }
      }

      // Poll tick — config reload + restart check
      _ = poll_timer.tick() => {
        // Check for config changes
        if let Ok(new_config) = config::load_config(&config_path) {
          let new_hash = config::config_hash(&new_config);
          if new_hash != last_config_hash {
            info!(old_hash = last_config_hash, new_hash = new_hash, "config change detected");
            apply_config_changes(&mut managed, &shared_state, &new_config, &config_path, state_tx.clone()).await;
            current_config = new_config;
            last_config_hash = new_hash;
          }
        }

        // Check for stuck orchestrator jobs (no heartbeat for 120s)
        let stale = orchestrator.stale_jobs(Duration::from_secs(120));
        for job_id in stale {
          warn!(job_id = %job_id, "job heartbeat timeout — marking failed");
          // Kill the worker process
          let worker_name = format!("__job_{}", job_id);
          if let Some(internal) = managed.get(&worker_name) {
            internal.handle.stop().await;
          }
          orchestrator.mark_failed(&job_id, "heartbeat timeout".into());

          // Spawn retries if re-queued
          while let Some(job) = orchestrator.next_job() {
            info!(job_id = %job.job_id, role = %job.role, "spawning job worker (retry after timeout)");
            orchestrator.mark_running(&job.job_id);
            spawn_job_worker(&job, &config_path, &current_config, state_tx.clone(), orch_event_tx.clone(), &mut managed, &shared_state, &orchestrator.store).await;
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

            match spawn_managed_agent(agent_config, &config_path, &current_config, state_tx.clone()) {
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
  gateway: Option<ResolvedGatewayConfig>,
}

async fn sync_agent_to_shared(shared: &SharedState, name: &str, internal: &InternalAgent) {
  let mut s = shared.write().await;
  s.agents.insert(
    name.to_string(),
    ManagedAgentRef {
      handle: internal.handle.clone(),
      state: internal.state.clone(),
      gateway: internal.gateway.clone(),
    },
  );
}

fn spawn_managed_agent(
  agent_config: &crate::config::AgentConfig,
  config_path: &str,
  _config: &Config,
  state_tx: mpsc::Sender<AgentStateUpdate>,
) -> Result<InternalAgent, String> {
  let (handle, join) = spawn_agent(&agent_config.name, config_path, state_tx);

  let gateway = agent_config.gateway.as_ref().map(|gw| ResolvedGatewayConfig {
    enabled: gw.enabled,
    api_key: gw.resolve_api_key(),
    rate_limit: gw.rate_limit,
  });

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
      logs: vec![],
      pid: None,
    },
    max_restarts: agent_config.max_restarts,
    restart_backoff_ms: agent_config.restart_backoff_ms,
    restart_at: None,
    gateway,
  })
}

async fn apply_config_changes(
  managed: &mut HashMap<String, InternalAgent>,
  shared_state: &SharedState,
  new_config: &Config,
  config_path: &str,
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
    match spawn_managed_agent(agent_config, config_path, new_config, state_tx.clone()) {
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

/// Spawn a temporary worker process for an orchestration job.
/// Sends a `job_execute` command via the worker's stdin.
async fn spawn_job_worker(
  job: &crate::orchestrator::types::Job,
  config_path: &str,
  config: &Config,
  state_tx: mpsc::Sender<AgentStateUpdate>,
  orch_event_tx: mpsc::Sender<serde_json::Value>,
  _managed: &mut HashMap<String, InternalAgent>,
  _shared_state: &SharedState,
  orchestrator_store: &crate::orchestrator::store::RunStore,
) {
  // Find an agent config matching the job's role/profile_id
  let agent_config = match config.agents.iter().find(|a| a.name == job.profile_id) {
    Some(c) => c,
    None => {
      warn!(job_id = %job.job_id, profile = %job.profile_id, "no agent profile found for job");
      let _ = orch_event_tx.send(serde_json::json!({
        "type": "job_failed",
        "job_id": job.job_id,
        "error": format!("no agent profile '{}' found in config", job.profile_id),
        "retryable": false
      })).await;
      return;
    }
  };

  // Spawn a worker using the matching agent profile
  let (handle, join) = crate::agent::spawn_agent_with_orch(
    &agent_config.name,
    config_path,
    state_tx.clone(),
    Some(orch_event_tx.clone()),
  );

  // Collect input artifact paths from completed dependencies
  let input_artifacts: Vec<String> = job.input_artifact_ids.iter()
    .filter_map(|aid| orchestrator_store.get_artifact(aid))
    .map(|a| a.path.to_string_lossy().to_string())
    .collect();

  // Send job_execute command — the worker will emit job_completed/job_failed events
  let _ = handle.send_job(
    job.job_id.clone(),
    job.instruction.clone(),
    job.workspace_dir.to_string_lossy().to_string(),
    input_artifacts,
  ).await;

  // Store the handle to keep the bridge alive until the job completes.
  // Using a unique name to avoid collisions with regular agents.
  let worker_name = format!("__job_{}", job.job_id);
  _managed.insert(worker_name, InternalAgent {
    handle,
    _join: join,
    state: AgentState {
      name: job.job_id.clone(),
      status: AgentStatus::Running,
      restart_count: 0,
      started_at: Some(Instant::now()),
      last_error: None,
      messages_handled: 0,
      tokens_used: 0,
      logs: vec![],
      pid: None,
    },
    max_restarts: 0,
    restart_backoff_ms: 1000,
    restart_at: None,
    gateway: None,
  });
}

fn get_run_status_data(
  orchestrator: &OrchestratorEngine,
  run_id: &str,
) -> Result<serde_json::Value, String> {
  let run = orchestrator.store.get_run(run_id)
    .ok_or_else(|| format!("run '{}' not found", run_id))?;

  let jobs = orchestrator.store.list_run_jobs(run_id);
  let now = Instant::now();
  let job_infos: Vec<serde_json::Value> = jobs.iter().map(|j| {
    let last_hb_secs = orchestrator.last_heartbeat.get(&j.job_id)
      .map(|t| now.duration_since(*t).as_secs());
    serde_json::json!({
      "job_id": j.job_id,
      "role": j.role,
      "status": format!("{:?}", j.status),
      "instruction": j.instruction.chars().take(100).collect::<String>(),
      "attempt": j.attempt,
      "error": j.error,
      "last_heartbeat_secs_ago": last_hb_secs,
    })
  }).collect();

  Ok(serde_json::json!({
    "run_id": run.run_id,
    "task": run.task,
    "status": format!("{:?}", run.status),
    "jobs": job_infos,
  }))
}

/// Extract JSON from text that may be wrapped in markdown code blocks.
fn extract_json(text: &str) -> String {
  let trimmed = text.trim();
  // Try to find JSON block in markdown
  if let Some(start) = trimmed.find("```json") {
    let after = &trimmed[start + 7..];
    if let Some(end) = after.find("```") {
      return after[..end].trim().to_string();
    }
  }
  if let Some(start) = trimmed.find("```") {
    let after = &trimmed[start + 3..];
    if let Some(end) = after.find("```") {
      return after[..end].trim().to_string();
    }
  }
  // Try to find raw JSON object
  if let Some(start) = trimmed.find('{') {
    if let Some(end) = trimmed.rfind('}') {
      return trimmed[start..=end].to_string();
    }
  }
  trimmed.to_string()
}

fn get_runs_list_data(orchestrator: &OrchestratorEngine) -> serde_json::Value {
  let runs: Vec<serde_json::Value> = orchestrator.store.list_runs().iter().map(|r| {
    serde_json::json!({
      "run_id": r.run_id,
      "task": r.task,
      "status": format!("{:?}", r.status),
    })
  }).collect();
  serde_json::json!({ "runs": runs })
}
