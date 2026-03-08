//! SQLite persistence sidecar for the orchestrator.
//!
//! The engine keeps its in-memory RunStore. This module persists
//! mutations to SQLite and hydrates the store on startup.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use rusqlite::{params, Connection};

use crate::orchestrator::types::*;

const SCHEMA_VERSION: u32 = 1;

pub struct SqlitePersistence {
    conn: Connection,
}

impl SqlitePersistence {
    /// Open (or create) the SQLite database at the given path.
    pub fn new(path: &str) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Ok(Self { conn })
    }

    /// Create a temporary in-memory database (for tests).
    pub fn new_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        Ok(Self { conn })
    }

    /// Create the schema if it doesn't exist.
    pub fn init_schema(&self) -> Result<(), rusqlite::Error> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS runs (
                run_id TEXT PRIMARY KEY,
                task TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at REAL NOT NULL,
                updated_at REAL NOT NULL,
                root_job_id TEXT NOT NULL,
                final_artifact_ids TEXT NOT NULL DEFAULT '[]',
                metadata TEXT NOT NULL DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS jobs (
                job_id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                parent_job_id TEXT,
                role TEXT NOT NULL,
                status TEXT NOT NULL,
                instruction TEXT NOT NULL,
                input_artifact_ids TEXT NOT NULL DEFAULT '[]',
                depends_on TEXT NOT NULL DEFAULT '[]',
                children TEXT NOT NULL DEFAULT '[]',
                profile_id TEXT NOT NULL,
                workspace_dir TEXT NOT NULL,
                attempt INTEGER NOT NULL DEFAULT 0,
                max_attempts INTEGER NOT NULL DEFAULT 3,
                revision_round INTEGER NOT NULL DEFAULT 0,
                created_at REAL NOT NULL,
                started_at REAL,
                finished_at REAL,
                output_artifact_ids TEXT NOT NULL DEFAULT '[]',
                error TEXT
            );

            CREATE TABLE IF NOT EXISTS artifacts (
                artifact_id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                job_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                path TEXT NOT NULL,
                summary TEXT NOT NULL,
                created_at REAL NOT NULL
            );"
        )?;

        // Set schema version if not exists
        let count: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM schema_version", [], |r| r.get(0)
        )?;
        if count == 0 {
            self.conn.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                params![SCHEMA_VERSION],
            )?;
        }

        Ok(())
    }

    // --- Persist operations ---

    pub fn persist_run(&self, run: &Run) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT OR REPLACE INTO runs
             (run_id, task, status, created_at, updated_at, root_job_id, final_artifact_ids, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run.run_id,
                run.task,
                format!("{:?}", run.status),
                to_epoch(run.created_at),
                to_epoch(run.updated_at),
                run.root_job_id,
                serde_json::to_string(&run.final_artifact_ids).unwrap_or_default(),
                serde_json::to_string(&run.metadata).unwrap_or_default(),
            ],
        )?;
        Ok(())
    }

    pub fn persist_job(&self, job: &Job) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT OR REPLACE INTO jobs
             (job_id, run_id, parent_job_id, role, status, instruction,
              input_artifact_ids, depends_on, children, profile_id, workspace_dir,
              attempt, max_attempts, revision_round, created_at, started_at, finished_at,
              output_artifact_ids, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                job.job_id,
                job.run_id,
                job.parent_job_id,
                job.role,
                format!("{:?}", job.status),
                job.instruction,
                serde_json::to_string(&job.input_artifact_ids).unwrap_or_default(),
                serde_json::to_string(&job.depends_on).unwrap_or_default(),
                serde_json::to_string(&job.children).unwrap_or_default(),
                job.profile_id,
                job.workspace_dir.to_string_lossy().to_string(),
                job.attempt,
                job.max_attempts,
                job.revision_round,
                to_epoch(job.created_at),
                job.started_at.map(to_epoch),
                job.finished_at.map(to_epoch),
                serde_json::to_string(&job.output_artifact_ids).unwrap_or_default(),
                job.error,
            ],
        )?;
        Ok(())
    }

    pub fn persist_artifact(&self, artifact: &Artifact) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT OR REPLACE INTO artifacts
             (artifact_id, run_id, job_id, kind, path, summary, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                artifact.artifact_id,
                artifact.run_id,
                artifact.job_id,
                artifact.kind,
                artifact.path.to_string_lossy().to_string(),
                artifact.summary,
                to_epoch(artifact.created_at),
            ],
        )?;
        Ok(())
    }

    // --- Load operations (for hydration on startup) ---

    pub fn load_runs(&self) -> Result<Vec<Run>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, task, status, created_at, updated_at, root_job_id, final_artifact_ids, metadata
             FROM runs"
        )?;
        let runs = stmt.query_map([], |row| {
            Ok(Run {
                run_id: row.get(0)?,
                task: row.get(1)?,
                status: parse_run_status(&row.get::<_, String>(2)?),
                created_at: from_epoch(row.get(3)?),
                updated_at: from_epoch(row.get(4)?),
                root_job_id: row.get(5)?,
                final_artifact_ids: parse_json_vec(&row.get::<_, String>(6)?),
                metadata: parse_json_map(&row.get::<_, String>(7)?),
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(runs)
    }

    pub fn load_jobs(&self) -> Result<Vec<Job>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT job_id, run_id, parent_job_id, role, status, instruction,
                    input_artifact_ids, depends_on, children, profile_id, workspace_dir,
                    attempt, max_attempts, revision_round, created_at, started_at, finished_at,
                    output_artifact_ids, error
             FROM jobs"
        )?;
        let jobs = stmt.query_map([], |row| {
            Ok(Job {
                job_id: row.get(0)?,
                run_id: row.get(1)?,
                parent_job_id: row.get(2)?,
                role: row.get(3)?,
                status: parse_job_status(&row.get::<_, String>(4)?),
                instruction: row.get(5)?,
                input_artifact_ids: parse_json_vec(&row.get::<_, String>(6)?),
                depends_on: parse_json_vec(&row.get::<_, String>(7)?),
                children: parse_json_vec(&row.get::<_, String>(8)?),
                profile_id: row.get(9)?,
                workspace_dir: PathBuf::from(row.get::<_, String>(10)?),
                attempt: row.get(11)?,
                max_attempts: row.get(12)?,
                revision_round: row.get(13)?,
                created_at: from_epoch(row.get(14)?),
                started_at: row.get::<_, Option<f64>>(15)?.map(from_epoch),
                finished_at: row.get::<_, Option<f64>>(16)?.map(from_epoch),
                output_artifact_ids: parse_json_vec(&row.get::<_, String>(17)?),
                error: row.get(18)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(jobs)
    }

    pub fn load_artifacts(&self) -> Result<Vec<Artifact>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT artifact_id, run_id, job_id, kind, path, summary, created_at
             FROM artifacts"
        )?;
        let artifacts = stmt.query_map([], |row| {
            Ok(Artifact {
                artifact_id: row.get(0)?,
                run_id: row.get(1)?,
                job_id: row.get(2)?,
                kind: row.get(3)?,
                path: PathBuf::from(row.get::<_, String>(4)?),
                summary: row.get(5)?,
                created_at: from_epoch(row.get(6)?),
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(artifacts)
    }

    /// Persist all jobs and the run for a given run_id (bulk persist after mutations).
    pub fn persist_run_state(
        &self,
        store: &crate::orchestrator::store::RunStore,
        run_id: &str,
    ) -> Result<(), rusqlite::Error> {
        if let Some(run) = store.get_run(run_id) {
            self.persist_run(run)?;
        }
        for job in store.list_run_jobs(run_id) {
            self.persist_job(job)?;
        }
        Ok(())
    }

    /// Delete a run and all its jobs and artifacts from SQLite.
    pub fn delete_run(&self, run_id: &str) -> Result<(), rusqlite::Error> {
        self.conn.execute("DELETE FROM artifacts WHERE run_id = ?1", params![run_id])?;
        self.conn.execute("DELETE FROM jobs WHERE run_id = ?1", params![run_id])?;
        self.conn.execute("DELETE FROM runs WHERE run_id = ?1", params![run_id])?;
        Ok(())
    }
}

// --- Serialization helpers ---

fn to_epoch(t: SystemTime) -> f64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn from_epoch(epoch: f64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs_f64(epoch)
}

fn parse_run_status(s: &str) -> RunStatus {
    match s {
        "Pending" => RunStatus::Pending,
        "Running" => RunStatus::Running,
        "Completed" => RunStatus::Completed,
        "Failed" => RunStatus::Failed,
        "Cancelled" => RunStatus::Cancelled,
        _ => RunStatus::Pending,
    }
}

fn parse_job_status(s: &str) -> JobStatus {
    match s {
        "Pending" => JobStatus::Pending,
        "Ready" => JobStatus::Ready,
        "Running" => JobStatus::Running,
        "Completed" => JobStatus::Completed,
        "Failed" => JobStatus::Failed,
        "Cancelled" => JobStatus::Cancelled,
        _ => JobStatus::Pending,
    }
}

fn parse_json_vec(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

fn parse_json_map(s: &str) -> HashMap<String, String> {
    serde_json::from_str(s).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::store::RunStore;

    fn make_test_run() -> Run {
        Run {
            run_id: "run_1".into(),
            task: "test task".into(),
            status: RunStatus::Running,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            root_job_id: "job_root".into(),
            final_artifact_ids: vec![],
            metadata: HashMap::new(),
        }
    }

    fn make_test_job(job_id: &str, run_id: &str) -> Job {
        Job {
            job_id: job_id.into(),
            run_id: run_id.into(),
            parent_job_id: None,
            role: "coder".into(),
            status: JobStatus::Running,
            instruction: "write code".into(),
            input_artifact_ids: vec![],
            depends_on: vec![],
            children: vec![],
            profile_id: "coder".into(),
            workspace_dir: PathBuf::from("/tmp/test"),
            attempt: 1,
            max_attempts: 3,
            revision_round: 0,
            created_at: SystemTime::now(),
            started_at: Some(SystemTime::now()),
            finished_at: None,
            output_artifact_ids: vec![],
            error: None,
        }
    }

    #[test]
    fn test_init_schema() {
        let db = SqlitePersistence::new_memory().unwrap();
        db.init_schema().unwrap();
        // Calling again should be idempotent
        db.init_schema().unwrap();
    }

    #[test]
    fn test_persist_and_load_run() {
        let db = SqlitePersistence::new_memory().unwrap();
        db.init_schema().unwrap();

        let run = make_test_run();
        db.persist_run(&run).unwrap();

        let runs = db.load_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "run_1");
        assert_eq!(runs[0].task, "test task");
        assert_eq!(runs[0].status, RunStatus::Running);
    }

    #[test]
    fn test_persist_and_load_job() {
        let db = SqlitePersistence::new_memory().unwrap();
        db.init_schema().unwrap();

        let mut job = make_test_job("job_1", "run_1");
        job.depends_on = vec!["job_0".into()];
        job.input_artifact_ids = vec!["art_0".into()];
        job.revision_round = 2;
        db.persist_job(&job).unwrap();

        let jobs = db.load_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_id, "job_1");
        assert_eq!(jobs[0].role, "coder");
        assert_eq!(jobs[0].depends_on, vec!["job_0".to_string()]);
        assert_eq!(jobs[0].input_artifact_ids, vec!["art_0".to_string()]);
        assert_eq!(jobs[0].revision_round, 2);
        assert!(jobs[0].started_at.is_some());
    }

    #[test]
    fn test_persist_and_load_artifact() {
        let db = SqlitePersistence::new_memory().unwrap();
        db.init_schema().unwrap();

        let art = Artifact {
            artifact_id: "art_1".into(),
            run_id: "run_1".into(),
            job_id: "job_1".into(),
            kind: "markdown".into(),
            path: PathBuf::from("/tmp/output.md"),
            summary: "test output".into(),
            created_at: SystemTime::now(),
        };
        db.persist_artifact(&art).unwrap();

        let artifacts = db.load_artifacts().unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_id, "art_1");
        assert_eq!(artifacts[0].kind, "markdown");
    }

    #[test]
    fn test_upsert_updates_existing() {
        let db = SqlitePersistence::new_memory().unwrap();
        db.init_schema().unwrap();

        let mut run = make_test_run();
        db.persist_run(&run).unwrap();

        run.status = RunStatus::Completed;
        run.task = "updated task".into();
        db.persist_run(&run).unwrap();

        let runs = db.load_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Completed);
        assert_eq!(runs[0].task, "updated task");
    }

    #[test]
    fn test_hydrate_run_store() {
        let db = SqlitePersistence::new_memory().unwrap();
        db.init_schema().unwrap();

        // Persist some data
        db.persist_run(&make_test_run()).unwrap();
        db.persist_job(&make_test_job("job_1", "run_1")).unwrap();
        db.persist_job(&make_test_job("job_2", "run_1")).unwrap();
        db.persist_artifact(&Artifact {
            artifact_id: "art_1".into(),
            run_id: "run_1".into(),
            job_id: "job_1".into(),
            kind: "json".into(),
            path: PathBuf::from("/tmp/out.json"),
            summary: "output".into(),
            created_at: SystemTime::now(),
        }).unwrap();

        // Hydrate into a fresh RunStore
        let mut store = RunStore::new();
        for run in db.load_runs().unwrap() {
            store.create_run(run);
        }
        for job in db.load_jobs().unwrap() {
            store.create_job(job);
        }
        for artifact in db.load_artifacts().unwrap() {
            store.create_artifact(artifact);
        }

        assert!(store.get_run("run_1").is_some());
        assert_eq!(store.list_run_jobs("run_1").len(), 2);
        assert!(store.get_artifact("art_1").is_some());
    }

    #[test]
    fn test_persist_run_state_bulk() {
        let db = SqlitePersistence::new_memory().unwrap();
        db.init_schema().unwrap();

        // Set up an in-memory store with some data
        let mut store = RunStore::new();
        store.create_run(make_test_run());
        store.create_job(make_test_job("job_1", "run_1"));
        store.create_job(make_test_job("job_2", "run_1"));

        // Bulk persist
        db.persist_run_state(&store, "run_1").unwrap();

        // Verify
        assert_eq!(db.load_runs().unwrap().len(), 1);
        assert_eq!(db.load_jobs().unwrap().len(), 2);
    }

    #[test]
    fn test_resume_finds_incomplete_runs() {
        let db = SqlitePersistence::new_memory().unwrap();
        db.init_schema().unwrap();

        // A completed run
        let mut run1 = make_test_run();
        run1.run_id = "run_done".into();
        run1.status = RunStatus::Completed;
        db.persist_run(&run1).unwrap();

        // A running run with a running job and a ready job
        let mut run2 = make_test_run();
        run2.run_id = "run_active".into();
        run2.status = RunStatus::Running;
        db.persist_run(&run2).unwrap();

        let mut running_job = make_test_job("j1", "run_active");
        running_job.status = JobStatus::Running;
        db.persist_job(&running_job).unwrap();

        let mut ready_job = make_test_job("j2", "run_active");
        ready_job.status = JobStatus::Ready;
        db.persist_job(&ready_job).unwrap();

        // Hydrate and check
        let runs = db.load_runs().unwrap();
        let incomplete: Vec<_> = runs.iter()
            .filter(|r| r.status == RunStatus::Running || r.status == RunStatus::Pending)
            .collect();
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].run_id, "run_active");

        let jobs = db.load_jobs().unwrap();
        let running_jobs: Vec<_> = jobs.iter()
            .filter(|j| j.status == JobStatus::Running)
            .collect();
        assert_eq!(running_jobs.len(), 1);
        assert_eq!(running_jobs[0].job_id, "j1");
    }
}
