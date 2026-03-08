# ZeptoPM — TODO & Roadmap

> **For agents:** Read this file first when picking up work. Run `cargo test` after every change — all 35 tests must pass before committing.

## Quick Context

**What is this?** PM2-style process manager for AI agents. Config-driven, process-isolated, single binary.

**Repo:** `/Users/dr.noranizaahmad/ios/zeptoPM/`

**Stack position:** ZeptoPM (this) → ZeptoKernel (isolation) → ZeptoClaw (worker)

**Current state:** Core PM shipped. Orchestration Phases 1–4 done. 39 tests passing. Zero warnings.

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
| Orch Phase 5: Review loop | 🔴 Not started | Reviewer job type, revision re-queueing |
| SQLite persistence | 🔴 Not started | Survive daemon restarts, audit trail |
| ZeptoKernel integration | 🔴 Not started | Isolated capsule execution |
| End-to-end testing | 🔴 Not started | Real daemon + LLM smoke test |

---

## Source File Map

| File | Lines | What's there |
|------|-------|-------------|
| `src/main.rs` | ~500 | CLI (clap): daemon, status, chat, logs, start/stop, pipeline, orchestrate, run |
| `src/lib.rs` | ~10 | Module exports |
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
| `src/orchestrator/engine.rs` | ~360 | OrchestratorEngine: submit_run, next_job, mark_completed/failed, heartbeat (12 tests) |
| `src/orchestrator/planner.rs` | ~80 | ExecutionPlan → child jobs materializer (2 tests) |

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

- [ ] **5.1 — Review decision parsing** (`src/orchestrator/review.rs`)
  - New file
  - Parse reviewer artifact for decision: `approved`, `revise`, `rejected`
  - Extract feedback text for revision instruction
  - **Test:** Parse sample reviewer outputs (approved, revise with feedback, rejected)

- [ ] **5.2 — Revision re-queueing** (`src/orchestrator/engine.rs`)
  - On `revise` decision: create new coder job with reviewer feedback as input
  - New coder job depends on nothing (it's a retry with new instructions)
  - Create new reviewer job depending on new coder job
  - Cap revision cycles (default: 3 max revisions)
  - **Test:** Engine test — mark reviewer completed with "revise" → new coder+reviewer jobs created

- [ ] **5.3 — Review-aware planner prompt** (`docs/` or config)
  - Document how to configure planner to emit review pairs:
    ```json
    { "local_id": "coder_1", "role": "coder", ... },
    { "local_id": "reviewer_1", "role": "reviewer", "depends_on": ["coder_1"] }
    ```
  - Reviewer system prompt template that outputs structured JSON with decision field

- [ ] **5.4 — Revision tracking in run status** (`src/server.rs`, `src/main.rs`)
  - Show revision count in `run status` output
  - Show review decision in job details
  - **Test:** API returns revision metadata

**Exit criteria:** Coder → reviewer → revise cycle runs automatically. Stops on `approved` or max revisions.

---

## Phase 6: SQLite Persistence

Replace in-memory RunStore with SQLite so runs survive daemon restarts.

### Tasks

- [ ] **6.1 — SQLite schema** (`src/orchestrator/sqlite_store.rs`)
  - Tables: `runs`, `jobs`, `artifacts`
  - Same interface as RunStore (create/get/update/list methods)
  - Use `rusqlite` crate
  - DB path: `~/.zeptopm/zeptopm.db`
  - **Test:** CRUD tests mirroring existing RunStore tests

- [ ] **6.2 — Migration on startup**
  - Create tables if not exist on daemon start
  - Version table for future schema migrations
  - **Test:** Fresh DB creates schema correctly

- [ ] **6.3 — Swap RunStore for SqliteStore in engine**
  - Make engine generic over store trait, or just swap implementation
  - Existing 35 tests must still pass
  - **Test:** All engine tests pass with SQLite backend

- [ ] **6.4 — Resume incomplete runs on restart**
  - On daemon start, scan for Running/Pending runs
  - Re-queue Ready jobs
  - Mark Running jobs as failed (process died) — engine retries if attempts remain
  - **Test:** Simulate crash → restart → verify run resumes

**Exit criteria:** `kill daemon && zeptopm daemon` — in-progress runs resume. Old runs queryable.

---

## Phase 7: ZeptoKernel Integration

Run orchestration jobs inside ZeptoKernel capsules instead of bare child processes.

### Tasks

- [ ] **7.1 — Decision: library vs binary**
  - Option A: Add `zk-host` as a Cargo dependency, call Backend::spawn() directly
  - Option B: Spawn `zk-host` binary and communicate via its JSON-line protocol
  - **Decision needed before implementation**

- [ ] **7.2 — JobSpec mapping**
  - Map ZeptoPM `Job` → ZeptoKernel `JobSpec`
  - Fields: job_id, run_id, role, instruction, env, limits
  - Handle input artifacts: resolve ZeptoPM artifact paths to capsule workspace paths

- [ ] **7.3 — Event translation**
  - Map ZeptoKernel `GuestEvent` → ZeptoPM orchestrator events
  - Started → mark_running, Heartbeat → update last_seen, Completed → mark_completed, Failed → mark_failed

- [ ] **7.4 — Backend selection config**
  - Add to `[daemon]` config: `isolation = "none" | "process" | "namespace" | "firecracker"`
  - Default: `"process"` (current behavior, no isolation)
  - When `"namespace"` or `"firecracker"`: use ZeptoKernel

- [ ] **7.5 — Integration test**
  - Submit run with ZeptoKernel process backend
  - Verify events flow correctly through capsule boundary

**Exit criteria:** `zeptopm run submit` with `isolation = "process"` uses ZeptoKernel capsule.

**Dependency:** ZeptoKernel M2.5 (real worker launching) must be done first. See `/Users/dr.noranizaahmad/ios/zeptokernel/TODO.md`.

---

## Infrastructure & Polish

Independent tasks, can be done anytime.

- [ ] **CLAUDE.md update** — Add orchestrator module docs, file map, test commands
- [ ] **CI setup** — GitHub Actions: build + test + clippy + fmt
- [ ] **Config validation** — Warn on unknown keys, validate provider/model combos
- [ ] **Graceful shutdown** — SIGTERM handler: cancel running jobs, wait for cleanup, then exit
- [ ] **Run cleanup** — TTL-based artifact cleanup (delete runs older than N days)
- [ ] **`run result` command** — Print final artifact content for a completed run
- [ ] **`run cancel` command** — Cancel a running run (cancel all active jobs)
- [ ] **Error messages** — Better error context when daemon is not running, config is invalid, etc.
- [ ] **Metrics endpoint** — `GET /metrics` with agent count, uptime, run stats

---

## Known Issues

1. **In-memory store** — All runs/jobs/artifacts lost on daemon restart. Phase 6 fixes this.
2. **No end-to-end test** — Orchestration hasn't been tested with real LLM calls yet. Need test config + API key.
3. **`test-persist.toml`** — Untracked test artifact in repo root. Should be gitignored or committed.
4. **Planner fragility** — Planner output must be valid JSON matching ExecutionPlan schema. No validation/retry on malformed plans yet.

---

## How to Pick Up Work

1. **Read this file** — you're doing it
2. **Read `CLAUDE.md`** — project conventions
3. **Run `cargo test`** — verify 39 tests pass
4. **Pick the next unchecked task** — Phase 5 is highest priority
5. **Implement, test, commit** — one task at a time
6. **Update this file** — check off completed tasks
