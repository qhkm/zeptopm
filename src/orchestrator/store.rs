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

    pub fn get_run_mut(&mut self, run_id: &str) -> Option<&mut Run> {
        self.runs.get_mut(run_id)
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

    /// Remove a run and all its jobs and artifacts. Returns artifact paths for cleanup.
    pub fn remove_run(&mut self, run_id: &str) -> Vec<std::path::PathBuf> {
        self.runs.remove(run_id);
        let job_ids: Vec<String> = self.jobs.values()
            .filter(|j| j.run_id == run_id)
            .map(|j| j.job_id.clone())
            .collect();
        for jid in &job_ids {
            self.jobs.remove(jid);
        }
        let artifact_ids: Vec<String> = self.artifacts.values()
            .filter(|a| a.run_id == run_id)
            .map(|a| a.artifact_id.clone())
            .collect();
        let mut paths = Vec::new();
        for aid in &artifact_ids {
            if let Some(artifact) = self.artifacts.remove(aid) {
                paths.push(artifact.path);
            }
        }
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

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
            revision_round: 0,
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
