//! Integration tests for the orchestrator engine.
//!
//! These tests exercise the full submit→plan→schedule→complete flow
//! without requiring a daemon or real LLM calls. They simulate
//! planner output and job completion events directly.

use std::collections::HashMap;
use std::io::Write;
use std::time::SystemTime;

use zeptopm::orchestrator::engine::OrchestratorEngine;
use zeptopm::orchestrator::planner;
use zeptopm::orchestrator::types::*;

/// Simulate a complete run: submit → planner completes → child jobs created → all complete.
#[test]
fn test_full_run_lifecycle() {
    let mut engine = OrchestratorEngine::new(4);

    // Step 1: Submit a run
    let run_id = engine.submit_run("Write a blog post about Rust".into());
    assert!(engine.store.get_run(&run_id).is_some());

    // Step 2: Dequeue and start the planner job
    let planner_job = engine.next_job().unwrap();
    assert_eq!(planner_job.role, "planner");
    engine.mark_running(&planner_job.job_id);

    let run = engine.store.get_run(&run_id).unwrap();
    assert_eq!(run.status, RunStatus::Running);

    // Step 3: Simulate planner completing with an execution plan artifact
    let plan = ExecutionPlan {
        jobs: vec![
            PlannedJob {
                local_id: "research".into(),
                role: "researcher".into(),
                profile_id: "researcher".into(),
                instruction: "Research Rust ecosystem".into(),
                depends_on: vec![],
            },
            PlannedJob {
                local_id: "write".into(),
                role: "writer".into(),
                profile_id: "writer".into(),
                instruction: "Write the blog post".into(),
                depends_on: vec!["research".into()],
            },
        ],
        channels: vec![],
    };

    // Validate the plan
    let errors = planner::validate_plan(&plan);
    assert!(errors.is_empty(), "plan validation failed: {:?}", errors);

    // Write plan artifact to a temp file
    let plan_json = serde_json::to_string(&plan).unwrap();
    let tmp_dir = std::env::temp_dir().join(format!("zeptopm_test_{}", run_id));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let plan_path = tmp_dir.join("plan.json");
    {
        let mut f = std::fs::File::create(&plan_path).unwrap();
        f.write_all(plan_json.as_bytes()).unwrap();
    }

    // Create artifact in store
    let plan_artifact_id = format!("art_{}_plan", run_id);
    engine.store.create_artifact(Artifact {
        artifact_id: plan_artifact_id.clone(),
        run_id: run_id.clone(),
        job_id: planner_job.job_id.clone(),
        kind: "json".into(),
        path: plan_path.clone(),
        summary: "execution plan".into(),
        created_at: SystemTime::now(),
    });

    // Mark planner completed
    engine.mark_completed(&planner_job.job_id, vec![plan_artifact_id]);

    // Step 4: Materialize the plan (simulating what the daemon does)
    let new_job_ids =
        planner::materialize_plan(&mut engine.store, &run_id, &planner_job.job_id, &plan);
    assert_eq!(new_job_ids.len(), 2);

    // Promote ready jobs
    for jid in &new_job_ids {
        if let Some(j) = engine.store.get_job(jid) {
            if j.status == JobStatus::Ready {
                engine.ready_queue.push_back(jid.clone());
            }
        }
    }

    // Step 5: Dequeue researcher job (writer should still be pending)
    let researcher_job = engine.next_job().unwrap();
    assert_eq!(researcher_job.role, "researcher");
    engine.mark_running(&researcher_job.job_id);

    // Writer should not be dequeued yet (depends on researcher)
    assert!(engine.next_job().is_none() || engine.ready_queue.is_empty());

    // Step 6: Complete researcher job
    engine.mark_completed(&researcher_job.job_id, vec![]);

    // Writer should now be promoted to ready
    let writer_job = engine.next_job().unwrap();
    assert_eq!(writer_job.role, "writer");
    engine.mark_running(&writer_job.job_id);

    // Step 7: Complete writer job
    engine.mark_completed(&writer_job.job_id, vec![]);

    // Step 8: Run should be completed
    let run = engine.store.get_run(&run_id).unwrap();
    assert_eq!(run.status, RunStatus::Completed);

    // Cleanup
    std::fs::remove_dir_all(&tmp_dir).ok();
}

/// Test that invalid plan is caught by validation.
#[test]
fn test_invalid_plan_rejected() {
    let plan = ExecutionPlan {
        jobs: vec![PlannedJob {
            local_id: "a".into(),
            role: "coder".into(),
            profile_id: "coder".into(),
            instruction: "Do something".into(),
            depends_on: vec!["missing_dep".into()],
        }],
        channels: vec![],
    };
    let errors = planner::validate_plan(&plan);
    assert!(!errors.is_empty());
    assert!(errors.iter().any(|e| e.contains("not found in plan")));
}

/// Test parallel jobs execute independently.
#[test]
fn test_parallel_execution() {
    let mut engine = OrchestratorEngine::new(4);
    let run_id = engine.submit_run("Parallel research".into());

    // Complete planner
    let planner = engine.next_job().unwrap();
    engine.mark_running(&planner.job_id);

    let plan = ExecutionPlan {
        jobs: vec![
            PlannedJob {
                local_id: "r1".into(),
                role: "researcher".into(),
                profile_id: "researcher".into(),
                instruction: "Research topic A".into(),
                depends_on: vec![],
            },
            PlannedJob {
                local_id: "r2".into(),
                role: "researcher".into(),
                profile_id: "researcher".into(),
                instruction: "Research topic B".into(),
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

    assert!(planner::validate_plan(&plan).is_empty());
    engine.mark_completed(&planner.job_id, vec![]);

    let new_ids = planner::materialize_plan(&mut engine.store, &run_id, &planner.job_id, &plan);
    for jid in &new_ids {
        if let Some(j) = engine.store.get_job(jid) {
            if j.status == JobStatus::Ready {
                engine.ready_queue.push_back(jid.clone());
            }
        }
    }

    // Both researchers should be dequeued (parallel)
    let j1 = engine.next_job().unwrap();
    let j2 = engine.next_job().unwrap();
    assert_eq!(j1.role, "researcher");
    assert_eq!(j2.role, "researcher");
    engine.mark_running(&j1.job_id);
    engine.mark_running(&j2.job_id);

    // Merge should NOT be available yet
    assert!(engine.next_job().is_none());

    // Complete both researchers
    engine.mark_completed(&j1.job_id, vec![]);
    engine.mark_completed(&j2.job_id, vec![]);

    // Now merge should be ready
    let merge = engine.next_job().unwrap();
    assert_eq!(merge.role, "analyst");
    engine.mark_running(&merge.job_id);
    engine.mark_completed(&merge.job_id, vec![]);

    let run = engine.store.get_run(&run_id).unwrap();
    assert_eq!(run.status, RunStatus::Completed);
}

/// Test run cancellation via the store.
#[test]
fn test_run_store_remove() {
    let mut store = zeptopm::orchestrator::store::RunStore::new();

    store.create_run(Run {
        run_id: "run_x".into(),
        task: "test".into(),
        status: RunStatus::Completed,
        created_at: SystemTime::now(),
        updated_at: SystemTime::now(),
        root_job_id: "j1".into(),
        final_artifact_ids: vec![],
        metadata: HashMap::new(),
    });
    store.create_job(Job {
        job_id: "j1".into(),
        run_id: "run_x".into(),
        parent_job_id: None,
        role: "planner".into(),
        status: JobStatus::Completed,
        instruction: "test".into(),
        input_artifact_ids: vec![],
        depends_on: vec![],
        children: vec![],
        profile_id: "planner".into(),
        workspace_dir: "/tmp".into(),
        attempt: 1,
        max_attempts: 3,
        created_at: SystemTime::now(),
        started_at: None,
        finished_at: None,
        output_artifact_ids: vec!["art1".into()],
        error: None,
        revision_round: 0,
    });
    store.create_artifact(Artifact {
        artifact_id: "art1".into(),
        run_id: "run_x".into(),
        job_id: "j1".into(),
        kind: "json".into(),
        path: "/tmp/nonexistent".into(),
        summary: "test".into(),
        created_at: SystemTime::now(),
    });

    // Remove should clean up everything
    let paths = store.remove_run("run_x");
    assert_eq!(paths.len(), 1);
    assert!(store.get_run("run_x").is_none());
    assert!(store.get_job("j1").is_none());
    assert!(store.get_artifact("art1").is_none());
}
