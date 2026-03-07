# CLAUDE.md — zeptoPM

## Project Overview

zeptoPM is a process manager for AI agents — like PM2 for Node.js, but for LLMs. Configure agents in `zeptopm.toml`, run `zeptopm daemon`, and agents run as managed processes with automatic restart, config hot-reload, and status monitoring.

## Architecture

Standalone Rust binary. No external dependencies on zeptoRT or erlangrt.

- `config.rs` — TOML config parsing and validation
- `llm.rs` — Direct HTTP client for OpenRouter/OpenAI-compatible APIs (reqwest)
- `agent.rs` — Agent process management (spawn as tokio tasks, message handling)
- `daemon.rs` — Main orchestration loop (spawn agents, monitor, restart, config reload)
- `status.rs` — Status display formatting
- `main.rs` — CLI entry point (clap)

## Build & Run

```bash
cargo build
cargo run -- daemon                    # start with default config
cargo run -- daemon -c myconfig.toml   # custom config
cargo run -- status                    # show configured agents
cargo run -- list                      # list agent names
cargo test                             # run all tests
```

## Config Format

See `zeptopm.toml` for the full example. Key sections:
- `[daemon]` — poll interval, log level
- `[[agents]]` — agent definitions (name, provider, model, system_prompt, tools, budget)
- `[providers.*]` — API keys and base URLs (supports `$ENV_VAR` expansion)

## Relationship to Other Projects

- **zeptoRT** (~/ios/zeptoclaw-rt) — Enterprise durable runtime. zeptoPM is the simple PM2-style interface; zeptoRT is the deep Temporal-style runtime.
- **Symphony** (OpenAI) — Inspiration for the daemon/orchestrator pattern.
- Future: zeptoPM can optionally use zeptoRT as its backend for durability.
