use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

/// The task status vocabulary — the single source of truth shared by the store
/// (which sets these on add/done/fail/restore), the `--status` filter help, and
/// the CLI's validation of a user-supplied filter. A task moves
/// `pending → done` (done) or `pending → failed` (fail); a deferred task is
/// restored to `pending` once its `defer_until` elapses. NB: `backlog` has no
/// `open` status — that vocabulary belongs to `hypothesis` (open/validated/
/// rejected), a different binary.
pub const STATUS_PENDING: &str = "pending";
pub const STATUS_DONE: &str = "done";
pub const STATUS_FAILED: &str = "failed";

/// All recognised status values, in lifecycle order. Used to enumerate the
/// valid `--status` arguments in help/validation so an unknown value is a loud
/// error instead of a silently-empty result.
pub const STATUSES: [&str; 3] = [STATUS_PENDING, STATUS_DONE, STATUS_FAILED];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub project: String,
    /// True when `project` above is a FALLBACK GUESS rather than a resolved
    /// identity: the write that created this task named a location that really
    /// exists but whose canonical project identity could not be determined (a
    /// dangling worktree `.git` link, an unreadable gitfile, a path whose very
    /// existence could not be checked), so
    /// `store::canonicalize_project_with_marker` substituted the plain
    /// git-toplevel label instead of blocking the write.
    ///
    /// NOT set for a path that is definitively ABSENT on this machine (a
    /// cross-machine label such as `C:/Users/.../harness`, or another
    /// checkout's path): nothing was guessed there — the caller's explicit
    /// label was adopted verbatim and every checkout normalizes it the same
    /// way, so it cannot go missing the way a guess can. See
    /// `store::canonicalize_project_with_marker`'s doc comment.
    ///
    /// It exists because the degrade is otherwise invisible: a guessed label is
    /// byte-identical in shape to a resolved one, so another checkout's default
    /// (project-filtered) `list` would drop the task with no diagnostic —
    /// "cannot determine" collapsing into "nothing here" (CLAUDE.md §3).
    /// `store::list` therefore keeps a task with this flag set even when its
    /// `project` does not match the filter, and `main.rs` renders it marked
    /// `unresolved`.
    ///
    /// Absent in tasks.toml files written before this field existed;
    /// `#[serde(default)]` loads those as `false`. `false` is the correct
    /// default here even though this repo normally defaults to the restrictive
    /// side: the absent field means "the writing binary never recorded either
    /// way", and treating every pre-existing task as a guess would flag the
    /// whole store and leave the marker carrying no signal at all.
    #[serde(default)]
    pub project_unresolved: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    pub status: String,
    #[serde(default)]
    pub notes: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Unix timestamp (seconds) before which this task is deferred.
    /// Absent in older tasks.toml files; treated as None (not deferred).
    #[serde(default)]
    pub defer_until: Option<i64>,
    /// Ordering weight (higher = surfaced sooner within the same priority tier).
    /// Carries a compass opportunity's weight so the source layer's queue order
    /// is driven by opportunity impact, not just priority + insertion time.
    /// Absent in older tasks.toml files; `#[serde(default)]` makes those load as
    /// 0.0, which preserves the legacy `(priority, created_at)` order exactly
    /// (all-equal weight → tie-break falls through to created_at).
    #[serde(default)]
    pub weight: f64,
    /// GitHub issue number this task was pushed to (Phase1: one-way push via
    /// `gh issue create`). Absent in older tasks.toml files; treated as None
    /// (not yet pushed).
    #[serde(default)]
    pub issue_number: Option<u64>,
    /// GitHub issue URL this task was pushed to. Absent in older tasks.toml
    /// files; treated as None (not yet pushed).
    #[serde(default)]
    pub issue_url: Option<String>,
    /// When this task's GitHub issue was observed **closed** by us (unix
    /// seconds), or `None` if it has not been.
    ///
    /// This field is deliberately the *only* record that the close happened,
    /// and its absence is what drives the retry. A close can fail for reasons
    /// that have nothing to do with the task being finished (gh absent, no
    /// network, auth expired, rate limit). Recording "done" locally while
    /// silently dropping that failure is how 60 finished tasks came to sit
    /// behind 60 still-open issues (measured 2026-08-14): the local store said
    /// the work was over and nothing anywhere said the mirror had not been
    /// updated. So the failure is not stored as a flag that could itself be
    /// wrong — it is stored as the *absence* of a confirmation, which
    /// `store::sync_plan` re-derives from scratch on every run. A close that
    /// did not happen is therefore indistinguishable from one never attempted,
    /// and both are retried, which is the safe direction: closing an
    /// already-closed issue is a no-op, whereas a lost close is invisible
    /// forever.
    #[serde(default)]
    pub issue_closed_at: Option<i64>,
}

impl Task {
    /// Returns priority derived from tags: "p0"→0, "p1"→1, "p2"→2, none→3.
    pub fn priority(&self) -> u8 {
        for tag in &self.tags {
            match tag.as_str() {
                "p0" => return 0,
                "p1" => return 1,
                "p2" => return 2,
                _ => {}
            }
        }
        3
    }

    /// Returns the first tag starting with "cycle:", if any.
    pub fn cycle_tag(&self) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.starts_with("cycle:"))
            .map(|t| t.as_str())
    }

    /// Returns true if status is "pending" or "failed".
    /// Note: does NOT consider defer_until. Callers combine with is_deferred()
    /// to decide whether to surface a task.
    pub fn is_pending(&self) -> bool {
        matches!(self.status.as_str(), STATUS_PENDING | STATUS_FAILED)
    }

    /// Returns true when the task is deferred past the given unix timestamp.
    /// A task with defer_until = None is never considered deferred.
    pub fn is_deferred(&self, now: i64) -> bool {
        matches!(self.defer_until, Some(t) if t > now)
    }
}

/// Returns a warning message when `status` is `Some` but not a recognised value,
/// else `None`. Centralised here (next to `STATUSES`) so the `--status` filter in
/// `main` can warn loudly — a typo'd status silently matched nothing ("no
/// tasks"), indistinguishable from a genuinely empty queue. The message names the
/// offending value and lists the valid ones.
pub fn status_warning(status: Option<&str>) -> Option<String> {
    match status {
        Some(s) if !STATUSES.contains(&s) => Some(format!(
            "warning: unknown status '{s}'; valid values are {}",
            STATUSES.join(" | ")
        )),
        _ => None,
    }
}

/// Generate an 8-char hex ID from title and unix timestamp using FNV-1a 32-bit
/// (the shared `harness_core::hash` implementation).
pub fn new_id(title: &str, now: i64) -> String {
    let input = format!("{}\x00{}", title, now);
    harness_core::hash::fnv1a32_hex(&input)
}

/// Normalize a title for content-hashing: trim → Unicode NFKC → lowercase →
/// collapse any run of whitespace to a single space → strip leading/trailing
/// punctuation. This makes trivially-different phrasings of the same task
/// (extra spaces, casing, a trailing "!") collapse to the same normalized
/// string, so [`hashkey`] collides on them.
fn normalize_title(title: &str) -> String {
    let nfkc: String = title.trim().nfkc().collect();
    let lower = nfkc.to_lowercase();
    let collapsed = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .trim_matches(|c: char| !c.is_alphanumeric() && !c.is_whitespace())
        .to_string()
}

/// Content key for cross-session dedup: a title+project fingerprint that is
/// ROBUST to trivial phrasing differences (whitespace, case, punctuation),
/// so the same underlying task always yields the same key regardless of who
/// typed it or how. Uses the shared `harness_core::hash::fnv1a64` (64-bit
/// FNV-1a), formatted as 16 lowercase hex digits. `\u{1f}` (unit separator)
/// joins the normalized title and the raw project path so two different
/// projects with the same title never collide.
pub fn hashkey(title: &str, project: &str) -> String {
    let norm_title = normalize_title(title);
    let input = format!("{}\u{1f}{}", norm_title, project);
    format!("{:016x}", harness_core::hash::fnv1a64(input.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_task(tags: Vec<&str>, status: &str) -> Task {
        Task {
            id: "00000000".to_string(),
            title: "test".to_string(),
            project: "/tmp/proj".to_string(),
            project_unresolved: false,
            tags: tags.into_iter().map(|s| s.to_string()).collect(),
            status: status.to_string(),
            notes: String::new(),
            created_at: 0,
            updated_at: 0,
            defer_until: None,
            weight: 0.0,
            issue_number: None,
            issue_url: None,
            issue_closed_at: None,
        }
    }

    #[test]
    fn status_vocabulary_is_consistent() {
        // The set, lifecycle order, and the values the store actually writes
        // (add → pending, done → done, fail → failed) must agree, since
        // STATUSES drives both the `--status` help/validation and `is_pending`.
        assert_eq!(STATUSES, [STATUS_PENDING, STATUS_DONE, STATUS_FAILED]);
        assert_eq!(STATUSES, ["pending", "done", "failed"]);
        // `open` is hypothesis's vocabulary, never backlog's.
        assert!(!STATUSES.contains(&"open"));
        // is_pending agrees with the vocabulary it filters on.
        assert!(make_task(vec![], STATUS_PENDING).is_pending());
        assert!(make_task(vec![], STATUS_FAILED).is_pending());
        assert!(!make_task(vec![], STATUS_DONE).is_pending());
    }

    #[test]
    fn status_warning_flags_unknown_and_suggests_valid() {
        // An invalid filter (the classic typo: hypothesis's `open`) must warn and
        // enumerate the valid values so the empty result isn't mistaken for an
        // empty queue.
        let w = status_warning(Some("open")).expect("unknown status warns");
        assert!(w.contains("unknown status 'open'"), "got: {w}");
        assert!(w.contains("pending"), "must suggest valid values: {w}");
        assert!(w.contains("done"), "must suggest valid values: {w}");
        assert!(w.contains("failed"), "must suggest valid values: {w}");
    }

    #[test]
    fn status_warning_silent_for_valid_or_absent() {
        assert!(status_warning(Some("pending")).is_none());
        assert!(status_warning(Some("done")).is_none());
        assert!(status_warning(Some("failed")).is_none());
        // No filter at all → no warning (listing everything is legitimate).
        assert!(status_warning(None).is_none());
    }

    #[test]
    fn priority_p0() {
        assert_eq!(
            make_task(vec!["p0", "cycle:test-fix"], "pending").priority(),
            0
        );
    }

    #[test]
    fn priority_p1() {
        assert_eq!(make_task(vec!["p1"], "pending").priority(), 1);
    }

    #[test]
    fn priority_p2() {
        assert_eq!(make_task(vec!["p2"], "pending").priority(), 2);
    }

    #[test]
    fn priority_none() {
        assert_eq!(make_task(vec![], "pending").priority(), 3);
    }

    #[test]
    fn cycle_tag_found() {
        let t = make_task(vec!["p1", "cycle:test-fix"], "pending");
        assert_eq!(t.cycle_tag(), Some("cycle:test-fix"));
    }

    #[test]
    fn cycle_tag_none() {
        let t = make_task(vec!["p1"], "pending");
        assert_eq!(t.cycle_tag(), None);
    }

    #[test]
    fn is_pending_true_for_pending_and_failed() {
        assert!(make_task(vec![], "pending").is_pending());
        assert!(make_task(vec![], "failed").is_pending());
    }

    #[test]
    fn is_pending_false_for_others() {
        assert!(!make_task(vec![], "running").is_pending());
        assert!(!make_task(vec![], "done").is_pending());
    }

    #[test]
    fn new_id_returns_8_hex_chars() {
        let id = new_id("hello", 1234567890);
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn new_id_is_deterministic() {
        assert_eq!(new_id("task", 100), new_id("task", 100));
    }

    #[test]
    fn new_id_differs_for_different_inputs() {
        assert_ne!(new_id("task-a", 100), new_id("task-b", 100));
        assert_ne!(new_id("task", 100), new_id("task", 101));
    }

    // --- hashkey (content key for cross-session dedup) ---

    #[test]
    fn hashkey_is_16_hex_chars() {
        let h = hashkey("Fix login", "/repo");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hashkey_normalizes_trivial_phrasing_differences() {
        // Whitespace, casing, punctuation must not matter — same underlying
        // task, same key.
        assert_eq!(hashkey("Fix login", "/r"), hashkey("  fix   LOGIN! ", "/r"));
    }

    #[test]
    fn hashkey_differs_across_projects() {
        assert_ne!(
            hashkey("Fix login", "/repo-a"),
            hashkey("Fix login", "/repo-b")
        );
    }

    #[test]
    fn hashkey_differs_for_different_titles() {
        assert_ne!(
            hashkey("Fix login", "/repo"),
            hashkey("Fix logout", "/repo")
        );
    }

    // --- is_deferred tests ---

    #[test]
    fn is_deferred_none_is_never_deferred() {
        let t = make_task(vec![], "pending");
        assert!(!t.is_deferred(0));
        assert!(!t.is_deferred(9_999_999_999));
    }

    #[test]
    fn is_deferred_future_timestamp_returns_true() {
        let mut t = make_task(vec![], "pending");
        t.defer_until = Some(2_000);
        // now = 1_000 < 2_000  →  deferred
        assert!(t.is_deferred(1_000));
    }

    #[test]
    fn is_deferred_past_timestamp_returns_false() {
        let mut t = make_task(vec![], "pending");
        t.defer_until = Some(500);
        // now = 1_000 >= 500  →  not deferred
        assert!(!t.is_deferred(1_000));
    }

    #[test]
    fn is_deferred_equal_timestamp_returns_false() {
        let mut t = make_task(vec![], "pending");
        t.defer_until = Some(1_000);
        // defer_until == now  →  not deferred (> is strict)
        assert!(!t.is_deferred(1_000));
    }

    #[test]
    fn is_pending_unaffected_by_defer_until() {
        // is_pending must ignore defer_until; callers decide with is_deferred()
        let mut t = make_task(vec![], "pending");
        t.defer_until = Some(9_999_999_999);
        assert!(t.is_pending());
    }

    #[test]
    fn serde_roundtrip_without_defer_until() {
        // Older tasks.toml records that lack defer_until must deserialize fine.
        let json = r#"{
            "id": "abcd1234",
            "title": "old task",
            "project": "/tmp/p",
            "tags": [],
            "status": "pending",
            "notes": "",
            "created_at": 0,
            "updated_at": 0
        }"#;
        let t: Task = serde_json::from_str(json).expect("deserialize without defer_until");
        assert!(t.defer_until.is_none());
        // weight is also absent in this legacy record → defaults to 0.0,
        // which keeps legacy tasks ordering identically to before.
        assert_eq!(t.weight, 0.0);
    }

    #[test]
    fn serde_roundtrip_without_issue_fields() {
        // Older tasks.toml records that lack issue_number/issue_url must
        // deserialize fine (backward compat for the GitHub push feature).
        let json = r#"{
            "id": "abcd1234",
            "title": "old task",
            "project": "/tmp/p",
            "tags": [],
            "status": "pending",
            "notes": "",
            "created_at": 0,
            "updated_at": 0
        }"#;
        let t: Task = serde_json::from_str(json).expect("deserialize without issue fields");
        assert!(t.issue_number.is_none());
        assert!(t.issue_url.is_none());
    }

    // =============================================================================

    /// Back-compat: a tasks.toml record written before `issue_closed_at`
    /// existed must load as `None` — i.e. "we have no confirmation that this
    /// issue was closed", which is what drives the retry. A `#[serde(default)]`
    /// on an `Option` is the right default here precisely because the
    /// restrictive reading (retry the close; closing twice is a no-op) IS the
    /// `None` reading.
    /// Dies if `#[serde(default)]` is dropped from `issue_closed_at` (every
    /// legacy record then fails to deserialize — the whole store stops
    /// loading).
    #[test]
    fn serde_roundtrip_without_issue_closed_at() {
        let json = r#"{
            "id": "abcd1234",
            "title": "old task",
            "project": "/tmp/p",
            "tags": [],
            "status": "done",
            "notes": "",
            "created_at": 0,
            "updated_at": 0,
            "issue_number": 42,
            "issue_url": "https://github.com/owner/repo/issues/42"
        }"#;
        let t: Task = serde_json::from_str(json).expect("deserialize without issue_closed_at");
        assert_eq!(t.issue_number, Some(42));
        assert!(
            t.issue_closed_at.is_none(),
            "an absent issue_closed_at must read as None (unconfirmed), so the close is retried"
        );
    }

    /// A record that DOES carry the field round-trips through serde with the
    /// value intact — the stamp must survive a save/load cycle or every sync
    /// re-closes every issue.
    /// Dies if the field is marked `#[serde(skip)]`.
    #[test]
    fn issue_closed_at_roundtrips_through_serde() {
        let mut t = make_task(vec![], "done");
        t.issue_number = Some(42);
        t.issue_closed_at = Some(1_800_000_000);
        let json = serde_json::to_string(&t).unwrap();
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(back.issue_closed_at, Some(1_800_000_000));
        assert_eq!(back.issue_number, Some(42));
    }
}
