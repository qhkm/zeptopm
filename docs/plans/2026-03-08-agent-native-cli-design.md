# Agent-Native CLI Design

## Goal

Make zeptoPM's CLI easy for AI agents (Claude Code, Cursor, Devin, etc.) to drive programmatically. Two capabilities: machine-parseable structured output, and self-describing command discovery.

## Architecture

### Global `--json` Flag

A global `--json` flag on the `Cli` struct. When set, every command outputs a consistent JSON envelope to stdout:

**Success:**
```json
{"ok": true, "data": { ... }}
```

**Error:**
```json
{"ok": false, "error": "Run not found", "code": "RUN_NOT_FOUND"}
```

Exit codes still work (0 = success, 1 = error). The flag only changes output format, not behavior.

**Implementation:** Each command handler produces a `serde_json::Value` result. A helper function `output_result(result, json_mode)` wraps rendering — JSON envelope in `--json` mode, existing human tables otherwise. This avoids duplicating logic per command.

### Error Codes

Simple string constants for agent branching:
- `DAEMON_UNREACHABLE` — daemon not running or wrong address
- `RUN_NOT_FOUND` — no run with this ID
- `AGENT_NOT_FOUND` — no agent with this name
- `INVALID_CONFIG` — config file error
- `PARSE_ERROR` — malformed response

### `agent-help` Command

Top-level `zeptopm agent-help` outputs a JSON manifest of the entire CLI. Always JSON (no `--json` needed).

The manifest includes:
- **version** — CLI version string
- **commands** — every command with description, args (name, type, required), flags, and output_shape (field names + types)
- **workflows** — multi-step recipes (e.g. "submit_and_wait": submit → poll status → get result)
- **error_codes** — all error codes with descriptions

### Per-Command `--agent-help`

Global `--agent-help` flag. When passed, prints just that command's schema slice from the manifest instead of executing.

```bash
zeptopm run submit --agent-help
```

Outputs the `run submit` entry only.

**Implementation:** The manifest is a function returning `serde_json::Value`, built from static data. `agent-help` dumps it all. `--agent-help` filters to the matching command entry.

## Commands with `--json` Support

| Command | Output Shape |
|---------|-------------|
| `status` | `{agents: [{name, status, restarts, tokens_used, uptime_secs}]}` |
| `list` | `{agents: [{name, auto_start, provider, model}]}` |
| `chat` | `{response: str}` |
| `logs` | `{logs: [{timestamp, level, message}]}` |
| `run submit` | `{run_id: str}` |
| `run status` | `{run_id, status, task, jobs: [{job_id, role, status, instruction}]}` |
| `run list` | `{runs: [{run_id, status, task}]}` |
| `run result` | `{status, artifacts: [{kind, summary, path, content}]}` |
| `run cancel` | `{status: str}` |
| `pipeline` | `{steps: [{agent, response}]}` |
| `orchestrate` | `{response, delegations: [{to, query, result}], rounds}` |
| `stop/start/restart` | `{status: str}` |

**Excluded:** `daemon` (long-running), `worker` (internal/hidden).

## Workflows in Manifest

### submit_and_wait
1. `zeptopm run submit "task" --json` -> get `run_id`
2. `zeptopm run status <run_id> --json` -> poll until status is `Completed`/`Failed`/`Cancelled`
3. `zeptopm run result <run_id> --json` -> get artifact content

### agent_chat
1. `zeptopm status --json` -> find available agents
2. `zeptopm chat <name> "message" --json` -> get response

### monitor_agents
1. `zeptopm status --json` -> check all agent health
2. `zeptopm logs <name> --json` -> investigate issues

## Testing

- Unit tests for manifest builder (all commands covered, valid JSON)
- Unit tests for JSON envelope helper (success + error cases)
- Integration test that parses `agent-help` output and checks expected structure

## Constraints

- No new dependencies
- Default behavior unchanged (no `--json` = same human output as today)
- REST API unchanged (already JSON)
- Manifest is compile-time static data, no runtime cost
