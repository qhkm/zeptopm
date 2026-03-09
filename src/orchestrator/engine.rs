use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime};

use crate::orchestrator::scheduler::{check_run_completion, gen_id, promote_unblocked_jobs};
use crate::orchestrator::store::RunStore;
use crate::orchestrator::types::*;

pub struct OrchestratorEngine {
    pub store: RunStore,
    pub ready_queue: VecDeque<JobId>,
    pub max_concurrency: usize,
    pub active_jobs: HashMap<JobId, RunId>,
    pub last_heartbeat: HashMap<JobId, Instant>,
}

impl OrchestratorEngine {
    pub fn new(max_concurrency: usize) -> Self {
        Self {
            store: RunStore::new(),
            ready_queue: VecDeque::new(),
            max_concurrency,
            active_jobs: HashMap::new(),
            last_heartbeat: HashMap::new(),
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
            revision_round: 0,
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

    /// Mark a job as running. Also transitions the parent run to Running if still Pending.
    pub fn mark_running(&mut self, job_id: &str) {
        if !self.active_jobs.contains_key(job_id) {
            return;
        }
        let run_id = self.active_jobs.get(job_id).cloned();
        if let Some(job) = self.store.get_job_mut(job_id) {
            job.status = JobStatus::Running;
            job.started_at = Some(SystemTime::now());
            job.attempt += 1;
        }
        self.last_heartbeat
            .insert(job_id.to_string(), Instant::now());
        // Transition run from Pending to Running on first job start
        if let Some(run_id) = run_id {
            if let Some(run) = self.store.get_run(&run_id) {
                if run.status == RunStatus::Pending {
                    let mut run = run.clone();
                    run.status = RunStatus::Running;
                    run.updated_at = SystemTime::now();
                    self.store.update_run(run);
                }
            }
        }
    }

    /// Record a heartbeat for an active job.
    pub fn record_heartbeat(&mut self, job_id: &str) {
        if self.active_jobs.contains_key(job_id) {
            self.last_heartbeat
                .insert(job_id.to_string(), Instant::now());
        }
    }

    /// Return job IDs that haven't sent a heartbeat within the given timeout.
    pub fn stale_jobs(&self, timeout: Duration) -> Vec<JobId> {
        let now = Instant::now();
        self.active_jobs
            .keys()
            .filter(|job_id| {
                self.last_heartbeat
                    .get(*job_id)
                    .map(|t| now.duration_since(*t) > timeout)
                    .unwrap_or(true)
            })
            .cloned()
            .collect()
    }

    /// Mark a job as completed. Promotes unblocked dependents. Checks run completion.
    pub fn mark_completed(&mut self, job_id: &str, output_artifact_ids: Vec<ArtifactId>) {
        self.last_heartbeat.remove(job_id);
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

    /// Handle completion of a reviewer job. If the decision is "revise"
    /// and we haven't exceeded the revision limit, creates new coder + reviewer jobs.
    /// Returns `(new_coder_id, new_reviewer_id)` if a revision was created.
    pub fn handle_review_completion(
        &mut self,
        reviewer_job_id: &str,
        decision: crate::orchestrator::review::ReviewDecision,
        max_revisions: u32,
    ) -> Option<(JobId, JobId)> {
        use crate::orchestrator::review::ReviewDecision;

        let feedback = match decision {
            ReviewDecision::Revise { feedback } => feedback,
            _ => return None,
        };

        let reviewer = self.store.get_job(reviewer_job_id)?.clone();

        // Find the coder job this reviewer depends on
        let coder_job_id = reviewer.depends_on.first()?;
        let coder = self.store.get_job(coder_job_id)?.clone();

        let new_round = reviewer.revision_round + 1;
        if new_round > max_revisions {
            return None;
        }

        let run_id = reviewer.run_id.clone();
        let now = SystemTime::now();

        // Create new coder job with original instruction + reviewer feedback
        let new_coder_id = gen_id("job");
        let new_coder = Job {
            job_id: new_coder_id.clone(),
            run_id: run_id.clone(),
            parent_job_id: reviewer.parent_job_id.clone(),
            role: coder.role.clone(),
            status: JobStatus::Ready,
            instruction: format!(
                "{}\n\n--- Reviewer feedback (revision {}/{}) ---\n{}",
                coder.instruction, new_round, max_revisions, feedback
            ),
            input_artifact_ids: coder.output_artifact_ids.clone(),
            depends_on: vec![],
            children: vec![],
            profile_id: coder.profile_id.clone(),
            workspace_dir: crate::orchestrator::planner::resolve_workspace(&run_id, &new_coder_id),
            attempt: 0,
            max_attempts: 3,
            created_at: now,
            started_at: None,
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
            revision_round: new_round,
        };

        // Create new reviewer job depending on the new coder
        let new_reviewer_id = gen_id("job");
        let new_reviewer = Job {
            job_id: new_reviewer_id.clone(),
            run_id: run_id.clone(),
            parent_job_id: reviewer.parent_job_id.clone(),
            role: reviewer.role.clone(),
            status: JobStatus::Pending,
            instruction: reviewer.instruction.clone(),
            input_artifact_ids: vec![],
            depends_on: vec![new_coder_id.clone()],
            children: vec![],
            profile_id: reviewer.profile_id.clone(),
            workspace_dir: crate::orchestrator::planner::resolve_workspace(
                &run_id,
                &new_reviewer_id,
            ),
            attempt: 0,
            max_attempts: 3,
            created_at: now,
            started_at: None,
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
            revision_round: new_round,
        };

        self.store.create_job(new_coder);
        self.store.create_job(new_reviewer);
        self.ready_queue.push_back(new_coder_id.clone());

        Some((new_coder_id, new_reviewer_id))
    }

    /// Activate channels whose participants are all Running.
    /// Returns list of activated channel IDs.
    pub fn activate_ready_channels(&mut self) -> Vec<ChannelId> {
        let run_ids: Vec<RunId> = self
            .active_jobs
            .values()
            .cloned()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let mut activated = Vec::new();

        for run_id in &run_ids {
            let channel_ids: Vec<ChannelId> = self
                .store
                .list_run_channels(run_id)
                .iter()
                .filter(|c| !c.active)
                .map(|c| c.channel_id.clone())
                .collect();

            for ch_id in channel_ids {
                let all_running = {
                    let ch = self.store.get_channel(&ch_id).unwrap();
                    ch.participants.iter().all(|p| {
                        self.store
                            .get_job(p)
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
    pub fn handle_channel_done(
        &mut self,
        channel_id: &str,
        _from_job: &str,
    ) -> ChannelAction {
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
            PeerFailure::KillAll => {
                ChannelAction::KillParticipants { job_ids: surviving }
            }
            PeerFailure::Continue => ChannelAction::NotifyPeers {
                job_ids: surviving,
                message: format!("peer '{}' disconnected", failed_job),
            },
        }
    }

    /// Mark a job as failed. Retries if attempts remain.
    pub fn mark_failed(&mut self, job_id: &str, error: String) {
        self.last_heartbeat.remove(job_id);
        let run_id = self.active_jobs.remove(job_id);
        if let Some(job) = self.store.get_job_mut(job_id) {
            job.error = Some(error);
            if job.attempt < job.max_attempts {
                job.status = JobStatus::Ready;
                job.finished_at = None;
                self.ready_queue.push_back(job_id.to_string());
            } else {
                job.status = JobStatus::Failed;
                job.finished_at = Some(SystemTime::now());
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
        let _run = engine.store.get_run(&run_id).unwrap().clone();

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
            revision_round: 0,
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
        assert!(j.finished_at.is_none()); // not finished yet — will retry
    }

    #[test]
    fn test_mark_failed_exhausts_retries() {
        let mut engine = OrchestratorEngine::new(4);
        engine.submit_run("test".into());
        let job_id = {
            let job = engine.next_job().unwrap();
            job.job_id.clone()
        };

        // Exhaust all 3 attempts
        for i in 0..3 {
            engine.mark_running(&job_id);
            engine.mark_failed(&job_id, format!("fail {}", i + 1));
            if i < 2 {
                // Re-dequeue for next attempt
                let _ = engine.next_job().unwrap();
            }
        }

        let j = engine.store.get_job(&job_id).unwrap();
        assert_eq!(j.status, JobStatus::Failed);
        assert!(j.finished_at.is_some());
    }

    #[test]
    fn test_record_heartbeat_updates_timestamp() {
        let mut engine = OrchestratorEngine::new(4);
        engine.submit_run("test".into());
        let job = engine.next_job().unwrap();
        engine.mark_running(&job.job_id);

        // Should have no stale jobs right after mark_running
        let stale = engine.stale_jobs(Duration::from_secs(60));
        assert!(stale.is_empty());

        // Record heartbeat again
        engine.record_heartbeat(&job.job_id);
        let stale = engine.stale_jobs(Duration::from_secs(60));
        assert!(stale.is_empty());
    }

    #[test]
    fn test_stale_jobs_detects_timeout() {
        let mut engine = OrchestratorEngine::new(4);
        engine.submit_run("test".into());
        let job = engine.next_job().unwrap();
        engine.mark_running(&job.job_id);

        // Fake an old heartbeat by inserting a past timestamp
        engine.last_heartbeat.insert(
            job.job_id.clone(),
            Instant::now() - Duration::from_secs(120),
        );

        let stale = engine.stale_jobs(Duration::from_secs(60));
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], job.job_id);
    }

    #[test]
    fn test_heartbeat_cleared_on_complete() {
        let mut engine = OrchestratorEngine::new(4);
        engine.submit_run("test".into());
        let job = engine.next_job().unwrap();
        engine.mark_running(&job.job_id);
        assert!(engine.last_heartbeat.contains_key(&job.job_id));

        engine.mark_completed(&job.job_id, vec![]);
        assert!(!engine.last_heartbeat.contains_key(&job.job_id));
    }

    #[test]
    fn test_heartbeat_cleared_on_fail() {
        let mut engine = OrchestratorEngine::new(4);
        engine.submit_run("test".into());
        let job = engine.next_job().unwrap();
        engine.mark_running(&job.job_id);
        assert!(engine.last_heartbeat.contains_key(&job.job_id));

        engine.mark_failed(&job.job_id, "error".into());
        // On retry, heartbeat is cleared from last_heartbeat (removed from active)
        // but job is re-queued as Ready
        assert!(!engine.last_heartbeat.contains_key(&job.job_id));
    }

    fn make_coder_reviewer_pair(
        engine: &mut OrchestratorEngine,
        run_id: &str,
        parent_job_id: &str,
    ) -> (JobId, JobId) {
        let coder_id = gen_id("job");
        let reviewer_id = gen_id("job");
        let now = SystemTime::now();

        let coder = Job {
            job_id: coder_id.clone(),
            run_id: run_id.into(),
            parent_job_id: Some(parent_job_id.into()),
            role: "coder".into(),
            status: JobStatus::Ready,
            instruction: "Write a function".into(),
            input_artifact_ids: vec![],
            depends_on: vec![],
            children: vec![],
            profile_id: "coder".into(),
            workspace_dir: std::path::PathBuf::from("/tmp"),
            attempt: 0,
            max_attempts: 3,
            created_at: now,
            started_at: None,
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
            revision_round: 0,
        };
        let reviewer = Job {
            job_id: reviewer_id.clone(),
            run_id: run_id.into(),
            parent_job_id: Some(parent_job_id.into()),
            role: "reviewer".into(),
            status: JobStatus::Pending,
            instruction: "Review the code".into(),
            input_artifact_ids: vec![],
            depends_on: vec![coder_id.clone()],
            children: vec![],
            profile_id: "reviewer".into(),
            workspace_dir: std::path::PathBuf::from("/tmp"),
            attempt: 0,
            max_attempts: 3,
            created_at: now,
            started_at: None,
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
            revision_round: 0,
        };

        engine.store.create_job(coder);
        engine.store.create_job(reviewer);
        engine.ready_queue.push_back(coder_id.clone());

        (coder_id, reviewer_id)
    }

    #[test]
    fn test_review_revise_creates_new_jobs() {
        use crate::orchestrator::review::ReviewDecision;

        let mut engine = OrchestratorEngine::new(4);
        let run_id = engine.submit_run("test".into());
        let planner = engine.next_job().unwrap();

        let (coder_id, reviewer_id) =
            make_coder_reviewer_pair(&mut engine, &run_id, &planner.job_id);

        // Simulate: coder completes, reviewer completes with "revise"
        engine.active_jobs.insert(coder_id.clone(), run_id.clone());
        engine.mark_running(&coder_id);
        engine.mark_completed(&coder_id, vec!["art_1".into()]);

        engine
            .active_jobs
            .insert(reviewer_id.clone(), run_id.clone());
        engine.mark_running(&reviewer_id);
        engine.mark_completed(&reviewer_id, vec![]);

        let decision = ReviewDecision::Revise {
            feedback: "Add error handling".into(),
        };
        let result = engine.handle_review_completion(&reviewer_id, decision, 3);
        assert!(result.is_some());

        let (new_coder_id, new_reviewer_id) = result.unwrap();

        let new_coder = engine.store.get_job(&new_coder_id).unwrap();
        assert_eq!(new_coder.role, "coder");
        assert_eq!(new_coder.status, JobStatus::Ready);
        assert_eq!(new_coder.revision_round, 1);
        assert!(new_coder.instruction.contains("Add error handling"));

        let new_reviewer = engine.store.get_job(&new_reviewer_id).unwrap();
        assert_eq!(new_reviewer.role, "reviewer");
        assert_eq!(new_reviewer.status, JobStatus::Pending);
        assert_eq!(new_reviewer.depends_on, vec![new_coder_id]);
        assert_eq!(new_reviewer.revision_round, 1);
    }

    #[test]
    fn test_review_approved_no_new_jobs() {
        use crate::orchestrator::review::ReviewDecision;

        let mut engine = OrchestratorEngine::new(4);
        let run_id = engine.submit_run("test".into());
        let planner = engine.next_job().unwrap();

        let (_coder_id, reviewer_id) =
            make_coder_reviewer_pair(&mut engine, &run_id, &planner.job_id);

        let result = engine.handle_review_completion(&reviewer_id, ReviewDecision::Approved, 3);
        assert!(result.is_none());
    }

    #[test]
    fn test_review_max_revisions_reached() {
        use crate::orchestrator::review::ReviewDecision;

        let mut engine = OrchestratorEngine::new(4);
        let run_id = engine.submit_run("test".into());
        let planner = engine.next_job().unwrap();

        let (_coder_id, reviewer_id) =
            make_coder_reviewer_pair(&mut engine, &run_id, &planner.job_id);

        // Set reviewer's revision_round to max
        if let Some(j) = engine.store.get_job_mut(&reviewer_id) {
            j.revision_round = 3;
        }

        let decision = ReviewDecision::Revise {
            feedback: "Still needs work".into(),
        };
        let result = engine.handle_review_completion(&reviewer_id, decision, 3);
        assert!(result.is_none()); // max revisions reached
    }

    #[test]
    fn test_mark_running_transitions_run_to_running() {
        let mut engine = OrchestratorEngine::new(4);
        let run_id = engine.submit_run("test".into());
        assert_eq!(
            engine.store.get_run(&run_id).unwrap().status,
            RunStatus::Pending
        );

        let job = engine.next_job().unwrap();
        engine.mark_running(&job.job_id);
        assert_eq!(
            engine.store.get_run(&run_id).unwrap().status,
            RunStatus::Running
        );
    }

    #[test]
    fn test_activate_channels_when_participants_running() {
        let mut engine = OrchestratorEngine::new(4);
        let run_id = engine.submit_run("test".into());

        let job_a = gen_id("job");
        let job_b = gen_id("job");
        engine.store.create_job(Job {
            job_id: job_a.clone(),
            run_id: run_id.clone(),
            parent_job_id: None,
            role: "writer".into(),
            status: JobStatus::Running,
            instruction: "write".into(),
            input_artifact_ids: vec![],
            depends_on: vec![],
            children: vec![],
            profile_id: "writer".into(),
            workspace_dir: std::path::PathBuf::from("/tmp"),
            attempt: 1,
            max_attempts: 3,
            created_at: SystemTime::now(),
            started_at: Some(SystemTime::now()),
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
            revision_round: 0,
        });
        engine.store.create_job(Job {
            job_id: job_b.clone(),
            run_id: run_id.clone(),
            parent_job_id: None,
            role: "reviewer".into(),
            status: JobStatus::Running,
            instruction: "review".into(),
            input_artifact_ids: vec![],
            depends_on: vec![],
            children: vec![],
            profile_id: "reviewer".into(),
            workspace_dir: std::path::PathBuf::from("/tmp"),
            attempt: 1,
            max_attempts: 3,
            created_at: SystemTime::now(),
            started_at: Some(SystemTime::now()),
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
            revision_round: 0,
        });
        engine.active_jobs.insert(job_a.clone(), run_id.clone());
        engine.active_jobs.insert(job_b.clone(), run_id.clone());

        engine.store.create_channel(Channel {
            channel_id: "ch1".into(),
            run_id: run_id.clone(),
            participants: vec![job_a.clone(), job_b.clone()],
            mode: ChannelMode::TurnBased,
            max_rounds: Some(3),
            on_peer_failure: PeerFailure::KillAll,
            current_round: 0,
            current_speaker_idx: 0,
            active: false,
            history: vec![],
        });

        let activated = engine.activate_ready_channels();
        assert_eq!(activated.len(), 1);
        assert_eq!(activated[0], "ch1");
        assert!(engine.store.get_channel("ch1").unwrap().active);
    }

    #[test]
    fn test_advance_turn_based_channel() {
        let mut engine = OrchestratorEngine::new(4);

        let ch = Channel {
            channel_id: "ch1".into(),
            run_id: "run_1".into(),
            participants: vec!["job_A".into(), "job_B".into()],
            mode: ChannelMode::TurnBased,
            max_rounds: Some(3),
            on_peer_failure: PeerFailure::KillAll,
            current_round: 0,
            current_speaker_idx: 0,
            active: true,
            history: vec![],
        };
        engine.store.create_channel(ch);

        let action =
            engine.route_channel_message("ch1", "job_A", "Here is my draft");
        match action {
            ChannelAction::SendTo { job_id, message } => {
                assert_eq!(job_id, "job_B");
                assert!(message.contains("Here is my draft"));
            }
            _ => panic!("expected SendTo"),
        }

        let ch = engine.store.get_channel("ch1").unwrap();
        assert_eq!(ch.current_speaker_idx, 1);
        assert_eq!(ch.history.len(), 1);
    }

    #[test]
    fn test_channel_max_rounds_termination() {
        let mut engine = OrchestratorEngine::new(4);

        let ch = Channel {
            channel_id: "ch1".into(),
            run_id: "run_1".into(),
            participants: vec!["job_A".into(), "job_B".into()],
            mode: ChannelMode::TurnBased,
            max_rounds: Some(1),
            on_peer_failure: PeerFailure::KillAll,
            current_round: 0,
            current_speaker_idx: 0,
            active: true,
            history: vec![],
        };
        engine.store.create_channel(ch);

        // A speaks (round 0, speaker 0 -> speaker 1)
        let _ = engine.route_channel_message("ch1", "job_A", "draft");
        // B speaks (round 0, speaker 1 -> round 1, speaker 0) — hits max_rounds
        let action =
            engine.route_channel_message("ch1", "job_B", "looks good");

        match action {
            ChannelAction::Close { channel_id } => {
                assert_eq!(channel_id, "ch1");
            }
            _ => {
                panic!(
                    "expected Close after max_rounds reached, got {:?}",
                    action
                )
            }
        }
    }

    #[test]
    fn test_channel_done_signal() {
        let mut engine = OrchestratorEngine::new(4);

        let ch = Channel {
            channel_id: "ch1".into(),
            run_id: "run_1".into(),
            participants: vec!["job_A".into(), "job_B".into()],
            mode: ChannelMode::TurnBased,
            max_rounds: None,
            on_peer_failure: PeerFailure::KillAll,
            current_round: 0,
            current_speaker_idx: 0,
            active: true,
            history: vec![],
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
            channel_id: "ch1".into(),
            run_id: "run_1".into(),
            participants: vec!["job_A".into(), "job_B".into()],
            mode: ChannelMode::TurnBased,
            max_rounds: None,
            on_peer_failure: PeerFailure::KillAll,
            current_round: 0,
            current_speaker_idx: 0,
            active: true,
            history: vec![],
        };
        engine.store.create_channel(ch);

        let action =
            engine.handle_channel_peer_failure("ch1", "job_A");
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
            channel_id: "ch1".into(),
            run_id: "run_1".into(),
            participants: vec!["job_A".into(), "job_B".into()],
            mode: ChannelMode::TurnBased,
            max_rounds: None,
            on_peer_failure: PeerFailure::Continue,
            current_round: 0,
            current_speaker_idx: 0,
            active: true,
            history: vec![],
        };
        engine.store.create_channel(ch);

        let action =
            engine.handle_channel_peer_failure("ch1", "job_A");
        match action {
            ChannelAction::NotifyPeers { job_ids, message } => {
                assert!(job_ids.contains(&"job_B".to_string()));
                assert!(message.contains("disconnected"));
            }
            _ => panic!("expected NotifyPeers"),
        }
    }

    #[test]
    fn test_channel_not_activated_if_participant_pending() {
        let mut engine = OrchestratorEngine::new(4);
        let run_id = engine.submit_run("test".into());

        let job_a = gen_id("job");
        let job_b = gen_id("job");
        engine.store.create_job(Job {
            job_id: job_a.clone(),
            run_id: run_id.clone(),
            parent_job_id: None,
            role: "writer".into(),
            status: JobStatus::Running,
            instruction: "write".into(),
            input_artifact_ids: vec![],
            depends_on: vec![],
            children: vec![],
            profile_id: "writer".into(),
            workspace_dir: std::path::PathBuf::from("/tmp"),
            attempt: 1,
            max_attempts: 3,
            created_at: SystemTime::now(),
            started_at: Some(SystemTime::now()),
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
            revision_round: 0,
        });
        engine.store.create_job(Job {
            job_id: job_b.clone(),
            run_id: run_id.clone(),
            parent_job_id: None,
            role: "reviewer".into(),
            status: JobStatus::Pending,
            instruction: "review".into(),
            input_artifact_ids: vec![],
            depends_on: vec![job_a.clone()],
            children: vec![],
            profile_id: "reviewer".into(),
            workspace_dir: std::path::PathBuf::from("/tmp"),
            attempt: 0,
            max_attempts: 3,
            created_at: SystemTime::now(),
            started_at: None,
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
            revision_round: 0,
        });
        engine.active_jobs.insert(job_a.clone(), run_id.clone());

        engine.store.create_channel(Channel {
            channel_id: "ch1".into(),
            run_id: run_id.clone(),
            participants: vec![job_a.clone(), job_b.clone()],
            mode: ChannelMode::TurnBased,
            max_rounds: None,
            on_peer_failure: PeerFailure::KillAll,
            current_round: 0,
            current_speaker_idx: 0,
            active: false,
            history: vec![],
        });

        let activated = engine.activate_ready_channels();
        assert!(activated.is_empty());
        assert!(!engine.store.get_channel("ch1").unwrap().active);
    }
}
