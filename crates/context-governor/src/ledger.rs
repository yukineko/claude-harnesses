//! Append-only action ledger (§13), with size metrics.
//!
//! Distinct from `harness_core::ledger` (that one is budgetguard's *daily spend*
//! ledger). This records, for every hook decision, a single node — satisfying
//! I6 (observability): each hook judgement leaves exactly one
//! injected / groomed{saved} / snapshotted / pinned / recalled trace. Per turn
//! the governor records `resident_tokens`, `groom saved_tokens`, and the growth
//! slope, then ships them to the metrics sink (beacon/Langfuse) via
//! `harness_core::metrics::emit`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::types::{ItemId, StoreKey};
use harness_core::metrics;
use harness_core::store::{context_state_dir, safe_session};

/// Default upper bound on rows kept in `ledger.jsonl`. The ledger is append-only
/// and otherwise grows without limit on long sessions (one line per hook
/// decision). 50_000 rows is generous headroom — at the JSONL line sizes this
/// crate writes (~150–250B) that is on the order of ~10MB before any prune — yet
/// it caps a runaway session at a bounded, queryable size. Overridable via
/// `CONTEXT_GOVERNOR_LEDGER_MAX_ROWS` (see [`max_rows`]).
const DEFAULT_LEDGER_MAX_ROWS: usize = 50_000;

/// Run the prune at most once per this many appends. Counting + rewriting the
/// whole file on every single append would make `append` O(n) per call; gating it
/// behind a stride keeps the amortized cost flat while still bounding growth. The
/// file can transiently exceed the cap by at most this many rows between prunes,
/// which is acceptable for a best-effort GC.
const PRUNE_STRIDE: u64 = 256;

/// Resolve the row cap from the env (mirrors how the crate reads
/// `CONTEXT_GOVERNOR_STATE_DIR`): a valid, non-zero `CONTEXT_GOVERNOR_LEDGER_MAX_ROWS`
/// wins, otherwise [`DEFAULT_LEDGER_MAX_ROWS`]. A zero or unparseable value falls
/// back to the default rather than disabling the bound (best-effort, fail-safe).
fn max_rows() -> usize {
    std::env::var("CONTEXT_GOVERNOR_LEDGER_MAX_ROWS")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_LEDGER_MAX_ROWS)
}

/// What a hook did, with the size delta where one applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Reference body / pins injected beside the prompt (size: retrieval).
    Injected,
    /// A tool result trimmed — `saved_tokens` is the size reclaimed (I4).
    Groomed { saved_tokens: u32 },
    /// Transcript/verbatim externalized to the backing store (correctness).
    Snapshotted { to: StoreKey },
    /// A pin re-asserted into the final context (I1).
    Pinned,
    /// An externalized item pulled back in (lossless round-trip, I2).
    Recalled { from: StoreKey },
}

/// One append-only ledger node. `reason` is a `&'static str` so the cause is a
/// fixed vocabulary, not free text — the ledger stays queryable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerNode {
    pub session: String,
    pub hook: String,
    pub item: Option<ItemId>,
    pub action: Action,
    pub reason: &'static str,
}

/// Resolve the session-scoped `state_dir` and the `safe_session` string the same
/// way [`crate::backing::TranscriptBackingStore::open`] does, so the ledger lands
/// beside the backing store for the same project+session. Returns
/// `(state_dir, session)`.
fn resolve_state(cwd: &str) -> (PathBuf, String) {
    let sid = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
    resolve_state_for(cwd, &sid)
}

/// Explicit-session core of [`resolve_state`]: the `(state_dir, session)` for
/// `cwd` and the given session id `sid`, with no env read for the session. An
/// empty `sid` resolves to the `default` session (same as `context_state_dir`).
/// Callers that hold the hook input's `session_id` use this so the ledger they
/// read and write is the one named by the input, not by `CLAUDE_CODE_SESSION_ID`
/// (backlog 04f53dbe).
fn resolve_state_for(cwd: &str, sid: &str) -> (PathBuf, String) {
    // Delegate the drift-prone path assembly (base env + canonicalize +
    // project_key) to the shared `context_state_dir`, so the ledger writer, the
    // backing store, and the session-insights reader can never diverge. The
    // session string is returned alongside for row tagging.
    let state_dir = context_state_dir(Path::new(cwd), sid);
    let session = if sid.is_empty() {
        safe_session("default")
    } else {
        safe_session(sid)
    };
    (state_dir, session)
}

/// Durable JSONL sink for ledger nodes. Wraps [`harness_core::metrics::emit`]
/// appending to `<state_dir>/ledger.jsonl` — the same parallel-session-safe
/// substrate the backing store uses. Every method is best-effort and never
/// panics: a hook decision must never break a turn over its own bookkeeping.
pub struct Ledger {
    sink: PathBuf,
    session: String,
}

impl Ledger {
    /// Open (or lazily create) the ledger under `cwd`. Best-effort: directory
    /// creation failures degrade rather than propagate, so this returns `Self`
    /// directly — call sites stay panic-free.
    ///
    /// The session is taken from the env var `CLAUDE_CODE_SESSION_ID`. Callers
    /// that hold the hook input's `session_id` should use
    /// [`Ledger::open_session`] instead so the row lands in that session's ledger.
    pub fn open(cwd: &str) -> Self {
        let (state_dir, session) = resolve_state(cwd);
        Self::from_state(state_dir, session)
    }

    /// Open the ledger of the explicitly named session `sid` under `cwd`, with
    /// no env read for the session. Rows are tagged with `safe_session(sid)`,
    /// the same session that scopes the sink path. Best-effort like [`open`].
    ///
    /// [`open`]: Ledger::open
    pub fn open_session(cwd: &str, sid: &str) -> Self {
        let (state_dir, session) = resolve_state_for(cwd, sid);
        Self::from_state(state_dir, session)
    }

    fn from_state(state_dir: PathBuf, session: String) -> Self {
        let _ = std::fs::create_dir_all(&state_dir);
        let sink = state_dir.join("ledger.jsonl");
        Self { sink, session }
    }

    /// Append one node as a single JSONL line via [`metrics::emit`]. `resident_tokens`
    /// is the caller's per-action estimate of post-action window occupancy; the
    /// ledger records whatever it is given. Best-effort — never panics or unwraps.
    pub fn append(&self, node: &LedgerNode, resident_tokens: u32) {
        let (event, saved): (&str, u32) = match node.action {
            Action::Injected => ("injected", 0),
            Action::Groomed { saved_tokens } => ("groomed", saved_tokens),
            Action::Snapshotted { .. } => ("snapshotted", 0),
            Action::Pinned => ("pinned", 0),
            Action::Recalled { .. } => ("recalled", 0),
        };

        let extra = json!({
            "saved_tokens": saved,
            "resident_tokens": resident_tokens,
            "hook": node.hook,
            "reason": node.reason,
            "item": node.item.map(|i| i.0),
        });

        // Use the resolved session that scopes `sink` (env-derived for `open`,
        // explicit for `open_session`) rather than `node.session`, so every line
        // in this file shares one canonical session id.
        metrics::emit(&self.sink, &self.session, event, extra);

        // Best-effort GC after the write. Gated on a cheap stat so the O(n)
        // read-rewrite only runs once the file is plausibly over cap, never on the
        // common small-file path. Each hook runs in a fresh process (no surviving
        // in-memory counter), so the gate is derived from on-disk file state, not a
        // call count. Any failure is swallowed — a prune must never break a turn.
        let cap = max_rows();
        if file_could_exceed(&self.sink, cap) {
            prune_jsonl(&self.sink, cap);
        }
    }
}

/// Cheap stat-based gate for whether `sink` is large enough to *possibly* hold
/// more than `max_rows` lines. JSONL rows this crate writes are well over
/// `MIN_ROW_BYTES`, so once the file exceeds `max_rows * MIN_ROW_BYTES` it cannot
/// be under cap and a real count is worthwhile; below that it provably cannot
/// exceed the cap and we skip the read entirely. A `metadata` failure returns
/// `false` (skip), keeping the hot path free of errors. The `+ PRUNE_STRIDE` slack
/// lets the file drift a little past the cap between prunes so we don't rewrite on
/// every single append once we're near the boundary (amortized cost).
fn file_could_exceed(sink: &Path, max_rows: usize) -> bool {
    /// Floor on a single JSONL row's byte length, including the trailing newline.
    /// Smaller than any real `metrics::emit` line (which always carries the
    /// `ts`/`session`/`event`/`saved_tokens`/`resident_tokens`/`hook`/`reason`
    /// keys), so the gate never skips a file that is actually over cap.
    const MIN_ROW_BYTES: u64 = 16;
    let Ok(meta) = std::fs::metadata(sink) else {
        return false;
    };
    let trigger_rows = (max_rows as u64).saturating_add(PRUNE_STRIDE);
    meta.len() >= trigger_rows.saturating_mul(MIN_ROW_BYTES)
}

/// Best-effort prune of a ledger JSONL file at `sink` to its most recent
/// `max_rows` non-empty lines. Pure function of `(path, max_rows)` — no env — so
/// it is deterministic and unit-testable like [`summarize_jsonl`]. Semantics:
///
/// - Under cap (`rows <= max_rows`) or a missing/unreadable file: no change.
/// - Over cap: rewrite the file keeping the LAST `max_rows` lines (the most recent
///   entries), each terminated by a newline.
///
/// Fail-soft throughout: a read error, a write-tempfile error, or a rename error
/// all leave the original file untouched rather than panicking or truncating it.
/// The rewrite goes via a sibling tempfile + atomic rename so a crash mid-prune
/// never leaves a half-written ledger.
fn prune_jsonl(sink: &Path, max_rows: usize) {
    let Ok(contents) = std::fs::read_to_string(sink) else {
        return;
    };

    let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= max_rows {
        // Under cap (or exactly at it): nothing to do.
        return;
    }

    let keep = &lines[lines.len() - max_rows..];
    let mut out = String::with_capacity(contents.len());
    for line in keep {
        out.push_str(line);
        out.push('\n');
    }

    // Atomic-ish replace via a sibling tempfile so a failure never corrupts the
    // existing ledger. Best-effort: bail (leaving the original intact) on any error.
    let tmp = sink.with_extension("jsonl.prune.tmp");
    if std::fs::write(&tmp, out.as_bytes()).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, sink).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Deterministic rollup of a session's ledger. `per_event` is a `BTreeMap` so the
/// summary serializes in a stable key order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerSummary {
    /// Sum of `saved_tokens` across every recorded row.
    pub total_saved_tokens: u64,
    /// Total number of rows parsed.
    pub rows: u64,
    /// Count of rows per `event` name, in deterministic key order.
    pub per_event: BTreeMap<String, u64>,
}

/// Read `<state_dir>/ledger.jsonl` for `cwd` and aggregate it. Fail-soft: a
/// missing file yields an empty summary, and corrupt/partial lines are skipped
/// rather than panicking.
pub fn rollup(cwd: &str) -> LedgerSummary {
    let (state_dir, _session) = resolve_state(cwd);
    summarize_jsonl(&state_dir.join("ledger.jsonl"))
}

/// Aggregate a ledger JSONL file at `sink`. Pure function of the path — no env —
/// so callers (and tests) get a deterministic result. Fail-soft as above.
fn summarize_jsonl(sink: &Path) -> LedgerSummary {
    let mut summary = LedgerSummary::default();
    let Ok(contents) = std::fs::read_to_string(sink) else {
        return summary;
    };

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        summary.rows += 1;
        summary.total_saved_tokens += value
            .get("saved_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if let Some(event) = value.get("event").and_then(|v| v.as_str()) {
            *summary.per_event.entry(event.to_string()).or_insert(0) += 1;
        }
    }

    summary
}

/// Fail-soft seen-state query for the injector's dedup (I6 observe→act, read
/// half): returns `true` iff this session's ledger already records an `injected`
/// row whose `item` equals `item_id`. A missing/corrupt file or absent match
/// yields `false`, so the caller treats "unknown" as "not yet injected" and
/// proceeds — the ledger becomes the control input without ever breaking a turn.
///
/// The session is taken from the env var `CLAUDE_CODE_SESSION_ID`; see
/// [`was_injected_in_session`] for the explicit-session form the injector uses.
pub fn was_injected(cwd: &str, item_id: u64) -> bool {
    let (state_dir, _session) = resolve_state(cwd);
    seen_injected_in(&state_dir.join("ledger.jsonl"), item_id)
}

/// [`was_injected`] against the ledger of the explicitly named session `sid`
/// (no env read for the session), so the dedup is answered by the same ledger
/// [`Ledger::open_session`] writes for that session. Same fail-soft semantics.
pub fn was_injected_in_session(cwd: &str, sid: &str, item_id: u64) -> bool {
    let (state_dir, _session) = resolve_state_for(cwd, sid);
    seen_injected_in(&state_dir.join("ledger.jsonl"), item_id)
}

/// Path-pinned core of [`was_injected`]: pure function of `sink` (no env), so it
/// is deterministic and unit-testable the same way [`summarize_jsonl`] is.
/// Fail-soft: a missing/corrupt file or absent match yields `false`.
fn seen_injected_in(sink: &Path, item_id: u64) -> bool {
    let Ok(contents) = std::fs::read_to_string(sink) else {
        return false;
    };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let is_injected = value.get("event").and_then(|v| v.as_str()) == Some("injected");
        let item_matches = value.get("item").and_then(|v| v.as_u64()) == Some(item_id);
        if is_injected && item_matches {
            return true;
        }
    }
    false
}

/// Snapshot-lever seen-state reader (the `snapshot` half of observe→act): returns
/// the `resident_tokens` of this session's most recent `snapshotted` row, or
/// `None` when the ledger holds no such row. The snapshot's resident size is a
/// monotone proxy for transcript growth, so it is the control input that gates
/// redundant same-band re-snapshots. A missing/corrupt file yields `None`, so the
/// caller treats "unknown" as "nothing snapshotted yet" and falls back to the
/// plain threshold gate — the ledger becomes a control input without ever breaking
/// a turn.
pub fn last_snapshotted_resident(cwd: &str) -> Option<u64> {
    let (state_dir, _session) = resolve_state(cwd);
    last_snapshotted_in(&state_dir.join("ledger.jsonl"))
}

/// Path-pinned core of [`last_snapshotted_resident`]: pure function of `sink` (no
/// env), so it is deterministic and unit-testable. Scans for the LAST `snapshotted`
/// row and returns its `resident_tokens`. Fail-soft: a missing/corrupt file or an
/// absence of any `snapshotted` row yields `None`.
fn last_snapshotted_in(sink: &Path) -> Option<u64> {
    let contents = std::fs::read_to_string(sink).ok()?;
    let mut latest = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let is_snapshotted = value.get("event").and_then(|v| v.as_str()) == Some("snapshotted");
        if is_snapshotted {
            if let Some(resident) = value.get("resident_tokens").and_then(|v| v.as_u64()) {
                latest = Some(resident);
            }
        }
    }
    latest
}

#[cfg(test)]
mod tests {
    use super::*;

    // Deliberately env-FREE. `CONTEXT_GOVERNOR_STATE_DIR` / `CLAUDE_CODE_SESSION_ID`
    // are process-global; a sibling unit test (`backing::open_is_ok…`) already
    // asserts on a base it sets in that var, so mutating it here would race and
    // flake *that* frozen test. Instead we construct `Ledger` directly against a
    // private tempdir (the same discipline `backing::tests::temp_store` uses) and
    // assert on the path-pinned summary core `summarize_jsonl`, which is the exact
    // logic `rollup` runs. `Ledger::open` + `rollup` are read-only over the env, so
    // they are smoke-called (no panic) without mutating anything global.
    #[test]
    fn append_and_rollup_roundtrip() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Missing file → empty summary (nonexistent path, no env).
        assert_eq!(
            summarize_jsonl(Path::new("/tmp/ledger-sink-absent-xyz/ledger.jsonl")),
            LedgerSummary::default()
        );

        // Construct directly: env-immune, never touches a real $HOME or the shared
        // state-dir var. Mirrors `backing::tests::temp_store`.
        let ledger = Ledger {
            sink: tmp.path().join("ledger.jsonl"),
            session: "S1".to_string(),
        };

        let groomed = LedgerNode {
            session: "S1".to_string(),
            hook: "PostToolUse".to_string(),
            item: Some(ItemId(7)),
            action: Action::Groomed { saved_tokens: 50 },
            reason: "oversized-tool-result",
        };
        let injected = LedgerNode {
            session: "S1".to_string(),
            hook: "UserPromptSubmit".to_string(),
            item: None,
            action: Action::Injected,
            reason: "pin-reinjection",
        };

        ledger.append(&groomed, 1200);
        ledger.append(&injected, 1180);

        let contents = std::fs::read_to_string(&ledger.sink).expect("ledger file");
        let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2, "one JSONL line per appended node");
        for line in &lines {
            assert!(
                line.contains("saved_tokens"),
                "line carries saved_tokens: {line}"
            );
            assert!(
                line.contains("resident_tokens"),
                "line carries resident_tokens: {line}"
            );
        }

        // Numeric assertions against the path-pinned core — the exact logic rollup runs.
        let summary = summarize_jsonl(&ledger.sink);
        assert_eq!(summary.total_saved_tokens, 50);
        assert_eq!(summary.rows, 2);
        assert_eq!(summary.per_event.get("groomed"), Some(&1));
        assert_eq!(summary.per_event.get("injected"), Some(&1));

        // Smoke-exercise the public env-derived entry points: they read the env
        // (never mutate it) and must never panic, even on a never-written cwd.
        let _ = Ledger::open("/tmp/ledger-sink-smoke-cwd");
        let _ = rollup("/tmp/ledger-sink-smoke-cwd");
    }

    /// `seen_injected_in` (the env-free core of `was_injected`) detects a prior
    /// `injected` row by its `item` id, ignores non-injected rows with the same
    /// id, and fail-softs on an absent file / no match.
    #[test]
    fn seen_injected_in_matches_injected_row_by_item() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sink = tmp.path().join("ledger.jsonl");

        // Missing file → false (fail-soft).
        assert!(!seen_injected_in(&sink, 42));

        let ledger = Ledger {
            sink: sink.clone(),
            session: "S1".to_string(),
        };

        // A groomed row carrying item=42 must NOT match (event != injected).
        ledger.append(
            &LedgerNode {
                session: "S1".to_string(),
                hook: "PostToolUse".to_string(),
                item: Some(ItemId(42)),
                action: Action::Groomed { saved_tokens: 10 },
                reason: "oversized-tool-result",
            },
            100,
        );
        assert!(
            !seen_injected_in(&sink, 42),
            "a groomed row must not count as injected"
        );

        // An injected row with item=42 → matches; a different id does not.
        ledger.append(
            &LedgerNode {
                session: "S1".to_string(),
                hook: "UserPromptSubmit".to_string(),
                item: Some(ItemId(42)),
                action: Action::Injected,
                reason: "reference-injection",
            },
            80,
        );
        assert!(
            seen_injected_in(&sink, 42),
            "injected row with matching item must be seen"
        );
        assert!(
            !seen_injected_in(&sink, 7),
            "a different item id must not match"
        );
    }

    /// `last_snapshotted_in` (the env-free core of `last_snapshotted_resident`)
    /// returns the resident of the LAST `snapshotted` row, ignores non-snapshot
    /// rows, and fail-softs to `None` on an absent file / no snapshot row.
    #[test]
    fn last_snapshotted_in_returns_latest_snapshot_resident() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sink = tmp.path().join("ledger.jsonl");

        // Missing file → None (fail-soft).
        assert_eq!(last_snapshotted_in(&sink), None);

        let ledger = Ledger {
            sink: sink.clone(),
            session: "S1".to_string(),
        };

        // An injected row (not a snapshot) must not register.
        ledger.append(
            &LedgerNode {
                session: "S1".to_string(),
                hook: "UserPromptSubmit".to_string(),
                item: Some(ItemId(1)),
                action: Action::Injected,
                reason: "reference-injection",
            },
            999,
        );
        assert_eq!(
            last_snapshotted_in(&sink),
            None,
            "an injected row must not count as a snapshot"
        );

        // First snapshot row → its resident is returned.
        ledger.append(
            &LedgerNode {
                session: "S1".to_string(),
                hook: "Stop".to_string(),
                item: Some(ItemId(StoreKey(1).0)),
                action: Action::Snapshotted { to: StoreKey(1) },
                reason: "stop-checkpoint",
            },
            4000,
        );
        assert_eq!(last_snapshotted_in(&sink), Some(4000));

        // A later snapshot row supersedes the earlier one (LAST wins).
        ledger.append(
            &LedgerNode {
                session: "S1".to_string(),
                hook: "Stop".to_string(),
                item: Some(ItemId(StoreKey(2).0)),
                action: Action::Snapshotted { to: StoreKey(2) },
                reason: "stop-checkpoint",
            },
            9500,
        );
        assert_eq!(last_snapshotted_in(&sink), Some(9500));
    }

    /// `prune_jsonl` (the env-free core of the GC) caps an over-cap ledger to its
    /// most-recent `max_rows` lines, leaves an under-cap ledger byte-identical, and
    /// fail-softs on a missing file.
    #[test]
    fn prune_jsonl_caps_to_most_recent_rows() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sink = tmp.path().join("ledger.jsonl");

        // Missing file → no panic, no file created.
        prune_jsonl(&sink, 3);
        assert!(!sink.exists(), "prune must not create a missing file");

        // Write 10 ordered rows (a stand-in JSONL; prune is schema-agnostic, it
        // keeps lines, not parsed nodes).
        let mut body = String::new();
        for i in 0..10u32 {
            body.push_str(&format!("{{\"event\":\"e\",\"n\":{i}}}\n"));
        }
        std::fs::write(&sink, &body).expect("seed ledger");

        // Over cap: keep the LAST 3 rows (the most recent), within cap, in order.
        prune_jsonl(&sink, 3);
        let after = std::fs::read_to_string(&sink).expect("read pruned");
        let lines: Vec<&str> = after.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 3, "pruned to cap");
        assert!(
            lines[0].contains("\"n\":7"),
            "oldest kept is n=7: {lines:?}"
        );
        assert!(
            lines[2].contains("\"n\":9"),
            "newest kept is n=9: {lines:?}"
        );
        // The summary core still parses the pruned file cleanly.
        assert_eq!(summarize_jsonl(&sink).rows, 3);

        // Under cap: a second prune at a larger cap is a no-op (byte-identical).
        let before = std::fs::read_to_string(&sink).expect("read before");
        prune_jsonl(&sink, 100);
        let unchanged = std::fs::read_to_string(&sink).expect("read after");
        assert_eq!(
            before, unchanged,
            "under-cap prune leaves the file untouched"
        );

        // No tempfile left behind.
        assert!(
            !sink.with_extension("jsonl.prune.tmp").exists(),
            "prune must not leave its tempfile"
        );
    }

    /// `append` runs the threshold-gated prune so a real ledger stays bounded.
    /// Drive it deterministically by setting the cap via the env var (serialised
    /// against the other env-mutating tests through the crate-shared lock), then
    /// append past `cap + PRUNE_STRIDE` and assert the file converged near the cap.
    #[test]
    fn append_prunes_when_over_env_cap() {
        let _env = crate::defaults::guard::acquire_env_lock();
        std::env::set_var("CONTEXT_GOVERNOR_LEDGER_MAX_ROWS", "4");

        let tmp = tempfile::tempdir().expect("tempdir");
        let ledger = Ledger {
            sink: tmp.path().join("ledger.jsonl"),
            session: "S1".to_string(),
        };
        let node = LedgerNode {
            session: "S1".to_string(),
            hook: "PostToolUse".to_string(),
            item: None,
            action: Action::Pinned,
            reason: "pin-reinjection",
        };

        // Append well past cap + PRUNE_STRIDE so the stat gate certainly fires.
        let total = 4 + PRUNE_STRIDE as usize + 50;
        for _ in 0..total {
            ledger.append(&node, 100);
        }

        std::env::remove_var("CONTEXT_GOVERNOR_LEDGER_MAX_ROWS");

        // After the gated prune, the file is bounded: at the cap, and never more
        // than one stride past it (the slack the gate allows between prunes).
        let rows = summarize_jsonl(&ledger.sink).rows;
        assert!(rows >= 4, "keeps at least the cap of recent rows: {rows}");
        assert!(
            rows <= 4 + PRUNE_STRIDE,
            "append keeps the ledger bounded near cap: {rows}"
        );
    }
}
