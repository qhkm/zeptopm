<div align="center">

# ⚡ ZeptoPM

**Process Manager for AI agents — like PM2, but for LLMs.**

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.85+-orange.svg)](https://www.rust-lang.org/)
[![macOS](https://img.shields.io/badge/macOS-supported-brightgreen.svg)]()
[![Linux](https://img.shields.io/badge/Linux-supported-brightgreen.svg)]()

**`~7 MB per agent`** · **`11 MB binary`** · **`near-zero idle CPU`** · **`500+ agents on 4 GB RAM`**

[Quick Start](#-quick-start) · [Features](#-features) · [Channels](#-agent-channels) · [HTTP API](#-http-api) · [Config](#-config-reference)

</div>

---

## 📖 The Story

We built [ZeptoClaw](https://github.com/qhkm/zeptoclaw) — an AI agent library in Rust with tool use, multi-provider support, and session management. A single ZeptoClaw agent can spawn sub-agents, delegate tasks, even run agents in parallel. It works great — but everything runs in one process. One agent leaks memory, the whole thing goes down. One agent panics, every agent dies with it.

Then OpenAI launched [Symphony](https://github.com/openai/symphony) — built on Elixir and the BEAM VM. Their insight: turn work into **isolated, autonomous runs** where agents operate independently. The BEAM has been doing this for decades in telecom — thousands of isolated processes, each with its own memory, supervised by a parent that restarts them on failure.

That was the spark. We applied the same model to AI agents:

> 🔸 Each ZeptoClaw agent runs as a **separate OS process** — isolated memory, isolated state, independent crash domains.
>
> 🔸 A **daemon supervisor** watches them all. If one crashes, only that agent restarts. Others keep running.
>
> 🔸 **Message passing** between agents goes through the daemon over JSON lines — never shared memory — just like the BEAM's actor model.

Symphony manages work at a high level. ZeptoPM manages the agents doing the work — process lifecycle, communication, and coordination. Same philosophy, different layer.

---

## ✨ Features

<table>
<tr>
<td width="50%">

🔧 **Config-driven** — define agents in TOML, no code required

🔒 **Process isolation** — separate OS process per agent (~7 MB)

💾 **Session persistence** — agents remember conversations across restarts

🔄 **Automatic restart** with exponential backoff

🔥 **Hot config reload** — add/remove agents without restarting

💰 **Per-agent budget limits** (tokens, USD)

</td>
<td width="50%">

🌐 **Multi-provider** — OpenAI, Anthropic, OpenRouter, Groq, Together

🎯 **[Orchestrated runs](#-agent-channels)** — planner decomposes into parallel jobs with DAG

💬 **[Agent channels](#-agent-channels)** — real-time TurnBased or Stream communication

🔗 **[Pipelines](#-pipeline)** — chain agents sequentially

🤖 **[Orchestrate](#-orchestrate)** — manager delegates via `@agent` mentions

🛡️ **Gateway mode** — API key auth + rate limiting

</td>
</tr>
</table>

---

## 📋 Requirements

- **Rust 1.85+** (`rustup update stable`) — uses edition 2024
- **macOS or Linux** (Windows: untested)
- An API key for at least one [supported LLM provider](#-supported-providers)

---

## 📦 Install

> ZeptoPM is not yet on crates.io. Build from source:

```bash
# Clone the repo and sibling dependencies
git clone https://github.com/qhkm/zeptopm.git
git clone https://github.com/qhkm/zeptoclaw.git
git clone https://github.com/qhkm/zeptocapsule.git

# Build (without capsule isolation — works on any OS)
cd zeptopm
cargo build --release --no-default-features

# Or with capsule isolation (macOS/Linux)
cargo build --release

# Install
cp target/release/zeptopm /usr/local/bin/
```

---

## 🚀 Quick Start

**1. Create a config file:**

```toml
# zeptopm.toml
[providers.openai]
api_key = "$OPENAI_API_KEY"

[[agents]]
name = "researcher"
provider = "openai"
model = "gpt-4o-mini"
system_prompt = "You are a research assistant."
auto_start = true
```

**2. Set your API key and start the daemon:**

```bash
export OPENAI_API_KEY="sk-..."
zeptopm daemon
```

**3. In another terminal — talk to your agent:**

```bash
zeptopm status                                        # check agent status
zeptopm chat researcher "What is quantum computing?"  # chat with agent
zeptopm logs researcher                               # view logs
zeptopm restart researcher                            # restart agent
```

---

## 🔗 Pipeline

Chain agents sequentially — the output of each agent becomes the input to the next.

```bash
# researcher finds info → writer turns it into a blog post
zeptopm pipeline "researcher,writer" "Find key facts about WebAssembly and write a blog post"
```

Agents must be defined in your config. Output: final agent's response to stdout. Use `--json` for machine-readable output.

---

## 🤖 Orchestrate

A manager agent coordinates other agents using `@agent` mentions in its responses.

```bash
# manager delegates research and writing to other agents
zeptopm orchestrate manager "Research Rust async patterns, then write a summary"
```

The manager sees all running agents as tools. Its system prompt should mention it can delegate with `@researcher`, `@writer`, etc. ZeptoPM intercepts these mentions and routes messages to the target agents.

---

## 💬 Agent Channels

Channels enable **real-time communication** between running agents. The orchestrator routes messages — agents themselves are unaware of channels and just see regular chat messages.

### Channel Modes

| Mode | Behavior |
|:-----|:---------|
| 🔄 **TurnBased** | Alternating speakers: A → B → A → B. Stops at `max_rounds`. |
| 📡 **Stream** | Broadcast: sender's message goes to all other participants. |

### Peer Failure Policies

| Policy | Behavior |
|:-------|:---------|
| 💀 **KillAll** (default) | If one participant dies, kill all others. |
| ✅ **Continue** | Survivors get a "peer disconnected" message and keep going. |

### Example: Writer + Reviewer with Live Feedback

Two agents collaborate through a TurnBased channel — the writer drafts, the reviewer gives feedback, the writer revises, and the reviewer approves.

> 📄 Full working config: [`channels-example.toml`](channels-example.toml)

```bash
export OPENAI_API_KEY="sk-..."

# Start the daemon
zeptopm daemon --config channels-example.toml --no-sandbox

# Submit a task (in another terminal)
curl -X POST http://127.0.0.1:9876/runs \
  -H "Content-Type: application/json" \
  -d '{"task": "write and review a short blog post about Rust async patterns"}'

# Check progress
curl http://127.0.0.1:9876/runs/<run_id>

# Get the result (includes full channel conversation history)
curl http://127.0.0.1:9876/runs/<run_id>/result
```

<details>
<summary><b>🔍 What happens under the hood</b></summary>

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

</details>

---

## 🔒 Process Isolation

Each agent runs as a **separate OS process**. One agent crashing or leaking memory cannot affect another.

| Resource | Isolation |
|:---------|:----------|
| 🧠 **Memory** | Separate address space per process |
| 💬 **Conversation history** | Independent per agent |
| 💾 **Session file** | `~/.zeptopm/sessions/{agent_name}.json` |
| 🌐 **LLM provider state** | Own HTTP client and auth context per worker |
| 💥 **Crash blast radius** | Supervisor catches crash, other agents unaffected |

### 🛡️ Capsule Sandbox (optional)

With `--sandbox` or `isolation = "capsule"` in config, orchestrated jobs run inside [ZeptoCapsule](https://github.com/qhkm/zeptocapsule) capsules with enforced memory limits, process count limits, filesystem isolation, and network restrictions.

---

## 📊 Resource Usage

**Measured on macOS (Apple Silicon), release build:**

| Component | RSS (idle) |
|:----------|:----------|
| Daemon (supervisor) | **~7 MB** |
| Each worker process | **~7 MB** |
| Release binary | **~11 MB** |

<details>
<summary><b>📈 Capacity estimates</b></summary>

| Machine RAM | Agents (max) | Agents (comfortable) |
|:------------|:-------------|:---------------------|
| 512 MB | ~70 | 30–50 |
| 1 GB | ~140 | 60–100 |
| 4 GB | ~570 | 300–450 |
| 8 GB | ~1,140 | 600–900 |

- CPU is near-zero while idle — workers block on stdin.
- Memory grows with conversation history. With `max_history = 200`, each agent adds ~40 KB on top of the base ~7 MB.
- The real constraint is **LLM API rate limits and cost**, not local resources.
- A **$5/month VPS** can comfortably run dozens of agents.

</details>

---

## 🖥️ CLI Reference

| Command | Description |
|:--------|:------------|
| `zeptopm daemon` | Start the daemon — runs all `auto_start` agents |
| `zeptopm status` | Show status of all running agents |
| `zeptopm list` | List configured agents (no daemon needed) |
| `zeptopm chat <name> <msg>` | Send a message to an agent |
| `zeptopm logs <name>` | Show recent logs for an agent |
| `zeptopm stop <name>` | Stop a running agent |
| `zeptopm start <name>` | Start an agent |
| `zeptopm restart <name>` | Restart an agent (stop + start) |
| `zeptopm pipeline <agents> <msg>` | Chain agents sequentially |
| `zeptopm orchestrate <manager> <msg>` | Manager delegates to other agents |
| `zeptopm run submit <task>` | Submit an orchestrated multi-agent run |
| `zeptopm run status <run_id>` | Show run progress |
| `zeptopm run result <run_id>` | Print final artifacts |
| `zeptopm run cancel <run_id>` | Cancel a running run |
| `zeptopm run list` | List all runs |
| `zeptopm agent-help` | Print CLI manifest as JSON |

<details>
<summary><b>🚩 Flags</b></summary>

**Global:**

| Flag | Default | Description |
|:-----|:--------|:------------|
| `-c, --config` | `zeptopm.toml` | Config file path |
| `-l, --log-level` | from config | Override log level |
| `--addr` | `127.0.0.1:9876` | Daemon HTTP address |
| `--json` | off | Machine-readable JSON output |

**Daemon:**

| Flag | Description |
|:-----|:------------|
| `--sandbox` | Force capsule isolation |
| `--no-sandbox` | Disable capsule isolation |
| `-b, --bind` | Override bind address |

**Run sub-commands:**

| Flag | Applies to | Description |
|:-----|:-----------|:------------|
| `-t, --tail` | `submit`, `status` | Stream progress in real-time |

</details>

---

## ⚙️ Config Reference

### Basic

```toml
[daemon]
log_level = "info"                  # trace | debug | info | warn | error
log_format = "pretty"               # pretty | compact | json
bind = "127.0.0.1:9876"            # HTTP API bind address
poll_interval_ms = 5000            # Config change polling interval
sessions_dir = "~/.zeptopm/sessions"
max_revisions = 3                   # Max revision rounds per job
run_ttl_days = 7                    # Auto-delete old runs (0 = disabled)

[[agents]]
name = "researcher"                 # Unique agent name
provider = "openai"                 # Must match [providers.*]
model = "gpt-4o-mini"
system_prompt = "You are a research assistant."
auto_start = true                   # Start with daemon
max_restarts = 5                    # Max auto-restarts
restart_backoff_ms = 1000           # Initial backoff (doubles each restart)
max_iterations = 10                 # Max tool-calling iterations
session_persist = true              # Save history across restarts
max_history = 200                   # Keep last N messages

[agents.budget]
token_limit = 100000
cost_limit_usd = 5.00

[agents.gateway]
enabled = true                      # Protect HTTP endpoint
api_key = "$ZEPTOPM_GATEWAY_KEY"
rate_limit = 100                    # Requests per minute

[providers.openai]
api_key = "$OPENAI_API_KEY"         # Supports $ENV_VAR expansion

[providers.anthropic]
api_key = "$ANTHROPIC_API_KEY"

[providers.openrouter]
api_key = "$OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"
```

<details>
<summary><b>🔧 Advanced (capsule isolation)</b></summary>

These options apply when using ZeptoCapsule capsule sandboxing:

```toml
[daemon]
isolation = "none"                  # none | process | namespace | capsule
worker_binary = "/usr/bin/zk-init"  # Init binary for namespace capsules
security = "standard"               # dev | standard | hardened

[[agents]]
memory_mib = 512                    # Memory limit per capsule job (MiB)
max_pids = 64                       # Max process count inside capsule
timeout_sec = 300                   # Wall clock timeout
```

</details>

### 🌐 Supported Providers

| Provider | Config name | Notes |
|:---------|:------------|:------|
| OpenAI | `openai` | GPT models |
| Anthropic | `anthropic` or `claude` | Direct Claude API |
| OpenRouter | `openrouter` | Multi-model gateway |
| Groq | `groq` | Fast inference |
| Together | `together` | Open-source models |
| Custom | any name | Set `base_url` for OpenAI-compatible endpoints |

---

## 🔌 HTTP API

The daemon exposes a REST API (default `127.0.0.1:9876`):

| Endpoint | Method | Description |
|:---------|:-------|:------------|
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
| `/gw/{name}/chat` | POST | Gateway-protected chat |
| `/runs` | POST | Submit orchestrated run |
| `/runs` | GET | List all runs |
| `/runs/{id}` | GET | Run status with job details |
| `/runs/{id}/result` | GET | Final artifacts |
| `/runs/{id}/cancel` | POST | Cancel a running run |

---

## 🏗️ Architecture

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

---

## 🗺️ Roadmap

| Feature | Status |
|:--------|:-------|
| Interactive REPL (`zeptopm repl researcher`) | 🔜 Next |
| Tool use in orchestrated runs | 🔜 Planned |
| Human-in-the-loop pause/resume | 🔜 Planned |
| Conversation checkpointing (resume from last tool call) | 🔜 Planned |
| Web dashboard / observability UI | 🔜 Planned |
| Stream mode channels with 3+ participants | 🔜 Planned |
| Publish to crates.io | ⏳ Blocked (path deps) |
| End-to-end integration test suite | 🔜 Planned |

---

## 🧩 Zepto Stack

ZeptoPM is part of the Zepto stack — a modular system for running AI agents in production.

```
ZeptoPM        — orchestration, supervision, retries, job lifecycle
    │
    │  create(spec) + spawn(worker, args, env)
    ▼
ZeptoCapsule   — capsule creation, process isolation, resource enforcement
    │
    │  fork/namespace/microVM + stdio transport
    ▼
ZeptoClaw      — LLM calls, tool use, artifact production
    │
    └── JSON-line IPC over stdin/stdout back to ZeptoPM
```

| Layer | Repo | Role |
|:------|:-----|:-----|
| **ZeptoPM** | [qhkm/zeptopm](https://github.com/qhkm/zeptopm) | Process manager — config-driven daemon, HTTP API, pipelines, orchestration |
| **ZeptoCapsule** | [qhkm/zeptocapsule](https://github.com/qhkm/zeptocapsule) | Sandbox — process/namespace/Firecracker isolation, resource limits, fallback chains |
| **ZeptoRT** | [qhkm/zeptort](https://github.com/qhkm/zeptort) | Durable runtime — journaled effects, snapshot recovery, OTP-style supervision |
| **ZeptoClaw** | [qhkm/zeptoclaw](https://github.com/qhkm/zeptoclaw) | Agent framework — 32 tools, 9 providers, 9 channels, container isolation |

---

## 🤝 Contributing

```bash
# Run tests
cargo test --no-default-features        # without capsule (works everywhere)
cargo test --features capsule            # with capsule (macOS/Linux)

# Check formatting and lints
cargo fmt -- --check
cargo clippy
```

---

<div align="center">

## 📄 License

[Apache 2.0](LICENSE)

Made with ❤️ and 🦀 by [Aisar Labs](https://github.com/qhkm)

</div>
