# Orchestration Layer — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a Run/Job/Artifact orchestration layer on top of the existing zeptoPM process manager, enabling multi-step workflows with dependency graphs, parallel execution, and structured data handoff.

**Architecture:** The orchestration layer sits above the existing daemon. It reuses the worker process model (agent.rs, worker.rs) for job execution. A new scheduler manages the job graph. The existing agent management (chat, start/stop, config reload) is untouched.

**Tech Stack:** Rust, tokio, serde_json, existing zeptoPM crate

**Prerequisite reading:**
- `docs/plans/2026-03-08-orchestration-design.md` — design decisions
- `docs/plans/2026-03-08-use-cases-and-benefits.md` — use cases

---

### Task 1: Orchestration types — Run, Job, Artifact

**Files:**
- Create: `src/orchestrator/mod.rs`
- Create: `src/orchestrator/types.rs`
- Modify: `src/lib.rs` (add `pub mod orchestrator;`)

**Step 1: Write the types**

`src/orchestrator/types.rs`:
```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

pub type RunId = String;
pub type JobId = String;
pub type ArtifactId = String;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JobStatus {
    Pending,    // waiting for dependencies
    Ready,      // dependencies met, waiting for a slot
    Running,    // worker process active
    Completed,  // finished successfully
    Failed,     // failed after max attempts
    Cancelled,  // cancelled by user or parent failure
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub run_id: RunId,
    pub task: String,
    pub status: RunStatus,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub root_job_id: JobId,
    pub final_artifact_ids: Vec<ArtifactId>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub job_id: JobId,
    pub run_id: RunId,
    pub parent_job_id: Option<JobId>,
    pub role: String,
    pub status: JobStatus,
    pub instruction: String,
    pub input_artifact_ids: Vec<ArtifactId>,
    pub depends_on: Vec<JobId>,
    pub children: Vec<JobId>,
    pub profile_id: String,
    pub workspace_dir: PathBuf,
    pub attempt: u32,
    pub max_attempts: u32,
    pub created_at: SystemTime,
    pub started_at: Option<SystemTime>,
    pub finished_at: Option<SystemTime>,
    pub output_artifact_ids: Vec<ArtifactId>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub artifact_id: ArtifactId,
    pub run_id: RunId,
    pub job_id: JobId,
    pub kind: String,
    pub path: PathBuf,
    pub summary: String,
    pub created_at: SystemTime,
}

/// Planner output: a list of jobs to create.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub jobs: Vec<PlannedJob>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedJob {
    pub local_id: String,
    pub role: String,
    pub profile_id: String,
    pub instruction: String,
    pub depends_on: Vec<String>,
}
```

`src/orchestrator/mod.rs`:
```rust
pub mod types;
```

**Step 2: Add module to lib.rs**

Add `pub mod orchestrator;` to `src/lib.rs`.

**Step 3: Run `cargo build` to verify compilation**

Run: `cargo build 2>&1`
Expected: clean build

**Step 4: Commit**

```bash
git add src/orchestrator/ src/lib.rs
git commit -m "feat(orchestrator): add Run, Job, Artifact, ExecutionPlan types"
```

---

### Task 2: In-memory run store

**Files:**
- Create: `src/orchestrator/store.rs`
- Modify: `src/orchestrator/mod.rs` (add `pub mod store;`)

**Step 1: Write the failing test**

At the bottom of `src/orchestrator/store.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::types::*;

    fn make_run() -> Run {
        Run {
            run_id: "run_1".into(),
            task: "test task".into(),
            status: RunStatus::Pending,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            root_job_id: "job_1".into(),
            final_artifact_ids: vec![],
            metadata: HashMap::new(),
        }
    }

    fn make_job(job_id: &str, run_id: &str) -> Job {
        Job {
            job_id: job_id.into(),
            run_id: run_id.into(),
            parent_job_id: None,
            role: "researcher".into(),
            status: JobStatus::Pending,
            instruction: "do research".into(),
            input_artifact_ids: vec![],
            depends_on: vec![],
            children: vec![],
            profile_id: "researcher".into(),
            workspace_dir: std::path::PathBuf::from("/tmp/test"),
            attempt: 0,
            max_attempts: 3,
            created_at: SystemTime::now(),
            started_at: None,
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
        }
    }

    #[test]
    fn test_create_and_get_run() {
        let mut store = RunStore::new();
        let run = make_run();
        store.create_run(run.clone());
        let got = store.get_run("run_1").unwrap();
        assert_eq!(got.task, "test task");
    }

    #[test]
    fn test_create_and_get_job() {
        let mut store = RunStore::new();
        store.create_job(make_job("job_1", "run_1"));
        let got = store.get_job("job_1").unwrap();
        assert_eq!(got.role, "researcher");
    }

    #[test]
    fn test_list_run_jobs() {
        let mut store = RunStore::new();
        store.create_job(make_job("job_1", "run_1"));
        store.create_job(make_job("job_2", "run_1"));
        store.create_job(make_job("job_3", "run_2"));
        let jobs = store.list_run_jobs("run_1");
        assert_eq!(jobs.len(), 2);
    }

    #[test]
    fn test_update_job_status() {
        let mut store = RunStore::new();
        store.create_job(make_job("job_1", "run_1"));
        let mut job = store.get_job("job_1").unwrap().clone();
        job.status = JobStatus::Running;
        store.update_job(job);
        assert_eq!(store.get_job("job_1").unwrap().status, JobStatus::Running);
    }

    #[test]
    fn test_create_and_get_artifact() {
        let mut store = RunStore::new();
        let art = Artifact {
            artifact_id: "art_1".into(),
            run_id: "run_1".into(),
            job_id: "job_1".into(),
            kind: "json".into(),
            path: "/tmp/test.json".into(),
            summary: "test output".into(),
            created_at: SystemTime::now(),
        };
        store.create_artifact(art);
        let got = store.get_artifact("art_1").unwrap();
        assert_eq!(got.summary, "test output");
    }

    #[test]
    fn test_list_runs() {
        let mut store = RunStore::new();
        store.create_run(make_run());
        let runs = store.list_runs();
        assert_eq!(runs.len(), 1);
    }
}
```

**Step 2: Implement the store**

```rust
use std::collections::HashMap;
use crate::orchestrator::types::*;

pub struct RunStore {
    runs: HashMap<RunId, Run>,
    jobs: HashMap<JobId, Job>,
    artifacts: HashMap<ArtifactId, Artifact>,
}

impl RunStore {
    pub fn new() -> Self {
        Self {
            runs: HashMap::new(),
            jobs: HashMap::new(),
            artifacts: HashMap::new(),
        }
    }

    pub fn create_run(&mut self, run: Run) {
        self.runs.insert(run.run_id.clone(), run);
    }

    pub fn update_run(&mut self, run: Run) {
        self.runs.insert(run.run_id.clone(), run);
    }

    pub fn get_run(&self, run_id: &str) -> Option<&Run> {
        self.runs.get(run_id)
    }

    pub fn list_runs(&self) -> Vec<&Run> {
        self.runs.values().collect()
    }

    pub fn create_job(&mut self, job: Job) {
        self.jobs.insert(job.job_id.clone(), job);
    }

    pub fn update_job(&mut self, job: Job) {
        self.jobs.insert(job.job_id.clone(), job);
    }

    pub fn get_job(&self, job_id: &str) -> Option<&Job> {
        self.jobs.get(job_id)
    }

    pub fn get_job_mut(&mut self, job_id: &str) -> Option<&mut Job> {
        self.jobs.get_mut(job_id)
    }

    pub fn list_run_jobs(&self, run_id: &str) -> Vec<&Job> {
        self.jobs.values().filter(|j| j.run_id == run_id).collect()
    }

    pub fn create_artifact(&mut self, artifact: Artifact) {
        self.artifacts.insert(artifact.artifact_id.clone(), artifact);
    }

    pub fn get_artifact(&self, artifact_id: &str) -> Option<&Artifact> {
        self.artifacts.get(artifact_id)
    }
}
```

**Step 3: Run tests**

Run: `cargo test orchestrator 2>&1`
Expected: 6 tests pass

**Step 4: Commit**

```bash
git add src/orchestrator/store.rs src/orchestrator/mod.rs
git commit -m "feat(orchestrator): in-memory RunStore for runs, jobs, artifacts"
```

---

### Task 3: Scheduler — dependency resolution and ready queue

**Files:**
- Create: `src/orchestrator/scheduler.rs`
- Modify: `src/orchestrator/mod.rs` (add `pub mod scheduler;`)

**Step 1: Write the failing tests**

At the bottom of `src/orchestrator/scheduler.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::store::RunStore;
    use crate::orchestrator::types::*;
    use std::time::SystemTime;
    use std::collections::HashMap;

    fn make_job(job_id: &str, run_id: &str, deps: Vec<&str>, status: JobStatus) -> Job {
        Job {
            job_id: job_id.into(),
            run_id: run_id.into(),
            parent_job_id: None,
            role: "researcher".into(),
            status,
            instruction: "do work".into(),
            input_artifact_ids: vec![],
            depends_on: deps.into_iter().map(String::from).collect(),
            children: vec![],
            profile_id: "researcher".into(),
            workspace_dir: std::path::PathBuf::from("/tmp"),
            attempt: 0,
            max_attempts: 3,
            created_at: SystemTime::now(),
            started_at: None,
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
        }
    }

    #[test]
    fn test_promote_no_deps_becomes_ready() {
        let mut store = RunStore::new();
        store.create_job(make_job("j1", "run_1", vec![], JobStatus::Pending));
        let promoted = promote_unblocked_jobs(&mut store, "run_1");
        assert_eq!(promoted, vec!["j1"]);
        assert_eq!(store.get_job("j1").unwrap().status, JobStatus::Ready);
    }

    #[test]
    fn test_promote_blocked_stays_pending() {
        let mut store = RunStore::new();
        store.create_job(make_job("j1", "run_1", vec![], JobStatus::Running));
        store.create_job(make_job("j2", "run_1", vec!["j1"], JobStatus::Pending));
        let promoted = promote_unblocked_jobs(&mut store, "run_1");
        assert!(promoted.is_empty());
        assert_eq!(store.get_job("j2").unwrap().status, JobStatus::Pending);
    }

    #[test]
    fn test_promote_after_dep_completes() {
        let mut store = RunStore::new();
        store.create_job(make_job("j1", "run_1", vec![], JobStatus::Completed));
        store.create_job(make_job("j2", "run_1", vec!["j1"], JobStatus::Pending));
        let promoted = promote_unblocked_jobs(&mut store, "run_1");
        assert_eq!(promoted, vec!["j2"]);
    }

    #[test]
    fn test_promote_multiple_deps_all_complete() {
        let mut store = RunStore::new();
        store.create_job(make_job("j1", "run_1", vec![], JobStatus::Completed));
        store.create_job(make_job("j2", "run_1", vec![], JobStatus::Completed));
        store.create_job(make_job("j3", "run_1", vec!["j1", "j2"], JobStatus::Pending));
        let promoted = promote_unblocked_jobs(&mut store, "run_1");
        assert_eq!(promoted, vec!["j3"]);
    }

    #[test]
    fn test_promote_multiple_deps_partial_complete() {
        let mut store = RunStore::new();
        store.create_job(make_job("j1", "run_1", vec![], JobStatus::Completed));
        store.create_job(make_job("j2", "run_1", vec![], JobStatus::Running));
        store.create_job(make_job("j3", "run_1", vec!["j1", "j2"], JobStatus::Pending));
        let promoted = promote_unblocked_jobs(&mut store, "run_1");
        assert!(promoted.is_empty());
    }

    #[test]
    fn test_check_run_completion_all_done() {
        let mut store = RunStore::new();
        let run = Run {
            run_id: "run_1".into(),
            task: "test".into(),
            status: RunStatus::Running,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            root_job_id: "j1".into(),
            final_artifact_ids: vec![],
            metadata: HashMap::new(),
        };
        store.create_run(run);
        store.create_job(make_job("j1", "run_1", vec![], JobStatus::Completed));
        store.create_job(make_job("j2", "run_1", vec![], JobStatus::Completed));
        let finished = check_run_completion(&mut store, "run_1");
        assert!(finished);
        assert_eq!(store.get_run("run_1").unwrap().status, RunStatus::Completed);
    }

    #[test]
    fn test_check_run_completion_has_failure() {
        let mut store = RunStore::new();
        let run = Run {
            run_id: "run_1".into(),
            task: "test".into(),
            status: RunStatus::Running,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            root_job_id: "j1".into(),
            final_artifact_ids: vec![],
            metadata: HashMap::new(),
        };
        store.create_run(run);
        store.create_job(make_job("j1", "run_1", vec![], JobStatus::Completed));
        store.create_job(make_job("j2", "run_1", vec![], JobStatus::Failed));
        let finished = check_run_completion(&mut store, "run_1");
        assert!(finished);
        assert_eq!(store.get_run("run_1").unwrap().status, RunStatus::Failed);
    }

    #[test]
    fn test_check_run_completion_still_running() {
        let mut store = RunStore::new();
        let run = Run {
            run_id: "run_1".into(),
            task: "test".into(),
            status: RunStatus::Running,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            root_job_id: "j1".into(),
            final_artifact_ids: vec![],
            metadata: HashMap::new(),
        };
        store.create_run(run);
        store.create_job(make_job("j1", "run_1", vec![], JobStatus::Completed));
        store.create_job(make_job("j2", "run_1", vec![], JobStatus::Running));
        let finished = check_run_completion(&mut store, "run_1");
        assert!(!finished);
    }
}
```

**Step 2: Implement the scheduler functions**

```rust
use crate::orchestrator::store::RunStore;
use crate::orchestrator::types::*;

/// Scan pending jobs in a run and promote those whose dependencies are all completed.
/// Returns the list of job IDs that were promoted to Ready.
pub fn promote_unblocked_jobs(store: &mut RunStore, run_id: &str) -> Vec<JobId> {
    // Collect pending job IDs and their deps
    let pending: Vec<(JobId, Vec<JobId>)> = store
        .list_run_jobs(run_id)
        .iter()
        .filter(|j| j.status == JobStatus::Pending)
        .map(|j| (j.job_id.clone(), j.depends_on.clone()))
        .collect();

    let mut promoted = Vec::new();
    for (job_id, deps) in pending {
        let all_complete = deps.iter().all(|dep_id| {
            store.get_job(dep_id)
                .map(|j| j.status == JobStatus::Completed)
                .unwrap_or(false)
        });
        if all_complete {
            if let Some(job) = store.get_job_mut(&job_id) {
                job.status = JobStatus::Ready;
                promoted.push(job_id);
            }
        }
    }
    promoted
}

/// Check if a run is finished (all jobs completed or any job failed with no retries left).
/// Updates the run status if finished. Returns true if the run is done.
pub fn check_run_completion(store: &mut RunStore, run_id: &str) -> bool {
    let jobs = store.list_run_jobs(run_id);
    if jobs.is_empty() {
        return false;
    }

    let any_active = jobs.iter().any(|j| matches!(
        j.status,
        JobStatus::Pending | JobStatus::Ready | JobStatus::Running
    ));
    if any_active {
        return false;
    }

    let any_failed = jobs.iter().any(|j| j.status == JobStatus::Failed);
    let new_status = if any_failed {
        RunStatus::Failed
    } else {
        RunStatus::Completed
    };

    // Collect final artifacts from leaf jobs (no children depend on them)
    let all_job_ids: Vec<JobId> = jobs.iter().map(|j| j.job_id.clone()).collect();
    let dep_targets: std::collections::HashSet<&str> = jobs
        .iter()
        .flat_map(|j| j.depends_on.iter().map(|s| s.as_str()))
        .collect();
    let leaf_artifacts: Vec<ArtifactId> = jobs
        .iter()
        .filter(|j| !dep_targets.contains(j.job_id.as_str()))
        .flat_map(|j| j.output_artifact_ids.clone())
        .collect();

    if let Some(run) = store.get_run(run_id) {
        let mut run = run.clone();
        run.status = new_status;
        run.updated_at = std::time::SystemTime::now();
        run.final_artifact_ids = leaf_artifacts;
        store.update_run(run);
    }

    true
}

/// Generate a unique ID with a prefix.
pub fn gen_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{}_{ts}_{n}", prefix)
}
```

**Step 3: Run tests**

Run: `cargo test orchestrator 2>&1`
Expected: all tests pass (store + scheduler)

**Step 4: Commit**

```bash
git add src/orchestrator/scheduler.rs src/orchestrator/mod.rs
git commit -m "feat(orchestrator): scheduler with dependency promotion and run completion"
```

---

### Task 4: Plan materializer — turn ExecutionPlan into child jobs

**Files:**
- Create: `src/orchestrator/planner.rs`
- Modify: `src/orchestrator/mod.rs`

**Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::store::RunStore;
    use crate::orchestrator::types::*;

    #[test]
    fn test_materialize_plan_creates_jobs() {
        let mut store = RunStore::new();
        let plan = ExecutionPlan {
            jobs: vec![
                PlannedJob {
                    local_id: "research".into(),
                    role: "researcher".into(),
                    profile_id: "researcher".into(),
                    instruction: "Research AI in Malaysia".into(),
                    depends_on: vec![],
                },
                PlannedJob {
                    local_id: "write".into(),
                    role: "writer".into(),
                    profile_id: "writer".into(),
                    instruction: "Write report".into(),
                    depends_on: vec!["research".into()],
                },
            ],
        };
        let job_ids = materialize_plan(&mut store, "run_1", "planner_job", &plan);
        assert_eq!(job_ids.len(), 2);

        // First job should be Ready (no deps)
        let j1 = store.get_job(&job_ids[0]).unwrap();
        assert_eq!(j1.status, JobStatus::Ready);
        assert_eq!(j1.role, "researcher");
        assert!(j1.depends_on.is_empty());

        // Second job should be Pending (depends on first)
        let j2 = store.get_job(&job_ids[1]).unwrap();
        assert_eq!(j2.status, JobStatus::Pending);
        assert_eq!(j2.role, "writer");
        assert_eq!(j2.depends_on.len(), 1);
        assert_eq!(j2.depends_on[0], job_ids[0]);
    }

    #[test]
    fn test_materialize_plan_parallel_jobs() {
        let mut store = RunStore::new();
        let plan = ExecutionPlan {
            jobs: vec![
                PlannedJob {
                    local_id: "r1".into(),
                    role: "researcher".into(),
                    profile_id: "researcher".into(),
                    instruction: "Research A".into(),
                    depends_on: vec![],
                },
                PlannedJob {
                    local_id: "r2".into(),
                    role: "researcher".into(),
                    profile_id: "researcher".into(),
                    instruction: "Research B".into(),
                    depends_on: vec![],
                },
                PlannedJob {
                    local_id: "merge".into(),
                    role: "analyst".into(),
                    profile_id: "analyst".into(),
                    instruction: "Merge findings".into(),
                    depends_on: vec!["r1".into(), "r2".into()],
                },
            ],
        };
        let job_ids = materialize_plan(&mut store, "run_1", "planner_job", &plan);
        assert_eq!(job_ids.len(), 3);

        // Both researchers should be Ready
        assert_eq!(store.get_job(&job_ids[0]).unwrap().status, JobStatus::Ready);
        assert_eq!(store.get_job(&job_ids[1]).unwrap().status, JobStatus::Ready);

        // Analyst should be Pending with 2 deps
        let merge = store.get_job(&job_ids[2]).unwrap();
        assert_eq!(merge.status, JobStatus::Pending);
        assert_eq!(merge.depends_on.len(), 2);
    }
}
```

**Step 2: Implement materialize_plan**

```rust
use std::collections::HashMap;
use std::time::SystemTime;

use crate::orchestrator::scheduler::gen_id;
use crate::orchestrator::store::RunStore;
use crate::orchestrator::types::*;

/// Turn an ExecutionPlan into real Jobs in the store.
/// Returns the list of created job IDs in plan order.
pub fn materialize_plan(
    store: &mut RunStore,
    run_id: &str,
    parent_job_id: &str,
    plan: &ExecutionPlan,
) -> Vec<JobId> {
    let now = SystemTime::now();

    // Map local_id -> real job_id
    let mut id_map: HashMap<String, JobId> = HashMap::new();
    for spec in &plan.jobs {
        id_map.insert(spec.local_id.clone(), gen_id("job"));
    }

    let mut created = Vec::new();
    for spec in &plan.jobs {
        let real_id = id_map[&spec.local_id].clone();
        let real_deps: Vec<JobId> = spec
            .depends_on
            .iter()
            .filter_map(|local| id_map.get(local).cloned())
            .collect();

        let status = if real_deps.is_empty() {
            JobStatus::Ready
        } else {
            JobStatus::Pending
        };

        let job = Job {
            job_id: real_id.clone(),
            run_id: run_id.into(),
            parent_job_id: Some(parent_job_id.into()),
            role: spec.role.clone(),
            status,
            instruction: spec.instruction.clone(),
            input_artifact_ids: vec![],
            depends_on: real_deps,
            children: vec![],
            profile_id: spec.profile_id.clone(),
            workspace_dir: resolve_workspace(run_id, &real_id),
            attempt: 0,
            max_attempts: 3,
            created_at: now,
            started_at: None,
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
        };

        store.create_job(job);
        created.push(real_id);
    }

    created
}

fn resolve_workspace(run_id: &str, job_id: &str) -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".zeptopm")
        .join("runs")
        .join(run_id)
        .join(job_id)
}
```

**Step 3: Run tests**

Run: `cargo test orchestrator 2>&1`
Expected: all tests pass

**Step 4: Commit**

```bash
git add src/orchestrator/planner.rs src/orchestrator/mod.rs
git commit -m "feat(orchestrator): plan materializer — ExecutionPlan to child jobs"
```

---

### Task 5: Run submission — create a run with a planner root job

**Files:**
- Create: `src/orchestrator/engine.rs`
- Modify: `src/orchestrator/mod.rs`

**Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_submit_run_creates_run_and_planner_job() {
        let mut engine = OrchestratorEngine::new(4);
        let run_id = engine.submit_run("Research AI startups in SEA".into());

        let run = engine.store.get_run(&run_id).unwrap();
        assert_eq!(run.status, RunStatus::Pending);
        assert_eq!(run.task, "Research AI startups in SEA");

        let root_job = engine.store.get_job(&run.root_job_id).unwrap();
        assert_eq!(root_job.role, "planner");
        assert_eq!(root_job.status, JobStatus::Ready);
        assert!(root_job.depends_on.is_empty());

        assert_eq!(engine.ready_queue.len(), 1);
    }
}
```

**Step 2: Implement OrchestratorEngine**

```rust
use std::collections::{HashMap, VecDeque};
use std::time::SystemTime;

use crate::orchestrator::scheduler::{gen_id, promote_unblocked_jobs, check_run_completion};
use crate::orchestrator::store::RunStore;
use crate::orchestrator::types::*;

pub struct OrchestratorEngine {
    pub store: RunStore,
    pub ready_queue: VecDeque<JobId>,
    pub max_concurrency: usize,
    pub active_jobs: HashMap<JobId, RunId>,
}

impl OrchestratorEngine {
    pub fn new(max_concurrency: usize) -> Self {
        Self {
            store: RunStore::new(),
            ready_queue: VecDeque::new(),
            max_concurrency,
            active_jobs: HashMap::new(),
        }
    }

    /// Submit a new run. Creates the run and a planner root job.
    pub fn submit_run(&mut self, task: String) -> RunId {
        let run_id = gen_id("run");
        let root_job_id = gen_id("job");
        let now = SystemTime::now();

        let run = Run {
            run_id: run_id.clone(),
            task: task.clone(),
            status: RunStatus::Pending,
            created_at: now,
            updated_at: now,
            root_job_id: root_job_id.clone(),
            final_artifact_ids: vec![],
            metadata: HashMap::new(),
        };

        let planner_job = Job {
            job_id: root_job_id.clone(),
            run_id: run_id.clone(),
            parent_job_id: None,
            role: "planner".into(),
            status: JobStatus::Ready,
            instruction: task,
            input_artifact_ids: vec![],
            depends_on: vec![],
            children: vec![],
            profile_id: "planner".into(),
            workspace_dir: crate::orchestrator::planner::resolve_workspace(&run_id, &root_job_id),
            attempt: 0,
            max_attempts: 3,
            created_at: now,
            started_at: None,
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
        };

        self.store.create_run(run);
        self.store.create_job(planner_job);
        self.ready_queue.push_back(root_job_id);

        run_id
    }

    /// Take the next ready job if under concurrency limit.
    pub fn next_job(&mut self) -> Option<Job> {
        if self.active_jobs.len() >= self.max_concurrency {
            return None;
        }
        let job_id = self.ready_queue.pop_front()?;
        let job = self.store.get_job(&job_id)?.clone();
        self.active_jobs.insert(job_id, job.run_id.clone());
        Some(job)
    }

    /// Mark a job as running.
    pub fn mark_running(&mut self, job_id: &str) {
        if let Some(job) = self.store.get_job_mut(job_id) {
            job.status = JobStatus::Running;
            job.started_at = Some(SystemTime::now());
            job.attempt += 1;
        }
    }

    /// Mark a job as completed. Promotes unblocked dependents. Checks run completion.
    pub fn mark_completed(&mut self, job_id: &str, output_artifact_ids: Vec<ArtifactId>) {
        let run_id = self.active_jobs.remove(job_id);
        if let Some(job) = self.store.get_job_mut(job_id) {
            job.status = JobStatus::Completed;
            job.finished_at = Some(SystemTime::now());
            job.output_artifact_ids = output_artifact_ids;
        }
        if let Some(run_id) = run_id {
            let promoted = promote_unblocked_jobs(&mut self.store, &run_id);
            for jid in promoted {
                self.ready_queue.push_back(jid);
            }
            check_run_completion(&mut self.store, &run_id);
        }
    }

    /// Mark a job as failed. Retries if attempts remain.
    pub fn mark_failed(&mut self, job_id: &str, error: String) {
        let run_id = self.active_jobs.remove(job_id);
        if let Some(job) = self.store.get_job_mut(job_id) {
            job.error = Some(error);
            job.finished_at = Some(SystemTime::now());
            if job.attempt < job.max_attempts {
                job.status = JobStatus::Ready;
                self.ready_queue.push_back(job_id.to_string());
            } else {
                job.status = JobStatus::Failed;
            }
        }
        if let Some(run_id) = run_id {
            check_run_completion(&mut self.store, &run_id);
        }
    }
}
```

**Step 3: Run tests**

Run: `cargo test orchestrator 2>&1`
Expected: all tests pass

**Step 4: Commit**

```bash
git add src/orchestrator/engine.rs src/orchestrator/mod.rs
git commit -m "feat(orchestrator): engine with submit_run, job lifecycle, concurrency"
```

---

### Task 6: Worker protocol extensions — job_execute and artifact_produced

**Files:**
- Modify: `src/worker.rs` (add `job_execute` command handling)
- Modify: `src/agent.rs` (add job-mode worker bridge)

**Step 1: Extend worker to handle `job_execute` command**

Add to `worker.rs` inside the main `match cmd` block, after `Some("chat")`:

```rust
Some("job_execute") => {
    let job_id = cmd.get("job_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let instruction = cmd.get("instruction").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let workspace = cmd.get("workspace").and_then(|v| v.as_str()).unwrap_or("/tmp").to_string();
    let input_artifacts: Vec<String> = cmd.get("input_artifacts")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    send_status("running", None);
    send_log("info", &format!("executing job {}", job_id));

    // Build context from input artifacts
    let mut context = instruction.clone();
    for artifact_path in &input_artifacts {
        if let Ok(content) = std::fs::read_to_string(artifact_path) {
            context.push_str(&format!("\n\n--- Input from previous step ---\n{}", content));
        }
    }

    // Create workspace directory
    std::fs::create_dir_all(&workspace).ok();

    match agent.chat(&context).await {
        Ok(response) => {
            // Write output artifact
            let artifact_path = format!("{}/output.md", workspace);
            std::fs::write(&artifact_path, &response).ok();
            let artifact_id = format!("art_{}", job_id);

            send(&serde_json::json!({
                "type": "artifact_produced",
                "job_id": job_id,
                "artifact_id": artifact_id,
                "kind": "markdown",
                "path": artifact_path,
                "summary": response.chars().take(200).collect::<String>()
            }));

            send(&serde_json::json!({
                "type": "job_completed",
                "job_id": job_id,
                "output_artifact_ids": [artifact_id]
            }));
            send_log("info", &format!("job {} completed", job_id));
        }
        Err(e) => {
            send(&serde_json::json!({
                "type": "job_failed",
                "job_id": job_id,
                "error": e.to_string(),
                "retryable": true
            }));
            send_log("error", &format!("job {} failed: {}", job_id, e));
        }
    }

    send_status("idle", None);
}
```

**Step 2: Extend agent.rs bridge to handle new event types**

Add to the worker message handler in `agent.rs`, inside the `match msg_type` block:

```rust
"artifact_produced" | "job_completed" | "job_failed" => {
    // Forward these events through the state update channel
    // so the daemon/orchestrator can handle them
    let _ = state_tx
        .send(AgentStateUpdate {
            name: agent_name.clone(),
            status: AgentStatus::Idle,
            error: None,
            tokens_delta: 0,
            log: Some(make_log("info", &format!("{}: {}", msg_type,
                msg.get("job_id").and_then(|v| v.as_str()).unwrap_or("?")))),
            pid: child_pid,
        })
        .await;
    // Also forward the raw message for orchestrator
    if let Some(ref orch_tx) = orch_event_tx {
        let _ = orch_tx.send(msg.clone()).await;
    }
}
```

Note: `orch_event_tx` will be added as an optional channel parameter to `spawn_agent` in Task 7.

**Step 3: Build and verify compilation**

Run: `cargo build 2>&1`
Expected: clean build (orch_event_tx not wired yet, but the match arm compiles)

**Step 4: Commit**

```bash
git add src/worker.rs src/agent.rs
git commit -m "feat(orchestrator): worker job_execute command + artifact protocol"
```

---

### Task 7: Wire orchestrator into daemon loop

**Files:**
- Modify: `src/daemon.rs` (add orchestrator engine + event channel)
- Modify: `src/agent.rs` (add optional orch channel to spawn_agent)
- Modify: `src/server.rs` (add run endpoints)
- Modify: `src/main.rs` (add run CLI commands)

**Step 1: Add OrchestratorEngine to daemon state**

In `daemon.rs`, after creating managed agents:

```rust
// Initialize orchestrator
let (orch_event_tx, mut orch_event_rx) = mpsc::channel::<serde_json::Value>(256);
let mut orchestrator = crate::orchestrator::engine::OrchestratorEngine::new(4);
```

**Step 2: Add orchestrator tick to the select loop**

In the `tokio::select!` block, add a new branch:

```rust
// Orchestrator events from workers
Some(event) = orch_event_rx.recv() => {
    let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match event_type {
        "job_completed" => {
            let job_id = event.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
            let artifacts: Vec<String> = event.get("output_artifact_ids")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            orchestrator.mark_completed(job_id, artifacts);
            info!(job_id = %job_id, "job completed");
        }
        "job_failed" => {
            let job_id = event.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
            let error = event.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
            orchestrator.mark_failed(job_id, error.to_string());
            warn!(job_id = %job_id, error = %error, "job failed");
        }
        "artifact_produced" => {
            // Store artifact metadata
            let artifact = crate::orchestrator::types::Artifact {
                artifact_id: event.get("artifact_id").and_then(|v| v.as_str()).unwrap_or("").into(),
                run_id: "".into(), // filled by job lookup
                job_id: event.get("job_id").and_then(|v| v.as_str()).unwrap_or("").into(),
                kind: event.get("kind").and_then(|v| v.as_str()).unwrap_or("").into(),
                path: event.get("path").and_then(|v| v.as_str()).unwrap_or("").into(),
                summary: event.get("summary").and_then(|v| v.as_str()).unwrap_or("").into(),
                created_at: std::time::SystemTime::now(),
            };
            orchestrator.store.create_artifact(artifact);
        }
        _ => {}
    }

    // Spawn next ready jobs
    while let Some(job) = orchestrator.next_job() {
        // Spawn a temporary worker for this job
        info!(job_id = %job.job_id, role = %job.role, "spawning job worker");
        orchestrator.mark_running(&job.job_id);
        // TODO: spawn_job_worker(job, orch_event_tx.clone(), state_tx.clone())
    }
}
```

**Step 3: Add run submission endpoint to server.rs**

```rust
// POST /runs — submit a new orchestrated run
async fn post_run_submit(
    State(state): State<SharedState>,
    Json(body): Json<RunSubmitRequest>,
) -> Result<Json<RunSubmitResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Send to daemon via command channel
    let daemon_tx = { state.read().await.daemon_tx.clone() };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    daemon_tx
        .send(DaemonCommand::SubmitRun { task: body.task, reply: reply_tx })
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: "daemon channel closed".into() })))?;
    match reply_rx.await {
        Ok(Ok(run_id)) => Ok(Json(RunSubmitResponse { run_id })),
        Ok(Err(e)) => Err((StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e }))),
        Err(_) => Err((StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: "daemon did not reply".into() }))),
    }
}
```

**Step 4: Add CLI commands to main.rs**

```rust
/// Submit a multi-step orchestrated run
Run {
    #[command(subcommand)]
    action: RunAction,
},

#[derive(clap::Subcommand, Debug)]
enum RunAction {
    /// Submit a new run
    Submit { task: String },
    /// Show run status
    Status { run_id: String },
    /// List all runs
    List,
}
```

**Step 5: Build, test, commit**

Run: `cargo build 2>&1`
Expected: clean build

```bash
git add src/daemon.rs src/agent.rs src/server.rs src/main.rs
git commit -m "feat(orchestrator): wire engine into daemon, add run endpoints and CLI"
```

---

### Task 8: End-to-end smoke test — submit a run with parallel researchers

**Step 1: Create a test config with planner + researcher profiles**

`test-orchestrate.toml`:
```toml
[daemon]
log_level = "info"

[[agents]]
name = "planner"
provider = "openai"
model = "gpt-4o-mini"
system_prompt = """You are a task planner. Given a task, produce a JSON execution plan.
Output ONLY valid JSON matching this format:
{"jobs": [{"local_id": "...", "role": "researcher", "profile_id": "researcher", "instruction": "...", "depends_on": []}]}
Roles: researcher, analyst, writer. Use depends_on to reference local_ids of jobs that must finish first."""
auto_start = false

[[agents]]
name = "researcher"
provider = "openai"
model = "gpt-4o-mini"
system_prompt = "You are a research assistant. Produce concise, factual findings. Keep answers under 3 sentences."
auto_start = false

[[agents]]
name = "writer"
provider = "openai"
model = "gpt-4o-mini"
system_prompt = "You are a writer. Produce clear, well-structured text. Keep answers under 5 sentences."
auto_start = false

[providers.openai]
api_key = "$OPENAI_API_KEY"
```

**Step 2: Start daemon and submit a run**

```bash
zeptopm daemon --config test-orchestrate.toml &
sleep 3
zeptopm run submit "Research the top 3 programming languages for AI development and write a brief comparison"
```

**Step 3: Monitor run progress**

```bash
zeptopm run status <run_id>
# Should show: planner → completed, researcher jobs → running/completed, writer → pending/completed
```

**Step 4: Check artifacts**

```bash
ls ~/.zeptopm/runs/<run_id>/
# Should have subdirectories per job with output.md files
```

**Step 5: Verify the final result**

```bash
zeptopm run result <run_id>
# Should print the writer's final output
```

---

### Task 9: Commit test config and update README

**Files:**
- Create: `test-orchestrate.toml`
- Modify: `README.md` (add orchestration section)

**Step 1: Add orchestration section to README**

Under Features, add:
```markdown
- **Orchestrated runs** — submit complex tasks, planner decomposes into parallel jobs with dependency graphs
```

Add new CLI commands to the reference table:
```markdown
| `zeptopm run submit <task>` | Submit a multi-step orchestrated run |
| `zeptopm run status <run_id>` | Show run progress (jobs, artifacts) |
| `zeptopm run list` | List all runs |
```

Add new API endpoints:
```markdown
| `/runs` | POST | Submit orchestrated run |
| `/runs/{id}` | GET | Run status with job details |
| `/runs` | GET | List all runs |
```

**Step 2: Commit**

```bash
git add test-orchestrate.toml README.md
git commit -m "docs: add orchestration to README, test config for orchestrated runs"
```
