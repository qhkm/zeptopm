# zeptoPM

Process manager for AI agents — like PM2, but for LLMs.

Configure agents in TOML. Run a daemon. Agents run as managed processes with automatic restart and config hot-reload.

## Quick Start

```bash
# 1. Configure your agents
cp zeptopm.toml my-agents.toml
# Edit: set your API keys, models, and system prompts

# 2. Set your API key
export OPENROUTER_API_KEY="sk-or-..."

# 3. Start the daemon
cargo run -- daemon -c my-agents.toml
```

## Config

```toml
[daemon]
poll_interval_ms = 5000
log_level = "info"

[[agents]]
name = "researcher"
provider = "openrouter"
model = "anthropic/claude-sonnet-4"
system_prompt = "You are a research assistant."
auto_start = true
max_restarts = 5

[agents.budget]
token_limit = 100000
cost_limit_usd = 5.00

[providers.openrouter]
api_key = "$OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"
```

## CLI

```bash
zeptopm daemon              # start all auto_start agents
zeptopm status              # show configured agents
zeptopm list                # list agent names
```

## Features

- Config-driven agent management (no code required)
- Automatic restart with exponential backoff
- Hot config reload (add/remove agents without restart)
- Per-agent budget limits (tokens, USD)
- Multiple provider support (OpenRouter, OpenAI, etc.)
- `$ENV_VAR` expansion for API keys

## Architecture

```
zeptopm.toml → Config Parser → Daemon Loop → Agent Processes
                                    ↑              ↓
                              Config Watcher    LLM Client (reqwest)
                              (hot reload)         ↓
                                              OpenRouter / OpenAI API
```

## Relationship to zeptoRT

zeptoPM is the simple, PM2-style process manager. For durable execution, journal/replay, supervision trees, and multi-agent coordination, see [zeptoRT](../zeptoclaw-rt/).

```
zeptoPM (simple)     →  zeptoRT (enterprise)
PM2 for agents          Temporal for agents
Config-driven           SDK-driven
Restart on failure      Replay from journal
```
