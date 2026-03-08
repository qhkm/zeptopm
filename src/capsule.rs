//! ZeptoKernel capsule integration — runs orchestration jobs inside isolated capsules.
//!
//! When `isolation` is `"capsule"`, `"process"`, or `"namespace"` in config,
//! jobs are executed inside a ZeptoKernel capsule. ZeptoPM talks to ZeptoClaw
//! directly through the capsule's stdin/stdout pipes — same IPC protocol as
//! isolation="none" mode, just wrapped in a sandbox.
//!
//! ZeptoKernel owns mechanisms (isolation, resource enforcement).
//! ZeptoPM owns meaning (job lifecycle, supervision, events).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{info, warn};

use zeptokernel::{
    CapsuleReport, CapsuleSpec, Isolation, ResourceLimits, ResourceViolation, Signal,
    WorkspaceConfig,
};

use crate::agent::{AgentCommand, AgentHandle};
use crate::orchestrator::store::RunStore;
use crate::orchestrator::types::Job;

/// Build a `CapsuleSpec` from ZeptoPM config.
pub fn capsule_spec_from_config(config: &crate::config::Config, job: &Job) -> CapsuleSpec {
    let agent = config.agents.iter().find(|a| a.name == job.profile_id);
    if agent.is_none() {
        warn!(
          job_id = %job.job_id,
          profile_id = %job.profile_id,
          "no agent profile matched — using default resource limits"
        );
    }

    let isolation = match config.daemon.isolation.as_str() {
        "namespace" => Isolation::Namespace,
        _ => Isolation::Process,
    };

    CapsuleSpec {
        isolation,
        workspace: WorkspaceConfig {
            host_path: Some(job.workspace_dir.clone()),
            guest_path: guest_workspace_path(config, job),
            size_mib: None,
        },
        limits: ResourceLimits {
            timeout_sec: agent
                .and_then(|a| a.timeout_sec)
                .unwrap_or(crate::config::DEFAULT_CAPSULE_TIMEOUT_SEC),
            memory_mib: agent.and_then(|a| a.memory_mib),
            cpu_quota: None,
            max_pids: agent.and_then(|a| a.max_pids),
        },
        init_binary: namespace_init_binary(config),
    }
}

fn namespace_init_binary(config: &crate::config::Config) -> Option<PathBuf> {
    if config.daemon.isolation != "namespace" {
        return None;
    }
    config.daemon.worker_binary.as_ref().map(PathBuf::from)
}

fn guest_workspace_path(config: &crate::config::Config, job: &Job) -> PathBuf {
    if config.daemon.isolation == "namespace" {
        PathBuf::from("/workspace")
    } else {
        job.workspace_dir.clone()
    }
}

/// Build environment variables for the worker process inside the capsule.
pub fn build_worker_env(config: &crate::config::Config) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars()
        .filter(|(k, _)| {
            k.starts_with("OPENROUTER_")
                || k.starts_with("OPENAI_")
                || k.starts_with("ANTHROPIC_")
                || k == "HOME"
                || k == "PATH"
        })
        .collect();

    if let Some(zeptoclaw) = config.daemon.zeptoclaw_binary.as_deref() {
        env.insert("ZEPTOCLAW_BINARY".into(), zeptoclaw.into());
    }

    env
}

/// Spawn an orchestration job inside a ZeptoKernel capsule.
///
/// Creates a capsule, spawns ZeptoClaw inside it, then drives the worker IPC
/// directly through the capsule's stdin/stdout pipes. Same JSON-line protocol
/// as isolation="none" mode — ZeptoPM interprets events, ZeptoKernel just
/// provides the sandbox.
pub fn spawn_capsule_job(
    job: &Job,
    config: &crate::config::Config,
    orch_event_tx: mpsc::Sender<serde_json::Value>,
    orchestrator_store: &RunStore,
) -> (AgentHandle, tokio::task::JoinHandle<()>) {
    let spec = capsule_spec_from_config(config, job);
    let worker_binary = config
        .daemon
        .zeptoclaw_binary
        .as_deref()
        .unwrap_or("zeptoclaw")
        .to_string();
    let env = build_worker_env(config);
    let job_id = job.job_id.clone();
    let instruction = job.instruction.clone();
    let host_workspace = job.workspace_dir.clone();
    let guest_workspace = spec.workspace.guest_path.clone();
    let job_spec_name = format!("job-{}.json", job_id);
    let host_spec_path = host_workspace.join(&job_spec_name);
    let guest_spec_path = guest_workspace.join(&job_spec_name);

    // Resolve input artifact paths for the job spec file
    let input_artifacts: Vec<serde_json::Value> = job
        .input_artifact_ids
        .iter()
        .filter_map(|aid| orchestrator_store.get_artifact(aid))
        .map(|a| {
            serde_json::json!({
              "artifact_id": a.artifact_id,
              "path": a.path.to_string_lossy(),
              "kind": a.kind,
              "summary": a.summary,
            })
        })
        .collect();

    info!(job_id = %job_id, "spawning capsule job via ZeptoKernel");

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<AgentCommand>(8);
    let handle = AgentHandle {
        name: format!("__job_{}", job_id),
        cmd_tx,
    };

    let join = tokio::spawn(async move {
        // Create capsule
        let mut capsule = match zeptokernel::create(spec) {
            Ok(c) => c,
            Err(e) => {
                warn!(job_id = %job_id, error = %e, "failed to create capsule");
                let _ = orch_event_tx
                    .send(serde_json::json!({
                      "type": "job_failed",
                      "job_id": job_id,
                      "error": format!("capsule creation failed: {e}"),
                      "retryable": true,
                    }))
                    .await;
                return;
            }
        };

        if let Err(e) = std::fs::create_dir_all(&host_workspace) {
            warn!(
                job_id = %job_id,
                workspace = %host_workspace.display(),
                error = %e,
                "failed to create host workspace"
            );
            let _ = orch_event_tx
                .send(serde_json::json!({
                  "type": "job_failed",
                  "job_id": job_id,
                  "error": format!("failed to create workspace: {e}"),
                  "retryable": true,
                }))
                .await;
            let _ = capsule.destroy();
            return;
        }

        // Write job spec where the worker can see it inside the guest workspace.
        let job_spec = serde_json::json!({
          "job_id": job_id,
          "instruction": instruction,
          "input_artifacts": input_artifacts,
        });
        if let Err(e) = std::fs::write(
            &host_spec_path,
            serde_json::to_string_pretty(&job_spec).unwrap(),
        ) {
            warn!(job_id = %job_id, error = %e, "failed to write job spec");
            let _ = orch_event_tx
                .send(serde_json::json!({
                  "type": "job_failed",
                  "job_id": job_id,
                  "error": format!("failed to write job spec: {e}"),
                  "retryable": true,
                }))
                .await;
            let _ = capsule.destroy();
            return;
        }

        // Spawn worker inside capsule (sync call)
        let args_owned = vec![
            "--job-spec".to_string(),
            guest_spec_path.to_string_lossy().to_string(),
            "--job-id".to_string(),
            job_id.clone(),
        ];
        let args: Vec<&str> = args_owned.iter().map(|s| s.as_str()).collect();

        let child = match capsule.spawn(&worker_binary, &args, env) {
            Ok(c) => c,
            Err(e) => {
                warn!(job_id = %job_id, error = %e, "failed to spawn worker in capsule");
                let _ = orch_event_tx
                    .send(serde_json::json!({
                      "type": "job_failed",
                      "job_id": job_id,
                      "error": format!("capsule spawn failed: {e}"),
                      "retryable": true,
                    }))
                    .await;
                let _ = capsule.destroy();
                return;
            }
        };

        // Drive IPC directly through pipes — same protocol as isolation="none"
        // ZeptoPM interprets events. ZeptoKernel just provides the sandbox.
        let mut reader = BufReader::new(child.stdout).lines();
        let _stdin = child.stdin; // kept alive so worker doesn't get broken pipe
        let mut saw_terminal_event = false;
        let mut suppress_terminal_event = false;
        let mut deferred_failure: Option<serde_json::Value> = None;

        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(AgentCommand::Stop) | None => {
                            suppress_terminal_event = true;
                            let _ = capsule.kill(Signal::Kill);
                            break;
                        }
                        Some(AgentCommand::UserMessage(..) | AgentCommand::JobExecute { .. }) => {}
                    }
                }
                line = reader.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            let event: serde_json::Value = match serde_json::from_str(&line) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };

                            let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

                            match event_type {
                                "heartbeat" | "progress" => {
                                    let _ = orch_event_tx.send(event.clone()).await;
                                }
                                "artifact_produced" => {
                                    let translated = translate_guest_event_paths(
                                        &event,
                                        &guest_workspace,
                                        &host_workspace,
                                    );
                                    let _ = orch_event_tx.send(translated).await;
                                }
                                "job_completed" => {
                                    saw_terminal_event = true;
                                    info!(job_id = %job_id, "capsule job completed");
                                    let output_ids = event
                                        .get("output_artifact_ids")
                                        .cloned()
                                        .unwrap_or(serde_json::json!([]));
                                    let _ = orch_event_tx
                                        .send(serde_json::json!({
                                          "type": "job_completed",
                                          "job_id": job_id,
                                          "output_artifact_ids": output_ids,
                                        }))
                                        .await;
                                    break;
                                }
                                "job_failed" => {
                                    saw_terminal_event = true;
                                    let error = event
                                        .get("error")
                                        .and_then(|e| e.as_str())
                                        .unwrap_or("unknown error");
                                    let retryable = event
                                        .get("retryable")
                                        .and_then(|r| r.as_bool())
                                        .unwrap_or(false);
                                    warn!(job_id = %job_id, error = %error, "capsule job failed");
                                    let _ = orch_event_tx
                                        .send(serde_json::json!({
                                          "type": "job_failed",
                                          "job_id": job_id,
                                          "error": error,
                                          "retryable": retryable,
                                        }))
                                        .await;
                                    break;
                                }
                                _ => {}
                            }
                        }
                        Ok(None) => {
                            warn!(job_id = %job_id, "worker pipe closed without terminal event");
                            break;
                        }
                        Err(e) => {
                            warn!(job_id = %job_id, error = %e, "error reading worker pipe");
                            deferred_failure = Some(serde_json::json!({
                              "type": "job_failed",
                              "job_id": job_id,
                              "error": format!("pipe read error: {e}"),
                              "retryable": true,
                            }));
                            break;
                        }
                    }
                }
            }
        }

        // Cleanup
        let _ = std::fs::remove_file(&host_spec_path);
        let report = capsule.destroy();
        if let Ok(report) = &report {
            if let Some(violation) = &report.killed_by {
                warn!(
                    job_id = %job_id,
                    violation = ?violation,
                    exit_code = ?report.exit_code,
                    exit_signal = ?report.exit_signal,
                    wall_time_secs = report.wall_time.as_secs(),
                    "capsule resource violation"
                );
            }
        }

        if !saw_terminal_event && !suppress_terminal_event {
            let failure_event = deferred_failure.unwrap_or_else(|| {
                capsule_failure_event(
                    &job_id,
                    report.as_ref().ok(),
                    "worker exited unexpectedly (pipe EOF)",
                )
            });
            let _ = orch_event_tx.send(failure_event).await;
        }
    });

    (handle, join)
}

fn translate_guest_event_paths(
    event: &serde_json::Value,
    guest_workspace: &Path,
    host_workspace: &Path,
) -> serde_json::Value {
    if guest_workspace == host_workspace {
        return event.clone();
    }

    let Some(path) = event.get("path").and_then(|value| value.as_str()) else {
        return event.clone();
    };

    let event_path = PathBuf::from(path);
    let Ok(relative) = event_path.strip_prefix(guest_workspace) else {
        return event.clone();
    };

    let mut translated = event.clone();
    translated["path"] =
        serde_json::Value::String(host_workspace.join(relative).to_string_lossy().into_owned());
    translated
}

fn capsule_failure_event(
    job_id: &str,
    report: Option<&CapsuleReport>,
    fallback: &str,
) -> serde_json::Value {
    let error = report
        .map(capsule_failure_message)
        .unwrap_or_else(|| fallback.to_string());
    serde_json::json!({
      "type": "job_failed",
      "job_id": job_id,
      "error": error,
      "retryable": true,
    })
}

fn capsule_failure_message(report: &CapsuleReport) -> String {
    if let Some(violation) = report.killed_by {
        return match violation {
            ResourceViolation::WallClock => "capsule killed by wall clock timeout".into(),
            ResourceViolation::Memory => "capsule killed by memory limit".into(),
            ResourceViolation::MaxPids => "capsule killed by process limit".into(),
        };
    }

    if let Some(signal) = report.exit_signal {
        return format!("worker exited by signal {signal}");
    }

    if let Some(code) = report.exit_code {
        return format!("worker exited with code {code}");
    }

    "worker exited unexpectedly".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
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

    fn make_test_config(isolation: &str) -> crate::config::Config {
        crate::config::Config {
            daemon: crate::config::DaemonConfig {
                isolation: isolation.into(),
                worker_binary: Some("/usr/bin/zk-init".into()),
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
    fn test_capsule_spec_process_isolation() {
        let job = make_test_job();
        let config = make_test_config("process");
        let spec = capsule_spec_from_config(&config, &job);

        assert_eq!(spec.isolation, Isolation::Process);
        assert_eq!(
            spec.workspace.host_path,
            Some(PathBuf::from("/tmp/workspace"))
        );
        assert_eq!(spec.workspace.guest_path, PathBuf::from("/tmp/workspace"));
    }

    #[test]
    fn test_capsule_spec_namespace_isolation() {
        let job = make_test_job();
        let config = make_test_config("namespace");
        let spec = capsule_spec_from_config(&config, &job);

        assert_eq!(spec.isolation, Isolation::Namespace);
        assert_eq!(
            spec.workspace.host_path,
            Some(PathBuf::from("/tmp/workspace"))
        );
        assert_eq!(spec.workspace.guest_path, PathBuf::from("/workspace"));
        assert_eq!(spec.init_binary, Some(PathBuf::from("/usr/bin/zk-init")));
    }

    #[test]
    fn test_capsule_spec_capsule_alias_maps_to_process() {
        let job = make_test_job();
        let config = make_test_config("capsule");
        let spec = capsule_spec_from_config(&config, &job);

        assert_eq!(spec.isolation, Isolation::Process);
    }

    #[test]
    fn test_capsule_spec_limits_from_agent_profile() {
        let job = make_test_job();
        let config = make_test_config("process");
        let spec = capsule_spec_from_config(&config, &job);

        assert_eq!(spec.limits.timeout_sec, 600);
        assert_eq!(spec.limits.memory_mib, Some(512));
        assert_eq!(spec.limits.max_pids, Some(64));
    }

    #[test]
    fn test_capsule_spec_default_limits_no_profile() {
        let job = make_test_job();
        let mut config = make_test_config("process");
        config.agents.clear();
        let spec = capsule_spec_from_config(&config, &job);

        assert_eq!(
            spec.limits.timeout_sec,
            crate::config::DEFAULT_CAPSULE_TIMEOUT_SEC
        );
        assert!(spec.limits.memory_mib.is_none());
        assert!(spec.limits.max_pids.is_none());
    }

    #[test]
    fn test_build_worker_env_injects_zeptoclaw_binary() {
        let config = make_test_config("process");
        let env = build_worker_env(&config);

        assert_eq!(
            env.get("ZEPTOCLAW_BINARY").map(String::as_str),
            Some("/usr/bin/zeptoclaw")
        );
    }

    #[test]
    fn test_build_worker_env_no_zeptoclaw_binary() {
        let mut config = make_test_config("process");
        config.daemon.zeptoclaw_binary = None;
        let env = build_worker_env(&config);

        assert!(!env.contains_key("ZEPTOCLAW_BINARY"));
    }

    #[test]
    fn test_build_worker_env_includes_home_path() {
        let config = make_test_config("process");
        let env = build_worker_env(&config);

        assert!(env.contains_key("HOME") || env.contains_key("PATH"));
    }

    #[test]
    fn test_translate_guest_event_paths_rewrites_namespace_workspace() {
        let event = serde_json::json!({
            "type": "artifact_produced",
            "path": "/workspace/output.md",
        });

        let translated =
            translate_guest_event_paths(&event, Path::new("/workspace"), Path::new("/tmp/run/job"));

        assert_eq!(
            translated.get("path").and_then(|value| value.as_str()),
            Some("/tmp/run/job/output.md")
        );
    }
}
