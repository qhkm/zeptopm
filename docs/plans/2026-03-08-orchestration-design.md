# ZeptoPM Orchestration Layer — Design Document

**Date:** 2026-03-08
**Status:** Draft — brainstorming complete, awaiting implementation plan
**Authors:** noranizaahmad, Claude Opus 4.6, Codex (external review)

---

## 1. Context and Motivation

### What we have today

ZeptoPM is a working PM2-style process manager for AI agents. It manages long-lived, independently-operating agents — each running as a separate OS process.

**Current architecture (shipped and tested):**

```
zeptopm.toml
    |
    v
Daemon (supervisor)          -- Rust, tokio, axum
    |
    +-- Worker Process (agent "researcher")   -- ~4 MB RSS
    |       |-- ZeptoAgent (zeptoclaw)
    |       |-- Session file (JSON persistence)
    |       +-- LLM Provider (OpenAI/Anthropic/etc.)
    |
    +-- Worker Process (agent "coder")
    +-- Worker Process (agent "writer")
    +-- Worker Process (agent "manager")
```

**What's implemented:**

| Component | Status | File |
|-----------|--------|------|
| Process isolation (OS process per agent) | Shipped | `src/agent.rs` |
| JSON-line protocol (stdin/stdout IPC) | Shipped | `src/worker.rs`, `src/agent.rs` |
| Supervisor with restart + backoff | Shipped | `src/daemon.rs` |
| Config hot-reload | Shipped | `src/daemon.rs` |
| Session persistence across restarts | Shipped | `src/worker.rs`, `src/config.rs` |
| Bounded history (max_history) | Shipped | `src/worker.rs` |
| REST API (status, chat, logs, start/stop) | Shipped | `src/server.rs` |
| Gateway with API key auth + rate limiting | Shipped | `src/server.rs` |
| Pipeline execution (agent chain) | Shipped | `src/main.rs` |
| Basic orchestration (@delegate pattern) | Shipped | `src/server.rs` |
| TOML-based config with $ENV_VAR expansion | Shipped | `src/config.rs` |

**Measured performance (Apple Silicon, release build):**

| Component | RSS (idle) |
|-----------|-----------|
| Daemon supervisor | ~4 MB |
| Each worker process | ~4 MB |
| Release binary | ~11 MB |

A $5/month VPS can run 50-80 agents comfortably.

### What's missing

The current system treats agents as **independent long-lived processes**. Each agent operates in isolation — there's no concept of:

- Multi-step workflows with dependencies
- Structured data handoff between agents
- A planner that decomposes tasks into subtasks
- Automatic retry of individual subtasks
- Progress tracking across a multi-agent run

The existing `pipeline` (linear chain) and `orchestrate` (@delegate pattern) commands are basic — they work for simple cases but don't support:

- Parallel subtask execution
- Dependency graphs (job B waits for job A)
- Artifact-based handoff (structured JSON, not just chat strings)
- Planner → worker separation (supervisor interprets plan, not workers spawning workers)

---

## 2. Design Goals

### Must have

1. **Layer on top of existing zeptoPM** — don't replace what works
2. **Run → Job → Artifact model** — multi-step workflows with dependency graphs
3. **Planner emits plan, supervisor executes** — clean separation of concerns
4. **Parallel job execution** with concurrency limits
5. **Per-job retry** — one failure doesn't kill the whole run
6. **Artifact-based handoff** — structured data between jobs, not just chat strings

### Nice to have

7. Heartbeat/progress monitoring for stuck workers
8. Review loop (reviewer checks coder output, can request revisions)
9. SQLite-backed job store for persistence
10. CLI commands for run management (`zeptopm run submit`, `zeptopm run status`)

### Explicitly out of scope

- Distributed cluster scheduling (single-node only)
- Worker-to-worker direct communication
- Event sourcing / replay engine
- OTP-style supervision trees
- Generalized DAG editor UI

---

## 3. Decision Log

### Decision 1: Process isolation model

**Question:** How should agents be isolated?

**Options considered:**
- A) Tokio tasks in the same process (fast, no isolation)
- B) Separate OS processes (more memory, full isolation)
- C) Docker containers (maximum isolation, complex setup)

**Decision:** Option B — separate OS processes.

**Rationale:** ~4 MB per worker is cheap. Full crash isolation is critical for reliability. One agent OOM or panic cannot bring down others. Docker adds operational complexity we don't need at this stage.

**Validated:** Smoke tested with 4 agents. Measured RSS. One agent crashing doesn't affect others.

### Decision 2: IPC protocol

**Question:** How should supervisor and workers communicate?

**Options considered:**
- A) JSON lines over stdin/stdout
- B) Unix domain sockets
- C) gRPC
- D) Job spec files on disk (Codex proposal)

**Decision:** Option A — JSON lines over stdin/stdout.

**Rationale:** Simplest to implement. No port allocation needed. Works on all platforms. Natural for long-lived processes that need bidirectional communication. The Codex proposal uses job spec files, which is better for one-shot jobs but worse for long-lived agents that need ongoing chat.

**Trade-off:** Slightly harder to debug than files on disk. Mitigated by stderr passthrough for debug output.

### Decision 3: Session persistence format

**Question:** How should agent conversation history survive restarts?

**Options considered:**
- A) In-memory only (no persistence)
- B) SQLite database
- C) Per-agent JSON files
- D) Shared database (Postgres)

**Decision:** Option C — per-agent JSON files at `~/.zeptopm/sessions/{agent}.json`.

**Rationale:** Zero dependencies. Each worker owns its own file — no contention. Easy to inspect, backup, and delete. JSON round-trips cleanly with serde. SQLite is overkill for conversation history; Postgres requires external infrastructure.

**Validated:** Tested with 4 agents. Chat → stop daemon → restart → agents recall conversation history. Session files grow predictably. `max_history` bounds growth.

### Decision 4: History compaction strategy

**Question:** How to prevent unbounded history growth?

**Options considered:**
- A) No limits (grow forever)
- B) Fixed message count cap (`max_history`)
- C) LLM-based summarization (compress old messages)
- D) Token-based limit (count tokens, not messages)

**Decision:** Option B — fixed message count cap.

**Rationale:** Simplest and most predictable. `max_history = 200` with ~200 bytes/message = ~40 KB overhead per agent. Keeps user/assistant pairs intact (rounds down to even). LLM summarization is complex, costs API calls, and can lose important context. Token counting requires model-specific tokenizers.

**Future:** Can add LLM summarization later as an opt-in feature. The truncation approach is the right default.

### Decision 5: Orchestration architecture

**Question:** How should multi-step workflows be orchestrated?

**Options considered:**
- A) Manager agent spawns other agents directly (agent-driven)
- B) Planner emits plan artifact, supervisor creates child jobs (supervisor-driven)
- C) External workflow engine (Temporal, Airflow)

**Decision:** Option B — supervisor-driven orchestration.

**Rationale from Codex review:** The planner should not spawn children from inside the worker process. The supervisor should interpret the plan and create child jobs. This is cleaner (workers are pure executors), safer (supervisor controls concurrency), and matches our existing architecture (daemon already owns spawn/restart logic).

**Why not agent-driven (Option A):** Our current @delegate pattern works but has limitations — the manager agent decides what to delegate via LLM output parsing, which is fragile. The planner-to-supervisor pattern is more structured and reliable.

**Why not external engine (Option C):** Adds massive operational complexity. ZeptoPM should be self-contained.

---

## 4. Architecture: Orchestration Layer

### New concepts

```
Run         — Top-level execution of a complex task
  |
  +-- Job   — Unit of work assigned to one worker
  |     |
  |     +-- Artifact  — Structured output from a job (JSON, markdown, code)
  |
  +-- Job   — Can depend on other jobs
  +-- Job   — Can run in parallel if no dependencies
```

### How it fits with existing zeptoPM

```
Existing (unchanged):
  Config → Daemon → Worker Processes → ZeptoAgent → LLM Provider

New (orchestration layer):
  "zeptopm run submit <task>" → Daemon creates Run
    → Planner job executes → emits ExecutionPlan artifact
    → Supervisor reads plan → creates child jobs with dependencies
    → Ready jobs spawned as workers (reusing existing process model)
    → Completed jobs produce artifacts → dependent jobs unblocked
    → All jobs done → Run completed → final artifacts returned
```

**Key insight:** The orchestration layer reuses the existing worker process model. A "job" is just a worker process with extra metadata (run_id, dependencies, artifacts). The existing `agent.rs` spawn/bridge code doesn't change — we add a scheduler layer above it.

### Data model

```rust
// A top-level execution
struct Run {
    run_id: String,
    task: String,              // Original user request
    status: RunStatus,         // Pending | Running | Completed | Failed
    root_job_id: String,       // The planner job
    final_artifact_ids: Vec<String>,
    created_at: SystemTime,
}

// A unit of work
struct Job {
    job_id: String,
    run_id: String,
    role: String,              // "planner" | "researcher" | "coder" | "reviewer" | etc.
    instruction: String,       // What to do
    status: JobStatus,         // Pending | Ready | Running | Completed | Failed
    depends_on: Vec<String>,   // Job IDs that must complete first
    input_artifact_ids: Vec<String>,
    output_artifact_ids: Vec<String>,
    attempt: u32,
    max_attempts: u32,
}

// Structured handoff between jobs
struct Artifact {
    artifact_id: String,
    job_id: String,
    kind: String,              // "json" | "markdown" | "code" | "plan"
    path: PathBuf,             // File on disk
    summary: String,           // Human-readable description
}
```

### Planner output format

The planner job produces an `ExecutionPlan` artifact:

```json
{
  "jobs": [
    {
      "local_id": "research_1",
      "role": "researcher",
      "instruction": "Research competitors in Malaysia.",
      "depends_on": []
    },
    {
      "local_id": "analyst_1",
      "role": "analyst",
      "instruction": "Synthesize research findings into key market gaps.",
      "depends_on": ["research_1"]
    },
    {
      "local_id": "writer_1",
      "role": "writer",
      "instruction": "Write final report.",
      "depends_on": ["analyst_1"]
    }
  ]
}
```

The supervisor reads this artifact, maps `local_id` to real `job_id`, resolves dependencies, and enqueues ready jobs.

### Worker protocol extensions

The existing JSON-line protocol gets two new event types:

```json
// Worker → Supervisor: artifact produced
{"type": "artifact_produced", "artifact_id": "art_1", "kind": "json", "path": "/tmp/run_x/findings.json", "summary": "Competitor analysis"}

// Worker → Supervisor: progress update
{"type": "progress", "phase": "researching", "message": "Found 5 sources", "percent": 60}
```

Existing events (`ready`, `status`, `chat_response`, `log`) remain unchanged.

### Supervisor extensions

The daemon loop gets a new branch in its `tokio::select!`:

```
// Existing
- state_rx.recv()     → agent state updates
- daemon_cmd_rx       → HTTP commands (start/stop/restart)
- poll_timer.tick()   → config reload + restart check

// New
- run_scheduler.tick() → check ready queue, spawn jobs, promote unblocked jobs
```

### Concurrency control

```toml
[daemon]
max_concurrent_jobs = 4    # Max parallel job workers at once
```

The scheduler respects this limit. Ready jobs queue until a slot opens.

---

## 5. Codex Proposal Review

An external review (Codex) proposed a full orchestrator architecture. Here's our assessment:

### Aligned with our direction

| Codex Concept | Our Assessment |
|---------------|----------------|
| Process-per-worker | Already shipped |
| JSON-line events | Already shipped |
| Supervisor with restart | Already shipped |
| Crash isolation | Already shipped |
| Job/Run/Artifact model | Agree — this is the key missing layer |
| Planner → supervisor → child jobs | Agree — right separation of concerns |
| "Don't add distributed cluster yet" | Agree completely |

### Diverges from our direction

| Codex Concept | Our Assessment | Resolution |
|---------------|----------------|------------|
| Replaces zeptoPM from scratch | We have working infra | Layer on top instead |
| Job spec via temp file | We use stdin/stdout | Keep stdin/stdout for long-lived, add file spec for one-shot jobs |
| Synchronous `tick()` with `sleep(500ms)` | We use async `tokio::select!` | Keep async — more efficient |
| `uuid` crate for IDs | Simple counter works | Use `run_{timestamp}_{counter}` pattern |
| No session persistence | We already solved this | Keep our session persistence |
| No HTTP API | We already have full REST API | Extend API with run endpoints |
| No config hot-reload | We already have this | Keep hot-reload |
| Heartbeat thread in worker | Good idea, we lack this | Add to roadmap |

### Cherry-pick list

From the Codex proposal, adopt:

1. **Run/Job/Artifact types** (adapted to our conventions)
2. **ExecutionPlan format** (planner output → supervisor interprets)
3. **Dependency promotion** (scan pending jobs, enqueue when deps met)
4. **Per-job retry with max_attempts** (already have per-agent restart, extend to per-job)
5. **WorkerProfile concept** (role-based tool/model restrictions)
6. **Review loop pattern** (reviewer job depends on coder job, can trigger re-run)

Do not adopt:
- The full crate structure (we don't need 15 new files)
- File-based job spec passing (keep stdin/stdout)
- Synchronous tick loop (keep async)
- uuid dependency (unnecessary)

---

## 6. Implementation Roadmap

### Phase 1: Job graph foundation

Add Run, Job, Artifact types. In-memory store. Basic scheduler that spawns jobs when dependencies are met.

**Files:** `src/orchestrator/mod.rs`, `src/orchestrator/types.rs`, `src/orchestrator/store.rs`, `src/orchestrator/scheduler.rs`

### Phase 2: Planner integration

Worker profile for planner role. Planner job produces ExecutionPlan artifact. Supervisor materializes plan into child jobs.

**Files:** `src/orchestrator/planner.rs`, extend `src/worker.rs` with artifact output

### Phase 3: CLI and API

`zeptopm run submit "task"`, `zeptopm run status <run_id>`, `zeptopm run list`. REST endpoints: `POST /runs`, `GET /runs/{id}`.

**Files:** extend `src/main.rs`, `src/server.rs`

### Phase 4: Heartbeat and progress

Workers send periodic heartbeat events. Supervisor detects stuck workers (no heartbeat for N seconds) and kills/retries them.

**Files:** extend `src/worker.rs`, `src/orchestrator/scheduler.rs`

### Phase 5: Review loop

Reviewer job depends on coder job. Reviewer output can be "approved", "revise", or "rejected". Supervisor handles re-queue on "revise".

**Files:** `src/orchestrator/review.rs`, extend scheduler

---

## 7. What We Explicitly Won't Build

These are interesting ideas that don't belong in v1:

- **Distributed scheduling** — single-node is sufficient for 1000+ agents
- **Worker-to-worker sockets** — all communication goes through supervisor
- **Event sourcing** — simple state machine is enough
- **Persistent replay** — not needed until we have audit requirements
- **OTP supervision trees** — our flat restart model works
- **Generalized DAG editor** — TOML config + planner output is enough
- **Shared memory/mailbox fabric** — artifacts on disk are the handoff mechanism

---

## 8. Open Questions

1. **Should jobs reuse long-lived agent processes or spawn fresh workers?**
   - Long-lived: faster startup, session continuity
   - Fresh: cleaner isolation, no state leakage between jobs
   - Leaning toward: fresh workers for orchestration jobs, long-lived for standalone agents

2. **Where should artifacts be stored?**
   - `/tmp/zeptopm/runs/{run_id}/{job_id}/` (ephemeral)
   - `~/.zeptopm/artifacts/{run_id}/` (persistent)
   - Leaning toward: persistent by default, with TTL cleanup

3. **Should the planner be a special built-in agent or a regular agent with a planner profile?**
   - Built-in: more control, predictable output format
   - Regular agent: more flexible, user can customize planner prompt
   - Leaning toward: regular agent with a default planner profile

4. **How should the orchestration layer interact with existing `pipeline` and `orchestrate` commands?**
   - Replace them entirely
   - Keep them as shortcuts that create Runs under the hood
   - Leaning toward: keep them as simple shortcuts, add `run` command family for full orchestration
