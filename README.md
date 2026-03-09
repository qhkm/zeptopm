# ZeptoPM

**Process manager for AI agents — like PM2, but for LLMs.**

ZeptoPM is a lightweight daemon that manages AI agent processes. Define agents in a TOML config file, and ZeptoPM handles process lifecycle, automatic restarts, session persistence, and inter-agent communication. Each agent runs as an isolated OS process (~4 MB RSS), communicating with the daemon over JSON lines on stdio.

Beyond single-agent management, ZeptoPM orchestrates multi-agent workflows. A planner decomposes complex tasks into parallel jobs with dependency graphs. Pipelines chain agents sequentially. Real-time channels let agents collaborate through turn-based dialogue or broadcast streams — all routed transparently so agents never need to know about each other.

## Why ZeptoPM?

| | PM2 / systemd | ZeptoPM |
|---|---|---|
| Agent config | Code + process config | Single TOML file |
| Inter-agent communication | DIY (queues, APIs, glue code) | Built-in channels, pipelines, orchestration |
| LLM-specific features | None | Budget limits, session persistence, orchestrated runs |
| Multi-provider | Manual per-agent | OpenAI, Anthropic, OpenRouter, Groq, Together — one config |
| Process isolation | Shared environment | Per-agent OS process, optional capsule sandbox |

## Features

- **Config-driven** — define agents in TOML, no code required
- **Process isolation** — each agent runs as a separate OS process (~4 MB each)
- **Session persistence** — agents remember conversations across restarts
- **Automatic restart** with exponential backoff
- **Hot config reload** — add/remove agents without restarting the daemon
- **Per-agent budget limits** (tokens, USD)
- **Multi-provider support** — OpenAI, Anthropic, OpenRouter, Groq, Together, or any OpenAI-compatible API
- **Orchestrated runs** — submit complex tasks, planner decomposes into parallel jobs with dependency graphs
- **[Agent channels](#agent-channels)** — real-time TurnBased or Stream communication between running agents
- **[Pipelines](#pipeline)** — chain agents sequentially (output of one feeds into the next)
- **[Orchestrate](#orchestrate)** — manager agent delegates work to other agents via `@agent` mentions
- **Gateway mode** — protect the HTTP API with API key auth and rate limiting
- **Capsule sandbox** — optional process isolation via [ZeptoKernel](https://github.com/qhkm/zeptokernel) (macOS/Linux)
- **REST API** on port 9876 for programmatic control
- **`$ENV_VAR` expansion** for API keys in config

## Requirements

- **Rust 1.85+** (`rustup update stable`) — uses edition 2024
- **macOS or Linux** (Windows: untested)
- An API key for at least one [supported LLM provider](#supported-providers)

## Install

ZeptoPM is not yet on crates.io (local dependencies on `zeptoclaw` and `zeptokernel`). Build from source:

```bash
# Clone the repo and sibling dependencies
git clone https://github.com/qhkm/zeptopm.git
git clone https://github.com/qhkm/zeptoclaw.git
git clone https://github.com/qhkm/zeptokernel.git

# Build (without capsule isolation — works on any OS)
cd zeptopm
cargo build --release --no-default-features

# Or with capsule isolation (macOS/Linux)
cargo build --release

# The binary is at target/release/zeptopm
cp target/release/zeptopm /usr/local/bin/
```

## Quick Start

```bash
# 1. Create a config file
cat > zeptopm.toml <<'EOF'
[providers.openai]
api_key = "$OPENAI_API_KEY"

[[agents]]
name = "researcher"
provider = "openai"
model = "gpt-4o-mini"
system_prompt = "You are a research assistant."
auto_start = true
EOF

# 2. Set your API key
export OPENAI_API_KEY="sk-..."

# 3. Start the daemon
zeptopm daemon
```

In another terminal:

```bash
# Check status
zeptopm status

# Chat with an agent
zeptopm chat researcher "What is quantum computing?"

# View agent logs
zeptopm logs researcher

# Restart / stop / start an agent
zeptopm restart researcher
zeptopm stop researcher
zeptopm start researcher
```

## Pipeline

Chain agents sequentially — the output of each agent becomes the input to the next.

Agents must be defined in your config file. The pipeline passes each agent's response as the message to the next agent in the chain.

```bash
# researcher finds info → writer turns it into a blog post
zeptopm pipeline "researcher,writer" "Find key facts about WebAssembly and write a blog post"
```

**Output:** The final agent's response is printed to stdout. Use `--json` for machine-readable output.

## Orchestrate

A manager agent coordinates other agents using `@agent` mentions in its responses. The manager sees all other running agents as tools it can delegate to.

```bash
# manager delegates research and writing to other agents
zeptopm orchestrate manager "Research Rust async patterns, then write a summary"
```

The manager's system prompt should mention it can delegate with `@researcher`, `@writer`, etc. ZeptoPM intercepts these mentions and routes messages to the target agents.

## Agent Channels

Channels enable real-time communication between running agents. The orchestrator routes messages — agents themselves are unaware of channels and just see regular chat messages.

### Channel Modes

| Mode | Behavior |
|------|----------|
| **TurnBased** | Alternating speakers: A → B → A → B. Stops at `max_rounds`. |
| **Stream** | Broadcast: sender's message goes to all other participants. |

### Peer Failure Policies

| Policy | Behavior |
|--------|----------|
| **KillAll** (default) | If one participant dies, kill all others. |
| **Continue** | Survivors get a "peer disconnected" message and keep going. |

### Example: Writer + Reviewer with Live Feedback

Two agents collaborate through a TurnBased channel — the writer drafts content, the reviewer gives feedback, the writer revises, and the reviewer approves.

A complete working config is included at [`channels-example.toml`](channels-example.toml). The key idea: the planner agent outputs a JSON execution plan that includes a `channels` array connecting agents.

**Run it:**

```bash
export OPENAI_API_KEY="sk-..."

# Start the daemon
zeptopm daemon --config channels-example.toml --no-sandbox

# In another terminal — submit a task
curl -X POST http://127.0.0.1:9876/runs \
  -H "Content-Type: application/json" \
  -d '{"task": "write and review a short blog post about Rust async patterns"}'
# → {"run_id":"run_..."}

# Check progress
curl http://127.0.0.1:9876/runs/<run_id>

# Get the result (includes full channel conversation history)
curl http://127.0.0.1:9876/runs/<run_id>/result
```

**What happens:**

```
1. Planner creates execution plan:
   - writer job (no dependencies)
   - reviewer job (no dependencies)
   - TurnBased channel "draft-review" connecting both, max 2 rounds

2. Both agents start in parallel. Channel activates when both are Running.

3. Channel conversation:
   Round 0: writer  → writes initial blog post
            reviewer → gives feedback (add examples, define terms)
   Round 1: writer  → revises incorporating all feedback
            reviewer → approves the revision

4. Channel closes at max_rounds. Both agents complete.
   Artifacts contain the full channel conversation history.
```

## Process Isolation

Each agent runs as a **separate OS process**. Memory, conversation history, and session storage are fully isolated between agents — one agent crashing or leaking memory cannot affect another.

| Resource | Isolation |
|----------|-----------|
| **Memory** | Separate address space per process |
| **Conversation history** | Independent per agent |
| **Session file** | `~/.zeptopm/sessions/{agent_name}.json` — one file per agent |
| **LLM provider state** | Each worker creates its own HTTP client and auth context |
| **Crash blast radius** | Worker crash is caught by supervisor, other agents unaffected |

The daemon supervisor communicates with each worker over JSON lines on stdin/stdout. Workers never share state directly.

### Capsule Sandbox (optional)

With `--sandbox` or `isolation = "capsule"` in config, orchestrated jobs run inside [ZeptoKernel](https://github.com/qhkm/zeptokernel) capsules with enforced memory limits, process count limits, filesystem isolation, and network restrictions. Requires building with the `capsule` feature (default).

## Resource Usage

**Measured on macOS (Apple Silicon), release build:**

| Component | RSS (idle) |
|-----------|-----------|
| Daemon (supervisor) | ~4 MB |
| Each worker process | ~4 MB |
| Release binary | ~11 MB |

### Capacity Estimates

| Machine RAM | Agents (theoretical max) | Agents (comfortable) |
|-------------|--------------------------|----------------------|
| 512 MB | ~120 | 50–80 |
| 1 GB | ~250 | 100–150 |
| 4 GB | ~1,000 | 500–800 |
| 8 GB | ~2,000 | 1,000+ |

**Notes:**
- CPU usage is near-zero while idle — workers block on stdin waiting for commands.
- Memory grows with conversation history. With `max_history = 200` and typical messages (~200 bytes each), each agent adds ~40 KB on top of the base ~4 MB.
- The real constraint for most deployments is **LLM API rate limits and cost**, not local resources. A $5/month VPS can comfortably run dozens of agents.

## CLI Reference

| Command | Description |
|---------|-------------|
| `zeptopm daemon` | Start the daemon — runs all `auto_start` agents |
| `zeptopm status` | Show status of all running agents (queries daemon) |
| `zeptopm list` | List configured agents (from config file, no daemon needed) |
| `zeptopm chat <name> <msg>` | Send a message to an agent and get the response |
| `zeptopm logs <name>` | Show recent logs for an agent |
| `zeptopm stop <name>` | Stop a running agent |
| `zeptopm start <name>` | Start an agent (must be defined in config) |
| `zeptopm restart <name>` | Restart an agent (stop + start) |
| `zeptopm pipeline <agents> <msg>` | Chain agents — output of one feeds into the next |
| `zeptopm orchestrate <manager> <msg>` | Manager agent delegates to other agents |
| `zeptopm run submit <task>` | Submit an orchestrated multi-agent run |
| `zeptopm run status <run_id>` | Show run progress (jobs, artifacts) |
| `zeptopm run result <run_id>` | Print final artifact content for a completed run |
| `zeptopm run cancel <run_id>` | Cancel a running run (stops all active jobs) |
| `zeptopm run list` | List all runs |
| `zeptopm agent-help` | Print CLI manifest as JSON (for AI agent integration) |

### Global Flags

| Flag | Default | Description |
|------|---------|-------------|
| `-c, --config` | `zeptopm.toml` | Config file path |
| `-l, --log-level` | from config | Override log level (trace/debug/info/warn/error) |
| `--addr` | `127.0.0.1:9876` | Daemon HTTP address |
| `--json` | off | Output results as JSON (machine-readable) |
| `--agent-help` | off | Show command schema for AI agents (JSON) |

### Daemon Flags

| Flag | Description |
|------|-------------|
| `--sandbox` | Force capsule isolation for orchestrated jobs |
| `--no-sandbox` | Disable capsule isolation — run jobs as plain child processes |
| `-b, --bind` | Override server bind address |

### Run Sub-command Flags

| Flag | Applies to | Description |
|------|-----------|-------------|
| `-t, --tail` | `run submit`, `run status` | Stream run progress in real-time |

## Config Reference

### Basic

```toml
[daemon]
log_level = "info"                  # trace | debug | info | warn | error
log_format = "pretty"               # pretty | compact | json
bind = "127.0.0.1:9876"            # HTTP API bind address
poll_interval_ms = 5000            # How often to check for config changes
sessions_dir = "~/.zeptopm/sessions"  # Where session files are stored
max_revisions = 3                   # Max revision rounds per job
run_ttl_days = 7                    # Auto-delete old runs (0 = disabled)

[[agents]]
name = "researcher"                 # Unique agent name
provider = "openai"                 # Provider name (must match [providers.*])
model = "gpt-4o-mini"              # Model identifier
system_prompt = "You are a research assistant."
auto_start = true                   # Start automatically with daemon
max_restarts = 5                    # Max auto-restarts on failure
restart_backoff_ms = 1000           # Initial backoff (doubles each restart)
max_iterations = 10                 # Max tool-calling iterations per message
session_persist = true              # Save conversation history across restarts
max_history = 200                   # Keep last N messages (omit for unlimited)

[agents.budget]
token_limit = 100000                # Max tokens per agent
cost_limit_usd = 5.00               # Max cost per agent

[agents.gateway]
enabled = true                      # Protect this agent's HTTP endpoint
api_key = "$ZEPTOPM_GATEWAY_KEY"   # Required for /gw/{name}/chat
rate_limit = 100                    # Requests per minute

[providers.openai]
api_key = "$OPENAI_API_KEY"         # Supports $ENV_VAR expansion

[providers.anthropic]
api_key = "$ANTHROPIC_API_KEY"

[providers.openrouter]
api_key = "$OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"
```

### Advanced (capsule isolation)

These options apply when using ZeptoKernel capsule sandboxing:

```toml
[daemon]
isolation = "none"                  # none | process | namespace | capsule
worker_binary = "/usr/bin/zk-init"  # Init binary for namespace capsules
security = "standard"               # dev | standard | hardened

[[agents]]
memory_mib = 512                    # Memory limit per capsule job (MiB)
max_pids = 64                       # Max process count inside capsule
timeout_sec = 300                   # Wall clock timeout for capsule jobs
```

### Supported Providers

| Provider | Config name | Notes |
|----------|-------------|-------|
| OpenAI | `openai` | GPT models |
| Anthropic | `anthropic` or `claude` | Direct Claude API |
| OpenRouter | `openrouter` | Multi-model gateway |
| Groq | `groq` | Fast inference |
| Together | `together` | Open-source models |
| Custom | any name | Set `base_url` for OpenAI-compatible endpoints |

## HTTP API

The daemon exposes a REST API (default `127.0.0.1:9876`):

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Health check |
| `/status` | GET | All agents status |
| `/metrics` | GET | Prometheus-style metrics |
| `/agents/{name}/status` | GET | Single agent status |
| `/agents/{name}/chat` | POST | Send message, get response |
| `/agents/{name}/logs` | GET | Recent agent logs |
| `/agents/{name}/stop` | POST | Stop agent |
| `/agents/{name}/start` | POST | Start agent |
| `/agents/{name}/restart` | POST | Restart agent |
| `/orchestrate/{name}` | POST | Orchestrate via manager agent |
| `/gw/{name}/chat` | POST | Gateway-protected chat (requires API key) |
| `/runs` | POST | Submit orchestrated run |
| `/runs` | GET | List all runs |
| `/runs/{id}` | GET | Run status with job details |
| `/runs/{id}/result` | GET | Final artifacts for a completed run |
| `/runs/{id}/cancel` | POST | Cancel a running run |

## Architecture

```
zeptopm.toml → Config Parser → Daemon (supervisor)
                                    ↑
                              Config Watcher (hot reload)
                              HTTP API (port 9876)
                                    ↓
                    ┌───────────────┼───────────────┐
                    ↓               ↓               ↓
              Worker Process   Worker Process   Worker Process
              (agent "foo")    (agent "bar")    (agent "baz")
                    ↓               ↓               ↓
              JSON lines       JSON lines       JSON lines
              over stdio       over stdio       over stdio
                    ↓               ↓               ↓
              ZeptoAgent       ZeptoAgent       ZeptoAgent
                    ↓               ↓               ↓
              LLM Provider     LLM Provider     LLM Provider
                    ↓               ↓               ↓
              ~/.zeptopm/sessions/{agent}.json (persistent history)

          Channel Router (orchestrator-routed):
              agent "bar" ←──TurnBased──→ agent "baz"
                    ↑                          ↑
                    └──── daemon routes ────────┘
```

## Contributing

```bash
# Run tests
cargo test --no-default-features        # without capsule (works everywhere)
cargo test --features capsule            # with capsule (macOS/Linux)

# Check formatting and lints
cargo fmt -- --check
cargo clippy
```

## License

[MIT](LICENSE)
