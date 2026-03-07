//! Agent process management — spawn, monitor, restart.
//!
//! Each agent runs as a tokio task wrapping a zeptoclaw ZeptoAgent
//! for full conversation history, tool calling, and provider support.

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zeptoclaw::providers::LLMProvider;

use crate::config::AgentConfig;

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

/// Spawn an agent as a tokio task backed by a zeptoclaw ZeptoAgent.
pub fn spawn_agent(
  config: AgentConfig,
  provider: Arc<dyn LLMProvider>,
  state_tx: mpsc::Sender<AgentStateUpdate>,
) -> (AgentHandle, tokio::task::JoinHandle<()>) {
  let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>(64);
  let name = config.name.clone();

  let handle = AgentHandle {
    name: name.clone(),
    cmd_tx,
  };

  let join = tokio::spawn(agent_loop(config, provider, cmd_rx, state_tx));

  (handle, join)
}

/// State update sent from agent process to daemon.
#[derive(Debug)]
pub struct AgentStateUpdate {
  pub name: String,
  pub status: AgentStatus,
  pub error: Option<String>,
  pub tokens_delta: u64,
}

async fn agent_loop(
  config: AgentConfig,
  provider: Arc<dyn LLMProvider>,
  mut cmd_rx: mpsc::Receiver<AgentCommand>,
  state_tx: mpsc::Sender<AgentStateUpdate>,
) {
  let name = config.name.clone();
  let model = config.model.clone();

  // Build ZeptoAgent with conversation history
  let mut builder = zeptoclaw::ZeptoAgentBuilder::new().provider_arc(provider);

  if let Some(ref prompt) = config.system_prompt {
    builder = builder.system_prompt(prompt);
  }
  if let Some(ref m) = model {
    builder = builder.model(m);
  }
  if let Some(max_iter) = config.max_iterations {
    builder = builder.max_iterations(max_iter);
  }

  let agent = match builder.build() {
    Ok(a) => a,
    Err(e) => {
      warn!(agent = %name, error = %e, "failed to build ZeptoAgent");
      let _ = state_tx
        .send(AgentStateUpdate {
          name: name.clone(),
          status: AgentStatus::Error,
          error: Some(format!("build failed: {}", e)),
          tokens_delta: 0,
        })
        .await;
      return;
    }
  };

  let display_model = model.as_deref().unwrap_or("default");
  info!(agent = %name, model = %display_model, "agent process started (zeptoclaw)");

  let _ = state_tx
    .send(AgentStateUpdate {
      name: name.clone(),
      status: AgentStatus::Idle,
      error: None,
      tokens_delta: 0,
    })
    .await;

  loop {
    match cmd_rx.recv().await {
      Some(AgentCommand::UserMessage(msg, resp_tx)) => {
        debug!(agent = %name, "processing user message");

        let _ = state_tx
          .send(AgentStateUpdate {
            name: name.clone(),
            status: AgentStatus::Running,
            error: None,
            tokens_delta: 0,
          })
          .await;

        match agent.chat(&msg).await {
          Ok(response) => {
            info!(agent = %name, "response received");
            debug!(agent = %name, content = %response, "response content");

            if let Some(tx) = resp_tx {
              let _ = tx.send(Ok(response));
            }

            let _ = state_tx
              .send(AgentStateUpdate {
                name: name.clone(),
                status: AgentStatus::Idle,
                error: None,
                tokens_delta: 0,
              })
              .await;
          }
          Err(e) => {
            warn!(agent = %name, error = %e, "agent chat failed");

            if let Some(tx) = resp_tx {
              let _ = tx.send(Err(e.to_string()));
            }

            let _ = state_tx
              .send(AgentStateUpdate {
                name: name.clone(),
                status: AgentStatus::Error,
                error: Some(e.to_string()),
                tokens_delta: 0,
              })
              .await;
          }
        }
      }
      Some(AgentCommand::Stop) => {
        info!(agent = %name, "stop command received");
        break;
      }
      None => {
        debug!(agent = %name, "command channel closed");
        break;
      }
    }
  }

  let _ = state_tx
    .send(AgentStateUpdate {
      name: name.clone(),
      status: AgentStatus::Stopped,
      error: None,
      tokens_delta: 0,
    })
    .await;

  info!(agent = %name, "agent process stopped");
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
