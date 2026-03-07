//! Worker process — runs a single agent in its own OS process.
//!
//! Communicates with the supervisor via JSON lines over stdin/stdout.
//! Stderr is used for panic/debug output only.

use std::io::Write;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::config;
use crate::provider::create_provider;

/// Run the worker process for a single agent.
pub async fn run(agent_name: String, config_path: String) {
  let config = match config::load_config(&config_path) {
    Ok(c) => c,
    Err(e) => {
      send_status("error", Some(&format!("config error: {}", e)));
      std::process::exit(1);
    }
  };

  let agent_config = match config.agents.iter().find(|a| a.name == agent_name) {
    Some(c) => c.clone(),
    None => {
      send_status("error", Some(&format!("agent '{}' not found in config", agent_name)));
      std::process::exit(1);
    }
  };

  let provider = match create_provider(&agent_config, &config) {
    Ok(p) => p,
    Err(e) => {
      send_status("error", Some(&e));
      std::process::exit(1);
    }
  };

  // Resolve session persistence
  let session_file = if agent_config.session_persist {
    Some(config::session_file(&config, &agent_name))
  } else {
    None
  };
  let max_history = agent_config.max_history;

  // Load saved history if session persistence is enabled
  let saved_history = session_file
    .as_ref()
    .and_then(|path| load_history(path))
    .unwrap_or_default();

  let history_len = saved_history.len();

  let mut builder = zeptoclaw::ZeptoAgentBuilder::new().provider_arc(provider);
  if let Some(ref prompt) = agent_config.system_prompt {
    builder = builder.system_prompt(prompt);
  }
  if let Some(ref m) = agent_config.model {
    builder = builder.model(m);
  }
  if let Some(max_iter) = agent_config.max_iterations {
    builder = builder.max_iterations(max_iter);
  }
  if !saved_history.is_empty() {
    builder = builder.with_history(saved_history);
  }

  let agent = match builder.build() {
    Ok(a) => a,
    Err(e) => {
      send_status("error", Some(&format!("build failed: {}", e)));
      std::process::exit(1);
    }
  };

  let model = agent_config.model.as_deref().unwrap_or("default");
  send(&serde_json::json!({"type":"ready"}));
  if history_len > 0 {
    send_log("info", &format!(
      "worker started (model={}, pid={}, restored {} messages)",
      model, std::process::id(), history_len
    ));
  } else {
    send_log("info", &format!("worker started (model={}, pid={})", model, std::process::id()));
  }
  send_status("idle", None);

  let stdin = BufReader::new(tokio::io::stdin());
  let mut lines = stdin.lines();

  while let Ok(Some(line)) = lines.next_line().await {
    let cmd: serde_json::Value = match serde_json::from_str(&line) {
      Ok(v) => v,
      Err(_) => continue,
    };

    match cmd.get("cmd").and_then(|v| v.as_str()) {
      Some("chat") => {
        let id = cmd
          .get("id")
          .and_then(|v| v.as_str())
          .unwrap_or("0")
          .to_string();
        let message = cmd
          .get("message")
          .and_then(|v| v.as_str())
          .unwrap_or("")
          .to_string();

        send_status("running", None);
        send_log("info", "processing message");

        match agent.chat(&message).await {
          Ok(response) => {
            send(&serde_json::json!({
              "type": "chat_response",
              "id": id,
              "response": response
            }));
            send_log("info", "response delivered");
          }
          Err(e) => {
            send(&serde_json::json!({
              "type": "chat_response",
              "id": id,
              "error": e.to_string()
            }));
            send_log("error", &format!("chat failed: {}", e));
          }
        }

        // Save history after each chat (truncated if max_history set)
        if let Some(ref path) = session_file {
          let history = agent.history().await;
          save_history(path, &truncate_history(history, max_history));
        }

        send_status("idle", None);
      }
      Some("job_execute") => {
        let job_id = cmd.get("job_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let instruction = cmd.get("instruction").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let workspace = cmd.get("workspace").and_then(|v| v.as_str()).unwrap_or("/tmp").to_string();
        let input_artifacts: Vec<String> = cmd.get("input_artifacts")
          .and_then(|v| v.as_array())
          .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
          .unwrap_or_default();

        send_status("running", None);
        send_log("info", &format!("executing job {}", job_id));

        // Build context from input artifacts
        let mut context = instruction.clone();
        for artifact_path in &input_artifacts {
          if let Ok(content) = std::fs::read_to_string(artifact_path) {
            context.push_str(&format!("\n\n--- Input from previous step ---\n{}", content));
          }
        }

        // Create workspace directory
        std::fs::create_dir_all(&workspace).ok();

        match agent.chat(&context).await {
          Ok(response) => {
            // Write output artifact
            let artifact_path = format!("{}/output.md", workspace);
            std::fs::write(&artifact_path, &response).ok();
            let artifact_id = format!("art_{}", job_id);

            send(&serde_json::json!({
              "type": "artifact_produced",
              "job_id": job_id,
              "artifact_id": artifact_id,
              "kind": "markdown",
              "path": artifact_path,
              "summary": response.chars().take(200).collect::<String>()
            }));

            send(&serde_json::json!({
              "type": "job_completed",
              "job_id": job_id,
              "output_artifact_ids": [artifact_id]
            }));
            send_log("info", &format!("job {} completed", job_id));
          }
          Err(e) => {
            send(&serde_json::json!({
              "type": "job_failed",
              "job_id": job_id,
              "error": e.to_string(),
              "retryable": true
            }));
            send_log("error", &format!("job {} failed: {}", job_id, e));
          }
        }

        send_status("idle", None);
      }
      Some("stop") => {
        // Save history before stopping (truncated if max_history set)
        if let Some(ref path) = session_file {
          let history = agent.history().await;
          let history = truncate_history(history, max_history);
          save_history(path, &history);
          send_log("info", &format!("session saved ({} messages)", history.len()));
        }
        send_log("info", "stop received");
        send_status("stopped", None);
        break;
      }
      _ => {}
    }
  }
}

/// Truncate history to at most `max` messages (keeping the most recent).
/// Always keeps an even number of messages to preserve user/assistant pairs.
fn truncate_history(
  history: Vec<zeptoclaw::session::Message>,
  max: Option<usize>,
) -> Vec<zeptoclaw::session::Message> {
  let Some(max) = max else { return history };
  let len = history.len();
  if len <= max {
    return history;
  }
  // Round max down to even to keep user/assistant pairs intact
  let max = max & !1;
  if max == 0 {
    return Vec::new();
  }
  history.into_iter().skip(len - max).collect()
}

/// Load conversation history from a JSON file.
fn load_history(path: &PathBuf) -> Option<Vec<zeptoclaw::session::Message>> {
  if !path.exists() {
    return None;
  }
  let content = std::fs::read_to_string(path).ok()?;
  serde_json::from_str(&content).ok()
}

/// Save conversation history to a JSON file.
fn save_history(path: &PathBuf, history: &[zeptoclaw::session::Message]) {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).ok();
  }
  if let Ok(json) = serde_json::to_string_pretty(history) {
    std::fs::write(path, json).ok();
  }
}

fn send(msg: &serde_json::Value) {
  let stdout = std::io::stdout();
  let mut lock = stdout.lock();
  writeln!(lock, "{}", msg).ok();
  lock.flush().ok();
}

fn send_status(status: &str, error: Option<&str>) {
  match error {
    Some(e) => send(&serde_json::json!({"type":"status","status":status,"error":e})),
    None => send(&serde_json::json!({"type":"status","status":status})),
  }
}

fn send_log(level: &str, message: &str) {
  send(&serde_json::json!({"type":"log","level":level,"message":message}));
}
