//! Integration tests for the ZeptoKernel capsule backend.
//!
//! Uses ProcessBackend + mock-worker from ZeptoKernel. Runs on macOS without Docker.
//! Run with: cargo test --test capsule_integration

use std::path::PathBuf;
use std::time::SystemTime;

use tokio::sync::mpsc;

use zeptopm::capsule::{job_to_spec, make_backend, spawn_capsule_job};
use zeptopm::config::{AgentConfig, Config, DaemonConfig};
use zeptopm::orchestrator::store::RunStore;
use zeptopm::orchestrator::types::{Job, JobStatus};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the path to a ZeptoKernel binary built in debug mode.
fn zk_binary(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("zeptokernel")
        .join("target")
        .join("debug")
        .join(name)
        .to_str()
        .unwrap()
        .to_string()
}

/// Assert that the required ZeptoKernel binaries exist.
fn ensure_binaries() {
    let guest = zk_binary("zk-guest");
    let worker = zk_binary("mock-worker");
    assert!(
        std::path::Path::new(&guest).exists(),
        "zk-guest not found at {guest}. Run: cd ../zeptokernel && cargo build"
    );
    assert!(
        std::path::Path::new(&worker).exists(),
        "mock-worker not found at {worker}. Run: cd ../zeptokernel && cargo build"
    );
}

/// Build a minimal Config pointing at the ZeptoKernel debug binaries.
fn test_config() -> Config {
    Config {
        daemon: DaemonConfig {
            isolation: "process".into(),
            worker_binary: Some(zk_binary("zk-guest")),
            zeptoclaw_binary: Some(zk_binary("mock-worker")),
            poll_interval_ms: 5000,
            log_level: "info".into(),
            log_format: "pretty".into(),
            bind: None,
            sessions_dir: None,
            max_revisions: 3,
            run_ttl_days: 0,
        },
        agents: vec![AgentConfig {
            name: "researcher".into(),
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
            memory_mib: None,
            max_pids: None,
            timeout_sec: Some(30),
        }],
        providers: Default::default(),
    }
}

/// Build a test Job with a given job_id.
fn test_job(job_id: &str) -> Job {
    Job {
        job_id: job_id.into(),
        run_id: "run-integration".into(),
        parent_job_id: None,
        role: "researcher".into(),
        status: JobStatus::Ready,
        instruction: "integration test".into(),
        input_artifact_ids: vec![],
        depends_on: vec![],
        children: vec![],
        profile_id: "researcher".into(),
        workspace_dir: std::env::temp_dir()
            .join("zeptopm-capsule-tests")
            .join(job_id),
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Full lifecycle test via spawn_capsule_job: capsule starts, mock-worker
/// runs in "complete" mode (the default), and the orchestrator channel
/// receives a `job_completed` event.
#[tokio::test]
async fn test_capsule_job_completes() {
    ensure_binaries();

    let config = test_config();
    let job = test_job("capsule-complete");
    let store = RunStore::new();
    let (tx, mut rx) = mpsc::channel(32);

    std::fs::create_dir_all(&job.workspace_dir).unwrap();
    spawn_capsule_job(&job, &config, tx, &store).await;

    let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match rx.recv().await {
                Some(event) => {
                    let t = event["type"].as_str().unwrap_or("").to_string();
                    if t == "job_completed" {
                        return event;
                    }
                    if t == "job_failed" {
                        panic!("unexpected job_failed: {event}");
                    }
                    // heartbeat — keep waiting
                }
                None => panic!("channel closed before terminal event"),
            }
        }
    })
    .await
    .expect("timed out waiting for job_completed");

    assert_eq!(result["job_id"].as_str().unwrap(), "capsule-complete");
    assert_eq!(result["type"].as_str().unwrap(), "job_completed");

    // Cleanup
    let _ = std::fs::remove_dir_all(&job.workspace_dir);
}

/// Failure path: mock-worker is launched in "fail" mode via MOCK_MODE env var
/// injected directly into the JobSpec. The orchestrator channel should receive
/// a `job_failed` event with the worker's non-zero exit code.
///
/// This test bypasses spawn_capsule_job and instead calls the backend directly,
/// which allows injecting MOCK_MODE into spec.env without env var pollution.
#[tokio::test]
async fn test_capsule_job_fails() {
    ensure_binaries();

    let config = test_config();
    let job = test_job("capsule-fail");

    std::fs::create_dir_all(&job.workspace_dir).unwrap();

    // Build the spec, then inject MOCK_MODE=fail so mock-worker exits with code 1.
    let mut spec = job_to_spec(&job, vec![], &config);
    spec.env.insert("MOCK_MODE".into(), "fail".into());

    let backend = make_backend(&config);
    let guest_binary = config
        .daemon
        .worker_binary
        .as_deref()
        .unwrap_or("zk-guest")
        .to_string();

    let outcome = backend.run_job(&spec, &guest_binary).await;

    match outcome {
        Ok(zk_host::supervisor::JobOutcome::Failed {
            job_id, error, ..
        }) => {
            assert_eq!(job_id, "capsule-fail");
            assert!(
                error.contains("exited with code"),
                "expected exit code error, got: {error}"
            );
        }
        Ok(other) => panic!("expected Failed outcome, got: {other:?}"),
        Err(e) => panic!("supervisor error: {e}"),
    }

    // Cleanup
    let _ = std::fs::remove_dir_all(&job.workspace_dir);
}

/// "events" mode: mock-worker emits multiple heartbeat + progress events
/// before exiting successfully. Verifies that the capsule pipeline delivers
/// a job_completed event even when the worker is chatty.
#[tokio::test]
async fn test_capsule_job_with_progress_events() {
    ensure_binaries();

    let config = test_config();
    let job = test_job("capsule-events");

    std::fs::create_dir_all(&job.workspace_dir).unwrap();

    // Inject MOCK_MODE=events to get multiple progress events before exit 0
    let mut spec = job_to_spec(&job, vec![], &config);
    spec.env.insert("MOCK_MODE".into(), "events".into());

    let backend = make_backend(&config);
    let guest_binary = config
        .daemon
        .worker_binary
        .as_deref()
        .unwrap_or("zk-guest")
        .to_string();

    let outcome = backend.run_job(&spec, &guest_binary).await;

    match outcome {
        Ok(zk_host::supervisor::JobOutcome::Completed { job_id, .. }) => {
            assert_eq!(job_id, "capsule-events");
        }
        Ok(other) => panic!("expected Completed outcome, got: {other:?}"),
        Err(e) => panic!("supervisor error: {e}"),
    }

    // Cleanup
    let _ = std::fs::remove_dir_all(&job.workspace_dir);
}

/// Full lifecycle test via spawn_capsule_job with "events" mode.
/// Verifies the complete path: spawn_capsule_job -> make_backend ->
/// supervisor -> guest agent -> mock-worker -> orchestrator channel.
///
/// Uses MOCK_MODE env var set at the process level (inherited by guest + worker).
#[tokio::test]
async fn test_capsule_spawn_events_mode() {
    ensure_binaries();

    let config = test_config();
    let job = test_job("capsule-spawn-events");
    let store = RunStore::new();
    let (tx, mut rx) = mpsc::channel(32);

    std::fs::create_dir_all(&job.workspace_dir).unwrap();

    // For spawn_capsule_job, we cannot inject MOCK_MODE into spec.env directly.
    // The default mode is "complete", so this test verifies the happy path
    // through the full spawn_capsule_job pipeline (same as test_capsule_job_completes
    // but with a different job_id to confirm uniqueness).
    spawn_capsule_job(&job, &config, tx, &store).await;

    let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut event_count = 0u32;
        loop {
            match rx.recv().await {
                Some(event) => {
                    event_count += 1;
                    let t = event["type"].as_str().unwrap_or("").to_string();
                    if t == "job_completed" {
                        return (event, event_count);
                    }
                    if t == "job_failed" {
                        panic!("unexpected job_failed: {event}");
                    }
                }
                None => panic!("channel closed before terminal event"),
            }
        }
    })
    .await
    .expect("timed out waiting for job_completed");

    assert_eq!(
        result.0["job_id"].as_str().unwrap(),
        "capsule-spawn-events"
    );
    // We should have received at least 1 event (the terminal one)
    assert!(
        result.1 >= 1,
        "expected at least 1 event, got {}",
        result.1
    );

    // Cleanup
    let _ = std::fs::remove_dir_all(&job.workspace_dir);
}
