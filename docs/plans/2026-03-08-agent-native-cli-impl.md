# Agent-Native CLI Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `--json` global flag for machine-parseable output and `agent-help` / `--agent-help` for self-describing command discovery to ZeptoPM's CLI.

**Architecture:** A `CliResult` type wraps every command's output as a `serde_json::Value`. A central `output_result()` helper renders it as either a JSON envelope (`{"ok": true, "data": ...}`) or the existing human-formatted text. A `build_manifest()` function returns a static JSON manifest describing all commands, args, output shapes, workflows, and error codes.

**Tech Stack:** Rust, clap (existing), serde_json (existing). No new dependencies.

---

### Task 1: CliError Type and JSON Envelope Helper

Add the core output infrastructure that all other tasks depend on.

**Files:**
- Modify: `src/main.rs`

**Step 1: Write the failing test**

Add a `#[cfg(test)] mod tests` block at the bottom of `src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_error_to_json() {
        let err = CliError {
            message: "Run not found".into(),
            code: "RUN_NOT_FOUND".into(),
        };
        let json = err.to_json();
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"], "Run not found");
        assert_eq!(json["code"], "RUN_NOT_FOUND");
    }

    #[test]
    fn test_format_success_json() {
        let data = serde_json::json!({"run_id": "run_123"});
        let output = format_output_json(&Ok(data.clone()));
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["data"]["run_id"], "run_123");
    }

    #[test]
    fn test_format_error_json() {
        let err = CliError {
            message: "Daemon unreachable".into(),
            code: "DAEMON_UNREACHABLE".into(),
        };
        let output = format_output_json(&Err(err));
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"], "Daemon unreachable");
        assert_eq!(parsed["code"], "DAEMON_UNREACHABLE");
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cd /Users/dr.noranizaahmad/ios/zeptoPM && cargo test --bin zeptopm test_cli_error 2>&1`
Expected: FAIL — `CliError` not defined

**Step 3: Write minimal implementation**

Add these types and functions above the `#[cfg(test)]` block in `src/main.rs`:

```rust
/// Structured CLI error for JSON output mode.
struct CliError {
    message: String,
    code: String,
}

impl CliError {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ok": false,
            "error": self.message,
            "code": self.code,
        })
    }

    fn daemon_unreachable(addr: &str) -> Self {
        CliError {
            message: format!("Failed to connect to daemon at {}", addr),
            code: "DAEMON_UNREACHABLE".into(),
        }
    }

    fn parse_error(detail: &str) -> Self {
        CliError {
            message: format!("Failed to parse response: {}", detail),
            code: "PARSE_ERROR".into(),
        }
    }

    fn not_found(kind: &str, id: &str) -> Self {
        let code = match kind {
            "run" => "RUN_NOT_FOUND",
            "agent" => "AGENT_NOT_FOUND",
            _ => "NOT_FOUND",
        };
        CliError {
            message: format!("{} '{}' not found", kind, id),
            code: code.into(),
        }
    }

    fn invalid_config(detail: &str) -> Self {
        CliError {
            message: format!("Invalid config: {}", detail),
            code: "INVALID_CONFIG".into(),
        }
    }
}

type CliResult = Result<serde_json::Value, CliError>;

/// Format a CliResult as a JSON envelope string.
fn format_output_json(result: &CliResult) -> String {
    match result {
        Ok(data) => {
            let envelope = serde_json::json!({ "ok": true, "data": data });
            serde_json::to_string_pretty(&envelope).unwrap()
        }
        Err(err) => {
            serde_json::to_string_pretty(&err.to_json()).unwrap()
        }
    }
}

/// Output a CliResult — JSON envelope if json_mode, otherwise run the human formatter.
fn output_result(result: CliResult, json_mode: bool, human_fn: impl FnOnce(&serde_json::Value)) {
    match (json_mode, &result) {
        (true, _) => {
            println!("{}", format_output_json(&result));
            if result.is_err() {
                std::process::exit(1);
            }
        }
        (false, Ok(data)) => human_fn(data),
        (false, Err(err)) => {
            eprintln!("Error: {}", err.message);
            std::process::exit(1);
        }
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cd /Users/dr.noranizaahmad/ios/zeptoPM && cargo test --bin zeptopm 2>&1`
Expected: 3 tests PASS

**Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): add CliError type and JSON envelope helpers"
```

---

### Task 2: Add `--json` and `--agent-help` Global Flags

Wire the two new global flags into the Cli struct and add the `AgentHelp` command variant.

**Files:**
- Modify: `src/main.rs` (Cli struct, Commands enum)

**Step 1: Add flags to Cli struct**

In the `Cli` struct, add two new fields after the existing `addr` field:

```rust
  /// Output results as JSON (machine-readable)
  #[arg(long, global = true)]
  json: bool,

  /// Show command schema for AI agents (JSON)
  #[arg(long, global = true)]
  agent_help: bool,
```

Add a new variant to the `Commands` enum:

```rust
  /// Show full CLI manifest for AI agents (JSON)
  AgentHelp,
```

**Step 2: Run to verify it compiles**

Run: `cd /Users/dr.noranizaahmad/ios/zeptoPM && cargo build 2>&1`
Expected: Compiles (new flags are defined but not yet used — clap doesn't warn on unused fields)

**Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): add --json and --agent-help global flags"
```

---

### Task 3: Build the Agent Help Manifest

Create the `build_manifest()` function that returns the full JSON manifest of all commands.

**Files:**
- Modify: `src/main.rs`

**Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn test_manifest_has_all_commands() {
        let manifest = build_manifest();
        let commands = manifest["commands"].as_object().unwrap();

        // All user-facing commands must be present
        let expected = vec![
            "status", "list", "chat", "logs",
            "stop", "start", "restart",
            "pipeline", "orchestrate",
            "run submit", "run status", "run list", "run result", "run cancel",
        ];
        for cmd in &expected {
            assert!(commands.contains_key(*cmd), "missing command: {}", cmd);
        }

        // Each command must have description and output_shape
        for (name, spec) in commands {
            assert!(spec.get("description").is_some(), "{} missing description", name);
            assert!(spec.get("output_shape").is_some(), "{} missing output_shape", name);
        }
    }

    #[test]
    fn test_manifest_has_workflows() {
        let manifest = build_manifest();
        let workflows = manifest["workflows"].as_object().unwrap();
        assert!(workflows.contains_key("submit_and_wait"));
        assert!(workflows.contains_key("agent_chat"));
        assert!(workflows.contains_key("monitor_agents"));

        // Each workflow must have steps
        for (name, wf) in workflows {
            assert!(wf.get("steps").and_then(|s| s.as_array()).is_some(),
                    "{} missing steps array", name);
        }
    }

    #[test]
    fn test_manifest_has_error_codes() {
        let manifest = build_manifest();
        let codes = manifest["error_codes"].as_object().unwrap();
        assert!(codes.contains_key("DAEMON_UNREACHABLE"));
        assert!(codes.contains_key("RUN_NOT_FOUND"));
        assert!(codes.contains_key("AGENT_NOT_FOUND"));
        assert!(codes.contains_key("INVALID_CONFIG"));
        assert!(codes.contains_key("PARSE_ERROR"));
    }

    #[test]
    fn test_manifest_version_present() {
        let manifest = build_manifest();
        assert!(manifest.get("version").and_then(|v| v.as_str()).is_some());
    }
```

**Step 2: Run test to verify it fails**

Run: `cd /Users/dr.noranizaahmad/ios/zeptoPM && cargo test --bin zeptopm test_manifest 2>&1`
Expected: FAIL — `build_manifest` not found

**Step 3: Write the implementation**

Add this function above the `#[cfg(test)]` block in `src/main.rs`:

```rust
/// Build the full CLI manifest for agent discovery.
fn build_manifest() -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "commands": {
            "status": {
                "description": "Show status of all running agents (queries daemon)",
                "args": [],
                "flags": ["--json"],
                "output_shape": {
                    "agents": [{"name": "str", "status": "str", "restarts": "int", "tokens_used": "int", "uptime_secs": "int"}]
                }
            },
            "list": {
                "description": "List configured agents from config file (no daemon needed)",
                "args": [],
                "flags": ["--json"],
                "output_shape": {
                    "agents": [{"name": "str", "auto_start": "bool", "provider": "str", "model": "str"}]
                }
            },
            "chat": {
                "description": "Send a message to an agent and get the response",
                "args": [
                    {"name": "name", "type": "string", "required": true, "description": "Agent name"},
                    {"name": "message", "type": "string", "required": true, "description": "Message to send"}
                ],
                "flags": ["--json"],
                "output_shape": {
                    "response": "str"
                }
            },
            "stop": {
                "description": "Stop a running agent",
                "args": [{"name": "name", "type": "string", "required": true, "description": "Agent name"}],
                "flags": ["--json"],
                "output_shape": {"status": "str"}
            },
            "start": {
                "description": "Start an agent (must be defined in config)",
                "args": [{"name": "name", "type": "string", "required": true, "description": "Agent name"}],
                "flags": ["--json"],
                "output_shape": {"status": "str"}
            },
            "restart": {
                "description": "Restart an agent (stop + start)",
                "args": [{"name": "name", "type": "string", "required": true, "description": "Agent name"}],
                "flags": ["--json"],
                "output_shape": {"status": "str"}
            },
            "logs": {
                "description": "Show recent logs for an agent",
                "args": [{"name": "name", "type": "string", "required": true, "description": "Agent name"}],
                "flags": ["--json"],
                "output_shape": {
                    "logs": [{"timestamp": "str", "level": "str", "message": "str"}]
                }
            },
            "pipeline": {
                "description": "Chain agents in a pipeline (output of one feeds into the next)",
                "args": [
                    {"name": "agents", "type": "string", "required": true, "description": "Comma-separated agent names"},
                    {"name": "message", "type": "string", "required": true, "description": "Message to start the pipeline"}
                ],
                "flags": ["--json"],
                "output_shape": {
                    "steps": [{"agent": "str", "response": "str"}]
                }
            },
            "orchestrate": {
                "description": "Orchestrate multi-agent collaboration (manager delegates to other agents)",
                "args": [
                    {"name": "manager", "type": "string", "required": true, "description": "Manager agent name"},
                    {"name": "message", "type": "string", "required": true, "description": "Task for the manager"}
                ],
                "flags": ["--json"],
                "output_shape": {
                    "response": "str",
                    "delegations": [{"to": "str", "query": "str", "result": "str"}],
                    "rounds": "int"
                }
            },
            "run submit": {
                "description": "Submit a new orchestrated run",
                "args": [{"name": "task", "type": "string", "required": true, "description": "Task description"}],
                "flags": ["--tail", "--json"],
                "output_shape": {"run_id": "str"}
            },
            "run status": {
                "description": "Check status of a run",
                "args": [{"name": "run_id", "type": "string", "required": true, "description": "Run ID"}],
                "flags": ["--tail", "--json"],
                "output_shape": {
                    "run_id": "str",
                    "status": "str",
                    "task": "str",
                    "jobs": [{"job_id": "str", "role": "str", "status": "str", "instruction": "str"}]
                }
            },
            "run list": {
                "description": "List all runs",
                "args": [],
                "flags": ["--json"],
                "output_shape": {
                    "runs": [{"run_id": "str", "status": "str", "task": "str"}]
                }
            },
            "run result": {
                "description": "Print final artifact content for a completed run",
                "args": [{"name": "run_id", "type": "string", "required": true, "description": "Run ID"}],
                "flags": ["--json"],
                "output_shape": {
                    "status": "str",
                    "artifacts": [{"kind": "str", "summary": "str", "path": "str", "content": "str"}]
                }
            },
            "run cancel": {
                "description": "Cancel a running run (cancels all active jobs)",
                "args": [{"name": "run_id", "type": "string", "required": true, "description": "Run ID"}],
                "flags": ["--json"],
                "output_shape": {"status": "str"}
            }
        },
        "workflows": {
            "submit_and_wait": {
                "description": "Submit a run and poll until completion, then get results",
                "steps": [
                    "zeptopm run submit \"<task>\" --json  →  {\"ok\": true, \"data\": {\"run_id\": \"...\"}}",
                    "zeptopm run status <run_id> --json  →  poll until data.status is \"Completed\", \"Failed\", or \"Cancelled\"",
                    "zeptopm run result <run_id> --json  →  {\"ok\": true, \"data\": {\"artifacts\": [...]}}"
                ]
            },
            "agent_chat": {
                "description": "Find an available agent and chat with it",
                "steps": [
                    "zeptopm status --json  →  find agents where status is \"running\"",
                    "zeptopm chat <name> \"<message>\" --json  →  {\"ok\": true, \"data\": {\"response\": \"...\"}}"
                ]
            },
            "monitor_agents": {
                "description": "Check agent health and investigate issues",
                "steps": [
                    "zeptopm status --json  →  check all agent health (restarts, uptime)",
                    "zeptopm logs <name> --json  →  get recent log entries for a specific agent"
                ]
            }
        },
        "error_codes": {
            "DAEMON_UNREACHABLE": "Daemon is not running or address is wrong. Start with: zeptopm daemon",
            "RUN_NOT_FOUND": "No run with this ID exists",
            "AGENT_NOT_FOUND": "No agent with this name in config or not running",
            "INVALID_CONFIG": "Config file is missing or has validation errors",
            "PARSE_ERROR": "Failed to parse response from daemon"
        }
    })
}
```

**Step 4: Run test to verify it passes**

Run: `cd /Users/dr.noranizaahmad/ios/zeptoPM && cargo test --bin zeptopm test_manifest 2>&1`
Expected: 4 tests PASS

**Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): build_manifest with commands, workflows, error codes"
```

---

### Task 4: Wire `agent-help` Command and `--agent-help` Flag

Connect the manifest to the CLI — `agent-help` dumps the full manifest, `--agent-help` on any command dumps that command's entry.

**Files:**
- Modify: `src/main.rs` (main function match arms)

**Step 1: Add the `AgentHelp` handler**

In the `main()` function's `match cli.command` block, add a new arm before the `None` arm:

```rust
    Some(Commands::AgentHelp) => {
        let manifest = build_manifest();
        println!("{}", serde_json::to_string_pretty(&manifest).unwrap());
    }
```

**Step 2: Add `--agent-help` interception**

At the very top of `main()`, right after `let cli = Cli::parse();`, add this block to intercept `--agent-help` before any command runs:

```rust
    if cli.agent_help {
        let manifest = build_manifest();
        // Determine which command is being asked about
        let cmd_name = match &cli.command {
            Some(Commands::Status) => Some("status"),
            Some(Commands::List) => Some("list"),
            Some(Commands::Chat { .. }) => Some("chat"),
            Some(Commands::Stop { .. }) => Some("stop"),
            Some(Commands::Start { .. }) => Some("start"),
            Some(Commands::Restart { .. }) => Some("restart"),
            Some(Commands::Logs { .. }) => Some("logs"),
            Some(Commands::Pipeline { .. }) => Some("pipeline"),
            Some(Commands::Orchestrate { .. }) => Some("orchestrate"),
            Some(Commands::Run { action }) => {
                Some(match action {
                    RunAction::Submit { .. } => "run submit",
                    RunAction::Status { .. } => "run status",
                    RunAction::List => "run list",
                    RunAction::Result { .. } => "run result",
                    RunAction::Cancel { .. } => "run cancel",
                })
            }
            _ => None,
        };

        if let Some(name) = cmd_name {
            if let Some(cmd_spec) = manifest["commands"].get(name) {
                let entry = serde_json::json!({ name: cmd_spec });
                println!("{}", serde_json::to_string_pretty(&entry).unwrap());
            } else {
                println!("{}", serde_json::to_string_pretty(&manifest).unwrap());
            }
        } else {
            // No specific command — dump full manifest
            println!("{}", serde_json::to_string_pretty(&manifest).unwrap());
        }
        return;
    }
```

**Step 3: Run to verify it compiles**

Run: `cd /Users/dr.noranizaahmad/ios/zeptoPM && cargo build 2>&1`
Expected: Compiles successfully

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): wire agent-help command and --agent-help flag"
```

---

### Task 5: Convert Simple Commands to Use `--json` Flag

Convert `status`, `list`, `stop`, `start`, `restart`, `logs`, `chat` commands to use `output_result()` with the `--json` flag.

**Files:**
- Modify: `src/main.rs`

**Step 1: Refactor `status` command**

Replace the `Some(Commands::Status)` match arm with:

```rust
    Some(Commands::Status) => {
        let result = match http_get(&cli.addr, "/status").await {
            Ok(body) => {
                serde_json::from_str::<serde_json::Value>(&body)
                    .map_err(|e| CliError::parse_error(&e.to_string()))
            }
            Err(e) => Err(CliError::daemon_unreachable(&cli.addr)),
        };
        output_result(result, cli.json, |data| {
            if let Some(agents) = data.get("agents").and_then(|a| a.as_array()) {
                if agents.is_empty() {
                    println!("No agents running.");
                    return;
                }
                println!(
                    "{:<20} {:<15} {:<8} {:<12} {}",
                    "NAME", "STATUS", "RESTARTS", "TOKENS", "UPTIME"
                );
                println!("{}", "-".repeat(70));
                for agent in agents {
                    let name = agent.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let status = agent.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                    let restarts = agent.get("restart_count").and_then(|v| v.as_u64()).unwrap_or(0);
                    let tokens = agent.get("tokens_used").and_then(|v| v.as_u64()).unwrap_or(0);
                    let uptime = agent
                        .get("uptime_secs")
                        .and_then(|v| v.as_u64())
                        .map(format_uptime)
                        .unwrap_or_else(|| "-".into());
                    println!(
                        "{:<20} {:<15} {:<8} {:<12} {}",
                        name, status, restarts, tokens, uptime
                    );
                }
            }
        });
    }
```

**Step 2: Refactor `list` command**

Replace the `Some(Commands::List)` arm:

```rust
    Some(Commands::List) => {
        let result = match zeptopm::config::load_config(&cli.config) {
            Ok(config) => {
                let agents: Vec<serde_json::Value> = config.agents.iter().map(|a| {
                    serde_json::json!({
                        "name": a.name,
                        "auto_start": a.auto_start,
                        "provider": a.provider,
                        "model": a.model.as_deref().unwrap_or("default"),
                    })
                }).collect();
                Ok(serde_json::json!({ "agents": agents }))
            }
            Err(e) => Err(CliError::invalid_config(&e.to_string())),
        };
        output_result(result, cli.json, |data| {
            if let Some(agents) = data.get("agents").and_then(|v| v.as_array()) {
                for agent in agents {
                    let name = agent.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let auto = if agent.get("auto_start").and_then(|v| v.as_bool()).unwrap_or(false) { "auto" } else { "manual" };
                    let provider = agent.get("provider").and_then(|v| v.as_str()).unwrap_or("?");
                    let model = agent.get("model").and_then(|v| v.as_str()).unwrap_or("default");
                    println!("{:<20} {:<10} provider={:<15} model={}", name, auto, provider, model);
                }
            }
        });
    }
```

**Step 3: Refactor `chat` command**

Replace the `Some(Commands::Chat { name, message })` arm:

```rust
    Some(Commands::Chat { name, message }) => {
        let body = serde_json::json!({ "message": message });
        let result = match http_post(&cli.addr, &format!("/agents/{}/chat", name), &body).await {
            Ok(resp_body) => {
                let resp: serde_json::Value = serde_json::from_str(&resp_body)
                    .map_err(|e| CliError::parse_error(&e.to_string()))?;
                if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
                    Err(CliError::not_found("agent", &name))
                } else {
                    Ok(resp)
                }
            }
            Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
        };
        output_result(result, cli.json, |data| {
            if let Some(content) = data.get("response").and_then(|v| v.as_str()) {
                println!("{}", content);
            }
        });
    }
```

**Note:** The `chat` handler uses `?` on the JSON parse. Since `main()` returns `()`, not `Result`, this won't work directly. Instead, nest the parse inside a match or use `unwrap_or`. The actual pattern should be:

```rust
    Some(Commands::Chat { name, message }) => {
        let body = serde_json::json!({ "message": message });
        let result = match http_post(&cli.addr, &format!("/agents/{}/chat", name), &body).await {
            Ok(resp_body) => {
                match serde_json::from_str::<serde_json::Value>(&resp_body) {
                    Ok(resp) => {
                        if resp.get("error").and_then(|v| v.as_str()).is_some() {
                            Err(CliError::not_found("agent", &name))
                        } else {
                            Ok(resp)
                        }
                    }
                    Err(e) => Err(CliError::parse_error(&e.to_string())),
                }
            }
            Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
        };
        output_result(result, cli.json, |data| {
            if let Some(content) = data.get("response").and_then(|v| v.as_str()) {
                println!("{}", content);
            }
        });
    }
```

**Step 4: Refactor `stop`, `start`, `restart` commands**

These three are nearly identical. Replace each with the same pattern (shown for `stop`, repeat for `start` and `restart` changing only the endpoint path):

```rust
    Some(Commands::Stop { name }) => {
        let result = match http_post(&cli.addr, &format!("/agents/{}/stop", name), &serde_json::json!({})).await {
            Ok(resp_body) => {
                match serde_json::from_str::<serde_json::Value>(&resp_body) {
                    Ok(resp) => {
                        if resp.get("error").and_then(|v| v.as_str()).is_some() {
                            Err(CliError::not_found("agent", &name))
                        } else {
                            Ok(resp)
                        }
                    }
                    Err(e) => Err(CliError::parse_error(&e.to_string())),
                }
            }
            Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
        };
        output_result(result, cli.json, |data| {
            if let Some(status) = data.get("status").and_then(|v| v.as_str()) {
                println!("{}", status);
            }
        });
    }
```

**Step 5: Refactor `logs` command**

```rust
    Some(Commands::Logs { name }) => {
        let result = match http_get(&cli.addr, &format!("/agents/{}/logs", name)).await {
            Ok(body) => {
                match serde_json::from_str::<serde_json::Value>(&body) {
                    Ok(resp) => {
                        if resp.get("error").and_then(|v| v.as_str()).is_some() {
                            Err(CliError::not_found("agent", &name))
                        } else {
                            Ok(resp)
                        }
                    }
                    Err(e) => Err(CliError::parse_error(&e.to_string())),
                }
            }
            Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
        };
        output_result(result, cli.json, |data| {
            if let Some(logs) = data.get("logs").and_then(|v| v.as_array()) {
                if logs.is_empty() {
                    println!("No logs for agent '{}'.", name);
                    return;
                }
                for entry in logs {
                    let ts = entry.get("timestamp").and_then(|v| v.as_str()).unwrap_or("?");
                    let level = entry.get("level").and_then(|v| v.as_str()).unwrap_or("?");
                    let msg = entry.get("message").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("{} [{}] {}", ts, level, msg);
                }
            }
        });
    }
```

**Step 6: Run to verify it compiles and existing tests pass**

Run: `cd /Users/dr.noranizaahmad/ios/zeptoPM && cargo build && cargo test 2>&1`
Expected: Compiles, all tests pass

**Step 7: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): --json support for status, list, chat, stop, start, restart, logs"
```

---

### Task 6: Convert Run Commands to Use `--json` Flag

Convert `run submit`, `run status`, `run list`, `run result`, `run cancel` to use `output_result()`.

**Files:**
- Modify: `src/main.rs`

**Step 1: Refactor `run submit`**

```rust
        RunAction::Submit { task, tail } => {
            let body = serde_json::json!({ "task": task });
            let result = match http_post(&cli.addr, "/runs", &body).await {
                Ok(resp_body) => {
                    match serde_json::from_str::<serde_json::Value>(&resp_body) {
                        Ok(resp) => {
                            if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
                                Err(CliError { message: error.into(), code: "SUBMIT_FAILED".into() })
                            } else {
                                Ok(resp)
                            }
                        }
                        Err(e) => Err(CliError::parse_error(&e.to_string())),
                    }
                }
                Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
            };

            let run_id = result.as_ref().ok()
                .and_then(|d| d.get("run_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            output_result(result, cli.json, |data| {
                if let Some(id) = data.get("run_id").and_then(|v| v.as_str()) {
                    println!("Run submitted: {}", id);
                }
            });

            if tail && !run_id.is_empty() {
                tail_run(&cli.addr, &run_id).await;
            }
        }
```

**Step 2: Refactor `run status`**

```rust
        RunAction::Status { run_id, tail } => {
            let result = match http_get(&cli.addr, &format!("/runs/{}", run_id)).await {
                Ok(resp_body) => {
                    match serde_json::from_str::<serde_json::Value>(&resp_body) {
                        Ok(resp) => {
                            if resp.get("error").and_then(|v| v.as_str()).is_some() {
                                Err(CliError::not_found("run", &run_id))
                            } else {
                                Ok(resp)
                            }
                        }
                        Err(e) => Err(CliError::parse_error(&e.to_string())),
                    }
                }
                Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
            };
            output_result(result, cli.json, |data| {
                // Reuse existing human formatter
                let body = serde_json::to_string(data).unwrap_or_default();
                print_run_status(&body);
            });
            if tail {
                tail_run(&cli.addr, &run_id).await;
            }
        }
```

**Step 3: Refactor `run list`**

```rust
        RunAction::List => {
            let result = match http_get(&cli.addr, "/runs").await {
                Ok(resp_body) => {
                    serde_json::from_str::<serde_json::Value>(&resp_body)
                        .map_err(|e| CliError::parse_error(&e.to_string()))
                }
                Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
            };
            output_result(result, cli.json, |data| {
                if let Some(runs) = data.get("runs").and_then(|v| v.as_array()) {
                    if runs.is_empty() {
                        println!("No runs.");
                        return;
                    }
                    println!("{:<24} {:<12} {}", "RUN ID", "STATUS", "TASK");
                    println!("{}", "-".repeat(70));
                    for run in runs {
                        let id = run.get("run_id").and_then(|v| v.as_str()).unwrap_or("?");
                        let status = run.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        let task = run.get("task").and_then(|v| v.as_str()).unwrap_or("");
                        let short_task: String = task.chars().take(40).collect();
                        println!("{:<24} {:<12} {}", id, status, short_task);
                    }
                }
            });
        }
```

**Step 4: Refactor `run result`**

```rust
        RunAction::Result { run_id } => {
            let result = match http_get(&cli.addr, &format!("/runs/{}/result", run_id)).await {
                Ok(resp_body) => {
                    match serde_json::from_str::<serde_json::Value>(&resp_body) {
                        Ok(resp) => {
                            if resp.get("error").and_then(|v| v.as_str()).is_some() {
                                Err(CliError::not_found("run", &run_id))
                            } else {
                                Ok(resp)
                            }
                        }
                        Err(e) => Err(CliError::parse_error(&e.to_string())),
                    }
                }
                Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
            };
            output_result(result, cli.json, |data| {
                let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                println!("Run: {}  Status: {}", run_id, status);
                if let Some(artifacts) = data.get("artifacts").and_then(|v| v.as_array()) {
                    for artifact in artifacts {
                        let kind = artifact.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                        let summary = artifact.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                        let path = artifact.get("path").and_then(|v| v.as_str()).unwrap_or("");
                        println!("\n--- artifact ({}) ---", kind);
                        if !summary.is_empty() {
                            println!("Summary: {}", summary);
                        }
                        if !path.is_empty() {
                            if let Ok(content) = std::fs::read_to_string(path) {
                                println!("{}", content);
                            } else {
                                println!("(file: {})", path);
                            }
                        }
                    }
                }
            });
        }
```

**Step 5: Refactor `run cancel`**

```rust
        RunAction::Cancel { run_id } => {
            let result = match http_post(&cli.addr, &format!("/runs/{}/cancel", run_id), &serde_json::json!({})).await {
                Ok(resp_body) => {
                    match serde_json::from_str::<serde_json::Value>(&resp_body) {
                        Ok(resp) => {
                            if resp.get("error").and_then(|v| v.as_str()).is_some() {
                                Err(CliError::not_found("run", &run_id))
                            } else {
                                Ok(resp)
                            }
                        }
                        Err(e) => Err(CliError::parse_error(&e.to_string())),
                    }
                }
                Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
            };
            output_result(result, cli.json, |data| {
                if let Some(status) = data.get("status").and_then(|v| v.as_str()) {
                    println!("{}", status);
                }
            });
        }
```

**Step 6: Run to verify it compiles and tests pass**

Run: `cd /Users/dr.noranizaahmad/ios/zeptoPM && cargo build && cargo test 2>&1`
Expected: Compiles, all tests pass

**Step 7: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): --json support for run submit/status/list/result/cancel"
```

---

### Task 7: Convert Pipeline and Orchestrate Commands to Use `--json` Flag

Convert the remaining `pipeline` and `orchestrate` commands.

**Files:**
- Modify: `src/main.rs`

**Step 1: Refactor `pipeline` command**

```rust
    Some(Commands::Pipeline { agents, message }) => {
        let agent_names: Vec<&str> = agents.split(',').map(|s| s.trim()).collect();
        if agent_names.is_empty() {
            let result: CliResult = Err(CliError { message: "No agents specified".into(), code: "INVALID_CONFIG".into() });
            output_result(result, cli.json, |_| {});
            return;
        }

        let mut steps = Vec::new();
        let mut current_message = message.clone();
        let mut failed = false;

        for (i, agent_name) in agent_names.iter().enumerate() {
            let body = serde_json::json!({ "message": current_message });
            match http_post(&cli.addr, &format!("/agents/{}/chat", agent_name), &body).await {
                Ok(resp_body) => {
                    let resp: serde_json::Value = serde_json::from_str(&resp_body).unwrap_or_default();
                    if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
                        let result: CliResult = Err(CliError::not_found("agent", agent_name));
                        output_result(result, cli.json, |_| {
                            eprintln!("Error from {}: {}", agent_name, error);
                        });
                        failed = true;
                        break;
                    }
                    let response = resp.get("response").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    steps.push(serde_json::json!({
                        "agent": agent_name,
                        "response": response,
                    }));
                    if i < agent_names.len() - 1 {
                        current_message = format!(
                            "Previous step (from {}): {}\n\nContinue with the original task: {}",
                            agent_name, response, message
                        );
                    }
                }
                Err(_) => {
                    let result: CliResult = Err(CliError::daemon_unreachable(&cli.addr));
                    output_result(result, cli.json, |_| {});
                    failed = true;
                    break;
                }
            }
        }

        if !failed {
            let result: CliResult = Ok(serde_json::json!({ "steps": steps }));
            output_result(result, cli.json, |data| {
                if let Some(steps) = data.get("steps").and_then(|v| v.as_array()) {
                    for (i, step) in steps.iter().enumerate() {
                        let agent = step.get("agent").and_then(|v| v.as_str()).unwrap_or("?");
                        let response = step.get("response").and_then(|v| v.as_str()).unwrap_or("");
                        println!("--- [{}] {} ---", i + 1, agent);
                        println!("{}\n", response);
                    }
                }
            });
        }
    }
```

**Step 2: Refactor `orchestrate` command**

```rust
    Some(Commands::Orchestrate { manager, message }) => {
        let body = serde_json::json!({ "message": message });
        let result = match http_post(&cli.addr, &format!("/orchestrate/{}", manager), &body).await {
            Ok(resp_body) => {
                match serde_json::from_str::<serde_json::Value>(&resp_body) {
                    Ok(resp) => {
                        if resp.get("error").and_then(|v| v.as_str()).is_some() {
                            Err(CliError::not_found("agent", &manager))
                        } else {
                            Ok(resp)
                        }
                    }
                    Err(e) => Err(CliError::parse_error(&e.to_string())),
                }
            }
            Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
        };
        output_result(result, cli.json, |data| {
            if let Some(delegations) = data.get("delegations").and_then(|v| v.as_array()) {
                if !delegations.is_empty() {
                    println!("--- delegations ---");
                    for d in delegations {
                        let to = d.get("to").and_then(|v| v.as_str()).unwrap_or("?");
                        let query = d.get("query").and_then(|v| v.as_str()).unwrap_or("?");
                        let result = d.get("result").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("  -> @{}: {}", to, query);
                        println!("  <- {}", result);
                        println!();
                    }
                    println!("--- final response ---");
                }
            }
            if let Some(response) = data.get("response").and_then(|v| v.as_str()) {
                println!("{}", response);
            }
            if let Some(rounds) = data.get("rounds").and_then(|v| v.as_u64()) {
                if rounds > 1 {
                    eprintln!("\n({} rounds)", rounds);
                }
            }
        });
    }
```

**Step 3: Run to verify it compiles and tests pass**

Run: `cd /Users/dr.noranizaahmad/ios/zeptoPM && cargo build && cargo test 2>&1`
Expected: Compiles, all tests pass

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): --json support for pipeline and orchestrate"
```

---

### Task 8: Update Docs and Final Verification

Update CLAUDE.md and TODO.md, then run full test suite.

**Files:**
- Modify: `CLAUDE.md`
- Modify: `TODO.md`

**Step 1: Update CLAUDE.md**

Add to the CLI commands section:

```
zeptopm agent-help                # full CLI manifest for AI agents (JSON)
zeptopm status --json             # machine-readable JSON output
zeptopm status --agent-help       # command schema for AI agents
```

**Step 2: Update TODO.md**

Add a new section for Agent-Native CLI and mark it complete. Update test counts.

**Step 3: Run full test suite**

Run: `cd /Users/dr.noranizaahmad/ios/zeptoPM && cargo test 2>&1`
Expected: All tests pass (74 existing + 7 new = ~81 tests)

**Step 4: Commit**

```bash
git add CLAUDE.md TODO.md
git commit -m "docs: agent-native CLI — --json flag, agent-help, command discovery"
```
