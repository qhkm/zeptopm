# zeptoPM

Process manager for AI agents — like PM2, but for LLMs.

Configure agents in TOML. Run a daemon. Agents run as managed processes with conversation history, automatic restart, and config hot-reload.

## Install

```bash
cargo install zeptopm
```

## Quick Start

```bash
# 1. Create a config file
cat > zeptopm.toml <<'EOF'
[daemon]
poll_interval_ms = 5000
log_level = "info"

[providers.openrouter]
api_key = "$OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"

[[agents]]
name = "researcher"
provider = "openrouter"
model = "anthropic/claude-sonnet-4"
system_prompt = "You are a research assistant."
auto_start = true
max_restarts = 5
EOF

# 2. Set your API key
export OPENROUTER_API_KEY="sk-or-..."

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

# Restart an agent
zeptopm restart researcher

# Stop an agent
zeptopm stop researcher

# Start a stopped agent
zeptopm start researcher
```

## CLI Reference

| Command | Description |
|---------|-------------|
| `zeptopm daemon` | Start the daemon — runs all `auto_start` agents |
| `zeptopm status` | Show status of all running agents |
| `zeptopm list` | List configured agents (from config file, no daemon needed) |
| `zeptopm chat <name> <message>` | Send a message to an agent and get the response |
| `zeptopm logs <name>` | Show recent logs for an agent |
| `zeptopm stop <name>` | Stop a running agent |
| `zeptopm start <name>` | Start an agent (must be defined in config) |
| `zeptopm restart <name>` | Restart an agent (stop + start) |

### Global flags

| Flag | Default | Description |
|------|---------|-------------|
| `-c, --config` | `zeptopm.toml` | Config file path |
| `-l, --log-level` | from config | Override log level (trace/debug/info/warn/error) |
| `--addr` | `127.0.0.1:9876` | Daemon HTTP address |

## Config Reference

```toml
[daemon]
poll_interval_ms = 5000       # How often to check for config changes
log_level = "info"             # trace | debug | info | warn | error
log_format = "pretty"          # pretty | compact | json
bind = "127.0.0.1:9876"       # HTTP API bind address

[[agents]]
name = "researcher"            # Unique agent name
provider = "openrouter"        # Provider name (must match [providers.*])
model = "anthropic/claude-sonnet-4"  # Model identifier
system_prompt = "You are a research assistant."
auto_start = true              # Start automatically with daemon
max_restarts = 5               # Max auto-restarts on failure
restart_backoff_ms = 1000      # Initial backoff (doubles each restart)
max_iterations = 10            # Max tool-calling iterations per message

[agents.budget]
token_limit = 100000           # Max tokens per agent
cost_limit_usd = 5.00          # Max cost per agent

[providers.openrouter]
api_key = "$OPENROUTER_API_KEY"         # Supports $ENV_VAR expansion
base_url = "https://openrouter.ai/api/v1"

[providers.openai]
api_key = "$OPENAI_API_KEY"

[providers.anthropic]
api_key = "$ANTHROPIC_API_KEY"
```

### Supported Providers

| Provider | Config name | Notes |
|----------|-------------|-------|
| Anthropic | `anthropic` or `claude` | Direct Claude API |
| OpenAI | `openai` | GPT models |
| OpenRouter | `openrouter` | Multi-model gateway |
| Groq | `groq` | Fast inference |
| Together | `together` | Open-source models |
| Custom | any name | Set `base_url` for OpenAI-compatible endpoints |

## Features

- **Config-driven** — define agents in TOML, no code required
- **Conversation history** — agents maintain full chat context (powered by [zeptoclaw](https://crates.io/crates/zeptoclaw))
- **Automatic restart** with exponential backoff
- **Hot config reload** — add/remove agents without restarting the daemon
- **Per-agent budget limits** (tokens, USD)
- **Multi-provider support** — OpenAI, Anthropic, OpenRouter, Groq, Together, or any OpenAI-compatible API
- **`$ENV_VAR` expansion** for API keys in config
- **REST API** on port 9876 for programmatic control

## HTTP API

The daemon exposes a REST API (default `127.0.0.1:9876`):

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/status` | GET | All agents status |
| `/agents/{name}/status` | GET | Single agent status |
| `/agents/{name}/chat` | POST | Send message, get response |
| `/agents/{name}/logs` | GET | Recent agent logs |
| `/agents/{name}/stop` | POST | Stop agent |
| `/agents/{name}/start` | POST | Start agent |
| `/agents/{name}/restart` | POST | Restart agent |

## Architecture

```
zeptopm.toml → Config Parser → Daemon Loop → Agent Processes (tokio tasks)
                                    ↑              ↓
                              Config Watcher    zeptoclaw ZeptoAgent
                              (hot reload)    (conversation + providers)
                                    ↑              ↓
                              HTTP API ←→    OpenRouter / OpenAI / Anthropic
                            (port 9876)
```

## License

MIT
