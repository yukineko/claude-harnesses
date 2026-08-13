/// Event aggregation and state projection.
use crate::store::{self, LeaseRegistry};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Liveness status of a lease.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LeaseInfo {
    /// Task content key.
    pub key: String,
    /// Human-readable task title.
    pub title: String,
    /// Time since last heartbeat in seconds.
    pub heartbeat_age_secs: i64,
    /// Is this lease stale (past TTL)?
    pub is_stale: bool,
}

/// Session roster: all leases for one session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionRoster {
    /// Session ID.
    pub session_id: String,
    /// Leases in this session.
    pub leases: Vec<LeaseInfo>,
    /// Live lease count.
    pub live_count: usize,
}

/// Backlog summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BacklogSummary {
    /// Total pending items.
    pub pending: usize,
    /// Total done items.
    pub done: usize,
    /// Total deferred items.
    pub deferred: usize,
    /// Pending count per priority (e.g. {"P0": 5, "P1": 3}). `default` pairs
    /// with `skip_serializing_if` so a round-trip through JSON that omitted
    /// this field (because it was empty) deserializes back to an empty map
    /// instead of erroring on "missing field" — load-bearing for the status
    /// cache (`build_cached`), which serializes/deserializes a `ProgressView`
    /// (and therefore this struct) on every cache write/read.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pending_by_priority: BTreeMap<String, usize>,
}

/// PDO hypothesis bucket.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HypoBuckets {
    /// Open hypotheses count.
    pub open: usize,
    /// Awaiting measurement count.
    pub awaiting_measurement: usize,
    /// Validated count.
    pub validated: usize,
    /// Rejected count.
    pub rejected: usize,
}

/// Condukt run row.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunRow {
    /// Run ID.
    pub run_id: String,
    /// Tasks completed.
    pub done: usize,
    /// Tasks total.
    pub total: usize,
    /// Goal description (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
}

/// The aggregated progress view.
///
/// NOTE: every field pairs `skip_serializing_if` with `default` (where the
/// type doesn't already deserialize a missing key as its default, i.e. every
/// `Vec` field — `Option` fields get this for free from serde). This is
/// load-bearing for `aggregate::build_cached`, which round-trips a whole
/// `ProgressView` through JSON on every cache write/read: an empty `Vec`
/// field is omitted on write (by `skip_serializing_if`), and without
/// `default` a subsequent read would fail with "missing field" instead of
/// reconstructing the empty `Vec`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProgressView {
    /// Overwatch ledger: per-session rosters of live leases.
    ///
    /// An empty vec means "the ledger was read and holds no live leases".
    /// It does NOT mean "the ledger could not be read" — that case is carried
    /// by an [`UndeterminedSource`] with key [`SOURCE_SESSIONS`] in
    /// [`ProgressView::undetermined`], which must be consulted before reading
    /// emptiness as absence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<SessionRoster>,
    /// Backlog summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backlog: Option<BacklogSummary>,
    /// PDO hypotheses buckets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hypotheses: Option<HypoBuckets>,
    /// Condukt runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunRow>,
    /// Compass gap (north_star / current gap).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compass_gap: Option<String>,
    /// Sources that could NOT be observed on this build, and why.
    ///
    /// This is the field that keeps every other field honest. Each of them is a
    /// count or a list, and an empty one is rendered as `(none)` — which reads
    /// as "checked, found nothing". Without this list there is no way for a
    /// reader (human or machine) to tell that apart from "could not check", and
    /// the whole view is then a fail-open: the quietest possible output is also
    /// the most reassuring one. Presence of an entry here means the matching
    /// field's emptiness is NOT an observation.
    ///
    /// `skip_serializing_if` keeps the key absent (rather than `[]`) on the
    /// happy path, so `overwatch status --json` is unchanged byte-for-byte when
    /// everything was readable; `default` pairs with it for the cache
    /// round-trip, like every other collection field above.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undetermined: Vec<UndeterminedSource>,
}

/// Machine key for the session-roster source.
pub const SOURCE_SESSIONS: &str = "sessions";
/// Machine key for the backlog source.
pub const SOURCE_BACKLOG: &str = "backlog";
/// Machine key for the PDO-hypotheses source.
pub const SOURCE_HYPOTHESES: &str = "hypotheses";
/// Machine key for the condukt-runs source.
pub const SOURCE_RUNS: &str = "runs";
/// Machine key for the compass-gap source.
pub const SOURCE_COMPASS_GAP: &str = "compass_gap";

/// One status source that could not be observed, and why.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UndeterminedSource {
    /// Stable machine key — one of the `SOURCE_*` constants.
    pub source: String,
    /// Human-readable reason, carried so the render can say *why* it is unknown
    /// rather than merely that it is.
    pub reason: String,
}

impl ProgressView {
    /// Record that `source` could not be observed. Idempotent per source: the
    /// first reason wins, so a caller cannot accidentally overwrite the root
    /// cause with a downstream symptom.
    pub fn mark_undetermined(&mut self, source: &str, reason: impl Into<String>) {
        if self.undetermined.iter().any(|u| u.source == source) {
            return;
        }
        self.undetermined.push(UndeterminedSource {
            source: source.to_string(),
            reason: reason.into(),
        });
    }

    /// The reason `source` is unknown, or `None` if it was observed.
    #[must_use]
    pub fn undetermined_reason(&self, source: &str) -> Option<&str> {
        self.undetermined
            .iter()
            .find(|u| u.source == source)
            .map(|u| u.reason.as_str())
    }

    /// True iff at least one source could not be observed.
    #[must_use]
    pub fn has_undetermined(&self) -> bool {
        !self.undetermined.is_empty()
    }
}

/// Build a session roster from a lease registry.
pub fn roster_from_leases(leases: &LeaseRegistry, now: i64) -> Vec<SessionRoster> {
    let mut by_session: BTreeMap<String, Vec<LeaseInfo>> = BTreeMap::new();

    for lease in leases.values() {
        let is_stale = store::is_stale(lease, now);
        let heartbeat_age_secs = (now - lease.heartbeat_at).max(0);
        let info = LeaseInfo {
            key: lease.key.clone(),
            title: lease.title.clone(),
            heartbeat_age_secs,
            is_stale,
        };
        by_session
            .entry(lease.session_id.clone())
            .or_default()
            .push(info);
    }

    by_session
        .into_iter()
        .map(|(session_id, leases)| {
            let live_count = leases.iter().filter(|l| !l.is_stale).count();
            SessionRoster {
                session_id,
                leases,
                live_count,
            }
        })
        .collect()
}

/// Parse backlog JSON (from `backlog list --json`).
/// Expected format: array of items with fields like {key, title, status, priority}.
///
/// Undecodable JSON is `Err`, NOT an empty summary: "the backlog tool emitted
/// something we cannot read" and "the backlog is empty" are different facts and
/// `pending: 0` states the second one.
pub fn parse_backlog(json: &str) -> Result<BacklogSummary, String> {
    #[derive(Deserialize)]
    struct BacklogItem {
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        priority: Option<String>,
    }

    let items = serde_json::from_str::<Vec<BacklogItem>>(json)
        .map_err(|e| format!("`backlog list --json` emitted undecodable JSON: {e}"))?;

    let mut summary = BacklogSummary::default();
    let mut pending_by_priority: BTreeMap<String, usize> = BTreeMap::new();

    for item in items {
        match item.status.as_deref() {
            Some("done") => summary.done += 1,
            Some("deferred") => summary.deferred += 1,
            _ => {
                summary.pending += 1;
                if let Some(pri) = item.priority {
                    *pending_by_priority.entry(pri).or_insert(0) += 1;
                }
            }
        }
    }

    summary.pending_by_priority = pending_by_priority;
    Ok(summary)
}

/// Parse PDO hypothesis JSON (from `hypothesis list --json`).
/// Expected format: array of items with a field like {status: "open"|"awaiting-measurement"|"validated"|"rejected"}.
/// Undecodable JSON is `Err`, NOT empty buckets — see [`parse_backlog`].
pub fn bucket_hypotheses(json: &str) -> Result<HypoBuckets, String> {
    #[derive(Deserialize)]
    struct HypoItem {
        #[serde(default)]
        status: Option<String>,
    }

    let items = serde_json::from_str::<Vec<HypoItem>>(json)
        .map_err(|e| format!("`hypothesis list --json` emitted undecodable JSON: {e}"))?;

    let mut buckets = HypoBuckets::default();
    for item in items {
        match item.status.as_deref() {
            Some("open") => buckets.open += 1,
            Some("awaiting-measurement") => buckets.awaiting_measurement += 1,
            Some("validated") => buckets.validated += 1,
            Some("rejected") => buckets.rejected += 1,
            _ => {}
        }
    }
    Ok(buckets)
}

/// Parse condukt runs TSV (from `condukt state list`).
/// Expected format: tab-separated, one run per line: `run_id<TAB>done/total<TAB>goal`
///
/// A line whose `done/total` field does not parse is `Err`, not a silent `0/0`:
/// the old `unwrap_or(0)` rendered a malformed row as "no progress", which reads
/// as a real observation about the run.
pub fn parse_condukt_runs(tsv: &str) -> Result<Vec<RunRow>, String> {
    let mut rows = Vec::new();
    for line in tsv.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 2 {
            let run_id = parts[0].trim().to_string();
            let (done_str, total_str) = parts[1].split_once('/').ok_or_else(|| {
                format!("`condukt state list` row {run_id:?}: progress field is not `done/total`")
            })?;
            let parse = |s: &str, which: &str| {
                s.trim().parse::<usize>().map_err(|e| {
                    format!(
                        "`condukt state list` row {run_id:?}: {which} count is unparseable: {e}"
                    )
                })
            };
            let done = parse(done_str, "done")?;
            let total = parse(total_str, "total")?;
            let goal = parts
                .get(2)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            rows.push(RunRow {
                run_id,
                done,
                total,
                goal,
            });
        }
    }
    Ok(rows)
}

/// The outcome of shelling one status source.
///
/// Three states, not two. The retired `shell_soft` returned `Option<String>`
/// and so collapsed "the tool is not installed", "the tool ran and failed", and
/// "the tool emitted non-UTF-8" into the same `None` that the caller then
/// rendered as `(none)` — indistinguishable from "the tool ran and found
/// nothing". Only `Output` is an observation; both other arms are refusals to
/// guess, and carry why (CLAUDE.md §3).
enum SourceOutput {
    /// The command ran to a successful exit and produced this stdout.
    Output(String),
    /// The source could not be observed. Carries the reason for the render.
    Undetermined(String),
}

/// Shell one status source. Never folds a failure into an empty observation.
fn shell_source(cmd: &str, args: &[&str]) -> SourceOutput {
    use std::process::Command;

    let output = match Command::new(cmd).args(args).output() {
        Ok(o) => o,
        Err(e) => {
            return SourceOutput::Undetermined(format!(
                "`{cmd}` could not be run ({e}); this is NOT a report that {cmd} is empty"
            ));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim().lines().next().unwrap_or("(no stderr)");
        return SourceOutput::Undetermined(format!(
            "`{cmd} {}` exited {} ({detail})",
            args.join(" "),
            output.status
        ));
    }
    match String::from_utf8(output.stdout) {
        Ok(s) => SourceOutput::Output(s),
        Err(e) => SourceOutput::Undetermined(format!("`{cmd}` emitted non-UTF-8 stdout: {e}")),
    }
}

/// Fold a lease-ledger load result into the view.
///
/// Split out from [`build`] so the load FAILURE path is reachable from a test
/// without touching the real `~/.overwatch` store: `build` shells out to four
/// other binaries, so testing it end-to-end would measure the environment
/// rather than this decision.
///
/// `store::load_leases` already distinguishes the three answers correctly — a
/// missing file is `Ok(empty)`, corrupt JSON and an unreadable file are both
/// `Err` — so the only question here is whether the caller PRESERVES that
/// distinction or collapses it. Binding it with `if let Ok(..)` — as `build`
/// once did — threw that distinction away and left `sessions` empty for BOTH,
/// which renders as `(none)` = "no other session is live". That is the single
/// most load-bearing fact in this output: CLAUDE.md §8 says to assume another
/// session always exists, and condukt's main-tree guard reads this roster as a
/// liveness input before allowing a commit in main's shared working tree.
pub(crate) fn apply_leases(
    view: &mut ProgressView,
    loaded: anyhow::Result<LeaseRegistry>,
    now: i64,
) {
    match loaded {
        Ok(mut leases) => {
            store::reap_stale(&mut leases, now);
            view.sessions = roster_from_leases(&leases, now);
        }
        // `sessions` deliberately stays empty — there is nothing to report —
        // but the REASON is carried alongside it so that emptiness is never
        // mistaken for a measurement. Dropping this arm was the fail-open.
        Err(e) => {
            view.sessions = Vec::new();
            view.mark_undetermined(
                SOURCE_SESSIONS,
                format!("the lease ledger could not be read ({e}); live sessions are UNKNOWN"),
            );
        }
    }
}

/// Build the full ProgressView.
///
/// Infallible in the sense that it always returns a view (this runs from the
/// SessionStart/Stop hooks and must not abort a turn), but NOT fail-soft in the
/// sense of substituting empties: every source that could not be observed is
/// recorded in [`ProgressView::undetermined`] so the renderer prints `(unknown)`
/// rather than `(none)`.
pub fn build(cwd: &Path) -> ProgressView {
    let now = store::now();
    let mut view = ProgressView::default();

    // 1. Overwatch ledger: load live leases, reap stale, build session rosters.
    //
    // The absent-vs-unreadable split lives in `apply_leases` so the FAILURE
    // path stays reachable from a test without touching the real store.
    apply_leases(&mut view, store::load_leases(cwd), now);

    // 2. Backlog: `backlog list --json`.
    match shell_source("backlog", &["list", "--json"]) {
        SourceOutput::Output(json) => match parse_backlog(&json) {
            Ok(summary) => view.backlog = Some(summary),
            Err(why) => view.mark_undetermined(SOURCE_BACKLOG, why),
        },
        SourceOutput::Undetermined(why) => view.mark_undetermined(SOURCE_BACKLOG, why),
    }

    // 3. PDO hypotheses: `hypothesis list --json`.
    match shell_source("hypothesis", &["list", "--json"]) {
        SourceOutput::Output(json) => match bucket_hypotheses(&json) {
            Ok(buckets) => view.hypotheses = Some(buckets),
            Err(why) => view.mark_undetermined(SOURCE_HYPOTHESES, why),
        },
        SourceOutput::Undetermined(why) => view.mark_undetermined(SOURCE_HYPOTHESES, why),
    }

    // 4. Condukt runs: `condukt state list` (TAB-separated).
    match shell_source("condukt", &["state", "list"]) {
        SourceOutput::Output(tsv) => match parse_condukt_runs(&tsv) {
            Ok(rows) => view.runs = rows,
            Err(why) => view.mark_undetermined(SOURCE_RUNS, why),
        },
        SourceOutput::Undetermined(why) => view.mark_undetermined(SOURCE_RUNS, why),
    }

    // 5. Compass gap: `compass gap` (plain text). An empty stdout here IS a real
    // observation ("no gap"), so it stays `None` without an undetermined mark.
    match shell_source("compass", &["gap"]) {
        SourceOutput::Output(gap) => {
            let gap_str = gap.trim().to_string();
            if !gap_str.is_empty() {
                view.compass_gap = Some(gap_str);
            }
        }
        SourceOutput::Undetermined(why) => view.mark_undetermined(SOURCE_COMPASS_GAP, why),
    }

    view
}

/// TTL for the short-lived status cache, in seconds. `overwatch status` runs
/// on BOTH SessionStart (of the next turn) and Stop (of the current turn);
/// when those two fire close together this TTL collapses the second call's
/// ~5 subprocess spawns + lease-store scan into a cache read. Chosen small
/// enough that a stale render is never visible for more than a few seconds
/// (observability is bounded-stale, never lost — a cold/expired cache always
/// falls through to a full `build`).
pub const STATUS_CACHE_TTL_SECS: i64 = 10;

/// On-disk shape of the status cache: the rendered view plus the unix
/// timestamp it was built at.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedView {
    built_at: i64,
    view: ProgressView,
}

/// Pure freshness check: is a cache entry built at `built_at` still usable at
/// `now`, given `ttl_secs`? Factored out (no I/O) so the TTL boundary logic is
/// directly unit-testable without touching the filesystem. Also guards
/// against a clock-skew/corrupt timestamp in the future (`built_at > now`),
/// which is treated as stale rather than trusted.
fn cache_is_fresh(built_at: i64, now: i64, ttl_secs: i64) -> bool {
    built_at <= now && now - built_at <= ttl_secs
}

/// Build the full ProgressView, reusing a short-lived on-disk cache when
/// fresh (see `STATUS_CACHE_TTL_SECS`). Fail-soft in both directions: any
/// cache read error (missing file, corrupt JSON) falls through to a fresh
/// `build`, and any cache write error is silently ignored (the render already
/// succeeded and must not be blocked by a failed cache write). A fresh build
/// is always persisted back to the cache so the NEXT call (e.g. the paired
/// SessionStart/Stop hook) can hit it.
pub fn build_cached(cwd: &Path) -> ProgressView {
    let now = store::now();

    if let Ok(cache_path) = store::status_cache_path(cwd) {
        if let Ok(txt) = std::fs::read_to_string(&cache_path) {
            if let Ok(cached) = serde_json::from_str::<CachedView>(&txt) {
                if cache_is_fresh(cached.built_at, now, STATUS_CACHE_TTL_SECS) {
                    return cached.view;
                }
            }
        }
    }

    let view = build(cwd);

    if let Ok(cache_path) = store::status_cache_path(cwd) {
        let cached = CachedView {
            built_at: now,
            view: view.clone(),
        };
        if let Ok(json) = serde_json::to_string(&cached) {
            if let Some(parent) = cache_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Best-effort write via temp+rename (same atomic idiom as
            // `store::save_leases`); any failure here is ignored since the
            // render itself already succeeded.
            let tmp = cache_path.with_extension("json.tmp");
            if std::fs::write(&tmp, &json).is_ok() {
                let _ = std::fs::rename(&tmp, &cache_path);
            }
        }
    }

    view
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Lease;
    use std::collections::BTreeMap;

    /// RED before the fix. `store::load_leases` already returns `Err` for a
    /// corrupt or unreadable ledger; the defect was that `build` dropped it
    /// with `if let Ok`, leaving `sessions` at its `Default` — so "could not
    /// read" produced byte-identical output to "read it, nobody is here".
    #[test]
    fn an_unreadable_lease_ledger_is_not_reported_as_no_sessions() {
        let mut view = ProgressView::default();
        apply_leases(
            &mut view,
            Err(anyhow::anyhow!(
                "leases.json could not be read at /x/leases.json: permission denied"
            )),
            0,
        );
        assert!(
            view.undetermined
                .iter()
                .any(|u| u.source == SOURCE_SESSIONS),
            "a ledger that could not be read must be distinguishable from an \
             empty one; no SOURCE_SESSIONS entry was recorded, so a reader sees \
             the same '(none)' that means nobody is working here"
        );
    }

    /// Anti-vacuity control. An implementation that marked EVERYTHING
    /// undetermined would satisfy the test above while destroying the signal:
    /// every status render would say "unknown" and the roster would become
    /// useless. A ledger that was read and holds nothing is a MEASUREMENT and
    /// must stay a measurement.
    #[test]
    fn a_genuinely_empty_ledger_is_still_reported_as_empty_not_unknown() {
        let mut view = ProgressView::default();
        apply_leases(&mut view, Ok(LeaseRegistry::new()), 0);
        assert!(
            !view
                .undetermined
                .iter()
                .any(|u| u.source == SOURCE_SESSIONS),
            "an empty-but-readable ledger must remain a measurement"
        );
        assert!(view.sessions.is_empty());
    }

    /// Non-regression: the ordinary populated path still produces a roster.
    #[test]
    fn a_populated_ledger_still_produces_its_roster() {
        let mut leases = LeaseRegistry::new();
        leases.insert(
            "k1".to_string(),
            Lease {
                key: "k1".to_string(),
                title: "Task 1".to_string(),
                session_id: "sess-a".to_string(),
                run_id: "run-1".to_string(),
                claimed_at: 100,
                heartbeat_at: 100,
                scope: Vec::new(),
                done_criteria: None,
            },
        );
        let mut view = ProgressView::default();
        apply_leases(&mut view, Ok(leases), 100);
        assert!(!view
            .undetermined
            .iter()
            .any(|u| u.source == SOURCE_SESSIONS));
        assert_eq!(view.sessions.len(), 1);
        assert_eq!(view.sessions[0].session_id, "sess-a");
    }

    #[test]
    fn test_roster_from_leases_empty() {
        let leases = LeaseRegistry::new();
        let roster = roster_from_leases(&leases, 1000);
        assert!(roster.is_empty());
    }

    #[test]
    fn test_roster_from_leases_single_session() {
        let mut leases = LeaseRegistry::new();
        let now = 2000i64;

        leases.insert(
            "task-1".to_string(),
            Lease {
                key: "task-1".to_string(),
                title: "Task 1".to_string(),
                session_id: "session-a".to_string(),
                run_id: "run-1".to_string(),
                claimed_at: 1000,
                heartbeat_at: 1900,
                scope: Vec::new(),
                done_criteria: None,
            },
        );

        leases.insert(
            "task-2".to_string(),
            Lease {
                key: "task-2".to_string(),
                title: "Task 2".to_string(),
                session_id: "session-a".to_string(),
                run_id: "run-1".to_string(),
                claimed_at: 1000,
                heartbeat_at: 1850,
                scope: Vec::new(),
                done_criteria: None,
            },
        );

        let roster = roster_from_leases(&leases, now);
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].session_id, "session-a");
        assert_eq!(roster[0].leases.len(), 2);
        assert_eq!(roster[0].live_count, 2);
    }

    #[test]
    fn test_roster_from_leases_multiple_sessions() {
        let mut leases = LeaseRegistry::new();
        let now = 2000i64;

        for i in 0..2 {
            leases.insert(
                format!("task-a{}", i),
                Lease {
                    key: format!("task-a{}", i),
                    title: format!("Task A{}", i),
                    session_id: "session-a".to_string(),
                    run_id: "run-1".to_string(),
                    claimed_at: 1000,
                    heartbeat_at: 1900,
                    scope: Vec::new(),
                    done_criteria: None,
                },
            );
        }

        leases.insert(
            "task-b1".to_string(),
            Lease {
                key: "task-b1".to_string(),
                title: "Task B1".to_string(),
                session_id: "session-b".to_string(),
                run_id: "run-2".to_string(),
                claimed_at: 1000,
                heartbeat_at: 1850,
                scope: Vec::new(),
                done_criteria: None,
            },
        );

        let roster = roster_from_leases(&leases, now);
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0].live_count, 2); // session-a (sorted first)
        assert_eq!(roster[1].live_count, 1); // session-b
    }

    #[test]
    fn test_roster_marks_stale_leases() {
        let mut leases = LeaseRegistry::new();
        let now = 2000i64;

        leases.insert(
            "fresh".to_string(),
            Lease {
                key: "fresh".to_string(),
                title: "Fresh".to_string(),
                session_id: "s1".to_string(),
                run_id: "r1".to_string(),
                claimed_at: 1000,
                heartbeat_at: 1900,
                scope: Vec::new(),
                done_criteria: None,
            },
        );

        leases.insert(
            "stale".to_string(),
            Lease {
                key: "stale".to_string(),
                title: "Stale".to_string(),
                session_id: "s1".to_string(),
                run_id: "r1".to_string(),
                claimed_at: 0,
                heartbeat_at: 0,
                scope: Vec::new(),
                done_criteria: None,
            },
        );

        let roster = roster_from_leases(&leases, now);
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].leases.len(), 2);

        // Find fresh and stale in the roster
        let fresh_info = roster[0].leases.iter().find(|l| l.key == "fresh").unwrap();
        let stale_info = roster[0].leases.iter().find(|l| l.key == "stale").unwrap();

        assert!(!fresh_info.is_stale);
        assert!(stale_info.is_stale);
        assert_eq!(roster[0].live_count, 1); // Only fresh is live
    }

    #[test]
    fn test_parse_backlog_empty() {
        let json = "[]";
        let summary = parse_backlog(json).expect("valid fixture JSON");
        assert_eq!(summary.pending, 0);
        assert_eq!(summary.done, 0);
        assert_eq!(summary.deferred, 0);
    }

    #[test]
    fn test_parse_backlog_mixed_statuses() {
        let json = r#"[
            {"status": "done", "priority": "P0"},
            {"status": "done", "priority": "P1"},
            {"status": "pending", "priority": "P0"},
            {"status": "pending", "priority": "P0"},
            {"status": "pending", "priority": "P1"},
            {"status": "deferred", "priority": "P2"}
        ]"#;

        let summary = parse_backlog(json).expect("valid fixture JSON");
        assert_eq!(summary.done, 2);
        assert_eq!(summary.pending, 3);
        assert_eq!(summary.deferred, 1);
        assert_eq!(summary.pending_by_priority.get("P0"), Some(&2));
        assert_eq!(summary.pending_by_priority.get("P1"), Some(&1));
    }

    #[test]
    fn test_parse_backlog_no_priority() {
        let json = r#"[
            {"status": "pending"},
            {"status": "done"},
            {"status": "deferred"}
        ]"#;

        let summary = parse_backlog(json).expect("valid fixture JSON");
        assert_eq!(summary.pending, 1);
        assert_eq!(summary.done, 1);
        assert_eq!(summary.deferred, 1);
        assert!(summary.pending_by_priority.is_empty());
    }

    // These three `_invalid_json` tests previously asserted that garbage input
    // yields `pending: 0` / all-zero buckets — i.e. they wrote the fail-open
    // down as the contract, so the assertion itself was what kept it in place
    // (CLAUDE.md §2: an assert can fix a defect as a specification). The
    // observation is unchanged; only the expected answer is: undecodable input
    // is `Err`, and the caller marks the source undetermined.

    #[test]
    fn test_parse_backlog_invalid_json_is_error_not_zero() {
        assert!(
            parse_backlog("not valid json").is_err(),
            "undecodable backlog JSON must not read as an empty backlog"
        );
    }

    #[test]
    fn test_bucket_hypotheses_empty() {
        let json = "[]";
        let buckets = bucket_hypotheses(json).expect("valid fixture JSON");
        assert_eq!(buckets.open, 0);
        assert_eq!(buckets.awaiting_measurement, 0);
        assert_eq!(buckets.validated, 0);
        assert_eq!(buckets.rejected, 0);
    }

    #[test]
    fn test_bucket_hypotheses_all_statuses() {
        let json = r#"[
            {"status": "open"},
            {"status": "open"},
            {"status": "awaiting-measurement"},
            {"status": "validated"},
            {"status": "validated"},
            {"status": "validated"},
            {"status": "rejected"}
        ]"#;

        let buckets = bucket_hypotheses(json).expect("valid fixture JSON");
        assert_eq!(buckets.open, 2);
        assert_eq!(buckets.awaiting_measurement, 1);
        assert_eq!(buckets.validated, 3);
        assert_eq!(buckets.rejected, 1);
    }

    #[test]
    fn test_bucket_hypotheses_invalid_json_is_error_not_zero() {
        assert!(
            bucket_hypotheses("not json").is_err(),
            "undecodable hypothesis JSON must not read as zero open hypotheses"
        );
    }

    #[test]
    fn test_parse_condukt_runs_unparseable_progress_is_error_not_zero() {
        // The retired `unwrap_or(0)` turned this row into a confident `0/0`.
        assert!(
            parse_condukt_runs("run-001\tabc/def\tGoal").is_err(),
            "an unparseable progress field must not read as a run at 0/0"
        );
        assert!(
            parse_condukt_runs("run-001\t3\tGoal").is_err(),
            "a progress field with no `/` must not read as a run at 0/0"
        );
    }

    /// The empty case must stay a real observation — the fix must not make
    /// everything undetermined, which would be a different wrong answer.
    #[test]
    fn genuinely_empty_sources_stay_known() {
        assert_eq!(parse_backlog("[]").unwrap().pending, 0);
        assert_eq!(bucket_hypotheses("[]").unwrap().open, 0);
        assert!(parse_condukt_runs("").unwrap().is_empty());
    }

    // ── ProgressView undetermined bookkeeping ───────────────────────────────

    #[test]
    fn default_view_has_nothing_undetermined() {
        assert!(!ProgressView::default().has_undetermined());
        assert_eq!(
            ProgressView::default().undetermined_reason(SOURCE_SESSIONS),
            None
        );
    }

    #[test]
    fn mark_undetermined_is_queryable_per_source() {
        let mut v = ProgressView::default();
        v.mark_undetermined(SOURCE_SESSIONS, "ledger unreadable");
        assert!(v.has_undetermined());
        assert_eq!(
            v.undetermined_reason(SOURCE_SESSIONS),
            Some("ledger unreadable")
        );
        assert_eq!(v.undetermined_reason(SOURCE_BACKLOG), None);
    }

    #[test]
    fn mark_undetermined_keeps_the_first_reason() {
        let mut v = ProgressView::default();
        v.mark_undetermined(SOURCE_BACKLOG, "root cause");
        v.mark_undetermined(SOURCE_BACKLOG, "downstream symptom");
        assert_eq!(v.undetermined.len(), 1);
        assert_eq!(v.undetermined_reason(SOURCE_BACKLOG), Some("root cause"));
    }

    /// The status cache round-trips a whole `ProgressView` through JSON on every
    /// write/read (`build_cached`). If `undetermined` did not survive that trip,
    /// a cached view would silently revert to the fail-open render for the
    /// entire TTL — the fix would hold on a cold build and evaporate on a warm
    /// one, which is worse than not having it (it would look fixed).
    #[test]
    fn undetermined_survives_the_status_cache_round_trip() {
        let mut v = ProgressView::default();
        v.mark_undetermined(SOURCE_SESSIONS, "leases.json present but corrupt");
        let json = serde_json::to_string(&v).unwrap();
        let back: ProgressView = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.undetermined_reason(SOURCE_SESSIONS),
            Some("leases.json present but corrupt")
        );
    }

    /// `--json` is the machine surface condukt's main-tree guard reads. The key
    /// must be absent on the clean path (so the existing contract is unchanged)
    /// and present the moment a source is unobservable (so a consumer CAN tell,
    /// which the old shape made impossible).
    #[test]
    fn json_omits_undetermined_when_clean_and_emits_it_when_not() {
        let clean = serde_json::to_value(ProgressView::default()).unwrap();
        assert!(clean.get("undetermined").is_none());

        let mut v = ProgressView::default();
        v.mark_undetermined(SOURCE_SESSIONS, "unreadable");
        let dirty = serde_json::to_value(&v).unwrap();
        assert_eq!(
            dirty["undetermined"][0]["source"].as_str(),
            Some(SOURCE_SESSIONS)
        );
    }

    #[test]
    fn test_parse_condukt_runs_empty() {
        let tsv = "";
        let runs = parse_condukt_runs(tsv).expect("valid fixture TSV");
        assert!(runs.is_empty());
    }

    #[test]
    fn test_parse_condukt_runs_single() {
        let tsv = "run-001\t5/10\tBuild feature X";
        let runs = parse_condukt_runs(tsv).expect("valid fixture TSV");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "run-001");
        assert_eq!(runs[0].done, 5);
        assert_eq!(runs[0].total, 10);
        assert_eq!(runs[0].goal, Some("Build feature X".to_string()));
    }

    #[test]
    fn test_parse_condukt_runs_multiple() {
        let tsv = "run-001\t5/10\tPhase 1\nrun-002\t2/5\t\nrun-003\t0/3\tInitial";
        let runs = parse_condukt_runs(tsv).expect("valid fixture TSV");
        assert_eq!(runs.len(), 3);

        assert_eq!(runs[0].run_id, "run-001");
        assert_eq!(runs[0].done, 5);
        assert_eq!(runs[0].total, 10);
        assert_eq!(runs[0].goal, Some("Phase 1".to_string()));

        assert_eq!(runs[1].run_id, "run-002");
        assert_eq!(runs[1].done, 2);
        assert_eq!(runs[1].total, 5);
        assert_eq!(runs[1].goal, None); // Empty goal

        assert_eq!(runs[2].run_id, "run-003");
        assert_eq!(runs[2].done, 0);
        assert_eq!(runs[2].total, 3);
        assert_eq!(runs[2].goal, Some("Initial".to_string()));
    }

    #[test]
    fn test_parse_condukt_runs_no_goal() {
        let tsv = "run-001\t3/7";
        let runs = parse_condukt_runs(tsv).expect("valid fixture TSV");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "run-001");
        assert_eq!(runs[0].done, 3);
        assert_eq!(runs[0].total, 7);
        assert_eq!(runs[0].goal, None);
    }

    #[test]
    fn test_parse_condukt_runs_malformed() {
        // Line with only run_id (missing progress) should be skipped
        let tsv = "run-001\nrun-002\t5/10\tGoal";
        let runs = parse_condukt_runs(tsv).expect("valid fixture TSV");
        assert_eq!(runs.len(), 1); // Only run-002 is valid
        assert_eq!(runs[0].run_id, "run-002");
    }

    #[test]
    fn test_progress_view_default_serializes() {
        let view = ProgressView::default();
        let json = serde_json::to_string(&view).unwrap();
        // Empty sessions and runs should be skipped due to skip_serializing_if
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Should be an empty object or only have null fields
        assert!(parsed.is_object());
    }

    #[test]
    fn test_progress_view_with_sessions() {
        let roster = SessionRoster {
            session_id: "s1".to_string(),
            leases: vec![LeaseInfo {
                key: "k1".to_string(),
                title: "t1".to_string(),
                heartbeat_age_secs: 100,
                is_stale: false,
            }],
            live_count: 1,
        };

        let view = ProgressView {
            sessions: vec![roster],
            ..Default::default()
        };

        let json = serde_json::to_string(&view).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["sessions"].is_array());
        assert_eq!(parsed["sessions"][0]["session_id"], "s1");
    }

    #[test]
    fn test_backlog_summary_serializes() {
        let mut summary = BacklogSummary {
            pending: 5,
            done: 10,
            deferred: 2,
            pending_by_priority: BTreeMap::new(),
        };
        summary.pending_by_priority.insert("P0".to_string(), 3);
        summary.pending_by_priority.insert("P1".to_string(), 2);

        let json = serde_json::to_string(&summary).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["pending"], 5);
        assert_eq!(parsed["done"], 10);
        assert_eq!(parsed["deferred"], 2);
        assert_eq!(parsed["pending_by_priority"]["P0"], 3);
    }

    #[test]
    fn test_hypo_buckets_serializes() {
        let buckets = HypoBuckets {
            open: 2,
            awaiting_measurement: 1,
            validated: 5,
            rejected: 1,
        };

        let json = serde_json::to_string(&buckets).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["open"], 2);
        assert_eq!(parsed["awaiting_measurement"], 1);
        assert_eq!(parsed["validated"], 5);
        assert_eq!(parsed["rejected"], 1);
    }

    #[test]
    fn test_run_row_serializes_with_goal() {
        let run = RunRow {
            run_id: "run-001".to_string(),
            done: 5,
            total: 10,
            goal: Some("Phase 1".to_string()),
        };

        let json = serde_json::to_string(&run).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["run_id"], "run-001");
        assert_eq!(parsed["done"], 5);
        assert_eq!(parsed["total"], 10);
        assert_eq!(parsed["goal"], "Phase 1");
    }

    #[test]
    fn test_run_row_serializes_without_goal() {
        let run = RunRow {
            run_id: "run-002".to_string(),
            done: 2,
            total: 5,
            goal: None,
        };

        let json = serde_json::to_string(&run).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["run_id"], "run-002");
        assert_eq!(parsed["done"], 2);
        assert_eq!(parsed["total"], 5);
        assert!(
            !parsed.get("goal").map(|v| !v.is_null()).unwrap_or(false) || parsed["goal"].is_null()
        );
    }

    #[test]
    fn test_parse_backlog_default_status() {
        // Items without a status field should be treated as pending
        let json = r#"[
            {"priority": "P0"},
            {"status": "done", "priority": "P1"}
        ]"#;

        let summary = parse_backlog(json).expect("valid fixture JSON");
        assert_eq!(summary.pending, 1);
        assert_eq!(summary.done, 1);
    }

    // -- status cache (cache_is_fresh / build_cached) --------------------

    #[test]
    fn test_cache_is_fresh_within_ttl() {
        assert!(cache_is_fresh(1000, 1005, STATUS_CACHE_TTL_SECS));
        // Exactly at the TTL boundary is still fresh (inclusive).
        assert!(cache_is_fresh(
            1000,
            1000 + STATUS_CACHE_TTL_SECS,
            STATUS_CACHE_TTL_SECS
        ));
    }

    #[test]
    fn test_cache_is_fresh_expired_past_ttl() {
        assert!(!cache_is_fresh(
            1000,
            1000 + STATUS_CACHE_TTL_SECS + 1,
            STATUS_CACHE_TTL_SECS
        ));
    }

    #[test]
    fn test_cache_is_fresh_rejects_future_built_at() {
        // A `built_at` after `now` is clock-skew/corruption, not "fresh".
        assert!(!cache_is_fresh(2000, 1000, STATUS_CACHE_TTL_SECS));
    }

    // `build_cached` and `store::status_cache_path` resolve under the real
    // `$HOME` (via `harness_core::config::base_dir`), so these tests sandbox
    // HOME the same way `store.rs`'s `read_review_findings_all_concatenates_*`
    // test does. MUST share `store::HOME_ENV_LOCK` (not a module-local copy):
    // two separate `Mutex`es don't serialize against each other, so a test
    // here and a test in `store.rs` could still both mutate the
    // process-global `$HOME` env var at once.
    use crate::store::HOME_ENV_LOCK;

    struct HomeSandbox {
        prev_home: Option<std::ffi::OsString>,
        dir: std::path::PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl HomeSandbox {
        fn new(tag: &str) -> Self {
            // Poison-RECOVERING (`unwrap_or_else(into_inner)`), not `unwrap()`: a
            // test that fails an assertion while holding this process-global
            // `$HOME` lock poisons it, and every LATER `$HOME` test in the binary
            // then dies with a `PoisonError` that says nothing about the property
            // it checks — one real red reported as a pile of noise.
            let guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev_home = std::env::var_os("HOME");
            let dir = std::env::temp_dir().join(format!(
                "overwatch-aggregate-test-{tag}-{}-{}",
                std::process::id(),
                store::now()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            std::env::set_var("HOME", &dir);
            Self {
                prev_home,
                dir,
                _guard: guard,
            }
        }
    }

    impl Drop for HomeSandbox {
        fn drop(&mut self) {
            match self.prev_home.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    #[test]
    fn test_build_cached_reuses_fresh_cache_without_rebuilding() {
        let sandbox = HomeSandbox::new("fresh-hit");
        let cwd = sandbox.dir.clone();

        // Seed a cache entry directly (bypassing `build`, which would spawn
        // subprocesses) so we can assert the cache-hit path returns exactly
        // what was cached, not a freshly-built (empty) view.
        let cache_path = store::status_cache_path(&cwd).unwrap();
        std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        let backlog = BacklogSummary {
            pending: 42,
            ..Default::default()
        };
        let seeded = CachedView {
            built_at: store::now(),
            view: ProgressView {
                backlog: Some(backlog),
                ..Default::default()
            },
        };
        std::fs::write(&cache_path, serde_json::to_string(&seeded).unwrap()).unwrap();

        let view = build_cached(&cwd);
        assert_eq!(view.backlog.unwrap().pending, 42);
    }

    #[test]
    fn test_build_cached_expired_entry_falls_through_to_fresh_build() {
        let sandbox = HomeSandbox::new("expired-miss");
        let cwd = sandbox.dir.clone();

        let cache_path = store::status_cache_path(&cwd).unwrap();
        std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        let stale_backlog = BacklogSummary {
            pending: 999,
            ..Default::default()
        };
        let expired = CachedView {
            built_at: store::now() - STATUS_CACHE_TTL_SECS - 100,
            view: ProgressView {
                backlog: Some(stale_backlog),
                ..Default::default()
            },
        };
        std::fs::write(&cache_path, serde_json::to_string(&expired).unwrap()).unwrap();

        // A fresh build (no `backlog`/`hypothesis`/`condukt`/`compass` binaries
        // on PATH in this sandboxed env) must NOT return the expired 999
        // value — it must re-derive from scratch (here, an empty/default
        // backlog since the subprocess calls fail-soft to None).
        let view = build_cached(&cwd);
        assert_ne!(view.backlog.map(|b| b.pending), Some(999));
    }

    #[test]
    fn test_build_cached_missing_cache_falls_back_to_full_build() {
        let sandbox = HomeSandbox::new("cold-miss");
        let cwd = sandbox.dir.clone();

        // No cache file at all. Should not panic, should return a valid
        // (fresh-built) view, and should persist a cache entry for next time.
        let view = build_cached(&cwd);
        assert!(view.sessions.is_empty());

        let cache_path = store::status_cache_path(&cwd).unwrap();
        assert!(
            cache_path.exists(),
            "build_cached should persist a cache entry after a fresh build"
        );
    }

    #[test]
    fn test_build_cached_corrupt_cache_falls_back_to_full_build() {
        let sandbox = HomeSandbox::new("corrupt-miss");
        let cwd = sandbox.dir.clone();

        let cache_path = store::status_cache_path(&cwd).unwrap();
        std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        std::fs::write(&cache_path, "not valid json at all").unwrap();

        // Must not panic on corrupt cache content; falls through to a fresh
        // build (fail-soft).
        let view = build_cached(&cwd);
        assert!(view.sessions.is_empty());
    }

    #[test]
    fn test_build_cached_second_call_within_ttl_hits_cache() {
        let sandbox = HomeSandbox::new("roundtrip");
        let cwd = sandbox.dir.clone();

        // First call: cold miss, does a full build and persists the cache.
        let first = build_cached(&cwd);

        // Mutate the persisted cache's `view` in place to a sentinel value,
        // simulating "time has NOT advanced past the TTL" — the second call
        // must reuse this sentinel via the cache rather than rebuilding.
        let cache_path = store::status_cache_path(&cwd).unwrap();
        let txt = std::fs::read_to_string(&cache_path).unwrap();
        let mut cached: CachedView = serde_json::from_str(&txt).unwrap();
        let sentinel_backlog = BacklogSummary {
            pending: 7,
            ..Default::default()
        };
        cached.view.backlog = Some(sentinel_backlog);
        std::fs::write(&cache_path, serde_json::to_string(&cached).unwrap()).unwrap();

        let second = build_cached(&cwd);
        assert_eq!(second.backlog.unwrap().pending, 7);
        // Both calls ran in an empty sandbox with no leases registered, so
        // the session roster is empty either way — assert that directly
        // (SessionRoster doesn't derive PartialEq, so we can't compare the
        // Vecs wholesale).
        assert!(first.sessions.is_empty());
        assert!(second.sessions.is_empty());
    }
}
