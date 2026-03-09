# Erlang/OTP Pattern Analysis for ZeptoPM

**Date:** 2026-03-09
**Context:** After implementing agent channels, evaluated whether zeptoPM should adopt more Erlang/OTP patterns.
**Reference:** [The Zen of Erlang](https://ferd.ca/the-zen-of-erlang.html)

## Decision: Keep orchestrator lean — runtime patterns belong in zeptoRT

zeptoPM is an AI agent orchestrator (5-20 jobs per run, LLM-speed operations).
Erlang patterns solve problems of massive scale and five-nines uptime.
These are fundamentally different operating regimes.

## Layer Separation

| Layer | Project | Owns Erlang Patterns? |
|---|---|---|
| Runtime semantics | zeptoRT | Yes — supervision trees, scheduling, links/monitors, hot code loading |
| Orchestration | zeptoPM | Only what maps to AI agent coordination |
| Agent logic | zeptoclaw | No — application layer |

## What zeptoPM Already Has (sufficient)

| Erlang Pattern | zeptoPM Implementation |
|---|---|
| Process isolation | Capsule sandbox (zeptocapsule) — memory, filesystem, network isolated |
| Message passing | JSON Lines IPC + Channel Router — no shared memory, copy semantics |
| one_for_all | `PeerFailure::KillAll` — if one channel participant dies, kill all |
| Continue (graceful) | `PeerFailure::Continue` — survivors get "peer disconnected", keep going |
| Monitors | Heartbeat + `stale_jobs()` — 120s timeout detects dead workers |
| Links | `PeerFailure::KillAll` — bidirectional failure propagation |
| Load shedding | `max_concurrency` on engine — excess jobs queue in `ready_queue` |
| Retry on failure | `max_attempts` per job with re-enqueue on failure |

## Patterns Rejected (with reasoning)

### Supervision Trees
**Why Erlang needs it:** Millions of processes in complex hierarchies.
**Why we don't:** 5-20 jobs per run. DAG `depends_on` encodes dependencies. `PeerFailure::KillAll` on channels IS the team supervisor. Adding OTP-style supervisor modules for 10 jobs is over-engineering.

### rest_for_one
**Why Erlang needs it:** Boot order implies dependency (B uses connection pool started by A).
**Why we don't:** Dependencies are explicit in `depends_on`. Boot order is an implementation detail, not a semantic relationship.

### Backpressure
**Why Erlang needs it:** Millions of messages/sec can flood slow consumers.
**Why we don't:** Stream mode produces ~1 artifact per 10-30s (LLM-speed). Tokio channel buffer of 256 will never fill. LLM calls are the bottleneck, not message throughput.

### Restart Intensity (N failures in T seconds)
**Why Erlang needs it:** Distinguish transient glitch from systemic failure. Escalate to parent supervisor.
**Why we don't:** For LLM agents, 3 fast failures = API down (user notices immediately). 3 slow failures = bad prompts (user needs to fix them). A time window doesn't change what the user has to do.

### Circuit Breaker
**Why Erlang needs it:** Prevent thundering herd across microservices.
**Why we don't:** Single-user tool. LLM client already has exponential backoff. 3 jobs failing and the run stopping IS the circuit breaker.

### Process Priorities
**Why Erlang needs it:** Supervisors and heartbeats must never be starved.
**Why we don't:** All jobs are roughly equal. DAG ordering handles sequencing. If you want "reviewer before writer," that's a DAG edge, not a priority level.

### Hot Code Reload
**Why Erlang needs it:** Zero-downtime telecom switches.
**Why we don't:** Config hot-reload exists for TOML. Swapping agent code mid-run makes no sense for LLM agents.

### Graceful Degradation
**Why Erlang needs it:** Handle millions of concurrent connections under load.
**Why we don't:** `max_concurrency` is the load shedding. If you submit 50 runs, 4 run concurrently and 46 wait. That's correct behavior.

## What zeptoPM Actually Needs (AI-agent-specific)

These are the real problems for AI orchestration that Erlang never had to solve:

| Problem | Why Erlang Can't Help | Right Solution |
|---|---|---|
| Agent produces garbage JSON | Restart replays same bad prompt | Structured output validation + re-prompt |
| Agent goes off-task | Restart doesn't fix the instruction | Guardrails / output scoring |
| LLM API cost spirals on retries | Circuit breaker is over-engineering for single-user | Budget limits per run/job |
| Agent stuck in a loop | Heartbeat helps but is coarse (120s) | Token/turn limits + progress detection |
| User wants to intervene | Erlang is fully automated | Human-in-the-loop pause/resume |
| Can't tell what agents are doing | Erlang has process introspection | Observability dashboard |
| Restarting wastes LLM tokens | Erlang processes restart in microseconds | Conversation checkpointing (resume from last tool call) |

## Key Insight

> Erlang processes cost microseconds to restart. AI agent jobs cost dollars and minutes.
> The economics are inverted — optimize for cost and correctness, not restartability.

## Where Erlang Patterns DO Belong

zeptoRT is a BEAM-inspired runtime where these patterns naturally live:

- Supervision trees (one_for_one, one_for_all, rest_for_one)
- Process priorities and preemptive scheduling
- Links and monitors with exit signal propagation
- Restart intensity (max N restarts in T seconds)
- Hot code loading
- Per-process heaps and GC
- Backpressure and mailbox overflow handling
