use std::collections::HashMap;
use std::time::SystemTime;

use crate::orchestrator::scheduler::gen_id;
use crate::orchestrator::store::RunStore;
use crate::orchestrator::types::*;

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
        };

        store.create_job(job);
        created.push(real_id);
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
        };
        let job_ids = materialize_plan(&mut store, "run_1", "planner_job", &plan);
        assert_eq!(job_ids.len(), 3);

        assert_eq!(store.get_job(&job_ids[0]).unwrap().status, JobStatus::Ready);
        assert_eq!(store.get_job(&job_ids[1]).unwrap().status, JobStatus::Ready);

        let merge = store.get_job(&job_ids[2]).unwrap();
        assert_eq!(merge.status, JobStatus::Pending);
        assert_eq!(merge.depends_on.len(), 2);
    }
}
