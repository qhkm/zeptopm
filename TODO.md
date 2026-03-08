# ZeptoPM — TODO & Roadmap

> **For agents:** Read this file first when picking up work. Run `cargo test` after every change — all 81 tests must pass before committing.

## Quick Context

**What is this?** PM2-style process manager for AI agents. Config-driven, process-isolated, single binary.

**Repo:** `/Users/dr.noranizaahmad/ios/zeptoPM/`

**Stack position:** ZeptoPM (this) → ZeptoKernel (isolation) → ZeptoClaw (worker)

**Current state:** Core PM shipped. Orchestration Phases 1–7 done. All infrastructure tasks done. Agent-native CLI done. 81 tests passing. Zero warnings.

**Design docs:**
- `docs/plans/2026-03-08-orchestration-design.md` — architecture decisions
- `docs/plans/2026-03-08-orchestration-impl.md` — implementation tasks (1–9 done)
- `docs/plans/2026-03-08-use-cases-and-benefits.md` — use cases, competitive positioning

---

## Overall Progress

| Area | Status | Description |
|------|--------|-------------|
| Core PM | ✅ Shipped | Process isolation, supervisor, restart, config hot-reload |
| Session persistence | ✅ Shipped | Per-agent JSON files, bounded history |
| REST API | ✅ Shipped | Axum server, status/chat/logs/start/stop endpoints |
| Gateway | ✅ Shipped | API key auth, sliding-window rate limiting |
| Pipeline execution | ✅ Shipped | Linear agent chain (A → B → C) |
| Manager delegation | ✅ Shipped | @delegate(agent) pattern |
| Orch Phase 1: Types + Store | ✅ Done | Run, Job, Artifact, RunStore (in-memory) |
| Orch Phase 2: Engine + Planner | ✅ Done | Scheduler, dependency promotion, plan materialization |
| Orch Phase 3: CLI + API | ✅ Done | `run submit/status/list`, `--tail` flag, REST endpoints |
| Orch Phase 4: Heartbeat | ✅ Done | Progress tracking, stuck job detection (4 tests) |
| Orch Phase 5: Review loop | ✅ Done | Review parsing, revision re-queueing (11 tests) |
| SQLite persistence | ✅ Done | WAL-mode SQLite, hydration on restart (8 tests) |
| ZeptoKernel integration | ✅ Done | Library integration, capsule job runner (4 tests) |
| Planner validation | ✅ Done | Plan structure validation, retry on malformed (5 tests) |
| Integration tests | ✅ Done | Full orchestrator flow tests without daemon/LLM (4 tests) |
| Agent-native CLI | ✅ Done | --json flag, agent-help, --agent-help (7 tests) |
| End-to-end testing | 🔴 Not started | Real daemon + LLM smoke test |

---

## Source File Map

| File | Lines | What's there |
|------|-------|-------------|
| `src/main.rs` | ~900 | CLI (clap): --json flag, agent-help, all commands with JSON envelope (7 tests) |
| `src/lib.rs` | ~10 | Module exports |
| `src/capsule.rs` | ~160 | ZeptoKernel integration: Job→JobSpec mapping, capsule job runner (4 tests) |
| `src/config.rs` | ~200 | TOML parsing, $ENV_VAR expansion, validation (5 tests) |
| `src/daemon.rs` | ~400 | Supervisor loop, agent lifecycle, config reload, orchestrator wiring |
| `src/agent.rs` | ~300 | Process spawn, worker bridge, JSON-line IPC, orch event forwarding (3 tests) |
| `src/worker.rs` | ~350 | Worker process, session persistence, ZeptoAgent, job_execute handler |
| `src/server.rs` | ~400 | Axum HTTP API, run endpoints, gateway auth, rate limiting |
| `src/status.rs` | ~80 | Status display formatting (2 tests) |
| `src/provider.rs` | ~30 | LLM provider factory |
| `src/llm.rs` | ~100 | HTTP client for OpenAI-compatible APIs |
| `src/orchestrator/mod.rs` | ~10 | Module exports |
| `src/orchestrator/types.rs` | ~115 | Run, Job, Artifact, ExecutionPlan structs |
| `src/orchestrator/store.rs` | ~120 | In-memory HashMap store (6 tests) |
| `src/orchestrator/scheduler.rs` | ~100 | Dependency promotion, run completion check (7 tests) |
| `src/orchestrator/engine.rs` | ~470 | OrchestratorEngine: submit_run, next_job, heartbeat, review loop (15 tests) |
| `src/orchestrator/review.rs` | ~140 | Review decision parsing: JSON + keyword fallback (8 tests) |
| `src/orchestrator/sqlite_store.rs` | ~350 | SQLite persistence sidecar: persist/load/hydrate (8 tests) |
| `src/orchestrator/planner.rs` | ~270 | Plan validation + ExecutionPlan → child jobs materializer (7 tests) |
| `tests/orchestrator_integration.rs` | ~270 | Integration tests: full lifecycle, parallel, validation (4 tests) |

---

## Phase 4: Heartbeat & Progress Tracking

Workers emit periodic heartbeats. Supervisor detects hung jobs and kills/retries them.

### Tasks

- [x] **4.1 — Heartbeat event from worker** (`src/worker.rs`)
  - `tokio::spawn` interval timer emits heartbeat every 10s during `job_execute`
  - Aborted on job completion/failure

- [x] **4.2 — Progress event from worker** (`src/worker.rs`)
  - Emits progress at: preparing, llm_call, writing_artifact (90%)
  - Forwarded through agent.rs to orchestrator

- [x] **4.3 — Heartbeat tracking in engine** (`src/orchestrator/engine.rs`)
  - `last_heartbeat: HashMap<JobId, Instant>` with `record_heartbeat()` and `stale_jobs()`
  - 4 new tests: record, stale detection, cleared on complete, cleared on fail

- [x] **4.4 — Stuck job detection in daemon** (`src/daemon.rs`)
  - Checks stale jobs every poll tick (120s timeout)
  - Kills worker, marks failed with "heartbeat timeout", engine retries if attempts remain

- [x] **4.5 — Heartbeat in run status API** (`src/daemon.rs`)
  - `last_heartbeat_secs_ago` field in job status response

**Exit criteria:** Hung worker detected within 60s. Retried automatically. Progress visible in `--tail`.

---

## Phase 5: Review Loop

Reviewer job can request revisions, triggering a re-run of the coder job.

### Tasks

- [x] **5.1 — Review decision parsing** (`src/orchestrator/review.rs`)
  - JSON + keyword fallback parser for approved/revise/rejected decisions
  - 8 tests covering JSON, markdown-wrapped JSON, keywords, ambiguous text

- [x] **5.2 — Revision re-queueing** (`src/orchestrator/engine.rs`)
  - `handle_review_completion()` creates new coder+reviewer pair on "revise"
  - Tracks `revision_round` on Job, caps at configurable `max_revisions`
  - 3 tests: revise creates jobs, approved no-ops, max revisions respected

- [x] **5.3 — Review-aware config** (`src/config.rs`)
  - Added `max_revisions` to `[daemon]` config (default: 3)
  - Planner can emit `{"role": "coder", ...}, {"role": "reviewer", "depends_on": ["coder_1"]}`

- [x] **5.4 — Revision tracking in run status** (`src/daemon.rs`)
  - `revision_round` field in job status API response
  - Review decisions logged with tracing

**Exit criteria:** Coder → reviewer → revise cycle runs automatically. Stops on `approved` or max revisions.

---

## Phase 6: SQLite Persistence

Replace in-memory RunStore with SQLite so runs survive daemon restarts.

### Tasks

- [x] **6.1 — SQLite schema** (`src/orchestrator/sqlite_store.rs`)
  - Tables: runs, jobs, artifacts + schema_version
  - WAL mode, PRAGMA synchronous=NORMAL
  - 5 CRUD tests + upsert test

- [x] **6.2 — Migration on startup** (`src/daemon.rs`)
  - `init_schema()` creates tables if not exist
  - Schema version table for future migrations

- [x] **6.3 — Sidecar persistence in daemon** (`src/daemon.rs`)
  - Write-through: engine keeps in-memory RunStore, daemon persists to SQLite after each mutation
  - `persist_run_state()` bulk persists run + all jobs after submit/complete/fail

- [x] **6.4 — Resume incomplete runs on restart** (`src/daemon.rs`)
  - On startup: hydrate engine from SQLite, re-queue Ready jobs, fail Running jobs (process lost)
  - Hydration test + resume test in sqlite_store.rs

**Exit criteria:** `kill daemon && zeptopm daemon` — in-progress runs resume. Old runs queryable.

---

## Phase 7: ZeptoKernel Integration

Run orchestration jobs inside ZeptoKernel capsules instead of bare child processes.

### Tasks

- [x] **7.1 — Decision: library integration (Option A)**
  - Added `zk-host` and `zk-proto` as Cargo path dependencies
  - Library integration chosen over binary — simpler, type-safe, no IPC overhead

- [x] **7.2 — JobSpec mapping** (`src/capsule.rs`)
  - `job_to_spec()` maps ZeptoPM `Job` → ZeptoKernel `JobSpec`
  - Fields: job_id, run_id, role, instruction, env, limits, workspace
  - Input artifacts mapped with paths and artifact IDs
  - Environment passthrough for API keys (OPENROUTER_, OPENAI_, ANTHROPIC_)
  - 4 tests: basic mapping, no artifacts, env passthrough, default limits

- [x] **7.3 — Event translation** (`src/capsule.rs`)
  - `spawn_capsule_job()` wraps `Supervisor::run_job()` in tokio::spawn
  - `JobOutcome::Completed` → `job_completed` event on orch channel
  - `JobOutcome::Failed` → `job_failed` event on orch channel
  - `JobOutcome::Cancelled` → `job_failed` (cancelled) event
  - Supervisor errors → `job_failed` (retryable) event
  - Periodic heartbeats (10s) sent while capsule runs to prevent stale detection

- [x] **7.4 — Backend selection config** (`src/config.rs`)
  - `isolation = "none" | "capsule"` in `[daemon]` config (default: `"none"`)
  - `worker_binary = "/path/to/zk-guest"` (required when isolation = "capsule")
  - `spawn_job_worker()` branches on isolation config

- [ ] **7.5 — End-to-end integration test**
  - Requires `zk-guest` binary built and available
  - Deferred until zk-guest has full agent capabilities

**Exit criteria:** `zeptopm run submit` with `isolation = "capsule"` + `worker_binary` spawns capsule.

**Dependency:** ZeptoKernel M2.5 ✅ complete. zk-guest worker capabilities needed for E2E test.

---

## Infrastructure & Polish

Independent tasks, can be done anytime.

- [x] **CLAUDE.md update** — Orchestrator module docs, file map, test commands updated
- [x] **CI setup** — `.github/workflows/ci.yml`: build + test + clippy + fmt on push/PR
- [x] **Config validation** — Provider ref check, isolation value validation, capsule requires worker_binary (3 tests)
- [x] **Graceful shutdown** — SIGTERM/SIGINT: cancel running runs, persist to SQLite, stop agents, log uptime
- [x] **Run cleanup** — `daemon.run_ttl_days` config: auto-deletes expired runs + artifacts hourly
- [x] **`run result` command** — `zeptopm run result <run_id>`: prints artifact content for completed runs
- [x] **`run cancel` command** — `zeptopm run cancel <run_id>`: kills workers, marks jobs failed, updates run status
- [x] **Error messages** — CLI commands show "Is the daemon running?" hint on connection failure, `run list` formatted table
- [x] **Metrics endpoint** — `GET /metrics`: uptime, agent count, worker count, run stats, pending jobs

---

## Known Issues

1. ~~**In-memory store**~~ — Fixed by Phase 6 SQLite persistence.
2. **No end-to-end test** — Orchestration hasn't been tested with real LLM calls yet. Need test config + API key.
3. ~~**`test-persist.toml`**~~ — Fixed: added to `.gitignore`.
4. ~~**Planner fragility**~~ — Fixed: `validate_plan()` checks structure before materializing; invalid plans trigger retry via `mark_failed()`.

---

## How to Pick Up Work

1. **Read this file** — you're doing it
2. **Read `CLAUDE.md`** — project conventions
3. **Run `cargo test`** — verify 81 tests pass
4. **Pick the next unchecked task** — Phase 7 or Infrastructure tasks
5. **Implement, test, commit** — one task at a time
6. **Update this file** — check off completed tasks
