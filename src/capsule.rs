//! ZeptoKernel capsule integration — runs orchestration jobs inside isolated capsules.
//!
//! When `isolation = "capsule"` in config, jobs are executed via ZeptoKernel's
//! ProcessBackend + Supervisor instead of bare child processes.

use std::collections::HashMap;
use std::path::PathBuf;

use tokio::sync::mpsc;
use tracing::{info, warn};

use zk_host::process_backend::ProcessBackend;
use zk_host::supervisor::{JobOutcome, Supervisor};
use zk_proto::{JobSpec, ResourceLimits, WorkspaceConfig};

use crate::orchestrator::store::RunStore;
use crate::orchestrator::types::Job;

/// Convert a ZeptoPM `Job` to a ZeptoKernel `JobSpec`.
pub fn job_to_spec(job: &Job, input_artifact_paths: Vec<String>) -> JobSpec {
  let input_artifacts = input_artifact_paths
    .into_iter()
    .enumerate()
    .map(|(i, path)| zk_proto::ArtifactRef {
      artifact_id: job
        .input_artifact_ids
        .get(i)
        .cloned()
        .unwrap_or_else(|| format!("input_{}", i)),
      guest_path: PathBuf::from(&path),
      kind: "file".into(),
      summary: String::new(),
    })
    .collect();

  // Pass ZeptoPM env to the capsule (provider keys, etc.)
  let env: HashMap<String, String> = std::env::vars()
    .filter(|(k, _)| {
      k.starts_with("OPENROUTER_")
        || k.starts_with("OPENAI_")
        || k.starts_with("ANTHROPIC_")
        || k == "HOME"
        || k == "PATH"
    })
    .collect();

  JobSpec {
    job_id: job.job_id.clone(),
    run_id: job.run_id.clone(),
    role: job.role.clone(),
    profile_id: job.profile_id.clone(),
    instruction: job.instruction.clone(),
    input_artifacts,
    env,
    limits: ResourceLimits::default(),
    workspace: WorkspaceConfig {
      guest_path: job.workspace_dir.clone(),
      size_mib: None,
    },
  }
}

/// Spawn an orchestration job inside a ZeptoKernel capsule.
///
/// Runs `Supervisor::run_job()` in a background tokio task.
/// Translates `JobOutcome` into events on the orchestrator event channel.
/// Sends periodic heartbeats so ZeptoPM's stale-job detection doesn't trigger.
pub async fn spawn_capsule_job(
  job: &Job,
  guest_binary: &str,
  orch_event_tx: mpsc::Sender<serde_json::Value>,
  orchestrator_store: &RunStore,
) {
  let input_artifacts: Vec<String> = job
    .input_artifact_ids
    .iter()
    .filter_map(|aid| orchestrator_store.get_artifact(aid))
    .map(|a| a.path.to_string_lossy().to_string())
    .collect();

  let spec = job_to_spec(job, input_artifacts);
  let guest_binary = guest_binary.to_string();
  let job_id = job.job_id.clone();

  info!(job_id = %job_id, "spawning capsule job via ZeptoKernel");

  tokio::spawn(async move {
    let backend = ProcessBackend::new(&guest_binary);
    let mut supervisor = Supervisor::new();

    // Send periodic heartbeats to ZeptoPM while the capsule runs
    let hb_tx = orch_event_tx.clone();
    let hb_job_id = job_id.clone();
    let hb_handle = tokio::spawn(async move {
      let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
      loop {
        interval.tick().await;
        let _ = hb_tx
          .send(serde_json::json!({
            "type": "heartbeat",
            "job_id": hb_job_id,
          }))
          .await;
      }
    });

    let result = supervisor.run_job(&backend, &spec, &guest_binary).await;
    hb_handle.abort(); // stop heartbeat timer

    match result {
      Ok(JobOutcome::Completed {
        job_id,
        output_artifact_ids,
        summary: _,
      }) => {
        info!(job_id = %job_id, "capsule job completed");
        let _ = orch_event_tx
          .send(serde_json::json!({
            "type": "job_completed",
            "job_id": job_id,
            "output_artifact_ids": output_artifact_ids,
          }))
          .await;
      }
      Ok(JobOutcome::Failed {
        job_id,
        error,
        retryable,
      }) => {
        warn!(job_id = %job_id, error = %error, "capsule job failed");
        let _ = orch_event_tx
          .send(serde_json::json!({
            "type": "job_failed",
            "job_id": job_id,
            "error": error,
            "retryable": retryable,
          }))
          .await;
      }
      Ok(JobOutcome::Cancelled { job_id }) => {
        info!(job_id = %job_id, "capsule job cancelled");
        let _ = orch_event_tx
          .send(serde_json::json!({
            "type": "job_failed",
            "job_id": job_id,
            "error": "cancelled",
            "retryable": false,
          }))
          .await;
      }
      Err(e) => {
        warn!(job_id = %job_id, error = %e, "capsule supervisor error");
        let _ = orch_event_tx
          .send(serde_json::json!({
            "type": "job_failed",
            "job_id": job_id,
            "error": e.to_string(),
            "retryable": true,
          }))
          .await;
      }
    }
  });
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::SystemTime;

  fn make_test_job() -> Job {
    Job {
      job_id: "job_1".into(),
      run_id: "run_1".into(),
      parent_job_id: None,
      role: "coder".into(),
      status: crate::orchestrator::types::JobStatus::Ready,
      instruction: "Write a hello world program".into(),
      input_artifact_ids: vec!["art_in_1".into()],
      depends_on: vec![],
      children: vec![],
      profile_id: "coder-agent".into(),
      workspace_dir: PathBuf::from("/tmp/workspace"),
      attempt: 1,
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
  fn test_job_to_spec_basic_mapping() {
    let job = make_test_job();
    let spec = job_to_spec(&job, vec!["/tmp/input.md".into()]);

    assert_eq!(spec.job_id, "job_1");
    assert_eq!(spec.run_id, "run_1");
    assert_eq!(spec.role, "coder");
    assert_eq!(spec.profile_id, "coder-agent");
    assert_eq!(spec.instruction, "Write a hello world program");
    assert_eq!(spec.input_artifacts.len(), 1);
    assert_eq!(
      spec.input_artifacts[0].guest_path,
      PathBuf::from("/tmp/input.md")
    );
    assert_eq!(spec.input_artifacts[0].artifact_id, "art_in_1");
    assert_eq!(spec.workspace.guest_path, PathBuf::from("/tmp/workspace"));
  }

  #[test]
  fn test_job_to_spec_no_artifacts() {
    let mut job = make_test_job();
    job.input_artifact_ids.clear();
    let spec = job_to_spec(&job, vec![]);

    assert!(spec.input_artifacts.is_empty());
  }

  #[test]
  fn test_job_to_spec_env_passthrough() {
    // HOME and PATH should always be present
    let job = make_test_job();
    let spec = job_to_spec(&job, vec![]);

    // At minimum HOME should be set in the test env
    assert!(spec.env.contains_key("HOME") || spec.env.contains_key("PATH"));
  }

  #[test]
  fn test_job_to_spec_default_limits() {
    let job = make_test_job();
    let spec = job_to_spec(&job, vec![]);

    assert_eq!(spec.limits.timeout_sec, 300);
    assert_eq!(spec.limits.heartbeat_timeout_sec, 60);
    assert!(!spec.limits.network);
  }
}
