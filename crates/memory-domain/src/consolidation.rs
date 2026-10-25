//! Nine-stage consolidation pipeline (§27.1, MAPI-18/19, CO-001).
//!
//! The order is fixed: `Cluster → ExtractFacts → Align → DetectContradictions →
//! Calibrate → Promote → WarmCache → OptimizeGraph → Rescore`. `Promote` is the
//! commit point: a later cache-warming failure may be retried or marked
//! `DEGRADED`, but it can never roll the committed fact back.

use crate::domain::{MemoryError, Result};

/// Pipeline version recorded on every stage record.
pub const PIPELINE_VERSION: &str = "consolidation-v1";

/// The nine stages of §27.1, in their only legal order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// Group candidate episodes.
    Cluster,
    /// Extract typed claim proposals from clusters.
    ExtractFacts,
    /// Align equivalent and translated claims.
    Align,
    /// Detect contradictions between aligned claims.
    DetectContradictions,
    /// Calibrate confidence from independent groups.
    Calibrate,
    /// Commit point: promote non-conflicted verified candidates.
    Promote,
    /// Warm translation and fact caches.
    WarmCache,
    /// Normalise the memory graph projection.
    OptimizeGraph,
    /// Update importance from real statistics.
    Rescore,
}

impl Stage {
    /// Every stage, in order.
    pub const ALL: [Stage; 9] = [
        Stage::Cluster,
        Stage::ExtractFacts,
        Stage::Align,
        Stage::DetectContradictions,
        Stage::Calibrate,
        Stage::Promote,
        Stage::WarmCache,
        Stage::OptimizeGraph,
        Stage::Rescore,
    ];

    /// Stable token used in job records.
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Cluster => "cluster",
            Stage::ExtractFacts => "extract_facts",
            Stage::Align => "align",
            Stage::DetectContradictions => "detect_contradictions",
            Stage::Calibrate => "calibrate",
            Stage::Promote => "promote",
            Stage::WarmCache => "warm_cache",
            Stage::OptimizeGraph => "optimize_graph",
            Stage::Rescore => "rescore",
        }
    }

    /// Position in the DAG, starting at 0.
    pub fn index(self) -> usize {
        match self {
            Stage::Cluster => 0,
            Stage::ExtractFacts => 1,
            Stage::Align => 2,
            Stage::DetectContradictions => 3,
            Stage::Calibrate => 4,
            Stage::Promote => 5,
            Stage::WarmCache => 6,
            Stage::OptimizeGraph => 7,
            Stage::Rescore => 8,
        }
    }

    /// The stage that must run after this one.
    pub fn next(self) -> Option<Stage> {
        Stage::ALL.get(self.index() + 1).copied()
    }

    /// Whether the stage is allowed to write canonical semantic memory.
    ///
    /// Only `Promote` commits new facts; every earlier stage produces proposals.
    pub fn writes_semantic_memory(self) -> bool {
        self >= Stage::Promote
    }
}

/// Pipeline state (§27.1 `PENDING → RUNNING → …`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineStatus {
    /// Accepted but not started.
    Pending,
    /// A stage is running.
    Running,
    /// Every stage finished.
    Committed,
    /// A stage failed but a retry may succeed; committed data is kept.
    FailedRetryable,
    /// A stage quarantined the batch; promotion is frozen.
    Quarantined,
    /// A stage failed and retrying will not help.
    FailedFinal,
}

impl PipelineStatus {
    /// Stable token.
    pub fn as_str(self) -> &'static str {
        match self {
            PipelineStatus::Pending => "pending",
            PipelineStatus::Running => "running",
            PipelineStatus::Committed => "committed",
            PipelineStatus::FailedRetryable => "failed_retryable",
            PipelineStatus::Quarantined => "quarantined",
            PipelineStatus::FailedFinal => "failed_final",
        }
    }

    /// Whether the pipeline finished, successfully or not.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            PipelineStatus::Committed
                | PipelineStatus::FailedRetryable
                | PipelineStatus::Quarantined
                | PipelineStatus::FailedFinal
        )
    }
}

/// One stage record (§27.1 fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageRecord {
    /// Job the stage belongs to.
    pub job_id: String,
    /// Pipeline version.
    pub pipeline_version: String,
    /// Stage.
    pub stage: Stage,
    /// Hash of the stage input.
    pub input_hash: String,
    /// Hash of the stage output.
    pub output_hash: String,
    /// Attempt number, starting at 1.
    pub attempt: u32,
    /// Stage status token.
    pub status: &'static str,
    /// Checkpoint the next stage can resume from.
    pub checkpoint_uri: String,
    /// Audit reference.
    pub audit_ref: String,
}

/// What running one stage produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageOutcome {
    /// Finished with an output hash.
    Completed {
        /// Hash of the produced output.
        output_hash: String,
    },
    /// Failed, but a retry may succeed.
    Retryable {
        /// Safe reason.
        reason: String,
    },
    /// Failed permanently.
    Fatal {
        /// Safe reason.
        reason: String,
    },
    /// The batch must be quarantined (§26.3).
    Quarantined {
        /// Safe reason.
        reason: String,
    },
}

fn audit_ref(job_id: &str, stage: Stage, input_hash: &str, output_hash: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in [
        job_id,
        stage.as_str(),
        input_hash,
        output_hash,
        PIPELINE_VERSION,
    ] {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A running consolidation job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationPipeline {
    job_id: String,
    status: PipelineStatus,
    records: Vec<StageRecord>,
    attempts: u32,
    semantic_committed: bool,
    diagnostics: Vec<String>,
}

impl ConsolidationPipeline {
    /// New pipeline in `PENDING`.
    pub fn new(job_id: impl Into<String>) -> Self {
        Self {
            job_id: job_id.into(),
            status: PipelineStatus::Pending,
            records: Vec::new(),
            attempts: 0,
            semantic_committed: false,
            diagnostics: Vec::new(),
        }
    }

    /// Job id.
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    /// Current status.
    pub fn status(&self) -> PipelineStatus {
        self.status
    }

    /// Stage records produced so far.
    pub fn records(&self) -> &[StageRecord] {
        &self.records
    }

    /// Diagnostics collected during the run.
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    /// Whether canonical semantic memory was committed by `Promote`.
    pub fn semantic_committed(&self) -> bool {
        self.semantic_committed
    }

    /// Total attempts across all stages.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// The stage that must run next, when the pipeline can continue.
    pub fn next_stage(&self) -> Option<Stage> {
        if self.status == PipelineStatus::Committed {
            return None;
        }
        let completed = self
            .records
            .iter()
            .filter(|record| record.status == "completed")
            .count();
        Stage::ALL.get(completed).copied()
    }

    /// Checkpoint of the last completed stage.
    pub fn checkpoint(&self) -> Option<&StageRecord> {
        self.records
            .iter()
            .rev()
            .find(|record| record.status == "completed")
    }

    /// Run one stage.
    ///
    /// A stage must be the expected next stage; a replay of an already-completed
    /// stage with the same input hash is idempotent and returns the existing
    /// record without consuming an attempt.
    pub fn run_stage<F>(&mut self, stage: Stage, input_hash: &str, runner: F) -> Result<StageRecord>
    where
        F: FnOnce(Stage, &str) -> StageOutcome,
    {
        if input_hash.trim().is_empty() {
            return Err(MemoryError::InvalidArgument {
                field: "input_hash".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        if matches!(
            self.status,
            PipelineStatus::Quarantined | PipelineStatus::FailedFinal
        ) {
            return Err(MemoryError::StateConflict {
                resource: self.job_id.clone(),
                state: self.status.as_str().to_string(),
                operation: format!("run stage {}", stage.as_str()),
            });
        }
        if let Some(existing) = self
            .records
            .iter()
            .find(|record| record.stage == stage && record.input_hash == input_hash)
        {
            return Ok(existing.clone());
        }
        let expected = self.next_stage();
        if expected != Some(stage) {
            return Err(MemoryError::PolicyDenied {
                reason: format!(
                    "stage {} must run after {:?} (§27.1 order)",
                    stage.as_str(),
                    expected.map(Stage::as_str)
                ),
            });
        }
        self.attempts += 1;
        self.status = PipelineStatus::Running;
        let outcome = runner(stage, input_hash);
        let attempt = self.attempts_for(stage).saturating_add(1);
        let record = match outcome {
            StageOutcome::Completed { output_hash } => {
                if stage == Stage::Promote {
                    self.semantic_committed = true;
                }
                StageRecord {
                    job_id: self.job_id.clone(),
                    pipeline_version: PIPELINE_VERSION.to_string(),
                    stage,
                    input_hash: input_hash.to_string(),
                    output_hash: output_hash.clone(),
                    attempt,
                    status: "completed",
                    checkpoint_uri: format!("checkpoint:{}/{}", self.job_id, stage.as_str()),
                    audit_ref: audit_ref(&self.job_id, stage, input_hash, &output_hash),
                }
            }
            StageOutcome::Retryable { reason } => {
                self.status = PipelineStatus::FailedRetryable;
                if self.semantic_committed {
                    self.diagnostics.push(format!(
                        "{} failed after the Promote commit point: committed facts are kept and the stage may be retried ({reason})",
                        stage.as_str()
                    ));
                } else {
                    self.diagnostics.push(format!(
                        "{} failed and may be retried: {reason}",
                        stage.as_str()
                    ));
                }
                StageRecord {
                    job_id: self.job_id.clone(),
                    pipeline_version: PIPELINE_VERSION.to_string(),
                    stage,
                    input_hash: input_hash.to_string(),
                    output_hash: String::new(),
                    attempt,
                    status: "failed_retryable",
                    checkpoint_uri: format!("checkpoint:{}/{}", self.job_id, stage.as_str()),
                    audit_ref: audit_ref(&self.job_id, stage, input_hash, &reason),
                }
            }
            StageOutcome::Fatal { reason } => {
                self.status = PipelineStatus::FailedFinal;
                self.diagnostics
                    .push(format!("{} failed permanently: {reason}", stage.as_str()));
                StageRecord {
                    job_id: self.job_id.clone(),
                    pipeline_version: PIPELINE_VERSION.to_string(),
                    stage,
                    input_hash: input_hash.to_string(),
                    output_hash: String::new(),
                    attempt,
                    status: "failed_final",
                    checkpoint_uri: format!("checkpoint:{}/{}", self.job_id, stage.as_str()),
                    audit_ref: audit_ref(&self.job_id, stage, input_hash, &reason),
                }
            }
            StageOutcome::Quarantined { reason } => {
                self.status = PipelineStatus::Quarantined;
                self.diagnostics.push(format!(
                    "{} quarantined the batch and froze promotion: {reason}",
                    stage.as_str()
                ));
                StageRecord {
                    job_id: self.job_id.clone(),
                    pipeline_version: PIPELINE_VERSION.to_string(),
                    stage,
                    input_hash: input_hash.to_string(),
                    output_hash: String::new(),
                    attempt,
                    status: "quarantined",
                    checkpoint_uri: format!("checkpoint:{}/{}", self.job_id, stage.as_str()),
                    audit_ref: audit_ref(&self.job_id, stage, input_hash, reason.as_str()),
                }
            }
        };
        let terminal = record.status != "completed";
        self.records.push(record.clone());
        if !terminal && stage == Stage::Rescore {
            self.status = PipelineStatus::Committed;
        } else if !terminal && self.status == PipelineStatus::Running {
            // Still running: the caller continues with `next_stage`.
            self.status = PipelineStatus::Running;
        }
        Ok(record)
    }

    /// Retry the last retryable stage.
    pub fn retry(&mut self) -> Result<Stage> {
        if self.status != PipelineStatus::FailedRetryable {
            return Err(MemoryError::StateConflict {
                resource: self.job_id.clone(),
                state: self.status.as_str().to_string(),
                operation: "retry".to_string(),
            });
        }
        let failed = self
            .records
            .iter()
            .position(|record| record.status == "failed_retryable")
            .ok_or_else(|| MemoryError::NotFound {
                resource: "retryable stage".to_string(),
            })?;
        self.records.remove(failed);
        self.status = PipelineStatus::Running;
        self.next_stage().ok_or_else(|| MemoryError::StateConflict {
            resource: self.job_id.clone(),
            state: self.status.as_str().to_string(),
            operation: "retry".to_string(),
        })
    }

    fn attempts_for(&self, stage: Stage) -> u32 {
        self.records
            .iter()
            .filter(|record| record.stage == stage)
            .count() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completed(hash: &str) -> impl FnOnce(Stage, &str) -> StageOutcome {
        let hash = hash.to_string();
        move |_stage, _input| StageOutcome::Completed { output_hash: hash }
    }

    fn run_all(pipeline: &mut ConsolidationPipeline) {
        for stage in Stage::ALL {
            pipeline
                .run_stage(stage, "in-hash", completed("out-hash"))
                .unwrap();
        }
    }

    #[test]
    fn the_stage_order_is_the_registered_dag() {
        let names: Vec<&str> = Stage::ALL.iter().map(|stage| stage.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "cluster",
                "extract_facts",
                "align",
                "detect_contradictions",
                "calibrate",
                "promote",
                "warm_cache",
                "optimize_graph",
                "rescore",
            ]
        );
        assert_eq!(Stage::Cluster.next(), Some(Stage::ExtractFacts));
        assert_eq!(Stage::Rescore.next(), None);
        assert_eq!(Stage::Cluster.index(), 0);
        assert!(!Stage::Cluster.writes_semantic_memory());
        assert!(Stage::Promote.writes_semantic_memory());
    }

    #[test]
    fn a_full_run_commits_and_records_every_stage() {
        let mut pipeline = ConsolidationPipeline::new("job-1");
        assert_eq!(pipeline.status(), PipelineStatus::Pending);
        assert_eq!(pipeline.next_stage(), Some(Stage::Cluster));
        run_all(&mut pipeline);
        assert_eq!(pipeline.status(), PipelineStatus::Committed);
        assert!(pipeline.semantic_committed());
        assert_eq!(pipeline.records().len(), 9);
        assert_eq!(pipeline.attempts(), 9);
        assert_eq!(pipeline.next_stage(), None);
        let promote = pipeline
            .records()
            .iter()
            .find(|record| record.stage == Stage::Promote)
            .unwrap();
        assert_eq!(promote.pipeline_version, PIPELINE_VERSION);
        assert!(promote.checkpoint_uri.starts_with("checkpoint:job-1/"));
        assert_eq!(promote.audit_ref.len(), 16);
        assert_eq!(pipeline.checkpoint().unwrap().stage, Stage::Rescore);
    }

    #[test]
    fn stages_cannot_run_out_of_order() {
        let mut pipeline = ConsolidationPipeline::new("job-2");
        let error = pipeline
            .run_stage(Stage::Promote, "in", completed("out"))
            .unwrap_err();
        assert_eq!(error.code(), "POLICY_DENIED");
        assert!(error.to_string().contains("cluster"));
        assert_eq!(pipeline.records().len(), 0);
    }

    #[test]
    fn replaying_a_completed_stage_is_idempotent() {
        let mut pipeline = ConsolidationPipeline::new("job-3");
        let first = pipeline
            .run_stage(Stage::Cluster, "in-hash", completed("out-1"))
            .unwrap();
        let replay = pipeline
            .run_stage(Stage::Cluster, "in-hash", completed("out-2"))
            .unwrap();
        assert_eq!(first, replay, "the recorded attempt is returned unchanged");
        assert_eq!(pipeline.attempts(), 1, "a replay does not consume attempt");
        assert_eq!(pipeline.records().len(), 1);

        // A different input hash is a different run of the same stage and is
        // still refused because the DAG pointer already moved on.
        let error = pipeline
            .run_stage(Stage::Cluster, "other-hash", completed("out-3"))
            .unwrap_err();
        assert_eq!(error.code(), "POLICY_DENIED");
    }

    #[test]
    fn a_cache_failure_after_promote_keeps_the_committed_facts() {
        let mut pipeline = ConsolidationPipeline::new("job-4");
        for stage in Stage::ALL
            .iter()
            .copied()
            .take_while(|stage| *stage <= Stage::Promote)
        {
            pipeline
                .run_stage(stage, "in-hash", completed("out-hash"))
                .unwrap();
        }
        assert!(pipeline.semantic_committed());

        let record = pipeline
            .run_stage(Stage::WarmCache, "in-hash", |_stage, _input| {
                StageOutcome::Retryable {
                    reason: "translation service unavailable".to_string(),
                }
            })
            .unwrap();
        assert_eq!(record.status, "failed_retryable");
        assert_eq!(pipeline.status(), PipelineStatus::FailedRetryable);
        assert!(
            pipeline.semantic_committed(),
            "the Promote commit point is not rolled back"
        );
        assert!(pipeline
            .diagnostics()
            .iter()
            .any(|line| line.contains("commit point")));

        // Retrying resumes at the failed stage, not at the beginning.
        let next = pipeline.retry().unwrap();
        assert_eq!(next, Stage::WarmCache);
        pipeline
            .run_stage(Stage::WarmCache, "in-hash", completed("out-hash"))
            .unwrap();
        pipeline
            .run_stage(Stage::OptimizeGraph, "in-hash", completed("out-hash"))
            .unwrap();
        pipeline
            .run_stage(Stage::Rescore, "in-hash", completed("out-hash"))
            .unwrap();
        assert_eq!(pipeline.status(), PipelineStatus::Committed);
    }

    #[test]
    fn a_quarantine_freezes_the_pipeline() {
        let mut pipeline = ConsolidationPipeline::new("job-5");
        pipeline
            .run_stage(Stage::Cluster, "in-hash", completed("out"))
            .unwrap();
        let record = pipeline
            .run_stage(Stage::ExtractFacts, "in-hash", |_stage, _input| {
                StageOutcome::Quarantined {
                    reason: "contradiction burst across one source".to_string(),
                }
            })
            .unwrap();
        assert_eq!(record.status, "quarantined");
        assert_eq!(pipeline.status(), PipelineStatus::Quarantined);
        assert!(!pipeline.semantic_committed());
        assert_eq!(
            pipeline
                .run_stage(Stage::Align, "in-hash", completed("out"))
                .unwrap_err()
                .code(),
            "LINK_STATE_CONFLICT"
        );
        assert_eq!(pipeline.retry().unwrap_err().code(), "LINK_STATE_CONFLICT");
    }

    #[test]
    fn a_fatal_failure_is_terminal_and_retry_is_refused() {
        let mut pipeline = ConsolidationPipeline::new("job-6");
        let record = pipeline
            .run_stage(Stage::Cluster, "in-hash", |_stage, _input| {
                StageOutcome::Fatal {
                    reason: "input hash mismatch".to_string(),
                }
            })
            .unwrap();
        assert_eq!(record.status, "failed_final");
        assert_eq!(pipeline.status(), PipelineStatus::FailedFinal);
        assert!(pipeline.status().is_terminal());
        assert_eq!(pipeline.retry().unwrap_err().code(), "LINK_STATE_CONFLICT");
    }

    #[test]
    fn an_empty_input_hash_is_rejected() {
        let mut pipeline = ConsolidationPipeline::new("job-7");
        assert_eq!(
            pipeline
                .run_stage(Stage::Cluster, " ", completed("out"))
                .unwrap_err()
                .code(),
            "INVALID_ARGUMENT"
        );
    }
}
