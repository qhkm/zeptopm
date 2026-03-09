# Real-Time Agent-to-Agent Communication Design

**Goal:** Enable live collaboration between agents (debate, review, streaming) without agents knowing about each other — the orchestrator routes everything.

**Approach:** Channel Router in the orchestrator. A Channel is a named message stream connecting 2+ running jobs. Orchestrator controls routing, turn order, and termination. Agents see regular chat messages — no protocol changes.

## Core Data Model

```rust
struct Channel {
    channel_id: String,
    run_id: String,
    participants: Vec<JobId>,
    mode: ChannelMode,
    max_rounds: Option<u32>,
    on_peer_failure: PeerFailure,
    history: Vec<ChannelMessage>,
}

enum ChannelMode {
    TurnBased,  // A speaks, then B speaks, alternating
    Stream,     // A streams to B continuously (one-way or multi-way)
}

enum PeerFailure {
    KillAll,    // default — if one dies, fail all connected jobs
    Continue,   // surviving agents get "peer disconnected", keep going
}

struct ChannelMessage {
    from_job: JobId,
    content: String,
    timestamp: SystemTime,
    round: u32,
}
```

## Message Flow

No new IPC protocol messages. Reuses existing `cmd: "chat"` and `chat_response`.

### Turn-Based (debate/review)

```
1. Orchestrator spawns writer + reviewer (both Running)
2. Sends writer:     {"cmd": "chat", "message": "Write a blog post about X"}
3. Writer responds:  {"type": "chat_response", "message": "Here's my draft..."}
4. Forwards to reviewer: {"cmd": "chat", "message": "Review this draft:\n\nHere's my draft..."}
5. Reviewer responds: {"type": "chat_response", "message": "Issues: 1) ..."}
6. Forwards to writer: {"cmd": "chat", "message": "Revision requested:\n\nIssues: 1) ..."}
7. Repeat until max_rounds or channel_done signal
```

### Stream (pipeline)

```
1. Researcher emits artifact_produced events as it works
2. Orchestrator forwards each to writer as a chat message
3. Writer starts drafting with partial data immediately
```

## Termination

- `max_rounds` reached → orchestrator sends "wrap up" to last speaker, then closes
- Agent emits `{"type": "channel_done"}` → close channel, mark jobs completed
- Wall-clock timeout → existing enforcement kills capsule
- Peer failure → `KillAll` (default) or `Continue` per channel config

## Channel Creation

Planner defines channels in the execution plan JSON:

```json
{
  "jobs": [
    {"local_id": "writer", "role": "writer"},
    {"local_id": "reviewer", "role": "reviewer"}
  ],
  "channels": [
    {
      "channel_id": "draft-review",
      "participants": ["writer", "reviewer"],
      "mode": "turn_based",
      "max_rounds": 3,
      "initial_message": "Write a blog post about Rust async patterns"
    }
  ]
}
```

Orchestrator materializes channels alongside jobs. Channel activates when all participants are Running.

## Codebase Changes

| Component | Change |
|-----------|--------|
| `orchestrator/types.rs` | Channel, ChannelMode, PeerFailure, ChannelMessage structs |
| `orchestrator/engine.rs` | Channel lifecycle: create, route, check termination, close |
| `orchestrator/planner.rs` | Parse `channels` from execution plan |
| `orchestrator/store.rs` | Store channels + message history |
| `daemon.rs` | Route `chat_response` events through channel router |

## What Does NOT Change

- **zeptoclaw (agent library)** — agents are completely unaware of channels
- **IPC protocol** — reuses existing cmd/response JSON-line messages
- **Capsule isolation** — channels route through orchestrator, not through sandbox
- **Existing DAG execution** — channels are additive; non-channel jobs unchanged
- **worker.rs** — no changes
- **capsule.rs** — no changes

## Design Principles

- Orchestrator is the single hub — agents never discover or address each other
- Agents think they're talking to the orchestrator (transparent routing)
- Default peer failure mode is KillAll (safe); override to Continue for loose coupling
- Channels complement the DAG — a channel can exist between jobs that also have depends_on edges
