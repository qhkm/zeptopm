use std::collections::HashMap;
use std::time::SystemTime;

use crate::orchestrator::scheduler::gen_id;
use crate::orchestrator::store::RunStore;
use crate::orchestrator::types::*;

/// Validate an ExecutionPlan. Returns a list of errors (empty = valid).
pub fn validate_plan(plan: &ExecutionPlan) -> Vec<String> {
    let mut errors = Vec::new();

    if plan.jobs.is_empty() {
        errors.push("plan has no jobs".into());
        return errors;
    }

    let local_ids: std::collections::HashSet<&str> =
        plan.jobs.iter().map(|j| j.local_id.as_str()).collect();

    for (i, job) in plan.jobs.iter().enumerate() {
        if job.local_id.trim().is_empty() {
            errors.push(format!("job[{}]: local_id is empty", i));
        }
        if job.role.trim().is_empty() {
            errors.push(format!("job[{}] '{}': role is empty", i, job.local_id));
        }
        if job.profile_id.trim().is_empty() {
            errors.push(format!(
                "job[{}] '{}': profile_id is empty",
                i, job.local_id
            ));
        }
        if job.instruction.trim().is_empty() {
            errors.push(format!(
                "job[{}] '{}': instruction is empty",
                i, job.local_id
            ));
        }
        for dep in &job.depends_on {
            if !local_ids.contains(dep.as_str()) {
                errors.push(format!(
                    "job[{}] '{}': depends_on '{}' not found in plan",
                    i, job.local_id, dep
                ));
            }
            if dep == &job.local_id {
                errors.push(format!("job[{}] '{}': depends on itself", i, job.local_id));
            }
        }
    }

    // Check for duplicate local_ids
    let mut seen = std::collections::HashSet::new();
    for job in &plan.jobs {
        if !seen.insert(&job.local_id) {
            errors.push(format!("duplicate local_id: '{}'", job.local_id));
        }
    }

    // Validate channels
    let mut seen_channels = std::collections::HashSet::new();
    for (i, ch) in plan.channels.iter().enumerate() {
        if !seen_channels.insert(&ch.channel_id) {
            errors.push(format!("duplicate channel_id: '{}'", ch.channel_id));
        }
        if ch.participants.is_empty() {
            errors.push(format!("channel[{}] '{}': no participants", i, ch.channel_id));
        }
        if ch.mode == ChannelMode::TurnBased && ch.participants.len() < 2 {
            errors.push(format!(
                "channel[{}] '{}': TurnBased requires at least 2 participants",
                i, ch.channel_id
            ));
        }
        for p in &ch.participants {
            if !local_ids.contains(p.as_str()) {
                errors.push(format!(
                    "channel[{}] '{}': participant '{}' not found in plan jobs",
                    i, ch.channel_id, p
                ));
            }
        }
    }

    errors
}

/// Turn an ExecutionPlan into real Jobs in the store.
/// Returns the list of created job IDs in plan order.
pub fn materialize_plan(
    store: &mut RunStore,
    run_id: &str,
    parent_job_id: &str,
    plan: &ExecutionPlan,
) -> Vec<JobId> {
    let now = SystemTime::now();

    // Map local_id -> real job_id
    let mut id_map: HashMap<String, JobId> = HashMap::new();
    for spec in &plan.jobs {
        id_map.insert(spec.local_id.clone(), gen_id("job"));
    }

    let mut created = Vec::new();
    for spec in &plan.jobs {
        let real_id = id_map[&spec.local_id].clone();
        let real_deps: Vec<JobId> = spec
            .depends_on
            .iter()
            .filter_map(|local| id_map.get(local).cloned())
            .collect();

        let status = if real_deps.is_empty() {
            JobStatus::Ready
        } else {
            JobStatus::Pending
        };

        let job = Job {
            job_id: real_id.clone(),
            run_id: run_id.into(),
            parent_job_id: Some(parent_job_id.into()),
            role: spec.role.clone(),
            status,
            instruction: spec.instruction.clone(),
            input_artifact_ids: vec![],
            depends_on: real_deps,
            children: vec![],
            profile_id: spec.profile_id.clone(),
            workspace_dir: resolve_workspace(run_id, &real_id),
            attempt: 0,
            max_attempts: 3,
            created_at: now,
            started_at: None,
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
            revision_round: 0,
        };

        store.create_job(job);
        created.push(real_id);
    }

    // Materialize channels — translate local_ids to real job IDs
    for planned_ch in &plan.channels {
        let real_participants: Vec<JobId> = planned_ch
            .participants
            .iter()
            .filter_map(|local| id_map.get(local).cloned())
            .collect();

        let channel = Channel {
            channel_id: planned_ch.channel_id.clone(),
            run_id: run_id.into(),
            participants: real_participants,
            mode: planned_ch.mode.clone(),
            max_rounds: planned_ch.max_rounds,
            on_peer_failure: planned_ch.on_peer_failure.clone(),
            current_round: 0,
            current_speaker_idx: 0,
            active: false,
            history: vec![],
            initial_message: planned_ch.initial_message.clone(),
        };
        store.create_channel(channel);
    }

    created
}

pub fn resolve_workspace(run_id: &str, job_id: &str) -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".zeptopm")
        .join("runs")
        .join(run_id)
        .join(job_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::store::RunStore;

    #[test]
    fn test_materialize_plan_creates_jobs() {
        let mut store = RunStore::new();
        let plan = ExecutionPlan {
            jobs: vec![
                PlannedJob {
                    local_id: "research".into(),
                    role: "researcher".into(),
                    profile_id: "researcher".into(),
                    instruction: "Research AI in Malaysia".into(),
                    depends_on: vec![],
                },
                PlannedJob {
                    local_id: "write".into(),
                    role: "writer".into(),
                    profile_id: "writer".into(),
                    instruction: "Write report".into(),
                    depends_on: vec!["research".into()],
                },
            ],
            channels: vec![],
        };
        let job_ids = materialize_plan(&mut store, "run_1", "planner_job", &plan);
        assert_eq!(job_ids.len(), 2);

        let j1 = store.get_job(&job_ids[0]).unwrap();
        assert_eq!(j1.status, JobStatus::Ready);
        assert_eq!(j1.role, "researcher");
        assert!(j1.depends_on.is_empty());

        let j2 = store.get_job(&job_ids[1]).unwrap();
        assert_eq!(j2.status, JobStatus::Pending);
        assert_eq!(j2.role, "writer");
        assert_eq!(j2.depends_on.len(), 1);
        assert_eq!(j2.depends_on[0], job_ids[0]);
    }

    #[test]
    fn test_materialize_plan_parallel_jobs() {
        let mut store = RunStore::new();
        let plan = ExecutionPlan {
            jobs: vec![
                PlannedJob {
                    local_id: "r1".into(),
                    role: "researcher".into(),
                    profile_id: "researcher".into(),
                    instruction: "Research A".into(),
                    depends_on: vec![],
                },
                PlannedJob {
                    local_id: "r2".into(),
                    role: "researcher".into(),
                    profile_id: "researcher".into(),
                    instruction: "Research B".into(),
                    depends_on: vec![],
                },
                PlannedJob {
                    local_id: "merge".into(),
                    role: "analyst".into(),
                    profile_id: "analyst".into(),
                    instruction: "Merge findings".into(),
                    depends_on: vec!["r1".into(), "r2".into()],
                },
            ],
            channels: vec![],
        };
        let job_ids = materialize_plan(&mut store, "run_1", "planner_job", &plan);
        assert_eq!(job_ids.len(), 3);

        assert_eq!(store.get_job(&job_ids[0]).unwrap().status, JobStatus::Ready);
        assert_eq!(store.get_job(&job_ids[1]).unwrap().status, JobStatus::Ready);

        let merge = store.get_job(&job_ids[2]).unwrap();
        assert_eq!(merge.status, JobStatus::Pending);
        assert_eq!(merge.depends_on.len(), 2);
    }

    #[test]
    fn test_validate_valid_plan() {
        let plan = ExecutionPlan {
            jobs: vec![PlannedJob {
                local_id: "a".into(),
                role: "coder".into(),
                profile_id: "coder".into(),
                instruction: "Write code".into(),
                depends_on: vec![],
            }],
            channels: vec![],
        };
        assert!(validate_plan(&plan).is_empty());
    }

    #[test]
    fn test_validate_empty_plan() {
        let plan = ExecutionPlan {
            jobs: vec![],
            channels: vec![],
        };
        let errors = validate_plan(&plan);
        assert!(errors.iter().any(|e| e.contains("no jobs")));
    }

    #[test]
    fn test_validate_missing_dep() {
        let plan = ExecutionPlan {
            jobs: vec![PlannedJob {
                local_id: "a".into(),
                role: "coder".into(),
                profile_id: "coder".into(),
                instruction: "Write code".into(),
                depends_on: vec!["nonexistent".into()],
            }],
            channels: vec![],
        };
        let errors = validate_plan(&plan);
        assert!(errors.iter().any(|e| e.contains("not found in plan")));
    }

    #[test]
    fn test_validate_self_dep() {
        let plan = ExecutionPlan {
            jobs: vec![PlannedJob {
                local_id: "a".into(),
                role: "coder".into(),
                profile_id: "coder".into(),
                instruction: "Write code".into(),
                depends_on: vec!["a".into()],
            }],
            channels: vec![],
        };
        let errors = validate_plan(&plan);
        assert!(errors.iter().any(|e| e.contains("depends on itself")));
    }

    #[test]
    fn test_validate_empty_fields() {
        let plan = ExecutionPlan {
            jobs: vec![PlannedJob {
                local_id: "a".into(),
                role: "".into(),
                profile_id: "".into(),
                instruction: "".into(),
                depends_on: vec![],
            }],
            channels: vec![],
        };
        let errors = validate_plan(&plan);
        assert!(errors.len() >= 3); // role, profile_id, instruction
    }

    #[test]
    fn test_validate_plan_with_valid_channels() {
        let plan = ExecutionPlan {
            jobs: vec![
                PlannedJob {
                    local_id: "writer".into(),
                    role: "writer".into(),
                    profile_id: "writer".into(),
                    instruction: "Write".into(),
                    depends_on: vec![],
                },
                PlannedJob {
                    local_id: "reviewer".into(),
                    role: "reviewer".into(),
                    profile_id: "reviewer".into(),
                    instruction: "Review".into(),
                    depends_on: vec![],
                },
            ],
            channels: vec![PlannedChannel {
                channel_id: "draft-review".into(),
                participants: vec!["writer".into(), "reviewer".into()],
                mode: ChannelMode::TurnBased,
                max_rounds: Some(3),
                on_peer_failure: PeerFailure::KillAll,
                initial_message: Some("Write a blog post".into()),
            }],
        };
        assert!(validate_plan(&plan).is_empty());
    }

    #[test]
    fn test_validate_channel_unknown_participant() {
        let plan = ExecutionPlan {
            jobs: vec![PlannedJob {
                local_id: "writer".into(),
                role: "writer".into(),
                profile_id: "writer".into(),
                instruction: "Write".into(),
                depends_on: vec![],
            }],
            channels: vec![PlannedChannel {
                channel_id: "ch1".into(),
                participants: vec!["writer".into(), "ghost".into()],
                mode: ChannelMode::TurnBased,
                max_rounds: None,
                on_peer_failure: PeerFailure::KillAll,
                initial_message: None,
            }],
        };
        let errors = validate_plan(&plan);
        assert!(errors.iter().any(|e| e.contains("ghost") && e.contains("not found")));
    }

    #[test]
    fn test_validate_channel_duplicate_id() {
        let plan = ExecutionPlan {
            jobs: vec![PlannedJob {
                local_id: "a".into(),
                role: "r".into(),
                profile_id: "r".into(),
                instruction: "do".into(),
                depends_on: vec![],
            }],
            channels: vec![
                PlannedChannel {
                    channel_id: "ch1".into(),
                    participants: vec!["a".into()],
                    mode: ChannelMode::TurnBased,
                    max_rounds: None,
                    on_peer_failure: PeerFailure::KillAll,
                    initial_message: None,
                },
                PlannedChannel {
                    channel_id: "ch1".into(),
                    participants: vec!["a".into()],
                    mode: ChannelMode::Stream,
                    max_rounds: None,
                    on_peer_failure: PeerFailure::Continue,
                    initial_message: None,
                },
            ],
        };
        let errors = validate_plan(&plan);
        assert!(errors.iter().any(|e| e.contains("duplicate") && e.contains("ch1")));
    }

    #[test]
    fn test_validate_channel_empty_participants() {
        let plan = ExecutionPlan {
            jobs: vec![PlannedJob {
                local_id: "a".into(),
                role: "r".into(),
                profile_id: "r".into(),
                instruction: "do".into(),
                depends_on: vec![],
            }],
            channels: vec![PlannedChannel {
                channel_id: "ch1".into(),
                participants: vec![],
                mode: ChannelMode::TurnBased,
                max_rounds: None,
                on_peer_failure: PeerFailure::KillAll,
                initial_message: None,
            }],
        };
        let errors = validate_plan(&plan);
        assert!(errors.iter().any(|e| e.contains("no participants")));
    }

    #[test]
    fn test_validate_turn_based_needs_two_participants() {
        let plan = ExecutionPlan {
            jobs: vec![PlannedJob {
                local_id: "a".into(),
                role: "r".into(),
                profile_id: "r".into(),
                instruction: "do".into(),
                depends_on: vec![],
            }],
            channels: vec![PlannedChannel {
                channel_id: "ch1".into(),
                participants: vec!["a".into()],
                mode: ChannelMode::TurnBased,
                max_rounds: None,
                on_peer_failure: PeerFailure::KillAll,
                initial_message: None,
            }],
        };
        let errors = validate_plan(&plan);
        assert!(errors.iter().any(|e| e.contains("TurnBased") && e.contains("at least 2")));
    }

    #[test]
    fn test_materialize_plan_creates_channels() {
        let mut store = RunStore::new();
        let plan = ExecutionPlan {
            jobs: vec![
                PlannedJob {
                    local_id: "writer".into(),
                    role: "writer".into(),
                    profile_id: "writer".into(),
                    instruction: "Write draft".into(),
                    depends_on: vec![],
                },
                PlannedJob {
                    local_id: "reviewer".into(),
                    role: "reviewer".into(),
                    profile_id: "reviewer".into(),
                    instruction: "Review draft".into(),
                    depends_on: vec![],
                },
            ],
            channels: vec![PlannedChannel {
                channel_id: "draft-review".into(),
                participants: vec!["writer".into(), "reviewer".into()],
                mode: ChannelMode::TurnBased,
                max_rounds: Some(3),
                on_peer_failure: PeerFailure::KillAll,
                initial_message: Some("Write a blog post about Rust".into()),
            }],
        };

        let job_ids = materialize_plan(&mut store, "run_1", "planner_job", &plan);
        assert_eq!(job_ids.len(), 2);

        // Channel should be created with real job IDs (not local_ids)
        let channels = store.list_run_channels("run_1");
        assert_eq!(channels.len(), 1);
        let ch = channels[0];
        assert_eq!(ch.channel_id, "draft-review");
        assert_eq!(ch.participants.len(), 2);
        assert_eq!(ch.participants[0], job_ids[0]); // writer's real ID
        assert_eq!(ch.participants[1], job_ids[1]); // reviewer's real ID
        assert_eq!(ch.mode, ChannelMode::TurnBased);
        assert_eq!(ch.max_rounds, Some(3));
        assert!(!ch.active); // not active until participants are Running
    }

    #[test]
    fn test_materialize_plan_no_channels() {
        let mut store = RunStore::new();
        let plan = ExecutionPlan {
            jobs: vec![PlannedJob {
                local_id: "a".into(),
                role: "r".into(),
                profile_id: "r".into(),
                instruction: "do".into(),
                depends_on: vec![],
            }],
            channels: vec![],
        };
        let _ = materialize_plan(&mut store, "run_1", "p", &plan);
        assert!(store.list_run_channels("run_1").is_empty());
    }
}
