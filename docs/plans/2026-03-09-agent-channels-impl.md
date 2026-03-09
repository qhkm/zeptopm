# Agent Channels Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add real-time orchestrator-routed communication channels between running agent jobs, enabling debate, review, and streaming collaboration without agents knowing about each other.

**Architecture:** The orchestrator gains a Channel Router — a HashMap of named channels in RunStore. When a planner includes `channels` in its execution plan, the orchestrator materializes them alongside jobs. The engine manages channel lifecycle: activation when participants are Running, message routing via existing `AgentHandle.chat()`, turn management, and termination (max_rounds, channel_done, peer failure). No IPC protocol changes — agents see regular chat messages.

**Tech Stack:** Rust, serde, tokio (async), existing zeptoPM orchestrator crate

---

## Task 1: Channel Data Types

**Files:**
- Modify: `src/orchestrator/types.rs`
- Test: inline `#[cfg(test)]` module in same file

**Step 1: Write the failing test**

Add to the bottom of `src/orchestrator/types.rs`, inside a new test or extending existing tests:

```rust
#[test]
fn test_channel_mode_serialization() {
    let mode = ChannelMode::TurnBased;
    let json = serde_json::to_string(&mode).unwrap();
    assert_eq!(json, "\"TurnBased\"");
    let back: ChannelMode = serde_json::from_str(&json).unwrap();
    assert_eq!(back, ChannelMode::TurnBased);
}

#[test]
fn test_peer_failure_default() {
    let pf = PeerFailure::default();
    assert_eq!(pf, PeerFailure::KillAll);
}

#[test]
fn test_channel_message_round_tracking() {
    let msg = ChannelMessage {
        from_job: "job_1".into(),
        content: "Hello".into(),
        timestamp: SystemTime::now(),
        round: 1,
    };
    assert_eq!(msg.round, 1);
}

#[test]
fn test_planned_channel_deserialization() {
    let json = r#"{
        "channel_id": "draft-review",
        "participants": ["writer", "reviewer"],
        "mode": "TurnBased",
        "max_rounds": 3,
        "initial_message": "Write a blog post"
    }"#;
    let pc: PlannedChannel = serde_json::from_str(json).unwrap();
    assert_eq!(pc.channel_id, "draft-review");
    assert_eq!(pc.participants.len(), 2);
    assert_eq!(pc.max_rounds, Some(3));
    assert_eq!(pc.mode, ChannelMode::TurnBased);
}

#[test]
fn test_execution_plan_with_channels() {
    let json = r#"{
        "jobs": [{
            "local_id": "w", "role": "writer", "profile_id": "writer",
            "instruction": "Write", "depends_on": []
        }],
        "channels": [{
            "channel_id": "ch1",
            "participants": ["w"],
            "mode": "TurnBased"
        }]
    }"#;
    let plan: ExecutionPlan = serde_json::from_str(json).unwrap();
    assert_eq!(plan.channels.len(), 1);
}
```

**Step 2: Run test to verify it fails**

Run: `cd ~/ios/zeptoPM && cargo test --lib orchestrator::types -- --nocapture 2>&1 | tail -20`
Expected: FAIL — `ChannelMode`, `PeerFailure`, `ChannelMessage`, `PlannedChannel` types don't exist yet.

**Step 3: Write minimal implementation**

Add these types to `src/orchestrator/types.rs` (before the `ExecutionPlan` struct):

```rust
pub type ChannelId = String;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChannelMode {
    TurnBased,
    Stream,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PeerFailure {
    KillAll,
    Continue,
}

impl Default for PeerFailure {
    fn default() -> Self {
        PeerFailure::KillAll
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub from_job: JobId,
    pub content: String,
    pub timestamp: SystemTime,
    pub round: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub channel_id: ChannelId,
    pub run_id: RunId,
    pub participants: Vec<JobId>,
    pub mode: ChannelMode,
    pub max_rounds: Option<u32>,
    pub on_peer_failure: PeerFailure,
    pub current_round: u32,
    pub current_speaker_idx: usize,
    pub active: bool,
    pub history: Vec<ChannelMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedChannel {
    pub channel_id: String,
    pub participants: Vec<String>,
    pub mode: ChannelMode,
    #[serde(default)]
    pub max_rounds: Option<u32>,
    #[serde(default)]
    pub on_peer_failure: PeerFailure,
    #[serde(default)]
    pub initial_message: Option<String>,
}
```

Then modify `ExecutionPlan` to include channels:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub jobs: Vec<PlannedJob>,
    #[serde(default)]
    pub channels: Vec<PlannedChannel>,
}
```

**Step 4: Run test to verify it passes**

Run: `cd ~/ios/zeptoPM && cargo test --lib orchestrator::types -- --nocapture 2>&1 | tail -20`
Expected: All type tests PASS. Existing tests still pass (ExecutionPlan gains `channels` field with `#[serde(default)]` so old JSON without channels still deserializes).

**Step 5: Verify existing tests still pass**

Run: `cd ~/ios/zeptoPM && cargo test 2>&1 | tail -5`
Expected: All existing tests pass — `#[serde(default)]` on `channels` ensures backward compatibility.

**Step 6: Commit**

```bash
git add src/orchestrator/types.rs
git commit -m "feat(channels): add Channel, ChannelMode, PeerFailure, PlannedChannel types"
```

---

## Task 2: Channel Store Integration

**Files:**
- Modify: `src/orchestrator/store.rs`
- Test: inline `#[cfg(test)]` module in same file

**Step 1: Write the failing test**

Add to `src/orchestrator/store.rs` tests:

```rust
#[test]
fn test_create_and_get_channel() {
    let mut store = RunStore::new();
    let channel = Channel {
        channel_id: "ch_1".into(),
        run_id: "run_1".into(),
        participants: vec!["job_1".into(), "job_2".into()],
        mode: ChannelMode::TurnBased,
        max_rounds: Some(3),
        on_peer_failure: PeerFailure::KillAll,
        current_round: 0,
        current_speaker_idx: 0,
        active: false,
        history: vec![],
    };
    store.create_channel(channel);
    let got = store.get_channel("ch_1").unwrap();
    assert_eq!(got.participants.len(), 2);
    assert_eq!(got.mode, ChannelMode::TurnBased);
}

#[test]
fn test_get_channel_mut() {
    let mut store = RunStore::new();
    let channel = Channel {
        channel_id: "ch_1".into(),
        run_id: "run_1".into(),
        participants: vec!["job_1".into()],
        mode: ChannelMode::Stream,
        max_rounds: None,
        on_peer_failure: PeerFailure::Continue,
        current_round: 0,
        current_speaker_idx: 0,
        active: false,
        history: vec![],
    };
    store.create_channel(channel);
    let ch = store.get_channel_mut("ch_1").unwrap();
    ch.active = true;
    ch.current_round = 1;
    assert!(store.get_channel("ch_1").unwrap().active);
}

#[test]
fn test_list_run_channels() {
    let mut store = RunStore::new();
    for i in 0..3 {
        store.create_channel(Channel {
            channel_id: format!("ch_{}", i),
            run_id: if i < 2 { "run_1".into() } else { "run_2".into() },
            participants: vec![],
            mode: ChannelMode::TurnBased,
            max_rounds: None,
            on_peer_failure: PeerFailure::KillAll,
            current_round: 0,
            current_speaker_idx: 0,
            active: false,
            history: vec![],
        });
    }
    assert_eq!(store.list_run_channels("run_1").len(), 2);
    assert_eq!(store.list_run_channels("run_2").len(), 1);
}

#[test]
fn test_channels_for_job() {
    let mut store = RunStore::new();
    store.create_channel(Channel {
        channel_id: "ch_1".into(),
        run_id: "run_1".into(),
        participants: vec!["job_A".into(), "job_B".into()],
        mode: ChannelMode::TurnBased,
        max_rounds: None,
        on_peer_failure: PeerFailure::KillAll,
        current_round: 0,
        current_speaker_idx: 0,
        active: true,
        history: vec![],
    });
    store.create_channel(Channel {
        channel_id: "ch_2".into(),
        run_id: "run_1".into(),
        participants: vec!["job_B".into(), "job_C".into()],
        mode: ChannelMode::Stream,
        max_rounds: None,
        on_peer_failure: PeerFailure::Continue,
        current_round: 0,
        current_speaker_idx: 0,
        active: true,
        history: vec![],
    });
    let b_channels = store.channels_for_job("job_B");
    assert_eq!(b_channels.len(), 2);
    let a_channels = store.channels_for_job("job_A");
    assert_eq!(a_channels.len(), 1);
}

#[test]
fn test_remove_run_clears_channels() {
    let mut store = RunStore::new();
    store.create_run(make_run());
    store.create_channel(Channel {
        channel_id: "ch_1".into(),
        run_id: "run_1".into(),
        participants: vec![],
        mode: ChannelMode::TurnBased,
        max_rounds: None,
        on_peer_failure: PeerFailure::KillAll,
        current_round: 0,
        current_speaker_idx: 0,
        active: false,
        history: vec![],
    });
    store.remove_run("run_1");
    assert!(store.get_channel("ch_1").is_none());
    assert!(store.list_run_channels("run_1").is_empty());
}
```

**Step 2: Run test to verify it fails**

Run: `cd ~/ios/zeptoPM && cargo test --lib orchestrator::store -- --nocapture 2>&1 | tail -20`
Expected: FAIL — `create_channel`, `get_channel`, etc. don't exist.

**Step 3: Write minimal implementation**

Add `channels` HashMap to `RunStore` and CRUD methods:

In `RunStore` struct, add:
```rust
channels: HashMap<ChannelId, Channel>,
```

In `RunStore::new()`, add:
```rust
channels: HashMap::new(),
```

Add methods to `impl RunStore`:
```rust
pub fn create_channel(&mut self, channel: Channel) {
    self.channels.insert(channel.channel_id.clone(), channel);
}

pub fn get_channel(&self, channel_id: &str) -> Option<&Channel> {
    self.channels.get(channel_id)
}

pub fn get_channel_mut(&mut self, channel_id: &str) -> Option<&mut Channel> {
    self.channels.get_mut(channel_id)
}

pub fn list_run_channels(&self, run_id: &str) -> Vec<&Channel> {
    self.channels.values().filter(|c| c.run_id == run_id).collect()
}

pub fn channels_for_job(&self, job_id: &str) -> Vec<&Channel> {
    self.channels.values()
        .filter(|c| c.active && c.participants.contains(&job_id.to_string()))
        .collect()
}
```

In `remove_run()`, add channel cleanup (before the `paths` return):
```rust
let channel_ids: Vec<String> = self.channels.values()
    .filter(|c| c.run_id == run_id)
    .map(|c| c.channel_id.clone())
    .collect();
for cid in &channel_ids {
    self.channels.remove(cid);
}
```

**Step 4: Run test to verify it passes**

Run: `cd ~/ios/zeptoPM && cargo test --lib orchestrator::store -- --nocapture 2>&1 | tail -20`
Expected: All store tests PASS.

**Step 5: Commit**

```bash
git add src/orchestrator/store.rs
git commit -m "feat(channels): add channel storage to RunStore with CRUD and lookup methods"
```

---

## Task 3: Channel Validation in Planner

**Files:**
- Modify: `src/orchestrator/planner.rs`
- Test: inline `#[cfg(test)]` module in same file

**Step 1: Write the failing test**

Add to `src/orchestrator/planner.rs` tests:

```rust
#[test]
fn test_validate_plan_with_valid_channels() {
    let plan = ExecutionPlan {
        jobs: vec![
            PlannedJob {
                local_id: "writer".into(),
                role: "writer".into(),
                profile_id: "writer".into(),
                instruction: "Write".into(),
                depends_on: vec![],
            },
            PlannedJob {
                local_id: "reviewer".into(),
                role: "reviewer".into(),
                profile_id: "reviewer".into(),
                instruction: "Review".into(),
                depends_on: vec![],
            },
        ],
        channels: vec![PlannedChannel {
            channel_id: "draft-review".into(),
            participants: vec!["writer".into(), "reviewer".into()],
            mode: ChannelMode::TurnBased,
            max_rounds: Some(3),
            on_peer_failure: PeerFailure::KillAll,
            initial_message: Some("Write a blog post".into()),
        }],
    };
    assert!(validate_plan(&plan).is_empty());
}

#[test]
fn test_validate_channel_unknown_participant() {
    let plan = ExecutionPlan {
        jobs: vec![PlannedJob {
            local_id: "writer".into(),
            role: "writer".into(),
            profile_id: "writer".into(),
            instruction: "Write".into(),
            depends_on: vec![],
        }],
        channels: vec![PlannedChannel {
            channel_id: "ch1".into(),
            participants: vec!["writer".into(), "ghost".into()],
            mode: ChannelMode::TurnBased,
            max_rounds: None,
            on_peer_failure: PeerFailure::KillAll,
            initial_message: None,
        }],
    };
    let errors = validate_plan(&plan);
    assert!(errors.iter().any(|e| e.contains("ghost") && e.contains("not found")));
}

#[test]
fn test_validate_channel_duplicate_id() {
    let plan = ExecutionPlan {
        jobs: vec![PlannedJob {
            local_id: "a".into(),
            role: "r".into(),
            profile_id: "r".into(),
            instruction: "do".into(),
            depends_on: vec![],
        }],
        channels: vec![
            PlannedChannel {
                channel_id: "ch1".into(),
                participants: vec!["a".into()],
                mode: ChannelMode::TurnBased,
                max_rounds: None,
                on_peer_failure: PeerFailure::KillAll,
                initial_message: None,
            },
            PlannedChannel {
                channel_id: "ch1".into(),
                participants: vec!["a".into()],
                mode: ChannelMode::Stream,
                max_rounds: None,
                on_peer_failure: PeerFailure::Continue,
                initial_message: None,
            },
        ],
    };
    let errors = validate_plan(&plan);
    assert!(errors.iter().any(|e| e.contains("duplicate") && e.contains("ch1")));
}

#[test]
fn test_validate_channel_empty_participants() {
    let plan = ExecutionPlan {
        jobs: vec![PlannedJob {
            local_id: "a".into(),
            role: "r".into(),
            profile_id: "r".into(),
            instruction: "do".into(),
            depends_on: vec![],
        }],
        channels: vec![PlannedChannel {
            channel_id: "ch1".into(),
            participants: vec![],
            mode: ChannelMode::TurnBased,
            max_rounds: None,
            on_peer_failure: PeerFailure::KillAll,
            initial_message: None,
        }],
    };
    let errors = validate_plan(&plan);
    assert!(errors.iter().any(|e| e.contains("no participants")));
}

#[test]
fn test_validate_turn_based_needs_two_participants() {
    let plan = ExecutionPlan {
        jobs: vec![PlannedJob {
            local_id: "a".into(),
            role: "r".into(),
            profile_id: "r".into(),
            instruction: "do".into(),
            depends_on: vec![],
        }],
        channels: vec![PlannedChannel {
            channel_id: "ch1".into(),
            participants: vec!["a".into()],
            mode: ChannelMode::TurnBased,
            max_rounds: None,
            on_peer_failure: PeerFailure::KillAll,
            initial_message: None,
        }],
    };
    let errors = validate_plan(&plan);
    assert!(errors.iter().any(|e| e.contains("TurnBased") && e.contains("at least 2")));
}
```

**Step 2: Run test to verify it fails**

Run: `cd ~/ios/zeptoPM && cargo test --lib orchestrator::planner -- --nocapture 2>&1 | tail -20`
Expected: FAIL — existing `validate_plan` doesn't check channels; new tests reference `PlannedChannel`, `ChannelMode` etc. that need importing; existing tests fail because `ExecutionPlan` now has a `channels` field.

**Step 3: Fix existing tests and add channel validation**

First, update imports at top of planner.rs (already has `use crate::orchestrator::types::*;`).

Add channel validation to `validate_plan()` after the existing duplicate local_id check:

```rust
// Validate channels
let mut seen_channels = std::collections::HashSet::new();
for (i, ch) in plan.channels.iter().enumerate() {
    if !seen_channels.insert(&ch.channel_id) {
        errors.push(format!("duplicate channel_id: '{}'", ch.channel_id));
    }
    if ch.participants.is_empty() {
        errors.push(format!("channel[{}] '{}': no participants", i, ch.channel_id));
    }
    if ch.mode == ChannelMode::TurnBased && ch.participants.len() < 2 {
        errors.push(format!(
            "channel[{}] '{}': TurnBased requires at least 2 participants",
            i, ch.channel_id
        ));
    }
    for p in &ch.participants {
        if !local_ids.contains(p.as_str()) {
            errors.push(format!(
                "channel[{}] '{}': participant '{}' not found in plan jobs",
                i, ch.channel_id, p
            ));
        }
    }
}
```

Update existing test constructors for `ExecutionPlan` to include `channels: vec![]`.

**Step 4: Run test to verify it passes**

Run: `cd ~/ios/zeptoPM && cargo test --lib orchestrator::planner -- --nocapture 2>&1 | tail -20`
Expected: All planner tests PASS.

**Step 5: Commit**

```bash
git add src/orchestrator/planner.rs
git commit -m "feat(channels): validate channel definitions in execution plan"
```

---

## Task 4: Channel Materialization in Planner

**Files:**
- Modify: `src/orchestrator/planner.rs`
- Test: inline `#[cfg(test)]` module in same file

**Step 1: Write the failing test**

```rust
#[test]
fn test_materialize_plan_creates_channels() {
    let mut store = RunStore::new();
    let plan = ExecutionPlan {
        jobs: vec![
            PlannedJob {
                local_id: "writer".into(),
                role: "writer".into(),
                profile_id: "writer".into(),
                instruction: "Write draft".into(),
                depends_on: vec![],
            },
            PlannedJob {
                local_id: "reviewer".into(),
                role: "reviewer".into(),
                profile_id: "reviewer".into(),
                instruction: "Review draft".into(),
                depends_on: vec![],
            },
        ],
        channels: vec![PlannedChannel {
            channel_id: "draft-review".into(),
            participants: vec!["writer".into(), "reviewer".into()],
            mode: ChannelMode::TurnBased,
            max_rounds: Some(3),
            on_peer_failure: PeerFailure::KillAll,
            initial_message: Some("Write a blog post about Rust".into()),
        }],
    };

    let job_ids = materialize_plan(&mut store, "run_1", "planner_job", &plan);
    assert_eq!(job_ids.len(), 2);

    // Channel should be created with real job IDs (not local_ids)
    let channels = store.list_run_channels("run_1");
    assert_eq!(channels.len(), 1);
    let ch = channels[0];
    assert_eq!(ch.channel_id, "draft-review");
    assert_eq!(ch.participants.len(), 2);
    assert_eq!(ch.participants[0], job_ids[0]); // writer's real ID
    assert_eq!(ch.participants[1], job_ids[1]); // reviewer's real ID
    assert_eq!(ch.mode, ChannelMode::TurnBased);
    assert_eq!(ch.max_rounds, Some(3));
    assert!(!ch.active); // not active until participants are Running
}

#[test]
fn test_materialize_plan_no_channels() {
    let mut store = RunStore::new();
    let plan = ExecutionPlan {
        jobs: vec![PlannedJob {
            local_id: "a".into(),
            role: "r".into(),
            profile_id: "r".into(),
            instruction: "do".into(),
            depends_on: vec![],
        }],
        channels: vec![],
    };
    let _ = materialize_plan(&mut store, "run_1", "p", &plan);
    assert!(store.list_run_channels("run_1").is_empty());
}
```

**Step 2: Run test to verify it fails**

Run: `cd ~/ios/zeptoPM && cargo test --lib orchestrator::planner::tests::test_materialize_plan_creates_channels -- --nocapture 2>&1 | tail -20`
Expected: FAIL — `materialize_plan` doesn't create channels yet.

**Step 3: Add channel materialization to `materialize_plan()`**

After the job creation loop (before `created`), add:

```rust
// Materialize channels — translate local_ids to real job IDs
for planned_ch in &plan.channels {
    let real_participants: Vec<JobId> = planned_ch
        .participants
        .iter()
        .filter_map(|local| id_map.get(local).cloned())
        .collect();

    let channel = Channel {
        channel_id: planned_ch.channel_id.clone(),
        run_id: run_id.into(),
        participants: real_participants,
        mode: planned_ch.mode.clone(),
        max_rounds: planned_ch.max_rounds,
        on_peer_failure: planned_ch.on_peer_failure.clone(),
        current_round: 0,
        current_speaker_idx: 0,
        active: false,
        history: vec![],
    };
    store.create_channel(channel);
}
```

**Step 4: Run test to verify it passes**

Run: `cd ~/ios/zeptoPM && cargo test --lib orchestrator::planner -- --nocapture 2>&1 | tail -20`
Expected: All planner tests PASS.

**Step 5: Commit**

```bash
git add src/orchestrator/planner.rs
git commit -m "feat(channels): materialize planned channels with real job IDs"
```

---

## Task 5: Channel Activation in Engine

**Files:**
- Modify: `src/orchestrator/engine.rs`
- Test: inline `#[cfg(test)]` module

**Step 1: Write the failing test**

```rust
#[test]
fn test_activate_channels_when_participants_running() {
    let mut engine = OrchestratorEngine::new(4);
    let run_id = engine.submit_run("test".into());

    // Create two jobs and a channel between them
    let job_a = gen_id("job");
    let job_b = gen_id("job");
    engine.store.create_job(Job {
        job_id: job_a.clone(), run_id: run_id.clone(),
        parent_job_id: None, role: "writer".into(),
        status: JobStatus::Running, instruction: "write".into(),
        input_artifact_ids: vec![], depends_on: vec![],
        children: vec![], profile_id: "writer".into(),
        workspace_dir: std::path::PathBuf::from("/tmp"),
        attempt: 1, max_attempts: 3, created_at: SystemTime::now(),
        started_at: Some(SystemTime::now()), finished_at: None,
        output_artifact_ids: vec![], error: None, revision_round: 0,
    });
    engine.store.create_job(Job {
        job_id: job_b.clone(), run_id: run_id.clone(),
        parent_job_id: None, role: "reviewer".into(),
        status: JobStatus::Running, instruction: "review".into(),
        input_artifact_ids: vec![], depends_on: vec![],
        children: vec![], profile_id: "reviewer".into(),
        workspace_dir: std::path::PathBuf::from("/tmp"),
        attempt: 1, max_attempts: 3, created_at: SystemTime::now(),
        started_at: Some(SystemTime::now()), finished_at: None,
        output_artifact_ids: vec![], error: None, revision_round: 0,
    });
    engine.active_jobs.insert(job_a.clone(), run_id.clone());
    engine.active_jobs.insert(job_b.clone(), run_id.clone());

    engine.store.create_channel(Channel {
        channel_id: "ch1".into(), run_id: run_id.clone(),
        participants: vec![job_a.clone(), job_b.clone()],
        mode: ChannelMode::TurnBased, max_rounds: Some(3),
        on_peer_failure: PeerFailure::KillAll,
        current_round: 0, current_speaker_idx: 0,
        active: false, history: vec![],
    });

    let activated = engine.activate_ready_channels();
    assert_eq!(activated.len(), 1);
    assert_eq!(activated[0], "ch1");
    assert!(engine.store.get_channel("ch1").unwrap().active);
}

#[test]
fn test_channel_not_activated_if_participant_pending() {
    let mut engine = OrchestratorEngine::new(4);
    let run_id = engine.submit_run("test".into());

    let job_a = gen_id("job");
    let job_b = gen_id("job");
    engine.store.create_job(Job {
        job_id: job_a.clone(), run_id: run_id.clone(),
        parent_job_id: None, role: "writer".into(),
        status: JobStatus::Running, instruction: "write".into(),
        input_artifact_ids: vec![], depends_on: vec![],
        children: vec![], profile_id: "writer".into(),
        workspace_dir: std::path::PathBuf::from("/tmp"),
        attempt: 1, max_attempts: 3, created_at: SystemTime::now(),
        started_at: Some(SystemTime::now()), finished_at: None,
        output_artifact_ids: vec![], error: None, revision_round: 0,
    });
    engine.store.create_job(Job {
        job_id: job_b.clone(), run_id: run_id.clone(),
        parent_job_id: None, role: "reviewer".into(),
        status: JobStatus::Pending, instruction: "review".into(),
        input_artifact_ids: vec![], depends_on: vec![job_a.clone()],
        children: vec![], profile_id: "reviewer".into(),
        workspace_dir: std::path::PathBuf::from("/tmp"),
        attempt: 0, max_attempts: 3, created_at: SystemTime::now(),
        started_at: None, finished_at: None,
        output_artifact_ids: vec![], error: None, revision_round: 0,
    });
    engine.active_jobs.insert(job_a.clone(), run_id.clone());

    engine.store.create_channel(Channel {
        channel_id: "ch1".into(), run_id: run_id.clone(),
        participants: vec![job_a.clone(), job_b.clone()],
        mode: ChannelMode::TurnBased, max_rounds: None,
        on_peer_failure: PeerFailure::KillAll,
        current_round: 0, current_speaker_idx: 0,
        active: false, history: vec![],
    });

    let activated = engine.activate_ready_channels();
    assert!(activated.is_empty());
    assert!(!engine.store.get_channel("ch1").unwrap().active);
}
```

**Step 2: Run test to verify it fails**

Run: `cd ~/ios/zeptoPM && cargo test --lib orchestrator::engine::tests::test_activate_channels -- --nocapture 2>&1 | tail -20`
Expected: FAIL — `activate_ready_channels()` doesn't exist.

**Step 3: Write minimal implementation**

Add to `impl OrchestratorEngine`:

```rust
/// Activate channels whose participants are all Running.
/// Returns list of activated channel IDs.
pub fn activate_ready_channels(&mut self) -> Vec<ChannelId> {
    let run_ids: Vec<RunId> = self.active_jobs.values().cloned().collect::<std::collections::HashSet<_>>().into_iter().collect();
    let mut activated = Vec::new();

    for run_id in &run_ids {
        let channel_ids: Vec<ChannelId> = self.store.list_run_channels(run_id)
            .iter()
            .filter(|c| !c.active)
            .map(|c| c.channel_id.clone())
            .collect();

        for ch_id in channel_ids {
            let all_running = {
                let ch = self.store.get_channel(&ch_id).unwrap();
                ch.participants.iter().all(|p| {
                    self.store.get_job(p)
                        .map(|j| j.status == JobStatus::Running)
                        .unwrap_or(false)
                })
            };
            if all_running {
                if let Some(ch) = self.store.get_channel_mut(&ch_id) {
                    ch.active = true;
                }
                activated.push(ch_id);
            }
        }
    }
    activated
}
```

**Step 4: Run test to verify it passes**

Run: `cd ~/ios/zeptoPM && cargo test --lib orchestrator::engine -- --nocapture 2>&1 | tail -20`
Expected: All engine tests PASS.

**Step 5: Commit**

```bash
git add src/orchestrator/engine.rs
git commit -m "feat(channels): activate channels when all participants are Running"
```

---

## Task 6: Channel Message Routing in Engine

**Files:**
- Modify: `src/orchestrator/engine.rs`
- Test: inline `#[cfg(test)]` module

**Step 1: Write the failing test**

```rust
#[test]
fn test_advance_turn_based_channel() {
    let mut engine = OrchestratorEngine::new(4);

    let ch = Channel {
        channel_id: "ch1".into(), run_id: "run_1".into(),
        participants: vec!["job_A".into(), "job_B".into()],
        mode: ChannelMode::TurnBased, max_rounds: Some(3),
        on_peer_failure: PeerFailure::KillAll,
        current_round: 0, current_speaker_idx: 0,
        active: true, history: vec![],
    };
    engine.store.create_channel(ch);

    // Simulate job_A sending a message
    let action = engine.route_channel_message("ch1", "job_A", "Here is my draft");
    match action {
        ChannelAction::SendTo { job_id, message } => {
            assert_eq!(job_id, "job_B");
            assert!(message.contains("Here is my draft"));
        }
        _ => panic!("expected SendTo"),
    }

    // Round should have advanced
    let ch = engine.store.get_channel("ch1").unwrap();
    assert_eq!(ch.current_speaker_idx, 1);
    assert_eq!(ch.history.len(), 1);
}

#[test]
fn test_channel_max_rounds_termination() {
    let mut engine = OrchestratorEngine::new(4);

    let ch = Channel {
        channel_id: "ch1".into(), run_id: "run_1".into(),
        participants: vec!["job_A".into(), "job_B".into()],
        mode: ChannelMode::TurnBased, max_rounds: Some(1),
        on_peer_failure: PeerFailure::KillAll,
        current_round: 0, current_speaker_idx: 0,
        active: true, history: vec![],
    };
    engine.store.create_channel(ch);

    // A speaks (round 0, speaker 0 → speaker 1)
    let _ = engine.route_channel_message("ch1", "job_A", "draft");
    // B speaks (round 0, speaker 1 → round 1, speaker 0)
    let action = engine.route_channel_message("ch1", "job_B", "looks good");

    match action {
        ChannelAction::Close { channel_id } => {
            assert_eq!(channel_id, "ch1");
        }
        _ => panic!("expected Close after max_rounds reached, got {:?}", action),
    }
}

#[test]
fn test_channel_done_signal() {
    let mut engine = OrchestratorEngine::new(4);

    let ch = Channel {
        channel_id: "ch1".into(), run_id: "run_1".into(),
        participants: vec!["job_A".into(), "job_B".into()],
        mode: ChannelMode::TurnBased, max_rounds: None,
        on_peer_failure: PeerFailure::KillAll,
        current_round: 0, current_speaker_idx: 0,
        active: true, history: vec![],
    };
    engine.store.create_channel(ch);

    let action = engine.handle_channel_done("ch1", "job_A");
    match action {
        ChannelAction::Close { channel_id } => {
            assert_eq!(channel_id, "ch1");
        }
        _ => panic!("expected Close"),
    }
    assert!(!engine.store.get_channel("ch1").unwrap().active);
}

#[test]
fn test_peer_failure_kill_all() {
    let mut engine = OrchestratorEngine::new(4);

    let ch = Channel {
        channel_id: "ch1".into(), run_id: "run_1".into(),
        participants: vec!["job_A".into(), "job_B".into()],
        mode: ChannelMode::TurnBased, max_rounds: None,
        on_peer_failure: PeerFailure::KillAll,
        current_round: 0, current_speaker_idx: 0,
        active: true, history: vec![],
    };
    engine.store.create_channel(ch);

    let action = engine.handle_channel_peer_failure("ch1", "job_A");
    match action {
        ChannelAction::KillParticipants { job_ids } => {
            assert!(job_ids.contains(&"job_B".to_string()));
        }
        _ => panic!("expected KillParticipants"),
    }
}

#[test]
fn test_peer_failure_continue() {
    let mut engine = OrchestratorEngine::new(4);

    let ch = Channel {
        channel_id: "ch1".into(), run_id: "run_1".into(),
        participants: vec!["job_A".into(), "job_B".into()],
        mode: ChannelMode::TurnBased, max_rounds: None,
        on_peer_failure: PeerFailure::Continue,
        current_round: 0, current_speaker_idx: 0,
        active: true, history: vec![],
    };
    engine.store.create_channel(ch);

    let action = engine.handle_channel_peer_failure("ch1", "job_A");
    match action {
        ChannelAction::NotifyPeers { job_ids, message } => {
            assert!(job_ids.contains(&"job_B".to_string()));
            assert!(message.contains("disconnected"));
        }
        _ => panic!("expected NotifyPeers"),
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cd ~/ios/zeptoPM && cargo test --lib orchestrator::engine::tests::test_advance_turn -- --nocapture 2>&1 | tail -20`
Expected: FAIL — `ChannelAction`, `route_channel_message()`, etc. don't exist.

**Step 3: Write minimal implementation**

Add `ChannelAction` enum to `src/orchestrator/types.rs`:

```rust
#[derive(Debug, Clone)]
pub enum ChannelAction {
    SendTo { job_id: JobId, message: String },
    Close { channel_id: ChannelId },
    KillParticipants { job_ids: Vec<JobId> },
    NotifyPeers { job_ids: Vec<JobId>, message: String },
    NoOp,
}
```

Add to `impl OrchestratorEngine`:

```rust
/// Route a message from a channel participant to the next.
/// Returns the action the daemon should take.
pub fn route_channel_message(
    &mut self,
    channel_id: &str,
    from_job: &str,
    content: &str,
) -> ChannelAction {
    let (participants, mode, max_rounds, next_speaker, new_round) = {
        let ch = match self.store.get_channel(channel_id) {
            Some(c) if c.active => c,
            _ => return ChannelAction::NoOp,
        };
        let next_idx = (ch.current_speaker_idx + 1) % ch.participants.len();
        let new_round = if next_idx == 0 {
            ch.current_round + 1
        } else {
            ch.current_round
        };
        (
            ch.participants.clone(),
            ch.mode.clone(),
            ch.max_rounds,
            next_idx,
            new_round,
        )
    };

    // Record the message in history
    if let Some(ch) = self.store.get_channel_mut(channel_id) {
        ch.history.push(ChannelMessage {
            from_job: from_job.into(),
            content: content.into(),
            timestamp: SystemTime::now(),
            round: ch.current_round,
        });
        ch.current_speaker_idx = next_speaker;
        ch.current_round = new_round;
    }

    // Check max_rounds termination
    if let Some(max) = max_rounds {
        if new_round >= max {
            if let Some(ch) = self.store.get_channel_mut(channel_id) {
                ch.active = false;
            }
            return ChannelAction::Close {
                channel_id: channel_id.into(),
            };
        }
    }

    match mode {
        ChannelMode::TurnBased => {
            let next_job = participants[next_speaker].clone();
            ChannelAction::SendTo {
                job_id: next_job,
                message: content.into(),
            }
        }
        ChannelMode::Stream => {
            // In stream mode, broadcast to all other participants
            let next_job = participants
                .iter()
                .find(|p| p.as_str() != from_job)
                .cloned()
                .unwrap_or_default();
            ChannelAction::SendTo {
                job_id: next_job,
                message: content.into(),
            }
        }
    }
}

/// Handle a channel_done signal from a participant.
pub fn handle_channel_done(&mut self, channel_id: &str, _from_job: &str) -> ChannelAction {
    if let Some(ch) = self.store.get_channel_mut(channel_id) {
        ch.active = false;
    }
    ChannelAction::Close {
        channel_id: channel_id.into(),
    }
}

/// Handle peer failure — when a participant dies or is killed.
pub fn handle_channel_peer_failure(
    &mut self,
    channel_id: &str,
    failed_job: &str,
) -> ChannelAction {
    let (on_failure, surviving) = {
        let ch = match self.store.get_channel(channel_id) {
            Some(c) => c,
            None => return ChannelAction::NoOp,
        };
        let surviving: Vec<JobId> = ch
            .participants
            .iter()
            .filter(|p| p.as_str() != failed_job)
            .cloned()
            .collect();
        (ch.on_peer_failure.clone(), surviving)
    };

    if let Some(ch) = self.store.get_channel_mut(channel_id) {
        ch.active = false;
    }

    match on_failure {
        PeerFailure::KillAll => ChannelAction::KillParticipants { job_ids: surviving },
        PeerFailure::Continue => ChannelAction::NotifyPeers {
            job_ids: surviving,
            message: format!("peer '{}' disconnected", failed_job),
        },
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cd ~/ios/zeptoPM && cargo test --lib orchestrator::engine -- --nocapture 2>&1 | tail -20`
Expected: All engine tests PASS.

**Step 5: Commit**

```bash
git add src/orchestrator/types.rs src/orchestrator/engine.rs
git commit -m "feat(channels): message routing, termination, and peer failure handling"
```

---

## Task 7: Daemon Event Wiring — Channel Activation

**Files:**
- Modify: `src/daemon.rs`
- Test: manual integration test (existing daemon loop handles events)

**Step 1: Wire channel activation after job spawning**

In `daemon.rs`, after every call to `spawn_job_worker()` (there are 4 locations), add channel activation check:

```rust
// Activate channels where all participants are now Running
let activated_channels = orchestrator.activate_ready_channels();
for ch_id in &activated_channels {
    info!(channel_id = %ch_id, "channel activated — all participants running");
    // Send initial message to first speaker if defined
    if let Some(ch) = orchestrator.store.get_channel(ch_id) {
        // Find the PlannedChannel initial_message — stored as first history entry
        // For now, the initial kick is handled in Task 8
    }
}
```

This is a wiring-only task — no new logic, just calling `activate_ready_channels()` at the right points.

**Step 2: Wire peer failure handling on job_failed**

In the `"job_failed"` event handler (around line 483), after `orchestrator.mark_failed()`, add:

```rust
// Check if failed job was a channel participant
let failed_channels: Vec<String> = orchestrator.store.channels_for_job(&job_id)
    .iter()
    .map(|c| c.channel_id.clone())
    .collect();
for ch_id in failed_channels {
    let action = orchestrator.handle_channel_peer_failure(&ch_id, &job_id);
    match action {
        crate::orchestrator::types::ChannelAction::KillParticipants { job_ids } => {
            for kill_id in &job_ids {
                warn!(job_id = %kill_id, channel = %ch_id, "killing channel peer due to KillAll policy");
                let worker_name = format!("__job_{}", kill_id);
                if let Some(internal) = managed.get(&worker_name) {
                    internal.handle.stop().await;
                }
                managed.remove(&worker_name);
                orchestrator.mark_failed(kill_id, format!("channel peer '{}' failed (KillAll)", job_id));
            }
        }
        crate::orchestrator::types::ChannelAction::NotifyPeers { job_ids, message } => {
            for peer_id in &job_ids {
                info!(job_id = %peer_id, channel = %ch_id, "notifying peer of disconnection");
                let worker_name = format!("__job_{}", peer_id);
                if let Some(internal) = managed.get(&worker_name) {
                    let _ = internal.handle.send_message(message.clone()).await;
                }
            }
        }
        _ => {}
    }
}
```

**Step 3: Verify compilation and existing tests pass**

Run: `cd ~/ios/zeptoPM && cargo test 2>&1 | tail -5`
Expected: All tests pass, no compilation errors.

**Step 4: Commit**

```bash
git add src/daemon.rs
git commit -m "feat(channels): wire channel activation and peer failure handling in daemon"
```

---

## Task 8: Daemon Event Wiring — Chat Response Routing

**Files:**
- Modify: `src/daemon.rs`
- Modify: `src/agent.rs` (add `chat_response` forwarding to orch_event_tx when in channel mode)

**Step 1: Add chat_response forwarding from worker to orchestrator**

Currently, `chat_response` is handled internally in `agent.rs` (line 358). For channel routing, when a worker responds to a channel-routed chat, the response needs to flow back to the orchestrator.

In the daemon event handler, add a new event type `"channel_message"`:

```rust
"channel_message" => {
    let channel_id = event.get("channel_id").and_then(|v| v.as_str()).unwrap_or("");
    let from_job = event.get("from_job").and_then(|v| v.as_str()).unwrap_or("");
    let content = event.get("content").and_then(|v| v.as_str()).unwrap_or("");

    let action = orchestrator.route_channel_message(channel_id, from_job, content);
    match action {
        crate::orchestrator::types::ChannelAction::SendTo { job_id, message } => {
            let worker_name = format!("__job_{}", job_id);
            if let Some(internal) = managed.get(&worker_name) {
                info!(channel = %channel_id, to = %job_id, "routing channel message");
                let _ = internal.handle.send_message(message).await;
            }
        }
        crate::orchestrator::types::ChannelAction::Close { channel_id } => {
            info!(channel = %channel_id, "channel closed — max rounds or done");
        }
        _ => {}
    }
}
```

Also handle `"channel_done"`:

```rust
"channel_done" => {
    let channel_id = event.get("channel_id").and_then(|v| v.as_str()).unwrap_or("");
    let from_job = event.get("from_job").and_then(|v| v.as_str()).unwrap_or("");
    let action = orchestrator.handle_channel_done(channel_id, from_job);
    info!(channel = %channel_id, from = %from_job, "channel_done signal received");
    // Channel is now closed — participants can finish their current work
}
```

**Step 2: Wire initial message sending on channel activation**

Enhance the channel activation code from Task 7. When a channel is activated and has an `initial_message`, send it to the first participant:

For this, store the initial_message in Channel. Add `initial_message: Option<String>` to the `Channel` struct in types.rs, and populate it during materialization in planner.rs.

Then in daemon.rs, after `activate_ready_channels()`:

```rust
for ch_id in &activated_channels {
    if let Some(ch) = orchestrator.store.get_channel(ch_id) {
        if let Some(ref initial_msg) = ch.initial_message {
            let first_job = ch.participants.first().cloned();
            if let Some(job_id) = first_job {
                let worker_name = format!("__job_{}", job_id);
                if let Some(internal) = managed.get(&worker_name) {
                    info!(channel = %ch_id, to = %job_id, "sending initial channel message");
                    let _ = internal.handle.send_message(initial_msg.clone()).await;
                }
            }
        }
    }
}
```

**Step 3: Add `initial_message` field to Channel struct**

In `src/orchestrator/types.rs`, add to `Channel`:
```rust
pub initial_message: Option<String>,
```

In `src/orchestrator/planner.rs` `materialize_plan()`, populate it:
```rust
initial_message: planned_ch.initial_message.clone(),
```

Update all existing Channel constructors in tests to include `initial_message: None`.

**Step 4: Verify compilation and all tests pass**

Run: `cd ~/ios/zeptoPM && cargo test 2>&1 | tail -5`
Expected: All tests pass.

**Step 5: Commit**

```bash
git add src/orchestrator/types.rs src/orchestrator/engine.rs src/orchestrator/planner.rs src/orchestrator/store.rs src/daemon.rs
git commit -m "feat(channels): wire channel message routing and initial message delivery in daemon"
```

---

## Summary

| Task | Component | Tests Added | Estimated New LOC |
|------|-----------|-------------|-------------------|
| 1 | Channel data types | 4 | ~80 |
| 2 | Store integration | 5 | ~40 |
| 3 | Planner validation | 5 | ~30 |
| 4 | Planner materialization | 2 | ~20 |
| 5 | Engine activation | 2 | ~35 |
| 6 | Engine routing + termination | 5 | ~120 |
| 7 | Daemon peer failure wiring | 0 (integration) | ~30 |
| 8 | Daemon chat routing + initial message | 0 (integration) | ~40 |
| **Total** | | **~23 new tests** | **~395 LOC** |

### What does NOT change
- `worker.rs` — workers are unaware of channels
- `capsule.rs` — capsule isolation unchanged
- IPC protocol — reuses existing `cmd: "chat"` / `chat_response` messages
- Existing DAG execution — channels are additive; non-channel jobs work identically
