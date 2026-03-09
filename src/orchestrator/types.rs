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
    Pending,
    Ready,
    Running,
    Completed,
    Failed,
    Cancelled,
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
    #[serde(default)]
    pub revision_round: u32,
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
    pub initial_message: Option<String>,
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

#[derive(Debug, Clone)]
pub enum ChannelAction {
    SendTo { job_id: JobId, message: String },
    Broadcast { job_ids: Vec<JobId>, message: String },
    Close { channel_id: ChannelId },
    KillParticipants { job_ids: Vec<JobId> },
    NotifyPeers { job_ids: Vec<JobId>, message: String },
    NoOp,
}

/// Planner output: a list of jobs to create.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub jobs: Vec<PlannedJob>,
    #[serde(default)]
    pub channels: Vec<PlannedChannel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedJob {
    pub local_id: String,
    pub role: String,
    pub profile_id: String,
    pub instruction: String,
    pub depends_on: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
