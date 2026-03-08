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
