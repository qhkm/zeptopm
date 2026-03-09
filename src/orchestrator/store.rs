use crate::orchestrator::types::*;
use std::collections::HashMap;

pub struct RunStore {
    runs: HashMap<RunId, Run>,
    jobs: HashMap<JobId, Job>,
    artifacts: HashMap<ArtifactId, Artifact>,
    channels: HashMap<ChannelId, Channel>,
}

impl RunStore {
    pub fn new() -> Self {
        Self {
            runs: HashMap::new(),
            jobs: HashMap::new(),
            artifacts: HashMap::new(),
            channels: HashMap::new(),
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
        self.artifacts
            .insert(artifact.artifact_id.clone(), artifact);
    }

    pub fn get_artifact(&self, artifact_id: &str) -> Option<&Artifact> {
        self.artifacts.get(artifact_id)
    }

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
        self.channels
            .values()
            .filter(|c| c.active && c.participants.contains(&job_id.to_string()))
            .collect()
    }

    /// Remove a run and all its jobs and artifacts. Returns artifact paths for cleanup.
    pub fn remove_run(&mut self, run_id: &str) -> Vec<std::path::PathBuf> {
        self.runs.remove(run_id);
        let job_ids: Vec<String> = self
            .jobs
            .values()
            .filter(|j| j.run_id == run_id)
            .map(|j| j.job_id.clone())
            .collect();
        for jid in &job_ids {
            self.jobs.remove(jid);
        }
        let artifact_ids: Vec<String> = self
            .artifacts
            .values()
            .filter(|a| a.run_id == run_id)
            .map(|a| a.artifact_id.clone())
            .collect();
        let mut paths = Vec::new();
        for aid in &artifact_ids {
            if let Some(artifact) = self.artifacts.remove(aid) {
                paths.push(artifact.path);
            }
        }
        let channel_ids: Vec<String> = self
            .channels
            .values()
            .filter(|c| c.run_id == run_id)
            .map(|c| c.channel_id.clone())
            .collect();
        for cid in &channel_ids {
            self.channels.remove(cid);
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
            initial_message: None,
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
            initial_message: None,
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
                initial_message: None,
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
            initial_message: None,
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
            initial_message: None,
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
            initial_message: None,
        });
        store.remove_run("run_1");
        assert!(store.get_channel("ch_1").is_none());
        assert!(store.list_run_channels("run_1").is_empty());
    }
}
