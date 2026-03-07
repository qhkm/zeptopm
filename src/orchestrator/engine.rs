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

    #[test]
    fn test_next_job_returns_ready_job() {
        let mut engine = OrchestratorEngine::new(4);
        engine.submit_run("test task".into());
        let job = engine.next_job().unwrap();
        assert_eq!(job.role, "planner");
        assert_eq!(engine.active_jobs.len(), 1);
    }

    #[test]
    fn test_next_job_respects_concurrency() {
        let mut engine = OrchestratorEngine::new(1);
        engine.submit_run("task 1".into());
        engine.submit_run("task 2".into());
        let _j1 = engine.next_job().unwrap();
        assert!(engine.next_job().is_none()); // at limit
    }

    #[test]
    fn test_mark_completed_promotes_dependents() {
        let mut engine = OrchestratorEngine::new(4);
        let run_id = engine.submit_run("test".into());
        let run = engine.store.get_run(&run_id).unwrap().clone();

        // Get the planner job and mark it running then completed
        let planner_job = engine.next_job().unwrap();
        engine.mark_running(&planner_job.job_id);

        // Add a child job that depends on the planner
        let child_id = gen_id("job");
        let child = Job {
            job_id: child_id.clone(),
            run_id: run_id.clone(),
            parent_job_id: Some(planner_job.job_id.clone()),
            role: "researcher".into(),
            status: JobStatus::Pending,
            instruction: "research".into(),
            input_artifact_ids: vec![],
            depends_on: vec![planner_job.job_id.clone()],
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
        };
        engine.store.create_job(child);

        // Complete the planner — child should be promoted
        engine.mark_completed(&planner_job.job_id, vec![]);
        assert_eq!(engine.ready_queue.len(), 1);
        assert_eq!(engine.ready_queue[0], child_id);
    }

    #[test]
    fn test_mark_failed_retries() {
        let mut engine = OrchestratorEngine::new(4);
        engine.submit_run("test".into());
        let job = engine.next_job().unwrap();
        engine.mark_running(&job.job_id);

        // Fail it — should retry (attempt 1 < max_attempts 3)
        engine.mark_failed(&job.job_id, "timeout".into());
        assert_eq!(engine.ready_queue.len(), 1); // re-queued
        let j = engine.store.get_job(&job.job_id).unwrap();
        assert_eq!(j.status, JobStatus::Ready);
    }
}
