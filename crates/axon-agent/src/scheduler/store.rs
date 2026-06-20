use anyhow::Context;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::ErrorCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopCondition {
    pub condition_type: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub task: String,
    pub schedule_nl: String,
    pub cron_expr: String,
    pub status: String,
    pub created_by: String,
    pub platform: String,
    pub chat_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub max_runs: Option<i64>,
    pub run_count: i64,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    pub last_result: Option<String>,
    pub stop_condition: Option<StopCondition>,
    pub created_at: String,
}

pub struct JobStore {
    db: Arc<Pool<SqliteConnectionManager>>,
}

impl JobStore {
    pub fn new(db: Arc<Pool<SqliteConnectionManager>>) -> Self {
        JobStore { db }
    }

    pub fn create(&self, job: &Job) -> anyhow::Result<()> {
        let conn = self.db.get().context("DB pool")?;
        let stop = job
            .stop_condition
            .as_ref()
            .and_then(|s| serde_json::to_string(s).ok());
        conn.execute(
            "INSERT INTO jobs (id,name,task,schedule_nl,cron_expr,status,created_by,platform,chat_id,parent_run_id,max_runs,stop_condition,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            rusqlite::params![job.id,job.name,job.task,job.schedule_nl,job.cron_expr,job.status,job.created_by,job.platform,job.chat_id,job.parent_run_id,job.max_runs,stop,job.created_at])?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> anyhow::Result<Option<Job>> {
        let job = {
            let conn = self.db.get().context("DB pool")?;
            let res = match conn.query_row(
                "SELECT id,name,task,schedule_nl,cron_expr,status,created_by,platform,chat_id,parent_run_id,max_runs,run_count,last_run_at,next_run_at,last_result,stop_condition,created_at FROM jobs WHERE id=?1",
                rusqlite::params![id], |r| Ok(row_to_job(r)),
            ) { Ok(j) => Some(j), Err(rusqlite::Error::QueryReturnedNoRows) => None, Err(e) => return Err(e.into()) };
            res
        };
        Ok(job)
    }

    pub fn active(&self) -> anyhow::Result<Vec<Job>> {
        let conn = self.db.get().context("DB pool")?;
        let mut s = conn.prepare("SELECT id,name,task,schedule_nl,cron_expr,status,created_by,platform,chat_id,parent_run_id,max_runs,run_count,last_run_at,next_run_at,last_result,stop_condition,created_at FROM jobs WHERE status='active'")?;
        let jobs: Vec<Job> = s
            .query_map([], |r| Ok(row_to_job(r)))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(jobs)
    }

    pub fn all(&self) -> anyhow::Result<Vec<Job>> {
        let conn = self.db.get().context("DB pool")?;
        let mut s = conn.prepare("SELECT id,name,task,schedule_nl,cron_expr,status,created_by,platform,chat_id,parent_run_id,max_runs,run_count,last_run_at,next_run_at,last_result,stop_condition,created_at FROM jobs WHERE status != 'deleted' ORDER BY created_at DESC")?;
        let jobs: Vec<Job> = s
            .query_map([], |r| Ok(row_to_job(r)))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(jobs)
    }

    pub fn set_status(&self, id: &str, status: &str) -> anyhow::Result<()> {
        let conn = self.db.get().context("DB pool")?;
        conn.execute(
            "UPDATE jobs SET status=?1 WHERE id=?2",
            rusqlite::params![status, id],
        )?;
        Ok(())
    }

    pub fn record_run(&self, id: &str, result: &str) -> anyhow::Result<()> {
        let conn = self.db.get().context("DB pool")?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE jobs SET run_count=run_count+1,last_run_at=?1,last_result=?2 WHERE id=?3",
            rusqlite::params![now, result, id],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.set_status(id, "deleted")
    }

    pub fn get_last_active_messaging_context(&self) -> anyhow::Result<Option<(String, String)>> {
        let conn = self.db.get().context("DB pool")?;
        let res = match conn.query_row(
            "SELECT platform, session_id FROM runs WHERE platform NOT IN ('dashboard', 'api') AND platform IS NOT NULL AND session_id IS NOT NULL ORDER BY created_at DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        ) {
            Ok(c) => Some(c),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        Ok(res)
    }

    pub fn update(
        &self,
        id: &str,
        name: &str,
        task: &str,
        schedule_nl: &str,
        cron_expr: &str,
    ) -> anyhow::Result<()> {
        let conn = self.db.get().context("DB pool")?;
        conn.execute(
            "UPDATE jobs SET name=?1, task=?2, schedule_nl=?3, cron_expr=?4 WHERE id=?5",
            rusqlite::params![name, task, schedule_nl, cron_expr, id],
        )?;
        Ok(())
    }

    pub fn claim_fire_slot(&self, job_id: &str, slot_key: &str) -> anyhow::Result<bool> {
        let conn = self.db.get().context("DB pool")?;
        let _ = conn.execute(
            "DELETE FROM job_fire_locks WHERE claimed_at < datetime('now', '-14 days')",
            [],
        );
        match conn.execute(
            "INSERT INTO job_fire_locks (job_id, slot_key, claimed_at) VALUES (?1, ?2, datetime('now'))",
            rusqlite::params![job_id, slot_key],
        ) {
            Ok(_) => Ok(true),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == ErrorCode::ConstraintViolation =>
            {
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }
}

fn row_to_job(r: &rusqlite::Row<'_>) -> Job {
    let stop_json: Option<String> = r.get(15).unwrap_or(None);
    Job {
        id: r.get(0).unwrap_or_default(),
        name: r.get(1).unwrap_or_default(),
        task: r.get(2).unwrap_or_default(),
        schedule_nl: r.get(3).unwrap_or_default(),
        cron_expr: r.get(4).unwrap_or_default(),
        status: r.get(5).unwrap_or_default(),
        created_by: r.get(6).unwrap_or_default(),
        platform: r.get(7).unwrap_or_else(|_| "dashboard".to_string()),
        chat_id: r.get(8).unwrap_or(None),
        parent_run_id: r.get(9).unwrap_or(None),
        max_runs: r.get(10).unwrap_or(None),
        run_count: r.get(11).unwrap_or(0),
        last_run_at: r.get(12).unwrap_or(None),
        next_run_at: r.get(13).unwrap_or(None),
        last_result: r.get(14).unwrap_or(None),
        stop_condition: stop_json
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok()),
        created_at: r.get(16).unwrap_or_default(),
    }
}
