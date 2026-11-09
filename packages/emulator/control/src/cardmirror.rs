//! The module store as a folder, kept up to date while the emulator runs (`plusdrive_format::mirror`): the state the controller
//! keeps for it. A write to the card starts a quiet period; when it has run out without another write (the guest's upload is over) the
//! store is mirrored to `<folder>/digikit/`. `card.sync` does it at once. The folder is a plus_drive folder (`--mirror-dir`: the
//! launcher gives `plus_drive/<device>/`).

use std::{
    collections::BTreeSet,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use plusdrive_format::mirror::MirrorReport;
use serde_json::{Value, json};

/// The time without a card write after which the store is mirrored.
pub const QUIET: Duration = Duration::from_secs(2);

pub struct Mirror {
    pub dir: PathBuf,
    /// The store paths this session has seen on the card (see `plusdrive_format::mirror`).
    pub seen: BTreeSet<String>,
    /// The card's write count at the last look, and the one the last mirror pass covered.
    writes: u64,
    synced_writes: u64,
    quiet_since: Option<Instant>,
    last_sync_ms: Option<u64>,
    last: Value,
    error: Option<String>,
}

impl Mirror {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            seen: BTreeSet::new(),
            writes: 0,
            synced_writes: 0,
            quiet_since: None,
            last_sync_ms: None,
            last: Value::Null,
            error: None,
        }
    }

    /// A new runtime (a load, a soft reset) with a card whose store holds `seen` and that has taken `writes` write commands.
    pub fn reset(&mut self, seen: BTreeSet<String>, writes: u64) {
        self.seen = seen;
        self.writes = writes;
        self.synced_writes = writes;
        self.quiet_since = None;
    }

    /// The card's write count now: a change starts (or restarts) the quiet period.
    pub fn note_writes(&mut self, writes: u64, now: Instant) {
        if writes != self.writes {
            self.writes = writes;
            self.quiet_since = Some(now);
        }
    }

    /// A change is waiting for its quiet period.
    pub fn pending(&self) -> bool {
        self.quiet_since.is_some()
    }

    /// The quiet period has run out.
    pub fn due(&self, now: Instant) -> bool {
        self.quiet_since
            .is_some_and(|t| now.duration_since(t) >= QUIET)
    }

    /// Record the outcome of a pass. A store that did not read (the guest is in the middle of an upload) is tried again after another
    /// quiet period.
    pub fn done(&mut self, outcome: Result<MirrorReport, String>, now: Instant) {
        match outcome {
            Ok(report) => {
                self.synced_writes = self.writes;
                self.quiet_since = None;
                self.last_sync_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as u64);
                self.error = report.issues.first().cloned();
                self.last = json!({
                    "added": report.added, "replaced": report.replaced, "removed": report.removed,
                    "unchanged": report.unchanged, "issues": report.issues,
                });
            }
            Err(e) => {
                self.error = Some(e);
                self.quiet_since = Some(now);
            }
        }
    }

    /// Writes since the last pass: a synced folder is not behind the card.
    pub fn behind(&self) -> bool {
        self.writes != self.synced_writes
    }

    pub fn status(&self, files: Vec<plusdrive_format::mirror::FolderFile>) -> Value {
        json!({
            "folder": self.dir.display().to_string(),
            "continuous": true,
            "files": files.iter().map(|f| json!({"path": f.path, "bytes": f.bytes, "hash": f.hash})).collect::<Vec<_>>(),
            "last_sync": self.last_sync_ms,
            "last": self.last,
            "behind": self.behind(),
            "error": self.error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_starts_the_quiet_period_and_the_next_write_restarts_it() {
        let t0 = Instant::now();
        let mut m = Mirror::new("f".into());
        m.reset(BTreeSet::new(), 5);
        assert!(!m.pending() && !m.behind());
        m.note_writes(5, t0); // no new write
        assert!(!m.pending());
        m.note_writes(6, t0);
        assert!(m.pending() && m.behind() && !m.due(t0));
        assert!(!m.due(t0 + QUIET - Duration::from_millis(1)));
        m.note_writes(7, t0 + Duration::from_secs(1)); // another write: the period starts over
        assert!(!m.due(t0 + QUIET));
        assert!(m.due(t0 + Duration::from_secs(1) + QUIET));
        m.done(Ok(MirrorReport::default()), t0 + Duration::from_secs(4));
        assert!(!m.pending() && !m.behind());
        assert!(m.status(vec![])["last_sync"].is_u64());
    }

    #[test]
    fn a_store_that_did_not_read_is_tried_again_after_another_quiet_period() {
        let t0 = Instant::now();
        let mut m = Mirror::new("f".into());
        m.reset(BTreeSet::new(), 0);
        m.note_writes(1, t0);
        m.done(Err("module store: table checksum".into()), t0 + QUIET);
        assert!(m.pending() && m.behind());
        assert!(!m.due(t0 + QUIET));
        assert!(m.due(t0 + QUIET + QUIET));
        assert_eq!(m.status(vec![])["error"], "module store: table checksum");
    }
}
