//! ZeptoKernel capsule integration — runs orchestration jobs inside isolated capsules.
//!
//! When `isolation = "capsule"` in config, jobs are executed via ZeptoKernel's
//! ProcessBackend + Supervisor instead of bare child processes.

use std::collections::HashMap;
use std::path::PathBuf;

use tokio::sync::mpsc;
use tracing::{info, warn};

use zk_host::process_backend::ProcessBackend;
use zk_host::supervisor::{JobOutcome, Supervisor, SupervisorError};
#[cfg(all(target_os = "linux", feature = "namespace"))]
use zk_host::namespace_backend::NamespaceBackend;
use zk_proto::{JobSpec, ResourceLimits, WorkspaceConfig};

use crate::orchestrator::store::RunStore;
use crate::orchestrator::types::Job;

/// Backend selector for capsule job execution.
///
/// Enum-dispatch avoids trait objects (Backend has an associated Handle type).
/// Adding Firecracker in M6 = one new variant + one new match arm.
pub enum CapsuleBackend {
    Process(ProcessBackend),
    #[cfg(all(target_os = "linux", feature = "namespace"))]
    Namespace(NamespaceBackend),
}

impl CapsuleBackend {
    /// Run a capsule job through this backend's Supervisor.
    pub async fn run_job(
        &self,
        spec: &JobSpec,
        worker_binary: &str,
    ) -> Result<JobOutcome, SupervisorError> {
        let mut supervisor = Supervisor::new();
        match self {
            Self::Process(b) => supervisor.run_job(b, spec, worker_binary).await,
            #[cfg(all(target_os = "linux", feature = "namespace"))]
            Self::Namespace(b) => supervisor.run_job(b, spec, worker_binary).await,
        }
    }
}

/// Create the backend based on `daemon.isolation` config.
///
/// Falls back to `ProcessBackend` on macOS regardless of the isolation setting,
/// since `NamespaceBackend` is Linux-only.
pub fn make_backend(config: &crate::config::Config) -> CapsuleBackend {
    let guest = config.daemon.worker_binary.as_deref().unwrap_or("zk-guest");
    match config.daemon.isolation.as_str() {
        #[cfg(all(target_os = "linux", feature = "namespace"))]
        "namespace" => CapsuleBackend::Namespace(NamespaceBackend::new(guest)),
        _ => CapsuleBackend::Process(ProcessBackend::new(guest)),
    }
}

/// Convert a ZeptoPM `Job` to a ZeptoKernel `JobSpec`.
///
/// Pulls resource limits from the matching `AgentConfig` in `config.agents`
/// (matched by `job.profile_id == agent.name`). Falls back to defaults when
/// no matching agent profile is found.
///
/// Injects `ZEPTOCLAW_BINARY` into `spec.env` when `config.daemon.zeptoclaw_binary`
/// is set, so the guest agent can locate the worker binary inside the capsule.
pub fn job_to_spec(job: &Job, input_artifact_paths: Vec<String>, config: &crate::config::Config) -> JobSpec {
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

  // Find the agent profile matching this job's profile_id
  let agent = config.agents.iter().find(|a| a.name == job.profile_id);
  if agent.is_none() {
    warn!(
      job_id = %job.job_id,
      profile_id = %job.profile_id,
      "no agent profile matched — using default resource limits; check that profile_id matches an [[agents]] name in config"
    );
  }

  // Build resource limits from agent profile (or defaults)
  let limits = ResourceLimits {
    memory_mib: agent.and_then(|a| a.memory_mib),
    max_pids: agent.and_then(|a| a.max_pids),
    timeout_sec: agent
      .and_then(|a| a.timeout_sec)
      .unwrap_or(crate::config::DEFAULT_CAPSULE_TIMEOUT_SEC),
    heartbeat_timeout_sec: 60,
    cpu_quota: None,
    network: false,
    max_output_bytes: None,
  };

  // Pass ZeptoPM env to the capsule (provider keys, etc.)
  let mut env: HashMap<String, String> = std::env::vars()
    .filter(|(k, _)| {
      k.starts_with("OPENROUTER_")
        || k.starts_with("OPENAI_")
        || k.starts_with("ANTHROPIC_")
        || k == "HOME"
        || k == "PATH"
    })
    .collect();

  // Inject ZEPTOCLAW_BINARY so the guest agent can locate the worker
  if let Some(zeptoclaw) = config.daemon.zeptoclaw_binary.as_deref() {
    env.insert("ZEPTOCLAW_BINARY".into(), zeptoclaw.into());
  }

  JobSpec {
    job_id: job.job_id.clone(),
    run_id: job.run_id.clone(),
    role: job.role.clone(),
    profile_id: job.profile_id.clone(),
    instruction: job.instruction.clone(),
    input_artifacts,
    env,
    limits,
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
  config: &crate::config::Config,
) {
  let input_artifacts: Vec<String> = job
    .input_artifact_ids
    .iter()
    .filter_map(|aid| orchestrator_store.get_artifact(aid))
    .map(|a| a.path.to_string_lossy().to_string())
    .collect();

  let spec = job_to_spec(job, input_artifacts, config);
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

  fn make_test_config_full(isolation: &str) -> crate::config::Config {
    crate::config::Config {
      daemon: crate::config::DaemonConfig {
        isolation: isolation.into(),
        worker_binary: Some("/usr/bin/zk-guest".into()),
        zeptoclaw_binary: Some("/usr/bin/zeptoclaw".into()),
        poll_interval_ms: 5000,
        log_level: "info".into(),
        log_format: "pretty".into(),
        bind: None,
        sessions_dir: None,
        max_revisions: 3,
        run_ttl_days: 0,
      },
      agents: vec![crate::config::AgentConfig {
        name: "coder-agent".into(),
        provider: "openrouter".into(),
        model: None,
        system_prompt: None,
        tools: vec![],
        auto_start: true,
        max_restarts: 5,
        restart_backoff_ms: 10_000,
        max_iterations: None,
        timeout_ms: None,
        budget: None,
        gateway: None,
        session_persist: true,
        max_history: None,
        memory_mib: Some(512),
        max_pids: Some(64),
        timeout_sec: Some(600),
      }],
      providers: Default::default(),
    }
  }

  #[test]
  fn test_job_to_spec_basic_mapping() {
    let job = make_test_job();
    let config = make_test_config_full("process");
    let spec = job_to_spec(&job, vec!["/tmp/input.md".into()], &config);

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
    let config = make_test_config_full("process");
    let spec = job_to_spec(&job, vec![], &config);

    assert!(spec.input_artifacts.is_empty());
  }

  #[test]
  fn test_job_to_spec_env_passthrough() {
    // HOME and PATH should always be present
    let job = make_test_job();
    let config = make_test_config_full("process");
    let spec = job_to_spec(&job, vec![], &config);

    // At minimum HOME should be set in the test env
    assert!(spec.env.contains_key("HOME") || spec.env.contains_key("PATH"));
  }

  #[test]
  fn test_job_to_spec_default_limits() {
    let job = make_test_job();
    let config = make_test_config_full("process");
    let spec = job_to_spec(&job, vec![], &config);

    // Agent profile has timeout_sec = 600, so it should use that
    assert_eq!(spec.limits.timeout_sec, 600);
    assert_eq!(spec.limits.heartbeat_timeout_sec, 60);
    assert!(!spec.limits.network);
  }

  #[test]
  fn test_job_to_spec_with_limits() {
    let job = make_test_job();
    let config = make_test_config_full("process");
    let spec = job_to_spec(&job, vec![], &config);

    assert_eq!(spec.limits.memory_mib, Some(512));
    assert_eq!(spec.limits.max_pids, Some(64));
    assert_eq!(spec.limits.timeout_sec, 600);
  }

  #[test]
  fn test_job_to_spec_injects_zeptoclaw_binary() {
    let job = make_test_job();
    let config = make_test_config_full("process");
    let spec = job_to_spec(&job, vec![], &config);

    assert_eq!(
      spec.env.get("ZEPTOCLAW_BINARY").map(String::as_str),
      Some("/usr/bin/zeptoclaw")
    );
  }

  #[test]
  fn test_job_to_spec_no_zeptoclaw_binary() {
    let job = make_test_job();
    let mut config = make_test_config_full("process");
    config.daemon.zeptoclaw_binary = None;
    let spec = job_to_spec(&job, vec![], &config);

    assert!(!spec.env.contains_key("ZEPTOCLAW_BINARY"));
  }

  #[test]
  fn test_job_to_spec_default_limits_when_no_agent_profile() {
    let job = make_test_job(); // profile_id = "coder-agent"
    let mut config = make_test_config_full("process");
    config.agents.clear(); // no matching agent
    let spec = job_to_spec(&job, vec![], &config);

    assert!(spec.limits.memory_mib.is_none());
    assert_eq!(spec.limits.timeout_sec, crate::config::DEFAULT_CAPSULE_TIMEOUT_SEC);
  }

  #[test]
  fn test_make_backend_process_isolation() {
    let config = make_test_config_full("process");
    let backend = make_backend(&config);
    assert!(matches!(backend, CapsuleBackend::Process(_)));
  }

  #[test]
  fn test_make_backend_capsule_alias() {
    // "capsule" is a backward-compat alias for "process"
    let config = make_test_config_full("capsule");
    let backend = make_backend(&config);
    assert!(matches!(backend, CapsuleBackend::Process(_)));
  }

  #[test]
  fn test_make_backend_none_fallback() {
    let config = make_test_config_full("none");
    let backend = make_backend(&config);
    assert!(matches!(backend, CapsuleBackend::Process(_)));
  }

  #[cfg(all(target_os = "linux", feature = "namespace"))]
  #[test]
  fn test_make_backend_namespace_isolation() {
    let config = make_test_config_full("namespace");
    let backend = make_backend(&config);
    assert!(matches!(backend, CapsuleBackend::Namespace(_)));
  }
}
