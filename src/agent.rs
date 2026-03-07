//! Agent process management — spawn, monitor, restart.

use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::AgentConfig;
use crate::llm::LlmClient;

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
  UserMessage(String),
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
  /// Send a user message to the agent.
  pub async fn send_message(&self, msg: String) -> Result<(), String> {
    self.cmd_tx
      .send(AgentCommand::UserMessage(msg))
      .await
      .map_err(|_| format!("agent '{}' channel closed", self.name))
  }

  /// Stop the agent.
  pub async fn stop(&self) {
    let _ = self.cmd_tx.send(AgentCommand::Stop).await;
  }
}

/// Spawn an agent as a tokio task. Returns a handle and a join handle.
pub fn spawn_agent(
  config: AgentConfig,
  llm_client: LlmClient,
  api_key: String,
  base_url: String,
  state_tx: mpsc::Sender<AgentStateUpdate>,
) -> (AgentHandle, tokio::task::JoinHandle<()>) {
  let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>(64);
  let name = config.name.clone();

  let handle = AgentHandle {
    name: name.clone(),
    cmd_tx,
  };

  let join = tokio::spawn(agent_loop(config, llm_client, api_key, base_url, cmd_rx, state_tx));

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
  client: LlmClient,
  api_key: String,
  base_url: String,
  mut cmd_rx: mpsc::Receiver<AgentCommand>,
  state_tx: mpsc::Sender<AgentStateUpdate>,
) {
  let name = config.name.clone();
  let model = config.model.as_deref().unwrap_or("openai/gpt-4o-mini");

  info!(agent = %name, model = %model, "agent process started");

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
      Some(AgentCommand::UserMessage(msg)) => {
        debug!(agent = %name, "processing user message");

        let _ = state_tx
          .send(AgentStateUpdate {
            name: name.clone(),
            status: AgentStatus::Running,
            error: None,
            tokens_delta: 0,
          })
          .await;

        match client
          .chat(&base_url, &api_key, model, config.system_prompt.as_deref(), &msg)
          .await
        {
          Ok(response) => {
            let tokens = response.usage.as_ref().map(|u| u.total_tokens).unwrap_or(0);
            info!(
              agent = %name,
              tokens = tokens,
              "LLM response received"
            );
            debug!(agent = %name, content = %response.content, "response content");

            let _ = state_tx
              .send(AgentStateUpdate {
                name: name.clone(),
                status: AgentStatus::Idle,
                error: None,
                tokens_delta: tokens,
              })
              .await;
          }
          Err(e) => {
            warn!(agent = %name, error = %e, "LLM call failed");
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
    assert!(matches!(cmd, AgentCommand::UserMessage(ref s) if s == "hello"));
  }
}
