//! Backup scheduling (§28.3, OPS-001, P10-WP4).
//!
//! The scheduler owns no clock: a host calls [`BackupScheduler::tick`] with the
//! current instant and takes the action it returns. That keeps the policy testable
//! (interval, retention, rotation) without a background thread, and it means a
//! missed tick is visible as a gap in the history rather than a silent skip.

use std::path::PathBuf;

/// How often to back up and how many backups to keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupPlan {
    /// Minimum seconds between two backups.
    pub interval_secs: u64,
    /// How many backups to keep (the oldest are pruned).
    pub retain: usize,
    /// Directory the backups are written to.
    pub destination: PathBuf,
    /// Name prefix of a backup file.
    pub prefix: String,
}

impl BackupPlan {
    /// A plan with a positive interval and a retention of at least one.
    pub fn new(destination: impl Into<PathBuf>, interval_secs: u64, retain: usize) -> Self {
        Self {
            interval_secs: interval_secs.max(1),
            retain: retain.max(1),
            destination: destination.into(),
            prefix: "ukma-backup".to_string(),
        }
    }

    /// File name of the backup taken at `instant`.
    pub fn file_name(&self, instant: i64) -> String {
        format!("{}-{instant}.snapshot", self.prefix)
    }

    /// Full path of the backup taken at `instant`.
    pub fn path_for(&self, instant: i64) -> PathBuf {
        self.destination.join(self.file_name(instant))
    }
}

/// One completed backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRecord {
    /// Instant the backup was taken.
    pub taken_at: i64,
    /// File that holds it.
    pub path: PathBuf,
    /// Bytes written (reported by the host, not guessed).
    pub bytes: u64,
}

/// What a tick asks the host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupAction {
    /// Take a backup of `path` now.
    Backup { path: PathBuf },
    /// Nothing is due.
    Idle,
}

/// Interval/retention state machine.
#[derive(Debug, Clone)]
pub struct BackupScheduler {
    plan: BackupPlan,
    history: Vec<BackupRecord>,
    last_tick: Option<i64>,
}

impl BackupScheduler {
    /// New scheduler with no history.
    pub fn new(plan: BackupPlan) -> Self {
        Self {
            plan,
            history: Vec::new(),
            last_tick: None,
        }
    }

    /// The plan in force.
    pub fn plan(&self) -> &BackupPlan {
        &self.plan
    }

    /// Completed backups, oldest first.
    pub fn history(&self) -> &[BackupRecord] {
        &self.history
    }

    /// Backups that retention allows to stay on disk.
    pub fn retained(&self) -> &[BackupRecord] {
        let keep = self.plan.retain.min(self.history.len());
        &self.history[self.history.len() - keep..]
    }

    /// Backups retention says to delete, oldest first.
    pub fn prunable(&self) -> Vec<BackupRecord> {
        if self.history.len() <= self.plan.retain {
            return Vec::new();
        }
        self.history[..self.history.len() - self.plan.retain].to_vec()
    }

    /// Whether a backup is due at `instant`.
    pub fn is_due(&self, instant: i64) -> bool {
        match self.history.last() {
            None => true,
            Some(last) => instant >= last.taken_at + self.plan.interval_secs as i64,
        }
    }

    /// Ask what to do at `instant`.
    ///
    /// A clock that moves backwards is refused by returning `Idle`, so a wrong
    /// system clock cannot overwrite a newer backup with an older one.
    pub fn tick(&mut self, instant: i64) -> BackupAction {
        if let Some(previous) = self.last_tick {
            if instant < previous {
                return BackupAction::Idle;
            }
        }
        self.last_tick = Some(instant);
        if self.is_due(instant) {
            BackupAction::Backup {
                path: self.plan.path_for(instant),
            }
        } else {
            BackupAction::Idle
        }
    }

    /// Record a completed backup (the host reports the real byte count).
    pub fn record(&mut self, taken_at: i64, path: PathBuf, bytes: u64) {
        self.history.push(BackupRecord {
            taken_at,
            path,
            bytes,
        });
        self.history.sort_by_key(|record| record.taken_at);
    }

    /// Machine-readable status for the operations report.
    pub fn report_json(&self, now: i64) -> String {
        serde_json::json!({
            "interval_secs": self.plan.interval_secs,
            "retain": self.plan.retain,
            "backups": self.history.len(),
            "last_backup_at": self.history.last().map(|record| record.taken_at),
            "age_secs": self.history.last().map(|record| now - record.taken_at),
            "due": self.is_due(now),
            "retained": self.retained().len(),
            "prunable": self.prunable().len(),
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> BackupPlan {
        BackupPlan::new("backups", 3600, 3)
    }

    #[test]
    fn the_first_tick_is_due_and_the_interval_then_holds() {
        let mut scheduler = BackupScheduler::new(plan());
        assert_eq!(
            scheduler.tick(1_000),
            BackupAction::Backup {
                path: PathBuf::from("backups").join("ukma-backup-1000.snapshot")
            }
        );
        scheduler.record(1_000, plan().path_for(1_000), 4096);
        assert_eq!(scheduler.tick(1_000 + 3599), BackupAction::Idle);
        assert!(matches!(
            scheduler.tick(1_000 + 3600),
            BackupAction::Backup { .. }
        ));
    }

    #[test]
    fn retention_keeps_the_newest_and_prunes_the_oldest() {
        let mut scheduler = BackupScheduler::new(plan());
        for step in 0..5 {
            let instant = 1_000 + step * 3_600;
            scheduler.record(instant, plan().path_for(instant), 1_000 + step as u64);
        }
        assert_eq!(scheduler.history().len(), 5);
        assert_eq!(scheduler.retained().len(), 3);
        let kept: Vec<i64> = scheduler.retained().iter().map(|r| r.taken_at).collect();
        assert_eq!(kept, vec![8_200, 11_800, 15_400]);
        let pruned: Vec<i64> = scheduler.prunable().iter().map(|r| r.taken_at).collect();
        assert_eq!(pruned, vec![1_000, 4_600]);
    }

    #[test]
    fn a_backwards_clock_never_triggers_a_backup() {
        let mut scheduler = BackupScheduler::new(plan());
        assert!(matches!(
            scheduler.tick(10_000),
            BackupAction::Backup { .. }
        ));
        scheduler.record(10_000, plan().path_for(10_000), 1);
        assert_eq!(scheduler.tick(5_000), BackupAction::Idle);
        assert_eq!(scheduler.history().len(), 1);
    }

    #[test]
    fn the_plan_refuses_a_zero_interval_or_zero_retention() {
        let plan = BackupPlan::new("backups", 0, 0);
        assert_eq!(plan.interval_secs, 1);
        assert_eq!(plan.retain, 1);
    }

    #[test]
    fn the_report_states_age_and_whether_a_backup_is_due() {
        let mut scheduler = BackupScheduler::new(plan());
        scheduler.record(1_000, plan().path_for(1_000), 512);
        let report: serde_json::Value =
            serde_json::from_str(&scheduler.report_json(2_000)).expect("json");
        assert_eq!(report["age_secs"], 1_000);
        assert_eq!(report["due"], false);
        assert_eq!(report["backups"], 1);
        assert_eq!(report["retain"], 3);
    }
}
