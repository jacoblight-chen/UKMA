//! Ingestion job state machine with orthogonal commit/projection phases.
//!
//! Two invariants drive this module:
//! - **§7.5**: the legal stage path is
//!   `QUEUED → PARSING → EXTRACTING → RESOLVING → STAGING → COMMITTING →
//!   INDEXING → READY`, branching to `FAILED`, `CANCELLED` or `STALE`.
//! - **§40.4 / ADR-037**: execution, commit and projection are *separate*
//!   phases. A cancel or a projection failure MUST NOT undo a commit, and
//!   `JobView.status` must never be used to infer `canonical_committed`.

use std::collections::BTreeMap;

/// Lifecycle stage of an ingestion job (§7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// Accepted, no work started.
    Queued,
    /// Parsing source artifacts.
    Parsing,
    /// Extracting entities/edges.
    Extracting,
    /// Resolving references.
    Resolving,
    /// Writing staged output.
    Staging,
    /// Committing canonical data.
    Committing,
    /// Building projections.
    Indexing,
    /// Everything published.
    Ready,
    /// Terminal failure.
    Failed,
    /// Cancelled before commit.
    Cancelled,
    /// Source moved on; output superseded.
    Stale,
}

impl Stage {
    /// Canonical name (audit/telemetry).
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Queued => "queued",
            Stage::Parsing => "parsing",
            Stage::Extracting => "extracting",
            Stage::Resolving => "resolving",
            Stage::Staging => "staging",
            Stage::Committing => "committing",
            Stage::Indexing => "indexing",
            Stage::Ready => "ready",
            Stage::Failed => "failed",
            Stage::Cancelled => "cancelled",
            Stage::Stale => "stale",
        }
    }

    /// Whether the stage is terminal.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Stage::Ready | Stage::Failed | Stage::Cancelled | Stage::Stale
        )
    }

    /// Next stage on the happy path.
    fn next(self) -> Option<Stage> {
        match self {
            Stage::Queued => Some(Stage::Parsing),
            Stage::Parsing => Some(Stage::Extracting),
            Stage::Extracting => Some(Stage::Resolving),
            Stage::Resolving => Some(Stage::Staging),
            Stage::Staging => Some(Stage::Committing),
            Stage::Committing => Some(Stage::Indexing),
            Stage::Indexing => Some(Stage::Ready),
            _ => None,
        }
    }

    /// Whether cancellation is still allowed (before the canonical commit).
    pub fn cancellable(self) -> bool {
        !matches!(
            self,
            Stage::Committing
                | Stage::Indexing
                | Stage::Ready
                | Stage::Failed
                | Stage::Cancelled
                | Stage::Stale
        )
    }

    /// Whether the canonical data is already committed at this stage.
    pub fn canonical_committed(self) -> bool {
        matches!(self, Stage::Indexing | Stage::Ready | Stage::Stale)
    }
}

/// Execution phase (§40.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPhase {
    /// Waiting to run.
    Queued,
    /// Running.
    Running,
    /// Finished (success or failure).
    Completed,
    /// Failed.
    Failed,
    /// Cancelled.
    Cancelled,
}

/// Commit phase (§40.4) — independent of execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitPhase {
    /// Nothing to commit yet.
    NotStarted,
    /// Cancelled/aborted before the commit point.
    NotCommitted,
    /// Canonical data is durable.
    Committed,
    /// Commit outcome unknown; resolve via operation id, never blind retry.
    Unknown,
}

/// Projection phase (§40.4) — independent of commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionPhase {
    /// No projection needed.
    NotRequired,
    /// Pending.
    Pending,
    /// Published.
    Ready,
    /// Failed; replayable.
    Failed,
    /// Rebuilding.
    Rebuilding,
}

/// Orthogonal outcome of an operation (§40.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationOutcome {
    /// Operation identity (idempotency receipt key).
    pub operation_id: String,
    /// Execution phase.
    pub execution: ExecutionPhase,
    /// Commit phase.
    pub commit: CommitPhase,
    /// Projection phase.
    pub projection: ProjectionPhase,
    /// Canonical revision written, when committed.
    pub canonical_revision: Option<u64>,
    /// Diagnostics (safe messages only).
    pub diagnostics: Vec<String>,
    /// Monotonic update counter used as `updated_at` ordering.
    pub updated_seq: u64,
}

impl OperationOutcome {
    /// New outcome for a submitted operation.
    pub fn submitted(operation_id: &str) -> Self {
        Self {
            operation_id: operation_id.to_string(),
            execution: ExecutionPhase::Queued,
            commit: CommitPhase::NotStarted,
            projection: ProjectionPhase::NotRequired,
            canonical_revision: None,
            diagnostics: Vec::new(),
            updated_seq: 0,
        }
    }
}

/// Job errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    /// Unknown job.
    JobNotFound {
        /// Job id.
        job_id: String,
    },
    /// Illegal stage transition.
    InvalidTransition {
        /// Current stage.
        from: Stage,
        /// Requested stage.
        to: Stage,
    },
    /// Cancellation arrived too late; the commit is preserved.
    JobNotCancellable {
        /// Job id.
        job_id: String,
        /// Stage that blocked cancellation.
        stage: Stage,
    },
    /// The requested source revision is older than the current generation.
    StaleGeneration {
        /// Requested revision.
        requested: String,
    },
}

impl JobError {
    /// Registered v1.2.3 error code.
    pub fn code(&self) -> &'static str {
        match self {
            JobError::JobNotFound { .. } => "JOB_NOT_FOUND",
            JobError::InvalidTransition { .. } => "LINK_STATE_CONFLICT",
            JobError::JobNotCancellable { .. } => "JOB_NOT_CANCELLABLE",
            JobError::StaleGeneration { .. } => "INDEX_STALE",
        }
    }
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobError::JobNotFound { job_id } => write!(f, "job {job_id} not found"),
            JobError::InvalidTransition { from, to } => {
                write!(f, "illegal transition {} -> {}", from.as_str(), to.as_str())
            }
            JobError::JobNotCancellable { job_id, stage } => {
                write!(
                    f,
                    "job {job_id} not cancellable at stage {}",
                    stage.as_str()
                )
            }
            JobError::StaleGeneration { requested } => {
                write!(f, "generation {requested} is not published")
            }
        }
    }
}

impl std::error::Error for JobError {}

fn outcome_for(stage: Stage) -> (ExecutionPhase, CommitPhase, ProjectionPhase) {
    match stage {
        Stage::Queued => (
            ExecutionPhase::Queued,
            CommitPhase::NotStarted,
            ProjectionPhase::NotRequired,
        ),
        Stage::Parsing | Stage::Extracting | Stage::Resolving | Stage::Staging => (
            ExecutionPhase::Running,
            CommitPhase::NotStarted,
            ProjectionPhase::NotRequired,
        ),
        Stage::Committing => (
            ExecutionPhase::Running,
            CommitPhase::Committed,
            ProjectionPhase::Pending,
        ),
        Stage::Indexing => (
            ExecutionPhase::Running,
            CommitPhase::Committed,
            ProjectionPhase::Pending,
        ),
        Stage::Ready => (
            ExecutionPhase::Completed,
            CommitPhase::Committed,
            ProjectionPhase::Ready,
        ),
        Stage::Failed => (
            ExecutionPhase::Failed,
            CommitPhase::NotStarted,
            ProjectionPhase::NotRequired,
        ),
        Stage::Cancelled => (
            ExecutionPhase::Cancelled,
            CommitPhase::NotCommitted,
            ProjectionPhase::NotRequired,
        ),
        Stage::Stale => (
            ExecutionPhase::Completed,
            CommitPhase::Committed,
            ProjectionPhase::Rebuilding,
        ),
    }
}

/// One ingestion job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Job identity.
    pub job_id: String,
    /// Job kind (e.g. `code.analyze_repository`).
    pub kind: String,
    /// Current stage.
    pub stage: Stage,
    /// Orthogonal outcome.
    pub outcome: OperationOutcome,
}

impl Job {
    /// Whether projection may be retried (it failed after a commit).
    pub fn projection_retryable(&self) -> bool {
        self.outcome.commit == CommitPhase::Committed
            && matches!(
                self.outcome.projection,
                ProjectionPhase::Failed | ProjectionPhase::Pending | ProjectionPhase::Rebuilding
            )
    }
}

/// In-memory job registry.
#[derive(Debug, Default)]
pub struct JobManager {
    jobs: BTreeMap<String, Job>,
    sequence: u64,
}

impl JobManager {
    /// New empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    fn next_seq(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }

    /// Submit a job at `Queued`.
    pub fn submit(&mut self, job_id: &str, kind: &str) -> Job {
        let mut outcome = OperationOutcome::submitted(job_id);
        outcome.updated_seq = self.next_seq();
        let job = Job {
            job_id: job_id.to_string(),
            kind: kind.to_string(),
            stage: Stage::Queued,
            outcome,
        };
        self.jobs.insert(job_id.to_string(), job.clone());
        job
    }

    /// Look up a job.
    pub fn get(&self, job_id: &str) -> Result<&Job, JobError> {
        self.jobs.get(job_id).ok_or_else(|| JobError::JobNotFound {
            job_id: job_id.to_string(),
        })
    }

    fn store(&mut self, mut job: Job) -> Job {
        job.outcome.updated_seq = self.next_seq();
        let projection_before = job.outcome.projection;
        let (execution, commit, projection) = outcome_for(job.stage);
        job.outcome.execution = execution;
        job.outcome.commit = commit;
        // A recorded projection failure is sticky until projection is retried;
        // recomputing from the stage must not silently clear it (§40.4).
        job.outcome.projection = if projection_before == ProjectionPhase::Failed
            && projection == ProjectionPhase::Pending
        {
            ProjectionPhase::Failed
        } else {
            projection
        };
        self.jobs.insert(job.job_id.clone(), job.clone());
        job
    }

    /// Advance to the next legal stage.
    pub fn advance(&mut self, job_id: &str) -> Result<Job, JobError> {
        let current = self.get(job_id)?.clone();
        let next = current.stage.next().ok_or(JobError::InvalidTransition {
            from: current.stage,
            to: current.stage,
        })?;
        let mut job = current;
        job.stage = next;
        Ok(self.store(job))
    }

    /// Mark the job failed, recording a safe diagnostic.
    pub fn fail(&mut self, job_id: &str, diagnostic: &str) -> Result<Job, JobError> {
        let mut job = self.get(job_id)?.clone();
        if job.stage.is_terminal() {
            return Err(JobError::InvalidTransition {
                from: job.stage,
                to: Stage::Failed,
            });
        }
        job.stage = Stage::Failed;
        job.outcome.diagnostics.push(diagnostic.to_string());
        Ok(self.store(job))
    }

    /// Cancel a job. Only legal before the canonical commit; afterwards the
    /// commit is preserved and the caller gets `JOB_NOT_CANCELLABLE` (§40.4).
    pub fn cancel(&mut self, job_id: &str, reason: &str) -> Result<Job, JobError> {
        let mut job = self.get(job_id)?.clone();
        if !job.stage.cancellable() {
            return Err(JobError::JobNotCancellable {
                job_id: job_id.to_string(),
                stage: job.stage,
            });
        }
        job.stage = Stage::Cancelled;
        job.outcome.diagnostics.push(format!("cancelled: {reason}"));
        Ok(self.store(job))
    }

    /// Record a projection failure **after** a successful commit: the commit
    /// stays, projection becomes retryable (§40.4).
    pub fn projection_failed(&mut self, job_id: &str, diagnostic: &str) -> Result<Job, JobError> {
        let mut job = self.get(job_id)?.clone();
        if job.outcome.commit != CommitPhase::Committed {
            return Err(JobError::InvalidTransition {
                from: job.stage,
                to: Stage::Failed,
            });
        }
        job.outcome.projection = ProjectionPhase::Failed;
        job.outcome.diagnostics.push(diagnostic.to_string());
        Ok(self.store(job))
    }

    /// Mark a job stale after its source moved on (commit preserved).
    pub fn mark_stale(&mut self, job_id: &str) -> Result<Job, JobError> {
        let mut job = self.get(job_id)?.clone();
        if job.outcome.commit != CommitPhase::Committed {
            return Err(JobError::InvalidTransition {
                from: job.stage,
                to: Stage::Stale,
            });
        }
        job.stage = Stage::Stale;
        Ok(self.store(job))
    }
}

/// Published-generation pointer with retention of superseded generations
/// (§7.5: readers only ever see a legal published generation).
#[derive(Debug, Default)]
pub struct GenerationPointer {
    current: Option<String>,
    retained: Vec<String>,
}

impl GenerationPointer {
    /// New empty pointer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Currently published generation.
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Generations still readable (superseded but retained).
    pub fn retained(&self) -> &[String] {
        &self.retained
    }

    /// Publish a new generation, retaining the previous one.
    pub fn publish(&mut self, generation: &str) {
        if let Some(previous) = self.current.replace(generation.to_string()) {
            if !self.retained.contains(&previous) {
                self.retained.push(previous);
            }
        }
    }

    /// Verify that a caller-requested generation is visible
    /// (`strict_generation` semantics, `INDEX_NOT_READY` otherwise).
    pub fn require(&self, generation: &str) -> Result<(), JobError> {
        if self.current.as_deref() == Some(generation)
            || self.retained.contains(&generation.to_string())
        {
            Ok(())
        } else {
            Err(JobError::StaleGeneration {
                requested: generation.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_reaches_ready_with_committed_canonical_data() {
        let mut manager = JobManager::new();
        manager.submit("job-1", "code.analyze_repository");
        for _ in 0..7 {
            manager.advance("job-1").unwrap();
        }
        let job = manager.get("job-1").unwrap();
        assert_eq!(job.stage, Stage::Ready);
        assert_eq!(job.outcome.execution, ExecutionPhase::Completed);
        assert_eq!(job.outcome.commit, CommitPhase::Committed);
        assert_eq!(job.outcome.projection, ProjectionPhase::Ready);
    }

    #[test]
    fn cancel_is_allowed_before_commit_and_forbidden_after() {
        let mut manager = JobManager::new();
        manager.submit("job-1", "code.analyze_repository");
        manager.advance("job-1").unwrap(); // parsing
        let cancelled = manager.cancel("job-1", "user request").unwrap();
        assert_eq!(cancelled.stage, Stage::Cancelled);
        assert_eq!(cancelled.outcome.commit, CommitPhase::NotCommitted);

        // a committed job cannot be cancelled (PROP-09: never destroy committed data)
        let mut manager = JobManager::new();
        manager.submit("job-2", "code.analyze_repository");
        for _ in 0..5 {
            manager.advance("job-2").unwrap(); // reaches Committing
        }
        let error = manager.cancel("job-2", "too late").unwrap_err();
        assert_eq!(error.code(), "JOB_NOT_CANCELLABLE");
        assert_eq!(
            manager.get("job-2").unwrap().outcome.commit,
            CommitPhase::Committed
        );
    }

    #[test]
    fn projection_failure_preserves_the_commit() {
        let mut manager = JobManager::new();
        manager.submit("job-1", "code.analyze_repository");
        for _ in 0..5 {
            manager.advance("job-1").unwrap();
        }
        let job = manager
            .projection_failed("job-1", "vector index unavailable")
            .unwrap();
        assert_eq!(job.outcome.commit, CommitPhase::Committed);
        assert_eq!(job.outcome.projection, ProjectionPhase::Failed);
        assert!(job.projection_retryable());
    }

    #[test]
    fn stale_requires_a_prior_commit() {
        let mut manager = JobManager::new();
        manager.submit("job-1", "code.analyze_repository");
        assert!(manager.mark_stale("job-1").is_err());

        for _ in 0..5 {
            manager.advance("job-1").unwrap();
        }
        let job = manager.mark_stale("job-1").unwrap();
        assert_eq!(job.stage, Stage::Stale);
        assert_eq!(job.outcome.commit, CommitPhase::Committed);
    }

    #[test]
    fn illegal_transition_is_reported() {
        let mut manager = JobManager::new();
        manager.submit("job-1", "code.analyze_repository");
        manager.fail("job-1", "parse error").unwrap();
        let error = manager.advance("job-1").unwrap_err();
        assert_eq!(error.code(), "LINK_STATE_CONFLICT");
    }

    #[test]
    fn unknown_job_is_a_typed_error() {
        let manager = JobManager::new();
        assert_eq!(manager.get("nope").unwrap_err().code(), "JOB_NOT_FOUND");
    }

    #[test]
    fn generation_pointer_retains_superseded_generations() {
        let mut pointer = GenerationPointer::new();
        assert!(pointer.require("gen-1").is_err());
        pointer.publish("gen-1");
        assert_eq!(pointer.current(), Some("gen-1"));
        pointer.publish("gen-2");
        assert_eq!(pointer.current(), Some("gen-2"));
        assert_eq!(pointer.retained(), &["gen-1".to_string()]);
        // both remain readable, an unpublished one does not
        assert!(pointer.require("gen-1").is_ok());
        assert!(pointer.require("gen-2").is_ok());
        assert_eq!(pointer.require("gen-3").unwrap_err().code(), "INDEX_STALE");
    }

    #[test]
    fn outcome_phases_stay_orthogonal() {
        // committing: execution running, commit committed, projection pending
        let (execution, commit, projection) = outcome_for(Stage::Committing);
        assert_eq!(execution, ExecutionPhase::Running);
        assert_eq!(commit, CommitPhase::Committed);
        assert_eq!(projection, ProjectionPhase::Pending);
        // cancelled: nothing committed
        let (_, commit, _) = outcome_for(Stage::Cancelled);
        assert_eq!(commit, CommitPhase::NotCommitted);
    }
}
