//! Integration tests for the ZeptoKernel capsule integration.
//!
//! Tests the full path: spawn_capsule_job → ZeptoKernel capsule → worker → events.
//!
//! Run with: cargo test --test capsule_integration

use std::time::SystemTime;

use tokio::sync::mpsc;

use zeptopm::capsule::{capsule_spec_from_config, build_worker_env, spawn_capsule_job};
use zeptopm::config::{AgentConfig, Config, DaemonConfig};
use zeptopm::orchestrator::store::RunStore;
use zeptopm::orchestrator::types::{Job, JobStatus};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_config() -> Config {
    Config {
        daemon: DaemonConfig {
            isolation: "process".into(),
            worker_binary: None,
            zeptoclaw_binary: None,
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

#[test]
fn test_capsule_spec_from_config_integration() {
    let config = test_config();
    let job = test_job("spec-test");
    let spec = capsule_spec_from_config(&config, &job);

    assert_eq!(spec.isolation, zeptokernel::Isolation::Process);
    assert_eq!(spec.limits.timeout_sec, 30);
    assert!(spec.limits.memory_mib.is_none());
    assert!(spec.init_binary.is_none());
}

#[test]
fn test_build_worker_env_integration() {
    let mut config = test_config();
    config.daemon.zeptoclaw_binary = Some("/usr/bin/zeptoclaw".into());
    let env = build_worker_env(&config);

    assert_eq!(
        env.get("ZEPTOCLAW_BINARY").map(String::as_str),
        Some("/usr/bin/zeptoclaw")
    );
    assert!(env.contains_key("HOME") || env.contains_key("PATH"));
}

/// Test that spawn_capsule_job handles a missing worker binary gracefully.
#[tokio::test]
async fn test_capsule_spawn_missing_binary() {
    let mut config = test_config();
    config.daemon.isolation = "process".into();
    config.daemon.zeptoclaw_binary = Some("/nonexistent/binary".into());

    let job = test_job("missing-binary");
    let store = RunStore::new();
    let (tx, mut rx) = mpsc::channel(32);

    std::fs::create_dir_all(&job.workspace_dir).unwrap();
    spawn_capsule_job(&job, &config, tx, &store).await;

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Some(event) => {
                    let t = event["type"].as_str().unwrap_or("").to_string();
                    if t == "job_failed" {
                        return event;
                    }
                }
                None => panic!("channel closed before event"),
            }
        }
    })
    .await
    .expect("timed out waiting for job_failed");

    assert_eq!(result["type"].as_str().unwrap(), "job_failed");
    assert_eq!(result["job_id"].as_str().unwrap(), "missing-binary");
    assert_eq!(result["retryable"].as_bool().unwrap(), true);

    let _ = std::fs::remove_dir_all(&job.workspace_dir);
}

/// Test that namespace isolation returns NotSupported on macOS.
#[tokio::test]
async fn test_capsule_namespace_not_supported() {
    let mut config = test_config();
    config.daemon.isolation = "namespace".into();
    config.daemon.zeptoclaw_binary = Some("/bin/echo".into());

    let job = test_job("namespace-unsupported");
    let store = RunStore::new();
    let (tx, mut rx) = mpsc::channel(32);

    std::fs::create_dir_all(&job.workspace_dir).unwrap();
    spawn_capsule_job(&job, &config, tx, &store).await;

    #[cfg(not(target_os = "linux"))]
    {
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Some(event) => {
                        let t = event["type"].as_str().unwrap_or("").to_string();
                        if t == "job_failed" {
                            return event;
                        }
                    }
                    None => panic!("channel closed"),
                }
            }
        })
        .await
        .expect("timed out");

        assert_eq!(result["type"].as_str().unwrap(), "job_failed");
        let error = result["error"].as_str().unwrap();
        assert!(
            error.contains("not supported") || error.contains("Not") || error.contains("Linux"),
            "expected 'not supported' error, got: {error}"
        );
    }

    let _ = std::fs::remove_dir_all(&job.workspace_dir);
    #[cfg(target_os = "linux")]
    let _ = rx;
}
