use crate::orchestrator::store::RunStore;
use crate::orchestrator::types::*;

/// Scan pending jobs in a run and promote those whose dependencies are all completed.
/// Returns the list of job IDs that were promoted to Ready.
pub fn promote_unblocked_jobs(store: &mut RunStore, run_id: &str) -> Vec<JobId> {
    let pending: Vec<(JobId, Vec<JobId>)> = store
        .list_run_jobs(run_id)
        .iter()
        .filter(|j| j.status == JobStatus::Pending)
        .map(|j| (j.job_id.clone(), j.depends_on.clone()))
        .collect();

    let mut promoted = Vec::new();
    for (job_id, deps) in pending {
        let all_complete = deps.iter().all(|dep_id| {
            store
                .get_job(dep_id)
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

    let any_active = jobs.iter().any(|j| {
        matches!(
            j.status,
            JobStatus::Pending | JobStatus::Ready | JobStatus::Running
        )
    });
    if any_active {
        return false;
    }

    let any_failed = jobs.iter().any(|j| j.status == JobStatus::Failed);
    let new_status = if any_failed {
        RunStatus::Failed
    } else {
        RunStatus::Completed
    };

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::store::RunStore;
    use std::collections::HashMap;
    use std::time::SystemTime;

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
            revision_round: 0,
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
        store.create_job(make_job(
            "j3",
            "run_1",
            vec!["j1", "j2"],
            JobStatus::Pending,
        ));
        let promoted = promote_unblocked_jobs(&mut store, "run_1");
        assert_eq!(promoted, vec!["j3"]);
    }

    #[test]
    fn test_promote_multiple_deps_partial_complete() {
        let mut store = RunStore::new();
        store.create_job(make_job("j1", "run_1", vec![], JobStatus::Completed));
        store.create_job(make_job("j2", "run_1", vec![], JobStatus::Running));
        store.create_job(make_job(
            "j3",
            "run_1",
            vec!["j1", "j2"],
            JobStatus::Pending,
        ));
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
