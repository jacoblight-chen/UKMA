//! Background maintenance jobs (§27.3, PROP-09, OPS-001).
//!
//! Every job carries a tenant partition, an idempotency key, a lease with a
//! fencing token, a retry budget with backoff, a dead-letter state and a
//! checkpoint so a restart resumes instead of repeating work.

use std::collections::BTreeMap;

use crate::domain::{MemoryError, Result};

/// The nine registered maintenance jobs (§27.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JobKind {
    /// Expire and archive records.
    TtlSweep,
    /// Refresh FSRS states.
    FsrsRefresh,
    /// Scan for redundant content that should be summarised.
    EntropyCandidateScan,
    /// Run the nine-stage consolidation pipeline.
    Consolidation,
    /// Close expired reconsolidation windows.
    ReconsolidationClose,
    /// Warm the translation cache.
    TranslationCacheWarm,
    /// Rebuild a vector index generation.
    IndexRebuild,
    /// Revalidate stale knowledge references.
    StaleKnowledgeRefScan,
    /// Review procedures.
    ProcedureReview,
}

impl JobKind {
    /// Every job kind.
    pub const ALL: [JobKind; 9] = [
        JobKind::TtlSweep,
        JobKind::FsrsRefresh,
        JobKind::EntropyCandidateScan,
        JobKind::Consolidation,
        JobKind::ReconsolidationClose,
        JobKind::TranslationCacheWarm,
        JobKind::IndexRebuild,
        JobKind::StaleKnowledgeRefScan,
        JobKind::ProcedureReview,
    ];

    /// Stable token.
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::TtlSweep => "ttl_sweep",
            JobKind::FsrsRefresh => "fsrs_refresh",
            JobKind::EntropyCandidateScan => "entropy_candidate_scan",
            JobKind::Consolidation => "consolidation",
            JobKind::ReconsolidationClose => "reconsolidation_close",
            JobKind::TranslationCacheWarm => "translation_cache_warm",
            JobKind::IndexRebuild => "index_rebuild",
            JobKind::StaleKnowledgeRefScan => "stale_knowledge_ref_scan",
            JobKind::ProcedureReview => "procedure_review",
        }
    }

    /// Interval of the periodic run, when the job is periodic.
    pub fn interval_seconds(self) -> Option<i64> {
        match self {
            JobKind::FsrsRefresh => Some(60 * 60),
            _ => None,
        }
    }
}

/// Job state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    /// Waiting for a lease.
    Pending,
    /// Leased by a worker.
    Running,
    /// Finished successfully.
    Succeeded,
    /// Failed but scheduled for another attempt.
    RetryScheduled,
    /// Out of attempts.
    DeadLettered,
}

impl JobStatus {
    /// Stable token.
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Succeeded => "succeeded",
            JobStatus::RetryScheduled => "retry_scheduled",
            JobStatus::DeadLettered => "dead_lettered",
        }
    }
}

/// Exclusive lease on a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    /// Worker holding the lease.
    pub owner: String,
    /// Instant the lease lapses.
    pub expires_at: i64,
    /// Monotonic fencing token; a stale token cannot complete the job.
    pub fencing_token: u64,
}

/// One maintenance job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Job id.
    pub id: String,
    /// Tenant partition.
    pub tenant_id: String,
    /// Job kind.
    pub kind: JobKind,
    /// Idempotency key.
    pub idempotency_key: String,
    /// Attempts made so far.
    pub attempts: u32,
    /// State.
    pub status: JobStatus,
    /// Checkpoint of the last successful step.
    pub checkpoint: Option<String>,
    /// Current lease.
    pub lease: Option<Lease>,
    /// Last safe error message.
    pub last_error: Option<String>,
    /// Instant the next attempt may start.
    pub next_attempt_at: Option<i64>,
}

/// Attempts allowed before dead-lettering.
pub const MAX_ATTEMPTS: u32 = 5;
/// Lease length in seconds.
pub const LEASE_SECONDS: i64 = 60;
/// Base backoff in seconds; the delay doubles per attempt.
pub const BACKOFF_BASE_SECONDS: i64 = 5;

/// Job store and lease manager.
#[derive(Debug, Default)]
pub struct Scheduler {
    jobs: BTreeMap<String, Job>,
    by_key: BTreeMap<String, String>,
    sequence: u64,
    fencing: u64,
}

impl Scheduler {
    /// Empty scheduler.
    pub fn new() -> Self {
        Self::default()
    }

    /// All jobs.
    pub fn jobs(&self) -> impl Iterator<Item = &Job> {
        self.jobs.values()
    }

    /// One job.
    pub fn job(&self, id: &str) -> Option<&Job> {
        self.jobs.get(id)
    }

    /// Submit a job, reusing the existing one for the same tenant+kind+key.
    pub fn submit(
        &mut self,
        tenant_id: &str,
        kind: JobKind,
        idempotency_key: &str,
    ) -> Result<&Job> {
        if tenant_id.trim().is_empty() || idempotency_key.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "tenant_id".to_string(),
                reason: "tenant and idempotency key are required".to_string(),
            });
        }
        let key = format!("{tenant_id}\u{0}{}\u{0}{idempotency_key}", kind.as_str());
        if let Some(existing) = self.by_key.get(&key) {
            let id = existing.clone();
            return Ok(self.jobs.get(&id).expect("job index is consistent"));
        }
        self.sequence += 1;
        let id = format!("job:{tenant_id}:{}", self.sequence);
        self.jobs.insert(
            id.clone(),
            Job {
                id: id.clone(),
                tenant_id: tenant_id.to_string(),
                kind,
                idempotency_key: idempotency_key.to_string(),
                attempts: 0,
                status: JobStatus::Pending,
                checkpoint: None,
                lease: None,
                last_error: None,
                next_attempt_at: None,
            },
        );
        self.by_key.insert(key, id.clone());
        Ok(self.jobs.get(&id).expect("job just inserted"))
    }

    /// Lease a job to a worker.
    ///
    /// A live lease held by another worker is a conflict; an expired lease is
    /// taken over with a fresh fencing token.
    pub fn lease(&mut self, job_id: &str, owner: &str, now: i64) -> Result<Lease> {
        if owner.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "owner".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        let job = self
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("job {job_id}"),
            })?;
        if matches!(job.status, JobStatus::Succeeded | JobStatus::DeadLettered) {
            return Err(MemoryError::StateConflict {
                resource: job_id.to_string(),
                state: job.status.as_str().to_string(),
                operation: "lease".to_string(),
            });
        }
        if let Some(lease) = &job.lease {
            if lease.expires_at > now && lease.owner != owner {
                return Err(MemoryError::ResourceExhausted {
                    detail: format!(
                        "job {job_id} is leased by {} until {}",
                        lease.owner, lease.expires_at
                    ),
                });
            }
        }
        self.fencing += 1;
        let lease = Lease {
            owner: owner.to_string(),
            expires_at: now + LEASE_SECONDS,
            fencing_token: self.fencing,
        };
        job.lease = Some(lease.clone());
        job.status = JobStatus::Running;
        Ok(lease)
    }

    /// Complete a job, recording a checkpoint.
    pub fn complete(
        &mut self,
        job_id: &str,
        lease: &Lease,
        checkpoint: Option<String>,
    ) -> Result<()> {
        let job = self
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("job {job_id}"),
            })?;
        let current = job
            .lease
            .as_ref()
            .ok_or_else(|| MemoryError::StateConflict {
                resource: job_id.to_string(),
                state: job.status.as_str().to_string(),
                operation: "complete".to_string(),
            })?;
        if current.fencing_token != lease.fencing_token {
            return Err(MemoryError::StateConflict {
                resource: job_id.to_string(),
                state: "stale_fencing_token".to_string(),
                operation: "complete".to_string(),
            });
        }
        job.status = JobStatus::Succeeded;
        job.lease = None;
        job.next_attempt_at = None;
        if checkpoint.is_some() {
            job.checkpoint = checkpoint;
        }
        Ok(())
    }

    /// Record a failure and schedule a retry, or dead-letter the job.
    pub fn fail(
        &mut self,
        job_id: &str,
        lease: &Lease,
        reason: &str,
        now: i64,
    ) -> Result<JobStatus> {
        let job = self
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| MemoryError::NotFound {
                resource: format!("job {job_id}"),
            })?;
        let current = job
            .lease
            .as_ref()
            .ok_or_else(|| MemoryError::StateConflict {
                resource: job_id.to_string(),
                state: job.status.as_str().to_string(),
                operation: "fail".to_string(),
            })?;
        if current.fencing_token != lease.fencing_token {
            return Err(MemoryError::StateConflict {
                resource: job_id.to_string(),
                state: "stale_fencing_token".to_string(),
                operation: "fail".to_string(),
            });
        }
        job.attempts += 1;
        job.last_error = Some(reason.to_string());
        job.lease = None;
        if job.attempts >= MAX_ATTEMPTS {
            job.status = JobStatus::DeadLettered;
            job.next_attempt_at = None;
        } else {
            job.status = JobStatus::RetryScheduled;
            let delay = BACKOFF_BASE_SECONDS * i64::from(2u32.pow(job.attempts - 1));
            job.next_attempt_at = Some(now + delay);
        }
        Ok(job.status)
    }

    /// Whether a periodic job is due.
    pub fn is_due(kind: JobKind, last_run: Option<i64>, now: i64) -> bool {
        match kind.interval_seconds() {
            Some(interval) => match last_run {
                None => true,
                Some(last) => now.saturating_sub(last) >= interval,
            },
            None => true,
        }
    }

    /// The checkpoint a restarting worker resumes from.
    pub fn resume_checkpoint(&self, job_id: &str) -> Result<Option<String>> {
        let job = self.jobs.get(job_id).ok_or_else(|| MemoryError::NotFound {
            resource: format!("job {job_id}"),
        })?;
        Ok(job.checkpoint.clone())
    }

    /// Jobs of one tenant.
    pub fn tenant_jobs(&self, tenant_id: &str) -> Vec<&Job> {
        self.jobs
            .values()
            .filter(|job| job.tenant_id == tenant_id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nine_registered_jobs_are_present() {
        let names: Vec<&str> = JobKind::ALL.iter().map(|kind| kind.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "ttl_sweep",
                "fsrs_refresh",
                "entropy_candidate_scan",
                "consolidation",
                "reconsolidation_close",
                "translation_cache_warm",
                "index_rebuild",
                "stale_knowledge_ref_scan",
                "procedure_review",
            ]
        );
        assert_eq!(JobKind::FsrsRefresh.interval_seconds(), Some(3_600));
        assert_eq!(JobKind::IndexRebuild.interval_seconds(), None);
    }

    #[test]
    fn submission_is_idempotent_per_tenant_and_key() {
        let mut scheduler = Scheduler::new();
        let first = scheduler
            .submit("tenant-a", JobKind::TtlSweep, "sweep-1")
            .unwrap()
            .id
            .clone();
        let again = scheduler
            .submit("tenant-a", JobKind::TtlSweep, "sweep-1")
            .unwrap()
            .id
            .clone();
        assert_eq!(first, again);
        assert_eq!(scheduler.jobs().count(), 1);

        let other_tenant = scheduler
            .submit("tenant-b", JobKind::TtlSweep, "sweep-1")
            .unwrap()
            .id
            .clone();
        assert_ne!(first, other_tenant);
        assert_eq!(scheduler.tenant_jobs("tenant-a").len(), 1);
        assert_eq!(scheduler.tenant_jobs("tenant-b").len(), 1);
        assert!(scheduler.submit(" ", JobKind::TtlSweep, "k").is_err());
    }

    #[test]
    fn a_live_lease_is_exclusive_and_an_expired_one_is_taken_over() {
        let mut scheduler = Scheduler::new();
        let id = scheduler
            .submit("tenant-a", JobKind::Consolidation, "job-1")
            .unwrap()
            .id
            .clone();
        let lease = scheduler.lease(&id, "worker-1", 100).unwrap();
        assert_eq!(lease.expires_at, 100 + LEASE_SECONDS);
        assert_eq!(
            scheduler.lease(&id, "worker-2", 100).unwrap_err().code(),
            "RESOURCE_EXHAUSTED"
        );
        let taken = scheduler
            .lease(&id, "worker-2", 100 + LEASE_SECONDS)
            .unwrap();
        assert!(taken.fencing_token > lease.fencing_token);
        assert_eq!(
            scheduler.complete(&id, &lease, None).unwrap_err().code(),
            "LINK_STATE_CONFLICT",
            "a stale fencing token cannot complete the job"
        );
        scheduler
            .complete(&id, &taken, Some("checkpoint:1".into()))
            .unwrap();
        assert_eq!(scheduler.job(&id).unwrap().status, JobStatus::Succeeded);
        assert_eq!(
            scheduler.resume_checkpoint(&id).unwrap().as_deref(),
            Some("checkpoint:1")
        );
        assert_eq!(
            scheduler.lease(&id, "worker-3", 200).unwrap_err().code(),
            "LINK_STATE_CONFLICT"
        );
    }

    #[test]
    fn retries_back_off_and_then_dead_letter() {
        let mut scheduler = Scheduler::new();
        let id = scheduler
            .submit("tenant-a", JobKind::TtlSweep, "sweep-9")
            .unwrap()
            .id
            .clone();
        let mut now = 1_000;
        for attempt in 1..=MAX_ATTEMPTS {
            let lease = scheduler.lease(&id, "worker-1", now).unwrap();
            let status = scheduler
                .fail(&id, &lease, "backend unavailable", now)
                .unwrap();
            if attempt < MAX_ATTEMPTS {
                assert_eq!(status, JobStatus::RetryScheduled);
                let job = scheduler.job(&id).unwrap();
                assert_eq!(job.attempts, attempt);
                assert_eq!(
                    job.next_attempt_at,
                    Some(now + BACKOFF_BASE_SECONDS * i64::from(2u32.pow(attempt - 1)))
                );
            } else {
                assert_eq!(status, JobStatus::DeadLettered);
                assert_eq!(scheduler.job(&id).unwrap().next_attempt_at, None);
            }
            now += 1_000;
        }
        assert_eq!(
            scheduler.lease(&id, "worker-1", now).unwrap_err().code(),
            "LINK_STATE_CONFLICT"
        );
    }

    #[test]
    fn the_fsrs_sweep_is_due_hourly() {
        assert!(Scheduler::is_due(JobKind::FsrsRefresh, None, 0));
        assert!(!Scheduler::is_due(JobKind::FsrsRefresh, Some(0), 3_000));
        assert!(Scheduler::is_due(JobKind::FsrsRefresh, Some(0), 3_600));
        assert!(Scheduler::is_due(JobKind::IndexRebuild, Some(0), 1));
    }

    #[test]
    fn unknown_jobs_are_reported() {
        let scheduler = Scheduler::new();
        assert_eq!(
            scheduler.resume_checkpoint("job:none").unwrap_err().code(),
            "NOT_FOUND"
        );
    }
}
