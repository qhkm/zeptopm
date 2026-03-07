//! Status display for agent processes.

use crate::agent::AgentState;

/// Format agent states as a status table.
pub fn format_status_table(agents: &[&AgentState]) -> String {
  if agents.is_empty() {
    return "No agents configured.".into();
  }

  let mut lines = Vec::new();
  lines.push(format!(
    "{:<20} {:<15} {:<8} {:<12} {}",
    "NAME", "STATUS", "RESTARTS", "TOKENS", "LAST ERROR"
  ));
  lines.push("-".repeat(75));

  for agent in agents {
    let error = agent
      .last_error
      .as_deref()
      .unwrap_or("-")
      .chars()
      .take(30)
      .collect::<String>();
    lines.push(format!(
      "{:<20} {:<15} {:<8} {:<12} {}",
      agent.name,
      format!("{}", agent.status),
      agent.restart_count,
      agent.tokens_used,
      error,
    ));
  }

  lines.join("\n")
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::agent::AgentStatus;
  use std::time::Instant;

  #[test]
  fn test_format_empty() {
    let output = format_status_table(&[]);
    assert_eq!(output, "No agents configured.");
  }

  #[test]
  fn test_format_with_agents() {
    let state = AgentState {
      name: "researcher".into(),
      status: AgentStatus::Running,
      restart_count: 0,
      started_at: Some(Instant::now()),
      last_error: None,
      messages_handled: 5,
      tokens_used: 1234,
      logs: vec![],
    };
    let output = format_status_table(&[&state]);
    assert!(output.contains("researcher"));
    assert!(output.contains("running"));
    assert!(output.contains("1234"));
  }
}
