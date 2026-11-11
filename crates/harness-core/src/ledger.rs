//! Persistent daily spend ledger, shared across the harness plugins.
//!
//! `~/.budgetguard/state/ledger.json` maps `YYYY-MM-DD` →
//! `{ sessions: { <id>: <cost_usd> } }`, where `<cost_usd>` is the session's
//! spend attributed to that day (its increment, not its cumulative cost).
//! budgetguard owns the *write* path (its Stop hook overwrites the current
//! session's entry for today and recomputes the day total); other plugins (harness-status) *read* the same type instead of
//! re-declaring a mirror struct that could drift.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DayEntry {
    /// session_id → that session's spend attributed to this day (its latest
    /// cumulative cost minus what earlier days hold for it; see
    /// [`Ledger::record`]).
    pub sessions: BTreeMap<String, f64>,
}

impl DayEntry {
    pub fn total(&self) -> f64 {
        self.sessions.values().sum()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Ledger {
    pub days: BTreeMap<String, DayEntry>,
}

/// Why a ledger couldn't be loaded as a clean, current value.
#[derive(Debug)]
pub enum LoadError {
    /// The file exists but isn't valid JSON for a `Ledger`. The on-disk bytes
    /// must be PRESERVED (never overwritten with a reset/default), or one bad
    /// write would erase the whole spend history and fail the budget open.
    Corrupt,
    /// The file exists but could not be read (permission denied, it is a
    /// directory, an I/O error, ...). Reading it as an empty ledger would let a
    /// caller record one session and save, erasing the day's spend, so this is
    /// an error, never a default. The on-disk entry must be left untouched.
    Unreadable(std::io::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Corrupt => write!(f, "ledger.json is corrupt (not valid ledger JSON)"),
            LoadError::Unreadable(e) => write!(f, "ledger.json is unreadable ({e})"),
        }
    }
}

/// budgetguard's default state directory (`~/.budgetguard/state`). Consumers
/// that want to read the ledger locate it here without depending on budgetguard.
pub fn default_state_dir() -> PathBuf {
    crate::config::base_dir("budgetguard").join("state")
}

impl Ledger {
    fn path(state_dir: &Path) -> PathBuf {
        state_dir.join("ledger.json")
    }

    /// Load the ledger, distinguishing "absent" (a fresh default is correct)
    /// from "present but corrupt" (the caller must not clobber it).
    ///
    /// - file missing (`NotFound`)  → `Ok(Ledger::default())`
    /// - file present & parses      → `Ok(parsed)`
    /// - file present & unparseable → `Err(LoadError::Corrupt)`
    /// - any other read error       → `Err(LoadError::Unreadable(_))`
    pub fn load_checked(state_dir: &Path) -> Result<Self, LoadError> {
        match std::fs::read_to_string(Self::path(state_dir)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(LoadError::Unreadable(e)),
            Ok(s) => serde_json::from_str(&s).map_err(|_| LoadError::Corrupt),
        }
    }

    /// Backwards-compatible lenient load: a corrupt or unreadable file reads as default.
    /// Prefer [`Ledger::load_checked`] where clobbering must be avoided.
    pub fn load(state_dir: &Path) -> Self {
        Self::load_checked(state_dir).unwrap_or_default()
    }

    /// Persist the ledger atomically (tmp file + rename) so a concurrent reader
    /// — or a crash mid-write — never observes a truncated/partial JSON file
    /// that would then parse-fail and be treated as corrupt.
    pub fn save(&self, state_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(state_dir)?;
        // A serialization failure is an error; nothing is written.
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let final_path = Self::path(state_dir);
        // Unique per writer (pid + per-process counter) so concurrent writers
        // never share a tmp file.
        let tmp_path = final_path.with_extension(format!(
            "json.tmp.{}.{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&tmp_path, json)?;
        if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
        Ok(())
    }

    /// Record `session_id`'s CUMULATIVE cost (its whole spend so far, as the
    /// Stop hook measures it) and return the updated `today` total.
    ///
    /// Each day holds only that day's increment for the session (#1553): the
    /// `today` entry is set to `cumulative` minus what days EARLIER than
    /// `today` already hold for the same session, so a session that crosses
    /// local midnight is not counted once under yesterday and again, in
    /// full, under today. Re-recording the same session on the same day
    /// overwrites its `today` entry (latest cumulative wins), never adds.
    /// Entries for days later than `today` are not subtracted.
    ///
    /// Fail-closed: when the earlier-day amount cannot be trusted as the
    /// session's prior spend — an earlier entry is negative or not finite,
    /// `cumulative` is not finite, or the earlier days hold MORE than
    /// `cumulative` — the `today` entry is the full `cumulative`. That
    /// over-counts today (the restrictive side for today's verdict) rather
    /// than under-counting it.
    pub fn record(&mut self, session_id: &str, today: &str, cumulative: f64) -> f64 {
        let today_cost = match self.earlier_days_cost(session_id, today) {
            Some(earlier) if cumulative.is_finite() && earlier <= cumulative => {
                cumulative - earlier
            }
            _ => cumulative,
        };
        let entry = self.days.entry(today.to_string()).or_default();
        entry.sessions.insert(session_id.to_string(), today_cost);
        entry.total()
    }

    /// Sum of `session_id`'s entries on days strictly before `today`.
    /// `None` when any of them is negative or not finite (the sum would not be
    /// a usable prior spend).
    fn earlier_days_cost(&self, session_id: &str, today: &str) -> Option<f64> {
        let mut sum = 0.0;
        use std::ops::Bound;
        let earlier = (Bound::Unbounded, Bound::Excluded(today));
        for (_, day) in self.days.range::<str, _>(earlier) {
            if let Some(&usd) = day.sessions.get(session_id) {
                if !(usd.is_finite() && usd >= 0.0) {
                    return None;
                }
                sum += usd;
            }
        }
        sum.is_finite().then_some(sum)
    }

    pub fn day_total(&self, date: &str) -> f64 {
        self.days.get(date).map(|e| e.total()).unwrap_or(0.0)
    }

    /// The session's recorded cost: the sum of its per-day entries across all
    /// days (each day holds only that day's increment, see [`Ledger::record`]),
    /// or `None` when it has no entry. Lets a reader reuse budgetguard's
    /// authoritative per-session USD instead of recomputing cost from tokens
    /// with a different override set.
    pub fn session_cost(&self, session_id: &str) -> Option<f64> {
        self.days
            .values()
            .filter_map(|d| d.sessions.get(session_id).copied())
            .reduce(|a, b| a + b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A throwaway temp dir (no external crate); removed on drop.
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "harness-core-ledger-{tag}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn record_sums_sessions_into_day_total() {
        let mut l = Ledger::default();
        assert_eq!(l.record("s1", "2026-06-27", 1.5), 1.5);
        // A different session adds to the same day.
        assert_eq!(l.record("s2", "2026-06-27", 2.0), 3.5);
        // Re-recording the SAME session overwrites (not adds) — latest cost wins.
        assert_eq!(l.record("s1", "2026-06-27", 4.0), 6.0);
        assert_eq!(l.day_total("2026-06-27"), 6.0);
        assert_eq!(l.day_total("2026-06-26"), 0.0);
    }

    #[test]
    fn session_cost_looks_up_across_days() {
        let mut l = Ledger::default();
        l.record("s1", "2026-06-26", 1.0);
        l.record("s1", "2026-06-27", 2.5); // later day wins
        l.record("s2", "2026-06-27", 9.0);
        assert_eq!(l.session_cost("s1"), Some(2.5));
        assert_eq!(l.session_cost("s2"), Some(9.0));
        assert_eq!(l.session_cost("missing"), None);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let d = TmpDir::new("roundtrip");
        let mut l = Ledger::default();
        l.record("s1", "2026-06-27", 3.25);
        l.save(d.path()).unwrap();
        let loaded = Ledger::load_checked(d.path()).unwrap();
        assert_eq!(loaded.day_total("2026-06-27"), 3.25);
    }

    #[test]
    fn absent_file_loads_as_default_not_error() {
        let d = TmpDir::new("absent");
        // Nothing written yet → Ok(default), not Err.
        let l = Ledger::load_checked(d.path()).unwrap();
        assert_eq!(l.day_total("2026-06-27"), 0.0);
    }

    #[test]
    fn corrupt_file_is_an_error_and_is_preserved() {
        let d = TmpDir::new("corrupt");
        let p = Ledger::path(d.path());
        std::fs::write(&p, b"{ this is not valid json").unwrap();

        // load_checked surfaces corruption rather than silently resetting.
        assert!(matches!(
            Ledger::load_checked(d.path()),
            Err(LoadError::Corrupt)
        ));
        // The caller (gate) must not have overwritten it — verify the bytes
        // are still the corrupt original (preserved, not reset to "{}").
        let after = std::fs::read_to_string(&p).unwrap();
        assert_eq!(after, "{ this is not valid json");

        // The lenient load() still degrades to default for back-compat.
        assert_eq!(Ledger::load(d.path()).day_total("x"), 0.0);
    }

    #[test]
    fn save_leaves_no_tmp_file_behind() {
        let d = TmpDir::new("notmp");
        Ledger::default().save(d.path()).unwrap();
        let tmp = Ledger::path(d.path()).with_extension("json.tmp");
        assert!(!tmp.exists(), "atomic save must rename, not leave a .tmp");
        assert!(Ledger::path(d.path()).exists());
    }
}
