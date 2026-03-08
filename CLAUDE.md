# CLAUDE.md — zeptoPM

## Project Overview

zeptoPM is a process manager for AI agents — like PM2 for Node.js, but for LLMs. Configure agents in `zeptopm.toml`, run `zeptopm daemon`, and agents run as managed processes with automatic restart, config hot-reload, and status monitoring.

## Architecture

Standalone Rust binary (~11 MB). No external dependencies on zeptoRT or erlangrt. ~4 MB RSS per worker process.

### Core modules
- `config.rs` — TOML config parsing, $ENV_VAR expansion, validation (5 tests)
- `llm.rs` — HTTP client for OpenAI-compatible APIs (reqwest)
- `provider.rs` — LLM provider factory
- `agent.rs` — Process spawn, worker bridge, JSON-line IPC (3 tests)
- `worker.rs` — Worker process, session persistence, ZeptoAgent integration, job_execute handler
- `daemon.rs` — Supervisor loop, config reload, orchestrator integration
- `server.rs` — Axum HTTP API, run endpoints, gateway auth, rate limiting
- `status.rs` — Status display formatting (2 tests)
- `main.rs` — CLI entry point (clap)

### Orchestrator modules (`src/orchestrator/`)
- `types.rs` — Run, Job, Artifact, ExecutionPlan structs
- `store.rs` — In-memory HashMap store (6 tests)
- `scheduler.rs` — Dependency promotion, run completion check (7 tests)
- `engine.rs` — OrchestratorEngine: submit_run, next_job, heartbeat, review loop (15 tests)
- `review.rs` — Review decision parsing: JSON + keyword fallback (8 tests)
- `sqlite_store.rs` — SQLite persistence sidecar: persist/load/hydrate (8 tests)
- `planner.rs` — ExecutionPlan → child jobs materializer (2 tests)

## Build & Run

```bash
cargo build                           # build
cargo test                            # run all 58 tests
cargo run -- daemon                   # start with default config
cargo run -- daemon -c config.toml    # custom config
cargo run -- status                   # show agents
cargo run -- chat <name> "message"    # chat with agent
cargo run -- run submit "task"        # submit orchestrated run
cargo run -- run status <run_id>      # check run progress
cargo run -- run list                 # list all runs
```

## Config Format

See `zeptopm.toml` for the full example. Key sections:
- `[daemon]` — poll interval, log level, max_concurrent_jobs
- `[[agents]]` — agent definitions (name, provider, model, system_prompt, tools, budget)
- `[providers.*]` — API keys and base URLs (supports `$ENV_VAR` expansion)

## Key Design Docs

- `docs/plans/2026-03-08-orchestration-design.md` — architecture decisions
- `docs/plans/2026-03-08-orchestration-impl.md` — implementation tasks
- `docs/plans/2026-03-08-use-cases-and-benefits.md` — use cases, competitive positioning
- `TODO.md` — current roadmap with task-level breakdown

## Relationship to Other Projects

- **ZeptoKernel** (~/ios/zeptokernel) — Secure per-worker execution capsule. Future: zeptoPM spawns jobs inside ZeptoKernel capsules for isolation.
- **ZeptoClaw** — AI worker binary. The actual task executor inside capsules.
- **zeptoRT** (~/ios/zeptoclaw-rt) — Enterprise durable runtime (Erlang-inspired). Separate from zeptoPM.

## IPC Protocol

Supervisor ↔ worker communication via JSON lines on stdin/stdout:

**Supervisor → Worker:**
- `{"type": "chat", "message": "..."}` — chat request
- `{"type": "job_execute", "job_id": "...", "instruction": "...", "workspace": "...", "input_artifacts": [...]}` — orchestrated job

**Worker → Supervisor:**
- `{"type": "ready"}` — worker initialized
- `{"type": "chat_response", "message": "..."}` — chat reply
- `{"type": "artifact_produced", "job_id": "...", "artifact_id": "...", "kind": "...", "path": "...", "summary": "..."}` — output artifact
- `{"type": "job_completed", "job_id": "...", "output_artifact_ids": [...]}` — job done
- `{"type": "job_failed", "job_id": "...", "error": "...", "retryable": bool}` — job failed
- `{"type": "status", "status": "idle|running", "message": "..."}` — status update
- `{"type": "log", "level": "info|error", "message": "..."}` — log entry
