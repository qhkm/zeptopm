//! Agent process management — spawn, monitor, restart.
//!
//! Each agent runs as a separate OS process (`zeptopm worker`),
//! communicating with the supervisor via JSON lines over stdin/stdout.

use std::collections::HashMap;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Runtime state of a managed agent.
#[derive(Debug, Clone)]
pub struct AgentState {
  pub name: String,
  pub status: AgentStatus,
  pub restart_count: u32,
  pub started_at: Option<Instant>,
  pub last_error: Option<String>,
  pub messages_handled: u64,
  pub tokens_used: u64,
  pub logs: Vec<LogEntry>,
  pub pid: Option<u32>,
}

/// A single log entry for an agent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogEntry {
  pub timestamp: String,
  pub level: String,
  pub message: String,
}

pub const MAX_LOG_ENTRIES: usize = 100;

fn make_log(level: &str, message: &str) -> LogEntry {
  LogEntry {
    timestamp: chrono::Utc::now().to_rfc3339(),
    level: level.to_string(),
    message: message.to_string(),
  }
}

pub fn push_log(logs: &mut Vec<LogEntry>, entry: LogEntry) {
  if logs.len() >= MAX_LOG_ENTRIES {
    logs.remove(0);
  }
  logs.push(entry);
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentStatus {
  Starting,
  Running,
  Idle,
  Error,
  Stopped,
  RestartPending,
}

impl std::fmt::Display for AgentStatus {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      AgentStatus::Starting => write!(f, "starting"),
      AgentStatus::Running => write!(f, "running"),
      AgentStatus::Idle => write!(f, "idle"),
      AgentStatus::Error => write!(f, "error"),
      AgentStatus::Stopped => write!(f, "stopped"),
      AgentStatus::RestartPending => write!(f, "restart-pending"),
    }
  }
}

/// Messages that can be sent to an agent process.
#[derive(Debug)]
pub enum AgentCommand {
  /// Send a user message to the agent for LLM processing.
  /// The optional sender receives the LLM response content.
  UserMessage(String, Option<tokio::sync::oneshot::Sender<Result<String, String>>>),
  /// Stop the agent gracefully.
  Stop,
}

/// Handle to a running agent process.
#[derive(Clone)]
pub struct AgentHandle {
  pub name: String,
  pub cmd_tx: mpsc::Sender<AgentCommand>,
}

impl AgentHandle {
  /// Send a user message to the agent (fire-and-forget).
  pub async fn send_message(&self, msg: String) -> Result<(), String> {
    self.cmd_tx
      .send(AgentCommand::UserMessage(msg, None))
      .await
      .map_err(|_| format!("agent '{}' channel closed", self.name))
  }

  /// Send a user message and wait for the LLM response.
  pub async fn chat(&self, msg: String) -> Result<String, String> {
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    self.cmd_tx
      .send(AgentCommand::UserMessage(msg, Some(resp_tx)))
      .await
      .map_err(|_| format!("agent '{}' channel closed", self.name))?;
    resp_rx
      .await
      .map_err(|_| format!("agent '{}' response channel dropped", self.name))?
  }

  /// Stop the agent.
  pub async fn stop(&self) {
    let _ = self.cmd_tx.send(AgentCommand::Stop).await;
  }
}

/// State update sent from worker bridge to daemon.
#[derive(Debug)]
pub struct AgentStateUpdate {
  pub name: String,
  pub status: AgentStatus,
  pub error: Option<String>,
  pub tokens_delta: u64,
  pub log: Option<LogEntry>,
  pub pid: Option<u32>,
}

/// Spawn an agent as a separate OS process (`zeptopm worker`).
pub fn spawn_agent(
  agent_name: &str,
  config_path: &str,
  state_tx: mpsc::Sender<AgentStateUpdate>,
) -> (AgentHandle, tokio::task::JoinHandle<()>) {
  spawn_agent_with_orch(agent_name, config_path, state_tx, None)
}

/// Spawn an agent with an optional orchestrator event channel.
pub fn spawn_agent_with_orch(
  agent_name: &str,
  config_path: &str,
  state_tx: mpsc::Sender<AgentStateUpdate>,
  orch_event_tx: Option<mpsc::Sender<serde_json::Value>>,
) -> (AgentHandle, tokio::task::JoinHandle<()>) {
  let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>(64);

  let handle = AgentHandle {
    name: agent_name.to_string(),
    cmd_tx,
  };

  let join = tokio::spawn(worker_bridge(
    agent_name.to_string(),
    config_path.to_string(),
    cmd_rx,
    state_tx,
    orch_event_tx,
  ));

  (handle, join)
}

/// Bridge between the in-process mpsc channel and a child worker process.
/// Translates AgentCommands to JSON stdin, and JSON stdout back to state updates.
async fn worker_bridge(
  agent_name: String,
  config_path: String,
  mut cmd_rx: mpsc::Receiver<AgentCommand>,
  state_tx: mpsc::Sender<AgentStateUpdate>,
  orch_event_tx: Option<mpsc::Sender<serde_json::Value>>,
) {
  let exe = match std::env::current_exe() {
    Ok(e) => e,
    Err(e) => {
      warn!(agent = %agent_name, error = %e, "failed to get current executable path");
      let _ = state_tx
        .send(AgentStateUpdate {
          name: agent_name,
          status: AgentStatus::Error,
          error: Some(format!("cannot find executable: {}", e)),
          tokens_delta: 0,
          log: Some(make_log("error", &format!("cannot find executable: {}", e))),
          pid: None,
        })
        .await;
      return;
    }
  };

  // Canonicalize config path so the worker resolves it correctly
  let abs_config = std::fs::canonicalize(&config_path)
    .unwrap_or_else(|_| std::path::PathBuf::from(&config_path));

  let mut child = match tokio::process::Command::new(&exe)
    .arg("worker")
    .arg("--agent")
    .arg(&agent_name)
    .arg("--config")
    .arg(&abs_config)
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::inherit())
    .spawn()
  {
    Ok(c) => c,
    Err(e) => {
      warn!(agent = %agent_name, error = %e, "failed to spawn worker process");
      let _ = state_tx
        .send(AgentStateUpdate {
          name: agent_name,
          status: AgentStatus::Error,
          error: Some(format!("spawn failed: {}", e)),
          tokens_delta: 0,
          log: Some(make_log("error", &format!("spawn failed: {}", e))),
          pid: None,
        })
        .await;
      return;
    }
  };

  let child_pid = child.id();
  info!(agent = %agent_name, pid = ?child_pid, "worker process spawned");

  let _ = state_tx
    .send(AgentStateUpdate {
      name: agent_name.clone(),
      status: AgentStatus::Starting,
      error: None,
      tokens_delta: 0,
      log: Some(make_log("info", &format!("worker spawned (pid={})", child_pid.unwrap_or(0)))),
      pid: child_pid,
    })
    .await;

  let child_stdin = child.stdin.take().expect("child stdin");
  let child_stdout = child.stdout.take().expect("child stdout");

  let mut stdin_writer = BufWriter::new(child_stdin);
  let stdout_reader = BufReader::new(child_stdout);

  // Background task: read stdout lines from worker
  let (msg_tx, mut msg_rx) = mpsc::channel::<serde_json::Value>(64);
  let reader_name = agent_name.clone();
  tokio::spawn(async move {
    let mut lines = stdout_reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
      match serde_json::from_str::<serde_json::Value>(&line) {
        Ok(msg) => {
          if msg_tx.send(msg).await.is_err() {
            break;
          }
        }
        Err(e) => {
          debug!(agent = %reader_name, error = %e, line = %line, "invalid worker output");
        }
      }
    }
  });

  // Pending chat requests: id -> oneshot sender
  let mut pending: HashMap<String, tokio::sync::oneshot::Sender<Result<String, String>>> =
    HashMap::new();
  let mut req_counter: u64 = 0;

  loop {
    tokio::select! {
      // Commands from the daemon/server
      cmd = cmd_rx.recv() => {
        match cmd {
          Some(AgentCommand::UserMessage(msg, resp_tx)) => {
            req_counter += 1;
            let id = format!("req-{}", req_counter);

            if let Some(tx) = resp_tx {
              pending.insert(id.clone(), tx);
            }

            let cmd_json = serde_json::json!({"cmd":"chat","id":id,"message":msg});
            let line = format!("{}\n", cmd_json);
            if stdin_writer.write_all(line.as_bytes()).await.is_err() {
              warn!(agent = %agent_name, "failed to write to worker stdin");
              break;
            }
            let _ = stdin_writer.flush().await;
          }
          Some(AgentCommand::Stop) => {
            let cmd_json = serde_json::json!({"cmd":"stop"});
            let line = format!("{}\n", cmd_json);
            let _ = stdin_writer.write_all(line.as_bytes()).await;
            let _ = stdin_writer.flush().await;
            // Don't break yet — wait for the worker to send "stopped" status
          }
          None => {
            // Command channel closed — supervisor is shutting down
            break;
          }
        }
      }

      // Messages from the worker process
      msg = msg_rx.recv() => {
        match msg {
          Some(msg) => {
            let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match msg_type {
              "ready" => {
                debug!(agent = %agent_name, "worker ready");
              }
              "chat_response" => {
                let id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if let Some(tx) = pending.remove(&id) {
                  if let Some(error) = msg.get("error").and_then(|v| v.as_str()) {
                    let _ = tx.send(Err(error.to_string()));
                  } else {
                    let response = msg.get("response").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let _ = tx.send(Ok(response));
                  }
                }
              }
              "status" => {
                let status_str = msg.get("status").and_then(|v| v.as_str()).unwrap_or("idle");
                let error = msg.get("error").and_then(|v| v.as_str()).map(String::from);
                let status = match status_str {
                  "idle" => AgentStatus::Idle,
                  "running" => AgentStatus::Running,
                  "error" => AgentStatus::Error,
                  "stopped" => AgentStatus::Stopped,
                  _ => AgentStatus::Idle,
                };

                let is_stopped = status == AgentStatus::Stopped;

                let _ = state_tx
                  .send(AgentStateUpdate {
                    name: agent_name.clone(),
                    status,
                    error,
                    tokens_delta: 0,
                    log: None,
                    pid: child_pid,
                  })
                  .await;

                if is_stopped {
                  break;
                }
              }
              "log" => {
                let level = msg.get("level").and_then(|v| v.as_str()).unwrap_or("info");
                let message = msg.get("message").and_then(|v| v.as_str()).unwrap_or("");
                let _ = state_tx
                  .send(AgentStateUpdate {
                    name: agent_name.clone(),
                    status: AgentStatus::Idle,
                    error: None,
                    tokens_delta: 0,
                    log: Some(make_log(level, message)),
                    pid: child_pid,
                  })
                  .await;
              }
              "artifact_produced" | "job_completed" | "job_failed" => {
                let _ = state_tx
                  .send(AgentStateUpdate {
                    name: agent_name.clone(),
                    status: AgentStatus::Idle,
                    error: None,
                    tokens_delta: 0,
                    log: Some(make_log("info", &format!("{}: {}",
                      msg_type,
                      msg.get("job_id").and_then(|v| v.as_str()).unwrap_or("?")))),
                    pid: child_pid,
                  })
                  .await;
                // Forward to orchestrator engine
                if let Some(ref orch_tx) = orch_event_tx {
                  let _ = orch_tx.send(msg.clone()).await;
                }
              }
              _ => {
                debug!(agent = %agent_name, msg_type = msg_type, "unknown worker message type");
              }
            }
          }
          None => {
            // Worker stdout closed — process exited
            warn!(agent = %agent_name, "worker process stdout closed");
            break;
          }
        }
      }
    }
  }

  // Fail any pending requests
  for (_, tx) in pending.drain() {
    let _ = tx.send(Err(format!("agent '{}' worker process exited", agent_name)));
  }

  // Wait for child to exit
  match child.wait().await {
    Ok(status) => {
      info!(agent = %agent_name, exit_code = ?status.code(), "worker process exited");
      if !status.success() {
        let _ = state_tx
          .send(AgentStateUpdate {
            name: agent_name.clone(),
            status: AgentStatus::Error,
            error: Some(format!("worker exited with {}", status)),
            tokens_delta: 0,
            log: Some(make_log("error", &format!("worker exited with {}", status))),
            pid: child_pid,
          })
          .await;
      }
    }
    Err(e) => {
      warn!(agent = %agent_name, error = %e, "failed to wait for worker");
    }
  }

  let _ = state_tx
    .send(AgentStateUpdate {
      name: agent_name.clone(),
      status: AgentStatus::Stopped,
      error: None,
      tokens_delta: 0,
      log: Some(make_log("info", "worker process stopped")),
      pid: child_pid,
    })
    .await;

  info!(agent = %agent_name, "worker bridge stopped");
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_agent_status_display() {
    assert_eq!(format!("{}", AgentStatus::Running), "running");
    assert_eq!(format!("{}", AgentStatus::Idle), "idle");
    assert_eq!(format!("{}", AgentStatus::Stopped), "stopped");
  }

  #[tokio::test]
  async fn test_agent_handle_stop() {
    let (tx, mut rx) = mpsc::channel(16);
    let handle = AgentHandle {
      name: "test".into(),
      cmd_tx: tx,
    };
    handle.stop().await;
    let cmd = rx.recv().await.unwrap();
    assert!(matches!(cmd, AgentCommand::Stop));
  }

  #[tokio::test]
  async fn test_agent_handle_send_message() {
    let (tx, mut rx) = mpsc::channel(16);
    let handle = AgentHandle {
      name: "test".into(),
      cmd_tx: tx,
    };
    handle.send_message("hello".into()).await.unwrap();
    let cmd = rx.recv().await.unwrap();
    assert!(matches!(cmd, AgentCommand::UserMessage(ref s, _) if s == "hello"));
  }
}
