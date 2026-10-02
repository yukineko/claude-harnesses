//! Independent, reusable SPEC/feature ↔ implementation ↔ test ↔ API mapping
//! store.
//!
//! This module owns a *persisted*, **feature/endpoint-centric** mapping. Each
//! [`MapEntry`] relates a spec/feature (or an HTTP endpoint) to the artifacts
//! that realize, exercise, and consume it across two dimensions:
//!   * `impl_files` + `test_files` — a feature relates to BOTH its
//!     implementation and its tests. Reading the tests reveals which features
//!     exist; reading the API implementation reveals which file implements a
//!     feature.
//!   * `api` + `client_refs` — for `Endpoint` entries, the method/route pattern
//!     plus the client-side call sites. This lets a consumer walk
//!     client → endpoint → server code: from a client call site you find the
//!     URL, which maps to the server handler file.
//!
//! It is deliberately **independent of any drift workflow**: it knows nothing
//! about sentinels, reports, ratification, or the audit agent. Future features
//! (spec-audit, drift-map, coverage reports, …) each *consume* this store rather
//! than the store depending on them, and the entry type is general/extensible
//! (see [`EntryKind`]) so it serves full-stack projects, not just this repo.
//!
//! ## Division of labour (important)
//!
//! The *semantic* attribution — deciding which file, test, or endpoint truly
//! belongs to which feature, and resolving the api↔server-code and
//! client↔server links by actually READING the test code, the API/route
//! definitions, and the client HTTP calls — is the **LLM CONSUMER's job** (the
//! `/specguard:drift-map` command and future spec-audit). This store only
//! **PERSISTS** those associations and offers a **deterministic, path-based
//! sync** as the skeleton the consumer refines. The deterministic sync cannot
//! know feature/endpoint boundaries, so on its own it keys a newly-seen file by
//! its own path and classifies it into `impl_files`/`test_files` via the simple
//! documented heuristic [`classify_path`].
//!
//! The impl↔test relation is written deterministically, by three writers:
//!   * [`SpecMap::relate_tests`] (run on every sync) merges a test file's
//!     per-file skeleton into the implementation entry that the path heuristic
//!     [`test_impl_candidates`] names: a same-named file under `src/`
//!     (`tests/foo.rs` → `src/foo.rs`), else the crate root (`src/lib.rs`, then
//!     `src/main.rs`) for a Cargo integration test, or the affix-named sibling
//!     (`foo_test.rs` → `foo.rs`). A test it cannot attribute stays test-only.
//!   * [`SpecMap::mark_inline_tests`] (run on every sync) lists a `.rs` impl
//!     file that carries its own `#[test]` functions in `test_files` too.
//!   * [`SpecMap::link_test`] (`specguard map link`) relates a test to an entry
//!     explicitly, for relations the heuristic cannot see.
//!
//! Because the relation is heuristic, an empty `test_files` means "no test was
//! attributed", not "no test exists": a crate's integration tests are related
//! to its root file only, so its other files stay untested until linked.
//! Merging per-file entries into real feature/endpoint entries (multiple impl
//! files under one key, an `api` ref, `client_refs`, a `spec_doc`) is still the
//! consumer's edit; once done, subsequent deterministic syncs find the owning
//! entry by path and update it in place.
//!
//! ## Layers (kept separate so derivation is unit-testable without git)
//!   * [`SpecMap`] — the TOML-persisted store: [`SpecMap::load`],
//!     [`SpecMap::load_or_init`], [`SpecMap::save`].
//!   * pure helpers — [`parse_name_status`] turns raw `git log --name-status`
//!     text into a `Vec<Change>`, [`classify_path`] decides impl-vs-test, and
//!     [`SpecMap::apply_changes`] reflects those changes into the map.
//!     [`SpecMap::sync`] is the thin git-invoking wrapper that glues them.

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

/// Default location of the persisted map, relative to the repo root.
pub const DEFAULT_MAP_PATH: &str = ".specguard/spec-map.toml";

/// The lifecycle status of a mapped feature/endpoint. Intentionally minimal and
/// workflow-agnostic — a consumer decides what to do with each state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The entry's spec-doc and its impl/test/client associations are in sync as
    /// far as the map knows (a state a consumer sets after reconciling).
    Tracked,
    /// A related file changed since it was last reconciled; the entry's spec-doc
    /// and associations may need review.
    Changed,
    /// The entry has no remaining implementation/test files (all deleted); its
    /// mapping is orphaned.
    Missing,
}

impl Status {
    /// Stable lowercase token (matches the serde representation).
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Tracked => "tracked",
            Status::Changed => "changed",
            Status::Missing => "missing",
        }
    }
}

fn default_status() -> Status {
    Status::Tracked
}

/// What a [`MapEntry`] represents. Extensible (add variants without breaking old
/// maps); serde default is `Feature` so a map that omits `kind` still parses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// A feature/module: related implementation + test files.
    #[default]
    Feature,
    /// An HTTP endpoint: additionally carries an `api` method/route and the
    /// `client_refs` that call it.
    Endpoint,
}

/// Which side of the impl/test dimension a changed path belongs to, per the
/// deterministic path heuristic ([`classify_path`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRole {
    /// Implementation / server-code file.
    Impl,
    /// Test file.
    Test,
}

/// An HTTP endpoint reference: the method + route (URL) pattern a client call
/// site resolves to and a server handler implements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiRef {
    /// HTTP method (e.g. `GET`, `POST`). Free-form; the consumer normalizes.
    pub method: String,
    /// Route/URL pattern (e.g. `/api/users/:id`).
    pub route: String,
}

/// One mapping: a feature or endpoint related to the implementation file(s),
/// test file(s), API route, and client call sites that realize / exercise /
/// consume it. Keyed in [`SpecMap::entries`] by [`MapEntry::key`] (a stable
/// feature/module name or endpoint id; the deterministic skeleton keys a
/// not-yet-attributed file by its own path).
///
/// Every optional/vec field carries a serde default, so an old or partial map
/// (missing newer fields) still parses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapEntry {
    /// Stable id: a feature/module name or an endpoint id (mirrors the map key).
    #[serde(default)]
    pub key: String,
    /// What this entry represents (feature vs endpoint).
    #[serde(default)]
    pub kind: EntryKind,
    /// Path to the spec document. `None`/empty means the spec is not yet
    /// authored (a consumer typically marks such an entry `Missing`).
    #[serde(default)]
    pub spec_doc: Option<String>,
    /// Lifecycle status of the mapping.
    #[serde(default = "default_status")]
    pub status: Status,
    /// Last git ref this entry was synced at (`None` until first synced).
    #[serde(default)]
    pub last_ref: Option<String>,
    /// Implementation / server-code file paths realizing this entry.
    #[serde(default)]
    pub impl_files: Vec<String>,
    /// Test file paths exercising this entry. Written by the sync's
    /// deterministic relation (see the module docs), by `specguard map link`,
    /// or by a consumer. An implementation file that carries its own `#[test]`
    /// functions is listed here as well as in `impl_files`.
    #[serde(default)]
    pub test_files: Vec<String>,
    /// Client-side call sites (files) that call this entry's api/url.
    #[serde(default)]
    pub client_refs: Vec<String>,
    /// Symbol names DECLARED in this entry's `impl_files`, from the
    /// deterministic symbol index. Populated by `specguard map enrich`; empty
    /// until then, and empty for entries whose impl files declare nothing.
    ///
    /// `#[serde(default)]`, like every field here, so a map written before this
    /// existed still parses — the field simply reads as not-yet-enriched.
    #[serde(default)]
    pub symbols: Vec<String>,
    /// Files that CALL into [`MapEntry::symbols`] from outside this entry's own
    /// `impl_files`, from the persisted call graph. This is the edge that spec
    /// drift travels along: change one of these symbols and every file listed
    /// here is a place the change is observable.
    ///
    /// Lexical, so it inherits the call graph's limits — no module or type
    /// resolution, and two same-named symbols in different modules collapse
    /// into one. Treat it as "look here", never as a proof of reachability.
    #[serde(default)]
    pub called_by: Vec<String>,
    /// Symbols from [`MapEntry::symbols`] deliberately LEFT OUT of the
    /// `called_by` derivation because too many files declare that same name
    /// ([`AMBIGUOUS_SYMBOL_DECLS`]).
    ///
    /// A lexical graph cannot tell forty `new`s apart, so an edge to one of
    /// them says only "somebody called something called `new`". Attributing all
    /// of those to this entry produced `called_by` lists of 170 files — a
    /// number large enough to look like insight and useless enough to be worse
    /// than nothing. They are recorded here rather than silently dropped: an
    /// empty `called_by` next to a populated list HERE means "not traced",
    /// which is a different fact from "nothing calls this".
    #[serde(default)]
    pub ambiguous_symbols: Vec<String>,
    /// Why a human (or agent) last asserted this entry `tracked` via
    /// `map resolve` / `map set-spec`. That assertion is a CLAIM that someone
    /// reviewed the entry — nothing is verified — so the claim must at least
    /// say what was reviewed. `None` on entries written before this field
    /// existed (serde default: old stores still load) and on entries never
    /// resolved by hand; `None` is "no recorded review", never "reviewed".
    #[serde(default)]
    pub reviewed_reason: Option<String>,
    /// When that claim was made: the repo HEAD commit and the run date. Paired
    /// with [`MapEntry::reviewed_reason`]; `None` under the same conditions.
    /// A sub-table, so declared after every scalar field.
    #[serde(default)]
    pub reviewed_at: Option<ReviewedAt>,
    /// For `Endpoint` entries: the method/route this entry maps to. A sub-table,
    /// declared LAST so TOML emits it after all scalar/array fields.
    #[serde(default)]
    pub api: Option<ApiRef>,
}

/// When a `tracked` claim was recorded: the HEAD commit it was made against and
/// the run date (`YYYY-MM-DD`, from `--date` / `SPECGUARD_NOW` / today).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedAt {
    /// `git rev-parse HEAD` at the time of the claim.
    pub commit: String,
    /// Run date of the claim.
    pub date: String,
}

/// A validated review claim — the only way to hand [`SpecMap::resolve`] /
/// [`SpecMap::set_spec`] the right to mark an entry `tracked`. Its fields are
/// private and [`Review::new`] rejects a blank reason, commit or date, so an
/// unexplained / undated `tracked` flip is unrepresentable rather than merely
/// discouraged (same discipline as `accept-prompt -m`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    reason: String,
    at: ReviewedAt,
}

impl Review {
    /// Build a review claim. Errors when `reason`, `commit` or `date` is empty
    /// or whitespace-only. The reason is stored trimmed.
    pub fn new(reason: &str, commit: &str, date: &str) -> Result<Review> {
        let reason = reason.trim();
        if reason.is_empty() {
            anyhow::bail!(
                "a non-blank --reason is required: marking an entry tracked records a review claim, and a claim must say what was reviewed"
            );
        }
        let (commit, date) = (commit.trim(), date.trim());
        if commit.is_empty() || date.is_empty() {
            anyhow::bail!("cannot record a review without a commit and a date");
        }
        Ok(Review {
            reason: reason.to_string(),
            at: ReviewedAt {
                commit: commit.to_string(),
                date: date.to_string(),
            },
        })
    }

    /// Stamp this claim onto `entry` and mark it `tracked`.
    fn apply(&self, entry: &mut MapEntry) {
        entry.status = Status::Tracked;
        entry.reviewed_reason = Some(self.reason.clone());
        entry.reviewed_at = Some(self.at.clone());
    }
}

/// Default review-staleness window: a `tracked` entry whose recorded review is
/// MORE than this many commits behind HEAD is `stale-review`. Used when
/// `[map] review_max_commits` is absent or `0` — never "never stale".
pub const DEFAULT_REVIEW_MAX_COMMITS: u64 = 50;

/// Resolve the configured `[map] review_max_commits` into the window actually
/// applied: a positive value is used as-is; an absent key or `0` resolves to
/// [`DEFAULT_REVIEW_MAX_COMMITS`]. `0` is deliberately NOT read as "disable":
/// a window that can never expire would make every recorded review fresh
/// forever, which is the permissive reading CLAUDE.md §3 forbids.
pub fn effective_review_max_commits(configured: Option<u64>) -> u64 {
    match configured {
        Some(n) if n > 0 => n,
        _ => DEFAULT_REVIEW_MAX_COMMITS,
    }
}

/// The two DETERMINED answers about a `tracked` entry's review. The third
/// answer — "cannot tell" — is not a variant here: it is the `Undetermined`
/// arm of the surrounding [`Determination`] (see [`ReviewState`]), so it can
/// never be mistaken for, or defaulted to, [`ReviewFreshness::Fresh`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewFreshness {
    /// `reviewed_at.commit` is an ancestor of HEAD and at most N commits behind.
    Fresh,
    /// No review recorded (`reviewed_at` absent — a legacy entry), or the
    /// recorded review commit is MORE than N commits behind HEAD.
    StaleReview,
}

/// A determined review observation: the freshness plus what was measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewObservation {
    /// Fresh or stale-review.
    pub freshness: ReviewFreshness,
    /// `git rev-list --count <reviewed_at.commit>..HEAD`, when a review commit
    /// was recorded; `None` for an entry with no recorded review.
    pub commits_behind: Option<u64>,
}

/// The review state of one `tracked` entry: `Known(fresh | stale-review)` or
/// `Undetermined` ("cannot tell" — unreadable HEAD, a review commit that is not
/// in HEAD's history, a git failure, an unparsable count). Three-valued by
/// construction; the undetermined arm is never resolved to fresh.
pub type ReviewState = Determination<ReviewObservation>;

/// Stable output token for a review state: `"fresh"`, `"stale-review"` or
/// `"undetermined"` (the values `map review-status` and `map list --json`
/// report).
pub fn review_state_token(state: &ReviewState) -> &'static str {
    match state {
        Determination::Known(o) => match o.freshness {
            ReviewFreshness::Fresh => "fresh",
            ReviewFreshness::StaleReview => "stale-review",
        },
        Determination::Undetermined(_) => "undetermined",
    }
}

/// Pure classification of one entry's review against a window of
/// `max_commits`, given a `distance` oracle that answers "how many commits is
/// this review commit behind HEAD" (`Undetermined` when it cannot say).
///
/// * not `tracked` → `None`: only a `tracked` entry carries a review claim, so
///   `changed` / `missing` entries get no review classification at all;
/// * `reviewed_at` absent → stale-review (no recorded review is not a fresh
///   review; this does not consult `distance`, so it holds even when HEAD is
///   unreadable);
/// * `distance` Undetermined → Undetermined (forwarded, never fresh);
/// * `distance > max_commits` → stale-review; otherwise (`<=`, so exactly N
///   behind is still fresh) → fresh.
pub fn classify_review(
    entry: &MapEntry,
    max_commits: u64,
    mut distance: impl FnMut(&str) -> Determination<u64>,
) -> Option<ReviewState> {
    if entry.status != Status::Tracked {
        return None;
    }
    let Some(at) = entry.reviewed_at.as_ref() else {
        return Some(Determination::known(ReviewObservation {
            freshness: ReviewFreshness::StaleReview,
            commits_behind: None,
        }));
    };
    Some(distance(&at.commit).map(|behind| ReviewObservation {
        freshness: if behind > max_commits {
            ReviewFreshness::StaleReview
        } else {
            ReviewFreshness::Fresh
        },
        commits_behind: Some(behind),
    }))
}

/// Git-backed `distance` oracle for [`classify_review`]: answers how many
/// commits a review commit is behind HEAD, every git call going through
/// `harness_core::boundary::run_with_timeout` and judged by exit status.
///
/// Each answer is `Undetermined` — never a number — when HEAD cannot be read
/// (not a git repo, unborn HEAD), when the recorded commit is not a plain hex
/// object name, when it is not an ancestor of HEAD (unknown object, or a commit
/// outside HEAD's history, e.g. rebased away: "how far behind" has no answer),
/// when any git call exits non-zero or times out, or when the count does not
/// parse. Answers are cached per commit, so a commit shared by many entries
/// costs one pair of git calls and records one give-up.
pub struct ReviewClock {
    repo_root: std::path::PathBuf,
    head: Determination<String>,
    cache: BTreeMap<String, Determination<u64>>,
}

/// Per-call bound on the git subprocesses [`ReviewClock`] runs.
const REVIEW_GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn review_git(
    repo_root: &Path,
    args: &[&str],
) -> Determination<harness_core::boundary::CommandOutput> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo_root).args(args);
    harness_core::boundary::run_with_timeout(&mut cmd, REVIEW_GIT_TIMEOUT)
}

fn is_hex_object_name(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl ReviewClock {
    /// Read HEAD once (`git rev-parse --verify HEAD^{commit}`). An unreadable
    /// HEAD is kept as `Undetermined` and makes every distance undetermined.
    pub fn open(repo_root: &Path) -> ReviewClock {
        let read = review_git(
            repo_root,
            &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        );
        let head = match read {
            Determination::Known(out) => match out.stdout_on_success() {
                Determination::Known(s) => {
                    let s = s.trim().to_string();
                    if is_hex_object_name(&s) {
                        Determination::known(s)
                    } else {
                        Determination::undetermined(format!(
                            "git rev-parse HEAD printed an unparsable object name {s:?}"
                        ))
                    }
                }
                Determination::Undetermined(why) => Determination::Undetermined(why),
            },
            Determination::Undetermined(why) => Determination::Undetermined(why),
        };
        ReviewClock {
            repo_root: repo_root.to_path_buf(),
            head,
            cache: BTreeMap::new(),
        }
    }

    /// The HEAD this clock measures against (`Undetermined` when unreadable).
    pub fn head(&self) -> &Determination<String> {
        &self.head
    }

    /// Commits `commit` is behind HEAD (`git rev-list --count <commit>..HEAD`),
    /// measured only after `git merge-base --is-ancestor <commit> HEAD` exited 0.
    pub fn distance(&mut self, commit: &str) -> Determination<u64> {
        let commit = commit.trim();
        if let Some(hit) = self.cache.get(commit) {
            return hit.clone();
        }
        let answer = self.measure(commit);
        self.cache.insert(commit.to_string(), answer.clone());
        answer
    }

    fn measure(&self, commit: &str) -> Determination<u64> {
        let head = match &self.head {
            Determination::Known(h) => h.clone(),
            Determination::Undetermined(why) => return Determination::Undetermined(why.clone()),
        };
        if !is_hex_object_name(commit) {
            return Determination::undetermined(format!(
                "reviewed_at.commit {commit:?} is not a hex object name"
            ));
        }
        let ancestry = review_git(
            &self.repo_root,
            &["merge-base", "--is-ancestor", commit, &head],
        );
        match ancestry {
            Determination::Known(out) => match out.code() {
                0 => {}
                1 => {
                    return Determination::undetermined(format!(
                        "reviewed_at.commit {commit} is not in HEAD's history (not an ancestor \
                         of {head}); its distance behind HEAD is unknown"
                    ))
                }
                // Any other exit (128: unknown object) is a git that did not
                // run to a conclusion; `stdout_on_success` mints that give-up
                // with the exit code and stderr.
                _ => {
                    return match out.stdout_on_success() {
                        Determination::Undetermined(why) => Determination::Undetermined(why),
                        Determination::Known(_) => Determination::undetermined(format!(
                            "git merge-base --is-ancestor {commit} {head} exited non-zero"
                        )),
                    }
                }
            },
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        }
        let range = format!("{commit}..{head}");
        let stdout = match review_git(&self.repo_root, &["rev-list", "--count", &range]) {
            Determination::Known(out) => match out.stdout_on_success() {
                Determination::Known(s) => s,
                Determination::Undetermined(why) => return Determination::Undetermined(why),
            },
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        };
        match stdout.trim().parse::<u64>() {
            Ok(n) => Determination::known(n),
            Err(e) => Determination::undetermined(format!(
                "git rev-list --count {range} printed an unparsable count {:?}: {e}",
                stdout.trim()
            )),
        }
    }
}

impl MapEntry {
    /// A fresh path-keyed skeleton entry (kind `Feature`, no spec-doc yet),
    /// created by the deterministic sync for a not-yet-attributed file.
    fn skeleton(key: &str, status: Status, last_ref: Option<String>) -> MapEntry {
        MapEntry {
            key: key.to_string(),
            kind: EntryKind::default(),
            spec_doc: None,
            status,
            last_ref,
            impl_files: Vec::new(),
            test_files: Vec::new(),
            client_refs: Vec::new(),
            symbols: Vec::new(),
            called_by: Vec::new(),
            ambiguous_symbols: Vec::new(),
            reviewed_reason: None,
            reviewed_at: None,
            api: None,
        }
    }

    /// True when this entry references `path` in the impl or test vector.
    fn has_path(&self, path: &str) -> bool {
        self.impl_files.iter().any(|p| p == path) || self.test_files.iter().any(|p| p == path)
    }

    /// Remove `path` from the impl and test vectors.
    fn remove_path(&mut self, path: &str) {
        self.impl_files.retain(|p| p != path);
        self.test_files.retain(|p| p != path);
    }

    /// Add `path` to the vector for `role` (deduped, sorted for stable diffs),
    /// first removing it from the other vector so a re-classified path never
    /// appears twice.
    fn add_path(&mut self, path: &str, role: FileRole) {
        self.remove_path(path);
        let v = match role {
            FileRole::Impl => &mut self.impl_files,
            FileRole::Test => &mut self.test_files,
        };
        v.push(path.to_string());
        v.sort();
        v.dedup();
    }

    /// True when no implementation or test file remains attributed.
    fn is_orphaned(&self) -> bool {
        self.impl_files.is_empty() && self.test_files.is_empty()
    }

    /// True when this entry is the untouched per-file skeleton the sync created
    /// for the test file `key` and nothing else: no impl files, exactly that one
    /// test file, no spec-doc, no endpoint data. Only such an entry is merged by
    /// [`SpecMap::relate_tests`]; anything a consumer authored is left alone.
    fn is_test_skeleton(&self, key: &str) -> bool {
        self.kind == EntryKind::Feature
            && self.impl_files.is_empty()
            && self.test_files.len() == 1
            && self.test_files[0] == key
            && self.spec_doc.as_deref().is_none_or(|s| s.trim().is_empty())
            && self.client_refs.is_empty()
            && self.api.is_none()
    }
}

/// Pure: does `entry` match the (case-insensitive substring) `query`? An
/// empty/blank query matches every entry (the whole-map default). Otherwise the
/// query is matched against the entry key, its spec_doc, any impl/test file
/// path, and — for endpoint entries — the api route. This lets a consumer scope
/// an operation to a specific command/crate/API by path or route (e.g.
/// `drift-map`, `crates/specguard`, `/health`). No filesystem access.
///
/// This is the single source of truth for entry targeting, shared by
/// `specguard audit --filter` and `specguard map list --filter`./// How many distinct files may declare a symbol name before a lexical call
/// edge to that name stops carrying attribution.
///
/// Not tuned against anything: it is the smallest number that still admits the
/// ordinary case of a name declared in a file and its test. Raising it trades
/// precision for reach.
pub const AMBIGUOUS_SYMBOL_DECLS: usize = 3;

/// What [`SpecMap::enrich`] wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnrichStats {
    /// Entries whose `symbols` or `called_by` changed.
    pub entries_changed: usize,
    /// Total symbol names attributed to entries.
    pub symbols: usize,
    /// Total (entry, calling file) pairs recorded.
    pub call_edges: usize,
    /// Total symbol names skipped as too ambiguous to attribute.
    pub ambiguous: usize,
}

impl SpecMap {
    /// Fill every entry's [`MapEntry::symbols`] and [`MapEntry::called_by`]
    /// from the repo's deterministic indexes.
    ///
    /// This is the join the map was missing. Before it, an entry knew which
    /// FILES realize a feature; it had no idea what those files declare or who
    /// depends on them, so "what else does changing this touch?" meant grepping
    /// the tree again. Both directions now come from indexes built once.
    ///
    /// `called_by` deliberately excludes an entry's own `impl_files`: internal
    /// calls are not drift signal, and including them would bury the handful of
    /// external callers that are.
    ///
    /// Fail-closed (CLAUDE.md 3): if the indexes cannot answer, the map is left
    /// **untouched** and the result is `Undetermined`. Writing empty vectors
    /// here would be the worst available outcome — a map that has been asked
    /// and a map that answered "nothing calls this" are indistinguishable once
    /// serialized, and the second one reads as permission to change anything.
    pub fn enrich(&mut self, repo_root: &Path) -> Determination<EnrichStats> {
        let symbols = match harness_core::index_store::all_symbols(repo_root) {
            Determination::Known(v) => v,
            Determination::Undetermined(u) => {
                return Determination::undetermined(u.reason().as_str().to_string())
            }
        };
        let edges = match harness_core::index_store::all_edges(repo_root) {
            Determination::Known(v) => v,
            Determination::Undetermined(u) => {
                return Determination::undetermined(u.reason().as_str().to_string())
            }
        };

        // file -> symbol names declared there, and name -> how many files
        // declare it (the ambiguity measure).
        let mut by_file: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut decl_files: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for sym in &symbols {
            by_file
                .entry(sym.file.as_str())
                .or_default()
                .push(sym.name.as_str());
            decl_files
                .entry(sym.name.as_str())
                .or_default()
                .insert(sym.file.as_str());
        }
        // callee name -> files calling it.
        let mut callers_of: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for e in &edges {
            callers_of
                .entry(e.callee.as_str())
                .or_default()
                .insert(e.file.as_str());
        }

        let mut stats = EnrichStats::default();
        for entry in self.entries.values_mut() {
            let own: BTreeSet<&str> = entry.impl_files.iter().map(String::as_str).collect();
            let mut names: BTreeSet<String> = BTreeSet::new();
            for f in &entry.impl_files {
                for n in by_file.get(f.as_str()).into_iter().flatten() {
                    names.insert((*n).to_string());
                }
            }
            let mut callers: BTreeSet<String> = BTreeSet::new();
            let mut ambiguous: BTreeSet<String> = BTreeSet::new();
            for n in &names {
                let declared_in = decl_files.get(n.as_str()).map_or(0, BTreeSet::len);
                if declared_in > AMBIGUOUS_SYMBOL_DECLS {
                    ambiguous.insert(n.clone());
                    continue;
                }
                for f in callers_of.get(n.as_str()).into_iter().flatten() {
                    if !own.contains(f) {
                        callers.insert((*f).to_string());
                    }
                }
            }
            let new_symbols: Vec<String> = names.into_iter().collect();
            let new_callers: Vec<String> = callers.into_iter().collect();
            let new_ambiguous: Vec<String> = ambiguous.into_iter().collect();
            if entry.symbols != new_symbols
                || entry.called_by != new_callers
                || entry.ambiguous_symbols != new_ambiguous
            {
                stats.entries_changed += 1;
            }
            stats.symbols += new_symbols.len();
            stats.call_edges += new_callers.len();
            stats.ambiguous += new_ambiguous.len();
            entry.symbols = new_symbols;
            entry.called_by = new_callers;
            entry.ambiguous_symbols = new_ambiguous;
        }
        Determination::known(stats)
    }
}

pub fn entry_matches(entry: &MapEntry, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let hay = |s: &str| s.to_lowercase().contains(&q);
    hay(&entry.key)
        || entry.spec_doc.as_deref().is_some_and(hay)
        || entry.impl_files.iter().any(|p| hay(p))
        || entry.test_files.iter().any(|p| hay(p))
        || entry.api.as_ref().is_some_and(|a| hay(&a.route))
}

/// The persisted feature/endpoint map. Keyed by entry id, kept in a `BTreeMap`
/// so serialization is deterministic (stable diffs).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecMap {
    /// Git ref the whole map was last synced up to (advisory; each entry also
    /// carries its own `last_ref`). Declared before `entries` so TOML emits this
    /// scalar before the `[entries.*]` tables.
    #[serde(default)]
    pub last_synced: String,
    /// entry id → mapping.
    #[serde(default)]
    pub entries: BTreeMap<String, MapEntry>,
}

/// A single file change parsed from `git log --name-status`. This is the pure
/// input the map reflection logic consumes, so the derivation is testable
/// without shelling out to git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// `A` — file added.
    Added(String),
    /// `M`/`T` — file modified (content or type changed).
    Modified(String),
    /// `R`/`C` — file renamed/copied from → to.
    Renamed { from: String, to: String },
    /// `D` — file deleted.
    Deleted(String),
}

/// Classify a repo-relative path as an implementation or a test file using a
/// simple, documented, purely path-based heuristic (the deterministic
/// skeleton; the LLM consumer refines true attribution):
///   * any path component named `tests` or `test` → [`FileRole::Test`];
///   * a `*.test.*` filename (e.g. `foo.test.ts`), or a file stem starting with
///     `test_`, ending with `_test`, or containing `_test_` → [`FileRole::Test`];
///   * otherwise → [`FileRole::Impl`].
pub fn classify_path(path: &str) -> FileRole {
    let norm = path.replace('\\', "/");
    if norm.split('/').any(|seg| seg == "tests" || seg == "test") {
        return FileRole::Test;
    }
    let file = norm.rsplit('/').next().unwrap_or(&norm);
    if file.contains(".test.") {
        return FileRole::Test;
    }
    let stem = file.split('.').next().unwrap_or(file);
    if stem.starts_with("test_") || stem.ends_with("_test") || stem.contains("_test_") {
        return FileRole::Test;
    }
    FileRole::Impl
}

/// File names too generic to identify an implementation file by name alone
/// (every Rust crate has one), so the unique-name rule of
/// [`test_impl_candidates`] never matches them.
const GENERIC_FILE_NAMES: &[&str] = &["lib.rs", "main.rs", "mod.rs"];

/// The file name a test file names by affix: `foo_test.rs` / `test_foo.rs` →
/// `foo.rs`, `foo.test.ts` → `foo.ts`. `None` when the name carries no such
/// affix (or stripping it leaves nothing).
fn strip_test_affix(file: &str) -> Option<String> {
    if let Some(i) = file.find(".test.") {
        let (stem, rest) = (&file[..i], &file[i + ".test".len()..]);
        return (!stem.is_empty()).then(|| format!("{stem}{rest}"));
    }
    let (stem, ext) = match file.split_once('.') {
        Some((s, e)) => (s, format!(".{e}")),
        None => (file, String::new()),
    };
    let base = stem
        .strip_suffix("_test")
        .or_else(|| stem.strip_prefix("test_"))?;
    (!base.is_empty()).then(|| format!("{base}{ext}"))
}

/// A rule of the test→implementation attribution heuristic, most specific
/// first. [`test_impl_candidates`] returns one of these per test path; the
/// caller takes the first rule that names exactly one mapped impl file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Candidate {
    /// This exact impl path.
    Path(String),
    /// The unique impl file under `dir` whose file name is `name`.
    UniqueName { dir: String, name: String },
}

/// Pure: the ordered candidate implementation files a test path is
/// attributed to by the deterministic heuristic (no filesystem, no map).
///
///   * Test file next to its implementation, named by affix
///     (`src/foo_test.rs`, `web/button.test.ts`, `pkg/test_foo.py` with no
///     `tests`/`test` directory in the path): the sibling `src/foo.rs` /
///     `web/button.ts` / `pkg/foo.py`.
///   * Test file under a `tests`/`test` directory (`<root>/tests/<rest>`):
///     1. the mirrored path `<root>/src/<rest>` (`tests/a/foo.rs` →
///        `src/a/foo.rs`);
///     2. the unique impl file under `<root>/src/` with the same file name
///        (never `lib.rs`/`main.rs`/`mod.rs` — see [`GENERIC_FILE_NAMES`]);
///     3. the crate root `<root>/src/lib.rs`, then `<root>/src/main.rs` — a
///        Cargo integration test (`tests/*.rs`) exercises the crate through
///        its root, so it is attributed there rather than dropped.
///
/// Anything else yields no candidate and stays a test-only entry: an
/// unattributable test is never attached to an arbitrary entry.
fn test_impl_candidates(test_path: &str) -> Vec<Candidate> {
    let norm = test_path.replace('\\', "/");
    let segs: Vec<&str> = norm.split('/').collect();
    let Some(t) = segs.iter().position(|s| *s == "tests" || *s == "test") else {
        // Affix-named test beside its implementation.
        let (dir, file) = match norm.rsplit_once('/') {
            Some((d, f)) => (format!("{d}/"), f),
            None => (String::new(), norm.as_str()),
        };
        return strip_test_affix(file)
            .map(|impl_name| vec![Candidate::Path(format!("{dir}{impl_name}"))])
            .unwrap_or_default();
    };
    let root: String = segs[..t].iter().map(|s| format!("{s}/")).collect();
    let rest = segs[t + 1..].join("/");
    if rest.is_empty() {
        return Vec::new();
    }
    let file = segs[segs.len() - 1];
    let src = format!("{root}src/");
    let mut out = vec![Candidate::Path(format!("{src}{rest}"))];
    if !GENERIC_FILE_NAMES.contains(&file) {
        out.push(Candidate::UniqueName {
            dir: src.clone(),
            name: file.to_string(),
        });
    }
    out.push(Candidate::Path(format!("{src}lib.rs")));
    out.push(Candidate::Path(format!("{src}main.rs")));
    out
}

/// Lexical: does this Rust source carry its own tests — a line-leading
/// `#[test]` attribute or a `#[<path>::test]` one (`#[tokio::test]`)? Covers
/// an inline `#[cfg(test)] mod tests { … }` in the implementation file, and a
/// `tests.rs` submodule file whose functions carry `#[test]`. A mention inside
/// a comment or doc comment does not start the line and is not counted.
pub fn has_inline_tests(source: &str) -> bool {
    source.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("#[test]")
            || (l.starts_with("#[")
                && l[2..]
                    .split([']', '('])
                    .next()
                    .is_some_and(|attr| attr.ends_with("::test")))
    })
}

/// True when `code` is a git `--name-status` status token: a leading status
/// letter (`A`/`M`/`D`/`R`/`C`/`T`) optionally followed by a similarity score
/// (e.g. `R100`). Checked against the *raw* first field (no leading-whitespace
/// trim) so indented commit-message lines never masquerade as change lines.
fn is_status_code(code: &str) -> bool {
    let mut chars = code.chars();
    match chars.next() {
        Some('A' | 'M' | 'D' | 'R' | 'C' | 'T') => chars.all(|c| c.is_ascii_digit()),
        _ => false,
    }
}

/// Parse raw `git log --name-status` output into a flat list of [`Change`]s
/// (in the order they appear — newest commit first as git emits them). Commit
/// headers, author/date lines and indented message bodies are ignored: only
/// tab-separated `<status>\t<path...>` lines are recognized. Pure — no I/O.
pub fn parse_name_status(text: &str) -> Vec<Change> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let Some(code) = fields.next() else {
            continue;
        };
        if !is_status_code(code) {
            continue;
        }
        // `is_status_code(code)` above already rejects the empty string, so the
        // else arm is unreachable; it mirrors the `!is_status_code` skip right
        // above rather than panicking on a line git could never emit.
        let Some(first) = code.chars().next() else {
            continue;
        };
        match first {
            'A' => {
                if let Some(p) = fields.next() {
                    out.push(Change::Added(p.trim().to_string()));
                }
            }
            'M' | 'T' => {
                if let Some(p) = fields.next() {
                    out.push(Change::Modified(p.trim().to_string()));
                }
            }
            'D' => {
                if let Some(p) = fields.next() {
                    out.push(Change::Deleted(p.trim().to_string()));
                }
            }
            'R' | 'C' => {
                if let (Some(from), Some(to)) = (fields.next(), fields.next()) {
                    out.push(Change::Renamed {
                        from: from.trim().to_string(),
                        to: to.trim().to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

impl SpecMap {
    /// Load the map from `path`. A missing file yields an empty (default) map —
    /// callers get a usable store on first use without special-casing. A present
    /// but unparseable file is a hard error (don't silently discard state).
    /// Backfills each entry's `key` from its map key if the persisted entry
    /// omitted it (older/partial maps).
    pub fn load(path: &Path) -> Result<SpecMap> {
        let mut map: SpecMap = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("parsing spec map {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => SpecMap::default(),
            Err(e) => {
                return Err(anyhow::Error::new(e))
                    .with_context(|| format!("reading spec map {}", path.display()))
            }
        };
        for (k, entry) in map.entries.iter_mut() {
            if entry.key.is_empty() {
                entry.key = k.clone();
            }
        }
        Ok(map)
    }

    /// Load the map, creating an empty one on disk if the file is absent
    /// (create-if-absent). Returns the loaded (or freshly created) map.
    pub fn load_or_init(path: &Path) -> Result<SpecMap> {
        if !path.exists() {
            let empty = SpecMap::default();
            empty.save(path)?;
            return Ok(empty);
        }
        SpecMap::load(path)
    }

    /// Persist the map to `path` as TOML, creating parent directories as needed.
    /// Deterministic output (BTreeMap key + sorted vectors) for clean diffs.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating spec map dir {}", dir.display()))?;
            }
        }
        let body = toml::to_string(self).context("serializing spec map")?;
        std::fs::write(path, body)
            .with_context(|| format!("writing spec map {}", path.display()))?;
        Ok(())
    }

    /// Number of mapped entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the map has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The key of the entry that already references `path` (in its impl or test
    /// vector), if any. Lets a consumer-authored feature/endpoint entry (multiple
    /// files under one key) claim a changed path instead of the skeleton spawning
    /// a per-file entry.
    fn key_owning(&self, path: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|(_, e)| e.has_path(path))
            .map(|(k, _)| k.clone())
    }

    /// Reflect a batch of parsed [`Change`]s into the map (pure — no I/O) using
    /// the deterministic path skeleton. `spec_dir` is available for a consumer's
    /// derivation conventions; `synced_ref` stamps every touched entry's
    /// `last_ref`. Behaviour:
    ///   * `A`/`M`/`T` (added/modified) → attribute the path via
    ///     [`classify_path`]. If an existing entry already owns it, add it to the
    ///     correct vector there and mark that entry `Changed`. Otherwise create
    ///     a new skeleton entry keyed by the path itself, status `Changed`.
    ///   * after the batch, [`SpecMap::relate_tests`] merges every test-only
    ///     skeleton the heuristic can attribute into its implementation entry,
    ///     so the relation does not depend on the order git listed the files.
    ///   * `R`/`C` (renamed) → detach the old path from its owning entry and
    ///     attribute the new path.
    ///   * `D` (deleted) → detach the path from its owning entry; if that leaves
    ///     the entry with no impl/test files it is marked `Missing`, else
    ///     `Changed`. A delete of an unmapped file is a no-op.
    ///
    /// **Ordering.** `changes` is expected in the order `git log --name-status`
    /// emits it — **reverse-chronological** (newest commit first). To land each
    /// path in its *final* state (last-writer-wins by real commit time), the
    /// events are folded in **true chronological order** by iterating the slice
    /// back-to-front. Without this, a full-history build would apply a recent
    /// `D` (delete) *before* an older `A` (add) of the same path, and the add
    /// would resurrect the deleted path as a dangling entry. Reversing makes the
    /// newest commit win, so a path deleted in the newest commit correctly ends
    /// up with no live entry regardless of the log's reverse order.
    pub fn apply_changes(&mut self, changes: &[Change], _spec_dir: &str, synced_ref: &str) {
        // git emits newest→oldest; fold oldest→newest so the most recent event
        // for each path is the last writer.
        for change in changes.iter().rev() {
            match change {
                Change::Added(p) | Change::Modified(p) => self.attribute_path(p, synced_ref),
                Change::Renamed { from, to } => {
                    self.detach_path(from, synced_ref);
                    self.attribute_path(to, synced_ref);
                }
                Change::Deleted(p) => self.detach_path(p, synced_ref),
            }
        }
        self.relate_tests();
        if !synced_ref.is_empty() {
            self.last_synced = synced_ref.to_string();
        }
    }

    /// The key of the entry that holds `impl_path` in its `impl_files`.
    fn key_owning_impl(&self, impl_path: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|(_, e)| e.impl_files.iter().any(|p| p == impl_path))
            .map(|(k, _)| k.clone())
    }

    /// The entry a test file is attributed to by the deterministic heuristic
    /// ([`test_impl_candidates`]): the first candidate rule naming exactly one
    /// mapped implementation file wins. `None` for a non-test path or when no
    /// rule resolves — the caller then keeps the test in its own entry.
    fn heuristic_owner(&self, test_path: &str) -> Option<String> {
        if classify_path(test_path) != FileRole::Test {
            return None;
        }
        for cand in test_impl_candidates(test_path) {
            match cand {
                Candidate::Path(p) => {
                    if let Some(k) = self.key_owning_impl(&p) {
                        return Some(k);
                    }
                }
                Candidate::UniqueName { dir, name } => {
                    let mut hits = self.entries.iter().flat_map(|(k, e)| {
                        e.impl_files
                            .iter()
                            .filter(|p| {
                                p.strip_prefix(dir.as_str())
                                    .is_some_and(|r| r == name || r.ends_with(&format!("/{name}")))
                            })
                            .map(move |_| k)
                    });
                    if let (Some(k), None) = (hits.next(), hits.next()) {
                        return Some(k.clone());
                    }
                }
            }
        }
        None
    }

    /// Relate test files to implementation entries: every test-only skeleton
    /// entry (see [`MapEntry::is_test_skeleton`]) whose test file the
    /// heuristic attributes to an implementation entry is merged into that
    /// entry's `test_files` and removed. A test the heuristic cannot attribute
    /// keeps its own test-only entry; consumer-authored entries are never
    /// touched. Pure — no I/O. Returns the merged test paths (sorted).
    ///
    /// Run after every sync so the result does not depend on the order in
    /// which git listed a test and its implementation. Once merged, later syncs
    /// find the test through [`SpecMap::key_owning`] and update it in place.
    pub fn relate_tests(&mut self) -> Vec<String> {
        let skeletons: Vec<String> = self
            .entries
            .iter()
            .filter(|(k, e)| e.is_test_skeleton(k))
            .map(|(k, _)| k.clone())
            .collect();
        let mut merged = Vec::new();
        for test in skeletons {
            let Some(owner) = self.heuristic_owner(&test) else {
                continue;
            };
            if owner == test {
                continue;
            }
            let Some(skel) = self.entries.remove(&test) else {
                continue;
            };
            let Some(entry) = self.entries.get_mut(&owner) else {
                // `heuristic_owner` just returned `owner` from this map, so
                // this arm is unreachable; put the skeleton back rather than
                // lose the test.
                self.entries.insert(test, skel);
                continue;
            };
            entry.add_path(&test, FileRole::Test);
            if skel.status == Status::Changed {
                entry.status = Status::Changed;
            }
            if skel.last_ref.is_some() {
                entry.last_ref = skel.last_ref;
            }
            merged.push(test);
        }
        merged
    }

    /// Credit implementation files that carry their own tests
    /// ([`has_inline_tests`]): such a `.rs` file is listed in its entry's
    /// `test_files` as well as its `impl_files`. Recomputed from disk for every
    /// entry on each call, so the credit disappears when the tests do. An absent
    /// file gets no credit (the audit reports it as a dangling reference). A
    /// file that cannot be read gets NO credit either — "could not check" must
    /// not read as "tested" (the entry is then reported untested, the
    /// restrictive side) — and is returned, with the reason, so the caller can
    /// say which entries were not checked rather than let the gap pass silently.
    pub fn mark_inline_tests(&mut self, repo_root: &Path) -> Vec<String> {
        let mut unreadable = Vec::new();
        for entry in self.entries.values_mut() {
            let impls = entry.impl_files.clone();
            for f in impls {
                let tested = f.ends_with(".rs")
                    && match harness_core::boundary::read_to_string(&repo_root.join(&f)) {
                        Determination::Known(src) => src.is_some_and(|s| has_inline_tests(&s)),
                        Determination::Undetermined(u) => {
                            unreadable.push(format!("{f}: {}", u.as_str()));
                            false
                        }
                    };
                let listed = entry.test_files.contains(&f);
                if tested {
                    if !listed {
                        entry.test_files.push(f);
                        entry.test_files.sort();
                    }
                } else if listed {
                    entry.test_files.retain(|p| *p != f);
                }
            }
        }
        unreadable
    }

    /// Explicit writer: relate `test_path` to the entry `key` by moving it into
    /// that entry's `test_files` (detaching it from any other entry and dropping
    /// the test-only skeleton that held it). This is the deterministic override
    /// for a relation the heuristic cannot see; later syncs keep it because
    /// [`SpecMap::key_owning`] finds the test in `key`. Errors when `key` is not
    /// in the map. Pure — no I/O; the caller checks the file exists.
    pub fn link_test(&mut self, test_path: &str, key: &str) -> Result<()> {
        if !self.entries.contains_key(key) {
            anyhow::bail!("no map entry with key '{key}'");
        }
        let holders: Vec<String> = self
            .entries
            .iter()
            .filter(|(k, e)| k.as_str() != key && e.has_path(test_path))
            .map(|(k, _)| k.clone())
            .collect();
        for k in holders {
            let drop = self
                .entries
                .get(&k)
                .is_some_and(|e| e.is_test_skeleton(&k) && k == test_path);
            if drop {
                self.entries.remove(&k);
            } else if let Some(e) = self.entries.get_mut(&k) {
                e.remove_path(test_path);
                if e.is_orphaned() {
                    e.status = Status::Missing;
                }
            }
        }
        if let Some(e) = self.entries.get_mut(key) {
            if !e.test_files.iter().any(|p| p == test_path) {
                e.test_files.push(test_path.to_string());
                e.test_files.sort();
            }
        }
        Ok(())
    }

    /// Attribute a changed/added path into the map: into its owning entry if one
    /// exists, else into a fresh path-keyed skeleton entry. Marks the entry
    /// `Changed`. A new test file's skeleton is merged into its implementation
    /// entry afterwards, by [`SpecMap::relate_tests`] at the end of the batch.
    fn attribute_path(&mut self, path: &str, synced_ref: &str) {
        let role = classify_path(path);
        let last_ref = ref_opt(synced_ref);
        let key = self.key_owning(path).unwrap_or_else(|| path.to_string());
        let entry = self
            .entries
            .entry(key.clone())
            .or_insert_with(|| MapEntry::skeleton(&key, Status::Changed, last_ref.clone()));
        entry.add_path(path, role);
        entry.status = Status::Changed;
        entry.last_ref = last_ref;
    }

    /// Detach a deleted/renamed-away path from its owning entry. The entry is
    /// marked `Missing` if it now has no impl/test files, else `Changed`.
    fn detach_path(&mut self, path: &str, synced_ref: &str) {
        let Some(key) = self.key_owning(path) else {
            return;
        };
        // `key_owning` just returned this key from the same map, so the lookup
        // cannot miss; the else arm mirrors the `let Some(key) … else { return }`
        // directly above rather than panicking.
        let Some(entry) = self.entries.get_mut(&key) else {
            return;
        };
        entry.remove_path(path);
        entry.last_ref = ref_opt(synced_ref);
        entry.status = if entry.is_orphaned() {
            Status::Missing
        } else {
            Status::Changed
        };
    }

    /// Reconcile the map against `git log --name-status <baseline>..HEAD`, run
    /// from `repo_root`. Parses the name-status output (via [`parse_name_status`])
    /// and reflects it (via [`apply_changes`]). `synced_ref` is stamped onto every
    /// touched entry (typically the current HEAD). Then credits implementation
    /// files that carry their own tests (via [`SpecMap::mark_inline_tests`],
    /// which reads the files under `repo_root`). The git invocation and that
    /// read are the only impure parts; the derivation is delegated to the pure
    /// helpers above. Returns the impl files whose inline tests could not be
    /// checked (see [`SpecMap::mark_inline_tests`]).
    ///
    /// [`apply_changes`]: SpecMap::apply_changes
    pub fn sync(
        &mut self,
        repo_root: &Path,
        baseline: &str,
        spec_dir: &str,
        synced_ref: &str,
        exclude: &GlobSet,
    ) -> Result<Vec<String>> {
        let text = git_log_name_status(repo_root, baseline)?;
        let changes = filter_excluded(parse_name_status(&text), exclude);
        self.apply_changes(&changes, spec_dir, synced_ref);
        Ok(self.mark_inline_tests(repo_root))
    }

    /// Remove every entry whose key matches one of the `exclude` globs — the
    /// non-spec-bearing paths (lockfiles, manifests, generated artifacts). Pure —
    /// no I/O. Returns the removed keys (sorted, for deterministic reporting).
    /// Combined with [`filter_excluded`] on `sync`, this drives the map to a
    /// state where every remaining entry is a genuine feature/endpoint, so a
    /// `changed` status reflects real spec drift rather than config churn.
    pub fn prune_excluded(&mut self, exclude: &GlobSet) -> Vec<String> {
        let removed: Vec<String> = self
            .entries
            .keys()
            .filter(|k| exclude.is_match(k.as_str()))
            .cloned()
            .collect();
        for k in &removed {
            self.entries.remove(k);
        }
        removed
    }

    /// Attach `doc` as the `spec_doc` of every entry whose key matches
    /// `selector` (an exact key or a glob such as `crates/foo/src/**`), marking
    /// each `Tracked` and stamping `review` (reason + commit + date) on it —
    /// the resolution for a mapped source file that now has an authored spec.
    /// Nothing checks that `doc` describes the code: `tracked` here records a
    /// reviewed CLAIM, which is why a [`Review`] is mandatory. Pure — no I/O.
    /// Returns the touched keys (sorted). Errors only on an invalid glob; a
    /// valid selector that matches nothing returns an empty vector (the caller
    /// decides whether that is worth reporting).
    pub fn set_spec(&mut self, selector: &str, doc: &str, review: &Review) -> Result<Vec<String>> {
        let set = compile_globs(std::slice::from_ref(&selector.to_string()))?;
        let keys: Vec<String> = self
            .entries
            .keys()
            .filter(|k| set.is_match(k.as_str()))
            .cloned()
            .collect();
        for k in &keys {
            if let Some(e) = self.entries.get_mut(k) {
                e.spec_doc = Some(doc.to_string());
                review.apply(e);
            }
        }
        Ok(keys)
    }

    /// Mark every entry whose key matches `selector` (exact key or glob) as
    /// `Tracked`, stamping `review` (reason + commit + date) on each — the
    /// "reviewed, no spec drift" resolution for entries that need no authored
    /// spec-doc. Nothing is verified here; the stamp records who-claimed-what
    /// so the claim can be re-examined later. Pure — no I/O. Returns the
    /// touched keys. Errors only on an invalid glob.
    pub fn resolve(&mut self, selector: &str, review: &Review) -> Result<Vec<String>> {
        let set = compile_globs(std::slice::from_ref(&selector.to_string()))?;
        let keys: Vec<String> = self
            .entries
            .keys()
            .filter(|k| set.is_match(k.as_str()))
            .cloned()
            .collect();
        for k in &keys {
            if let Some(e) = self.entries.get_mut(k) {
                review.apply(e);
            }
        }
        Ok(keys)
    }
}

/// Compile repo-root-relative globs into a [`GlobSet`]. An empty slice yields an
/// empty set that matches nothing (so an unset `[map].exclude` preserves prior
/// behaviour). Errors on an invalid glob pattern.
pub fn compile_globs(globs: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for g in globs {
        builder.add(Glob::new(g).with_context(|| format!("invalid map glob '{g}'"))?);
    }
    builder.build().context("building map glob set")
}

/// Drop `Added`/`Modified` changes whose path matches an `exclude` glob so they
/// are never attributed as new map entries. `Renamed` and `Deleted` are kept so
/// detach/cleanup of an existing entry still runs. Pure — no I/O.
fn filter_excluded(changes: Vec<Change>, exclude: &GlobSet) -> Vec<Change> {
    changes
        .into_iter()
        .filter(|c| match c {
            Change::Added(p) | Change::Modified(p) => !exclude.is_match(p),
            _ => true,
        })
        .collect()
}

/// `Some(ref)` for a non-empty git ref, else `None` (used for `last_ref`).
fn ref_opt(r: &str) -> Option<String> {
    if r.is_empty() {
        None
    } else {
        Some(r.to_string())
    }
}

/// Run `git log --name-status <baseline>..HEAD` from `repo_root` and return its
/// raw stdout. The `baseline` is validated with the same safe-ref guard the
/// scope resolver uses, so a hostile ref can never reach git.
fn git_log_name_status(repo_root: &Path, baseline: &str) -> Result<String> {
    if !crate::scope::is_safe_ref(baseline) {
        anyhow::bail!(
            "refusing unsafe baseline ref '{baseline}': only [A-Za-z0-9_./~^-] are allowed and it must not start with '-'"
        );
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("log")
        .arg("--name-status")
        .arg("--no-color")
        .arg(format!("{baseline}..HEAD"))
        .output()
        .context("spawning git log")?;
    if !out.status.success() {
        anyhow::bail!(
            "git log --name-status {baseline}..HEAD failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_absent_returns_empty_default() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".specguard/spec-map.toml");
        // File does not exist yet.
        let map = SpecMap::load(&path).unwrap();
        assert!(map.is_empty());
        assert_eq!(map.last_synced, "");
        // load() must not have created the file.
        assert!(!path.exists());
    }

    #[test]
    fn load_or_init_creates_file_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".specguard/spec-map.toml");
        let map = SpecMap::load_or_init(&path).unwrap();
        assert!(map.is_empty());
        assert!(path.exists(), "load_or_init should create the file");
        // Reloading the created file yields the same empty map.
        assert_eq!(SpecMap::load(&path).unwrap(), map);
    }

    /// Seed a map with per-path skeleton entries for the given added paths.
    fn seeded(paths: &[&str]) -> SpecMap {
        let mut map = SpecMap::default();
        let changes: Vec<Change> = paths.iter().map(|p| Change::Added(p.to_string())).collect();
        map.apply_changes(&changes, "docs/specs", "r1");
        map
    }

    #[test]
    fn compile_globs_empty_matches_nothing() {
        let set = compile_globs(&[]).unwrap();
        assert!(!set.is_match("anything/at/all.rs"));
        assert!(!set.is_match("Cargo.lock"));
    }

    #[test]
    fn compile_globs_rejects_invalid_pattern() {
        assert!(compile_globs(&["a/[".to_string()]).is_err());
    }

    #[test]
    fn filter_excluded_drops_added_modified_keeps_delete_rename() {
        let set = compile_globs(&["**/Cargo.lock".to_string(), "**/*.toml".to_string()]).unwrap();
        let changes = vec![
            Change::Added("crates/foo/Cargo.lock".to_string()),
            Change::Modified("crates/foo/src/lib.rs".to_string()),
            Change::Deleted("crates/foo/Cargo.toml".to_string()),
            Change::Renamed {
                from: "a/old.rs".to_string(),
                to: "a/new.rs".to_string(),
            },
        ];
        let kept = filter_excluded(changes, &set);
        // Cargo.lock (Added, excluded) dropped; lib.rs kept; Deleted + Renamed kept.
        assert_eq!(kept.len(), 3);
        assert!(kept
            .iter()
            .any(|c| matches!(c, Change::Modified(p) if p == "crates/foo/src/lib.rs")));
        assert!(kept
            .iter()
            .any(|c| matches!(c, Change::Deleted(p) if p == "crates/foo/Cargo.toml")));
        assert!(kept.iter().any(|c| matches!(c, Change::Renamed { .. })));
    }

    #[test]
    fn prune_excluded_removes_matching_keys() {
        let mut map = seeded(&[
            "Cargo.lock",
            ".claude-plugin/marketplace.json",
            "crates/foo/src/lib.rs",
        ]);
        assert_eq!(map.len(), 3);
        let set = compile_globs(&["Cargo.lock".to_string(), "**/*.json".to_string()]).unwrap();
        let removed = map.prune_excluded(&set);
        assert_eq!(removed.len(), 2);
        assert_eq!(map.len(), 1);
        assert!(map.entries.contains_key("crates/foo/src/lib.rs"));
    }

    fn review() -> Review {
        Review::new("reviewed in unit test", "cafef00d", "2026-01-01").unwrap()
    }

    #[test]
    fn review_rejects_blank_reason_commit_or_date() {
        assert!(Review::new("", "c", "d").is_err());
        assert!(Review::new(" \t\n", "c", "d").is_err());
        assert!(Review::new("why", "  ", "d").is_err());
        assert!(Review::new("why", "c", "").is_err());
        assert!(Review::new("why", "c", "d").is_ok());
    }

    #[test]
    fn resolve_stamps_reason_commit_and_date_on_touched_entries_only() {
        let mut map = seeded(&["a/b.rs", "d/e.rs"]);
        map.resolve(
            "a/**",
            &Review::new("  looked at it  ", "abc123", "2026-02-03").unwrap(),
        )
        .unwrap();
        let e = &map.entries["a/b.rs"];
        assert_eq!(e.reviewed_reason.as_deref(), Some("looked at it"));
        assert_eq!(
            e.reviewed_at,
            Some(ReviewedAt {
                commit: "abc123".to_string(),
                date: "2026-02-03".to_string()
            })
        );
        let other = &map.entries["d/e.rs"];
        assert!(other.reviewed_reason.is_none() && other.reviewed_at.is_none());
    }

    #[test]
    fn set_spec_attaches_doc_and_tracks_on_glob() {
        let mut map = seeded(&[
            "crates/benchkit/src/harness.rs",
            "crates/benchkit/src/lib.rs",
            "crates/benchkit/src/main.rs",
            "crates/other/src/main.rs",
        ]);
        // Every seeded entry starts Changed with no spec_doc.
        assert!(map
            .entries
            .values()
            .all(|e| e.status == Status::Changed && e.spec_doc.is_none()));
        let touched = map
            .set_spec(
                "crates/benchkit/src/**",
                "docs/specs/benchkit.md",
                &review(),
            )
            .unwrap();
        assert_eq!(touched.len(), 3);
        for k in &touched {
            let e = &map.entries[k];
            assert_eq!(e.spec_doc.as_deref(), Some("docs/specs/benchkit.md"));
            assert_eq!(e.status, Status::Tracked);
        }
        // The unrelated crate is untouched.
        let other = &map.entries["crates/other/src/main.rs"];
        assert_eq!(other.status, Status::Changed);
        assert!(other.spec_doc.is_none());
    }

    #[test]
    fn set_spec_exact_key_matches_one() {
        let mut map = seeded(&["crates/difflog/src/main.rs", "crates/ship/src/main.rs"]);
        let touched = map
            .set_spec(
                "crates/difflog/src/main.rs",
                "docs/specs/difflog.md",
                &review(),
            )
            .unwrap();
        assert_eq!(touched, vec!["crates/difflog/src/main.rs".to_string()]);
        assert_eq!(
            map.entries["crates/ship/src/main.rs"].status,
            Status::Changed
        );
    }

    #[test]
    fn set_spec_no_match_is_empty() {
        let mut map = seeded(&["crates/foo/src/lib.rs"]);
        let touched = map
            .set_spec("crates/nope/**", "docs/specs/x.md", &review())
            .unwrap();
        assert!(touched.is_empty());
    }

    #[test]
    fn resolve_marks_tracked_without_spec() {
        let mut map = seeded(&["a/b.rs", "a/c.rs", "d/e.rs"]);
        let touched = map.resolve("a/**", &review()).unwrap();
        assert_eq!(touched.len(), 2);
        assert_eq!(map.entries["a/b.rs"].status, Status::Tracked);
        assert!(map.entries["a/b.rs"].spec_doc.is_none());
        assert_eq!(map.entries["d/e.rs"].status, Status::Changed);
    }

    #[test]
    fn save_then_load_round_trips_endpoint_entry_with_api_and_client_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/spec-map.toml");
        let mut map = SpecMap::default();
        // A consumer-authored ENDPOINT entry exercising every field, to prove the
        // api + client_refs dimensions serialize and round-trip.
        map.entries.insert(
            "GET /api/users/:id".to_string(),
            MapEntry {
                key: "GET /api/users/:id".to_string(),
                kind: EntryKind::Endpoint,
                spec_doc: Some("docs/specs/users.md".to_string()),
                status: Status::Tracked,
                last_ref: Some("cafef00d".to_string()),
                impl_files: vec!["src/server/users.rs".to_string()],
                test_files: vec!["tests/users_test.rs".to_string()],
                client_refs: vec!["web/api/users.ts".to_string()],
                symbols: vec![],
                called_by: vec![],
                ambiguous_symbols: vec![],
                reviewed_reason: None,
                reviewed_at: None,
                api: Some(ApiRef {
                    method: "GET".to_string(),
                    route: "/api/users/:id".to_string(),
                }),
            },
        );
        map.last_synced = "cafef00d".to_string();
        map.save(&path).unwrap();

        let loaded = SpecMap::load(&path).unwrap();
        assert_eq!(loaded, map);
        let e = &loaded.entries["GET /api/users/:id"];
        assert_eq!(e.kind, EntryKind::Endpoint);
        assert_eq!(e.spec_doc.as_deref(), Some("docs/specs/users.md"));
        assert_eq!(e.impl_files, vec!["src/server/users.rs".to_string()]);
        assert_eq!(e.test_files, vec!["tests/users_test.rs".to_string()]);
        assert_eq!(e.client_refs, vec!["web/api/users.ts".to_string()]);
        assert_eq!(
            e.api,
            Some(ApiRef {
                method: "GET".to_string(),
                route: "/api/users/:id".to_string()
            })
        );
        assert_eq!(e.last_ref.as_deref(), Some("cafef00d"));
    }

    #[test]
    fn partial_map_still_parses_and_backfills_key() {
        // An older/partial map that predates the newer fields (no kind, no
        // api/client_refs, no inner key) must still parse via serde defaults.
        let toml = "\
last_synced = \"r0\"
[entries.\"feat-x\"]
status = \"tracked\"
impl_files = [\"src/x.rs\"]
";
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("spec-map.toml");
        std::fs::write(&path, toml).unwrap();
        let map = SpecMap::load(&path).unwrap();
        let e = &map.entries["feat-x"];
        assert_eq!(e.kind, EntryKind::Feature); // default
        assert_eq!(e.key, "feat-x"); // backfilled from map key
        assert!(e.api.is_none());
        assert!(e.client_refs.is_empty());
        assert_eq!(e.impl_files, vec!["src/x.rs".to_string()]);
    }

    #[test]
    fn classify_path_impl_vs_test() {
        assert_eq!(classify_path("src/server/api.rs"), FileRole::Impl);
        assert_eq!(classify_path("crates/x/src/lib.rs"), FileRole::Impl);
        // path component `tests`/`test`
        assert_eq!(classify_path("tests/integration.rs"), FileRole::Test);
        assert_eq!(classify_path("crates/x/tests/e2e.rs"), FileRole::Test);
        // filename conventions
        assert_eq!(classify_path("src/foo_test.rs"), FileRole::Test);
        assert_eq!(classify_path("src/test_foo.py"), FileRole::Test);
        assert_eq!(classify_path("web/button.test.ts"), FileRole::Test);
    }

    #[test]
    fn added_test_path_lands_in_test_files_impl_path_in_impl_files() {
        let mut map = SpecMap::default();
        map.apply_changes(
            &[
                Change::Added("src/server/api.rs".to_string()),
                Change::Added("tests/api_it.rs".to_string()),
            ],
            "docs/specs",
            "ref1",
        );
        // Each unattributed file becomes its own skeleton entry keyed by path.
        let impl_entry = &map.entries["src/server/api.rs"];
        assert_eq!(impl_entry.impl_files, vec!["src/server/api.rs".to_string()]);
        assert!(impl_entry.test_files.is_empty());
        assert_eq!(impl_entry.status, Status::Changed);
        assert_eq!(impl_entry.last_ref.as_deref(), Some("ref1"));

        let test_entry = &map.entries["tests/api_it.rs"];
        assert_eq!(test_entry.test_files, vec!["tests/api_it.rs".to_string()]);
        assert!(test_entry.impl_files.is_empty());
    }

    #[test]
    fn modified_marks_owning_entry_changed() {
        let mut map = SpecMap::default();
        // Consumer-authored feature: one impl + one test under a feature key.
        map.entries.insert(
            "login".to_string(),
            MapEntry {
                key: "login".to_string(),
                kind: EntryKind::Feature,
                spec_doc: Some("docs/specs/login.md".to_string()),
                status: Status::Tracked,
                last_ref: Some("ref0".to_string()),
                impl_files: vec!["src/login.rs".to_string()],
                test_files: vec!["tests/login_test.rs".to_string()],
                client_refs: vec![],
                symbols: vec![],
                called_by: vec![],
                ambiguous_symbols: vec![],
                reviewed_reason: None,
                reviewed_at: None,
                api: None,
            },
        );
        map.apply_changes(
            &[Change::Modified("src/login.rs".to_string())],
            "docs/specs",
            "ref2",
        );
        let e = &map.entries["login"];
        assert_eq!(e.status, Status::Changed);
        assert_eq!(e.last_ref.as_deref(), Some("ref2"));
        // No stray per-file entry was created (the owning feature claimed it).
        assert_eq!(map.len(), 1);
        assert_eq!(e.impl_files, vec!["src/login.rs".to_string()]);
    }

    #[test]
    fn renamed_moves_path_between_entries() {
        let mut map = SpecMap::default();
        map.apply_changes(
            &[Change::Added("src/old.rs".to_string())],
            "docs/specs",
            "ref1",
        );
        map.apply_changes(
            &[Change::Renamed {
                from: "src/old.rs".to_string(),
                to: "src/new.rs".to_string(),
            }],
            "docs/specs",
            "ref2",
        );
        // Old skeleton entry emptied → Missing; new path attributed.
        assert_eq!(map.entries["src/old.rs"].status, Status::Missing);
        assert!(map.entries["src/old.rs"].is_orphaned());
        let e = &map.entries["src/new.rs"];
        assert_eq!(e.impl_files, vec!["src/new.rs".to_string()]);
        assert_eq!(e.status, Status::Changed);
        assert_eq!(e.last_ref.as_deref(), Some("ref2"));
    }

    #[test]
    fn deleted_detaches_and_marks_missing_when_empty() {
        let mut map = SpecMap::default();
        // Feature with two impl files: deleting one keeps it Changed (still has
        // files); deleting the last leaves it Missing.
        map.entries.insert(
            "feat".to_string(),
            MapEntry {
                key: "feat".to_string(),
                kind: EntryKind::Feature,
                spec_doc: Some("docs/specs/feat.md".to_string()),
                status: Status::Tracked,
                last_ref: Some("ref0".to_string()),
                impl_files: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
                test_files: vec![],
                client_refs: vec![],
                symbols: vec![],
                called_by: vec![],
                ambiguous_symbols: vec![],
                reviewed_reason: None,
                reviewed_at: None,
                api: None,
            },
        );
        map.apply_changes(
            &[Change::Deleted("src/a.rs".to_string())],
            "docs/specs",
            "ref1",
        );
        assert_eq!(map.entries["feat"].status, Status::Changed);
        assert_eq!(map.entries["feat"].impl_files, vec!["src/b.rs".to_string()]);

        map.apply_changes(
            &[Change::Deleted("src/b.rs".to_string())],
            "docs/specs",
            "ref2",
        );
        assert_eq!(map.entries["feat"].status, Status::Missing);
        assert!(map.entries["feat"].is_orphaned());

        // Deleting an unmapped file is a no-op.
        map.apply_changes(
            &[Change::Deleted("src/never.rs".to_string())],
            "docs/specs",
            "ref3",
        );
        assert!(!map.entries.contains_key("src/never.rs"));
    }

    #[test]
    fn parse_name_status_recognizes_all_kinds() {
        // Realistic `git log --name-status` fragment: commit headers + message
        // body (indented) must be ignored; only status lines are parsed.
        let text = "commit abc123\n\
                    Author: A U Thor <a@b.c>\n\
                    Date:   Mon Jan 1 00:00:00 2026 +0000\n\
                    \n    Add and modify things\n\n\
                    A\tsrc/added.rs\n\
                    M\tsrc/modified.rs\n\
                    R100\tsrc/old.rs\tsrc/new.rs\n\
                    D\tsrc/deleted.rs\n\
                    T\tsrc/typechange.rs\n";
        let changes = parse_name_status(text);
        assert_eq!(
            changes,
            vec![
                Change::Added("src/added.rs".to_string()),
                Change::Modified("src/modified.rs".to_string()),
                Change::Renamed {
                    from: "src/old.rs".to_string(),
                    to: "src/new.rs".to_string(),
                },
                Change::Deleted("src/deleted.rs".to_string()),
                Change::Modified("src/typechange.rs".to_string()),
            ]
        );
    }

    #[test]
    fn parse_name_status_ignores_message_lines_starting_with_status_letter() {
        // A message body line that happens to start with 'A'/'M' etc. (indented,
        // no tab-separated path) must not be mistaken for a change line.
        let text = "    Added a new module\n    Move things around\nA\tsrc/real.rs\n";
        let changes = parse_name_status(text);
        assert_eq!(changes, vec![Change::Added("src/real.rs".to_string())]);
    }

    #[test]
    fn end_to_end_name_status_text_reflects_into_map() {
        // The pure git-log→map pipeline without shelling out to real git:
        // feed synthetic --name-status text through parse + apply. A test path
        // and an impl path are attributed to their respective vectors.
        let mut map = SpecMap::default();
        let text = "A\tsrc/keep.rs\nA\ttests/keep_test.rs\nM\tsrc/keep.rs\n";
        let changes = parse_name_status(text);
        map.apply_changes(&changes, "docs/specs", "r1");

        assert_eq!(
            map.entries["src/keep.rs"].impl_files,
            vec!["src/keep.rs".to_string()]
        );
        assert_eq!(map.entries["src/keep.rs"].status, Status::Changed);
        assert_eq!(
            map.entries["tests/keep_test.rs"].test_files,
            vec!["tests/keep_test.rs".to_string()]
        );
        assert_eq!(map.last_synced, "r1");
    }

    // -- full-history ordering (reverse-chronological log) -------------------

    #[test]
    fn full_history_add_then_delete_leaves_no_live_entry() {
        // Simulate a full-history `git log --name-status` build: git emits
        // newest→oldest, so a path Added long ago and Deleted in a RECENT commit
        // appears as [Deleted (newer), Added (older)] in the log. After the fix
        // the delete (newest) wins and the path must NOT linger as a dangling
        // entry.
        let mut map = SpecMap::default();
        let changes = vec![
            // newest commit first (git order): the path was deleted last.
            Change::Deleted("gauge/src/pricing.rs".to_string()),
            // older commit: the path was originally added.
            Change::Added("gauge/src/pricing.rs".to_string()),
        ];
        map.apply_changes(&changes, "docs/specs", "head");

        // The only skeleton entry is keyed by the path; after add-then-delete
        // (chronologically) it must hold no impl/test files → orphaned/Missing,
        // never a live impl_files entry that would dangle in the map.
        let e = &map.entries["gauge/src/pricing.rs"];
        assert!(
            e.is_orphaned(),
            "deleted-in-newest-commit path must not retain impl/test files, got impl={:?} test={:?}",
            e.impl_files,
            e.test_files
        );
        assert_eq!(e.status, Status::Missing);
    }

    #[test]
    fn full_history_delete_then_readd_keeps_live_entry() {
        // The reverse case: a path Deleted in an OLDER commit and re-Added in a
        // RECENT commit appears as [Added (newer), Deleted (older)] in git order.
        // The add (newest) wins → the path is present with a live impl entry.
        let mut map = SpecMap::default();
        let changes = vec![
            // newest commit first: the path was re-added last.
            Change::Added("src/resurrected.rs".to_string()),
            // older commit: the path had been deleted.
            Change::Deleted("src/resurrected.rs".to_string()),
        ];
        map.apply_changes(&changes, "docs/specs", "head");

        let e = &map.entries["src/resurrected.rs"];
        assert_eq!(e.impl_files, vec!["src/resurrected.rs".to_string()]);
        assert_eq!(e.status, Status::Changed);
        assert!(!e.is_orphaned());
    }

    #[test]
    fn full_history_add_then_rename_resolves_to_final_path() {
        // Rename case under reverse-chronological order: a path is Added (older)
        // then Renamed away (newer). git order = [Renamed (newer), Added (older)].
        // Chronologically the add lands first, then the rename detaches `from`
        // and attributes `to`, so only the destination path holds a live entry.
        let mut map = SpecMap::default();
        let changes = vec![
            // newest commit first: the rename.
            Change::Renamed {
                from: "src/old_name.rs".to_string(),
                to: "src/new_name.rs".to_string(),
            },
            // older commit: original add of the source path.
            Change::Added("src/old_name.rs".to_string()),
        ];
        map.apply_changes(&changes, "docs/specs", "head");

        // Source entry emptied by the rename → Missing/orphaned.
        let old = &map.entries["src/old_name.rs"];
        assert!(old.is_orphaned());
        assert_eq!(old.status, Status::Missing);
        // Destination path carries the live impl entry.
        let new = &map.entries["src/new_name.rs"];
        assert_eq!(new.impl_files, vec!["src/new_name.rs".to_string()]);
        assert_eq!(new.status, Status::Changed);
    }

    // -- enrichment from the deterministic indexes --------------------------

    /// Unwrap a `Determination` in a test, surfacing the reason when it is
    /// `Undetermined`. `expect` rather than `panic!` because this crate does
    /// not allow `clippy::panic`, not even under `cfg(test)`.
    fn known<T>(what: &str, d: Determination<T>) -> T {
        let mut reason = String::new();
        let value = match d {
            Determination::Known(v) => Some(v),
            Determination::Undetermined(u) => {
                reason = u.as_str().to_string();
                None
            }
        };
        let Some(v) = value else {
            unreachable!("{what}: expected an answer, got: {reason}")
        };
        v
    }

    /// A committed repo whose `alpha.rs` declares `alpha_only` (unique) and
    /// `load` (shared with `noise.rs`, so ambiguous), with `caller.rs` calling
    /// both from outside.
    fn enrich_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src/alpha.rs"),
            "pub fn alpha_only() {}\npub fn load() {}\n",
        )
        .unwrap();
        for n in ["n1", "n2", "n3"] {
            std::fs::write(tmp.path().join(format!("src/{n}.rs")), "pub fn load() {}\n").unwrap();
        }
        std::fs::write(
            tmp.path().join("src/caller.rs"),
            "pub fn go() {\n    alpha_only();\n    load();\n}\n",
        )
        .unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["add", "-A"],
            vec![
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "x",
            ],
        ] {
            assert!(Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args(&args)
                .output()
                .unwrap()
                .status
                .success());
        }
        tmp
    }

    fn map_with_alpha() -> SpecMap {
        let mut map = SpecMap::default();
        let mut e = MapEntry::skeleton("alpha", Status::Tracked, None);
        e.impl_files = vec!["src/alpha.rs".to_string()];
        map.entries.insert("alpha".to_string(), e);
        map
    }

    #[test]
    fn enrich_records_declared_symbols_and_external_callers() {
        let tmp = enrich_repo();
        let mut map = map_with_alpha();
        let stats = known("enrich", map.enrich(tmp.path()));
        let e = &map.entries["alpha"];
        assert!(
            e.symbols.contains(&"alpha_only".to_string()),
            "{:?}",
            e.symbols
        );
        assert_eq!(
            e.called_by,
            vec!["src/caller.rs".to_string()],
            "external caller not recorded"
        );
        assert_eq!(stats.entries_changed, 1);
    }

    #[test]
    fn enrich_names_the_symbols_it_would_not_trace_rather_than_dropping_them() {
        // `load` is declared in four files, so a lexical edge to it attributes
        // nothing. CLAUDE.md 3: the gap is recorded, not silently absorbed --
        // an empty `called_by` must not be able to mean two different things.
        let tmp = enrich_repo();
        let mut map = map_with_alpha();
        let _ = map.enrich(tmp.path());
        let e = &map.entries["alpha"];
        assert!(
            e.ambiguous_symbols.contains(&"load".to_string()),
            "ambiguous symbol not disclosed: {:?}",
            e.ambiguous_symbols
        );
        assert!(
            e.symbols.contains(&"load".to_string()),
            "it is still declared here, so it stays in `symbols`"
        );
    }

    #[test]
    fn enrich_excludes_an_entrys_own_files_from_its_callers() {
        let tmp = enrich_repo();
        let mut map = map_with_alpha();
        map.entries.get_mut("alpha").unwrap().impl_files =
            vec!["src/alpha.rs".to_string(), "src/caller.rs".to_string()];
        let _ = map.enrich(tmp.path());
        assert!(
            map.entries["alpha"].called_by.is_empty(),
            "internal calls are not drift signal: {:?}",
            map.entries["alpha"].called_by
        );
    }

    #[test]
    fn enrich_leaves_the_map_untouched_when_the_index_cannot_answer() {
        // The fail-closed contract: a map that was asked and a map that could
        // not be asked must not serialize to the same thing.
        let tmp = tempfile::tempdir().unwrap();
        let not_a_repo = tmp.path().join("no-git-here");
        std::fs::create_dir_all(&not_a_repo).unwrap();
        let mut map = map_with_alpha();
        map.entries.get_mut("alpha").unwrap().called_by = vec!["previously/known.rs".to_string()];
        let before = map.clone();
        assert!(matches!(
            map.enrich(&not_a_repo),
            Determination::Undetermined(_)
        ));
        assert_eq!(map, before, "an unanswerable enrich must not write");
    }

    #[test]
    fn enrich_is_idempotent() {
        let tmp = enrich_repo();
        let mut map = map_with_alpha();
        let _ = map.enrich(tmp.path());
        let after_first = map.clone();
        let second = known("second enrich", map.enrich(tmp.path()));
        assert_eq!(map, after_first);
        assert_eq!(second.entries_changed, 0);
    }

    /// A map written before these fields existed must still parse, with the new
    /// fields reading as not-yet-enriched.
    #[test]
    fn a_map_without_the_new_fields_still_loads() {
        let toml = r#"
last_synced = "abc"

[entries."legacy"]
key = "legacy"
kind = "feature"
status = "tracked"
impl_files = ["src/legacy.rs"]
"#;
        let map: SpecMap = toml::from_str(toml).expect("old map must still parse");
        let e = &map.entries["legacy"];
        assert!(e.symbols.is_empty());
        assert!(e.called_by.is_empty());
        assert!(e.ambiguous_symbols.is_empty());
        assert_eq!(e.impl_files, vec!["src/legacy.rs".to_string()]);
    }

    // -- targeted filter (entry_matches) ------------------------------------

    fn filter_entry(
        key: &str,
        spec_doc: Option<&str>,
        impl_files: &[&str],
        test_files: &[&str],
    ) -> MapEntry {
        MapEntry {
            key: key.to_string(),
            kind: EntryKind::Feature,
            spec_doc: spec_doc.map(|s| s.to_string()),
            status: Status::Tracked,
            last_ref: None,
            impl_files: impl_files.iter().map(|s| s.to_string()).collect(),
            test_files: test_files.iter().map(|s| s.to_string()).collect(),
            client_refs: vec![],
            symbols: vec![],
            called_by: vec![],
            ambiguous_symbols: vec![],
            reviewed_reason: None,
            reviewed_at: None,
            api: None,
        }
    }

    #[test]
    fn entry_matches_on_key_spec_paths_route_case_insensitive() {
        let mut e = filter_entry(
            "drift-map",
            Some("docs/specs/DriftMap.md"),
            &["crates/specguard/src/drift.rs"],
            &["crates/specguard/tests/drift_test.rs"],
        );
        // Matches on the key (case-insensitive).
        assert!(entry_matches(&e, "drift-map"));
        assert!(entry_matches(&e, "DRIFT-MAP"));
        // Matches on the spec_doc.
        assert!(entry_matches(&e, "driftmap.md"));
        // Matches on an impl path fragment.
        assert!(entry_matches(&e, "crates/specguard"));
        // Matches on a test path fragment.
        assert!(entry_matches(&e, "drift_test"));
        // Non-match.
        assert!(!entry_matches(&e, "unrelated-feature"));

        // Matches on the api route for an endpoint entry.
        e.api = Some(ApiRef {
            method: "GET".to_string(),
            route: "/api/health".to_string(),
        });
        assert!(entry_matches(&e, "/health"));
    }

    #[test]
    fn entry_matches_empty_query_matches_all() {
        let e = filter_entry("anything", None, &["src/x.rs"], &[]);
        assert!(entry_matches(&e, ""));
        assert!(entry_matches(&e, "   "));
    }

    fn reviewed_entry(status: Status, commit: Option<&str>) -> MapEntry {
        let mut e = MapEntry::skeleton("src/x.rs", status, None);
        e.reviewed_at = commit.map(|c| ReviewedAt {
            commit: c.to_string(),
            date: "2026-01-01".to_string(),
        });
        e
    }

    fn token(st: Option<ReviewState>) -> Option<&'static str> {
        st.as_ref().map(review_state_token)
    }

    #[test]
    fn effective_review_max_commits_never_disables() {
        assert_eq!(effective_review_max_commits(None), 50);
        assert_eq!(effective_review_max_commits(Some(0)), 50);
        assert_eq!(effective_review_max_commits(Some(7)), 7);
    }

    #[test]
    fn classify_review_window_is_inclusive_of_n() {
        let e = reviewed_entry(Status::Tracked, Some("abcd"));
        let at = |n: u64| token(classify_review(&e, 5, |_| Determination::known(n)));
        assert_eq!(at(5), Some("fresh"), "exactly N behind is fresh");
        assert_eq!(at(6), Some("stale-review"), "more than N is stale");
        assert_eq!(at(0), Some("fresh"));
    }

    #[test]
    fn classify_review_forwards_undetermined_and_skips_non_tracked() {
        let e = reviewed_entry(Status::Tracked, Some("abcd"));
        assert_eq!(
            token(classify_review(&e, 5, |_| Determination::undetermined(
                "test: no answer"
            ))),
            Some("undetermined")
        );
        for s in [Status::Changed, Status::Missing] {
            let e = reviewed_entry(s, None);
            assert_eq!(token(classify_review(&e, 5, |_| unreachable!())), None);
        }
    }

    #[test]
    fn classify_review_without_reviewed_at_is_stale_without_asking_git() {
        let e = reviewed_entry(Status::Tracked, None);
        let mut asked = false;
        let st = classify_review(&e, 5, |_| {
            asked = true;
            Determination::known(0)
        });
        assert_eq!(token(st), Some("stale-review"));
        assert!(!asked);
    }

    #[test]
    fn review_clock_without_repo_is_undetermined() {
        let tmp = tempfile::tempdir().unwrap();
        let mut clock = ReviewClock::open(tmp.path());
        // No repo: HEAD is unreadable, so every distance is undetermined.
        assert!(matches!(clock.head(), Determination::Undetermined(_)));
        assert!(matches!(
            clock.distance("abcdef12"),
            Determination::Undetermined(_)
        ));
    }

    #[test]
    fn review_commit_must_be_a_hex_object_name() {
        assert!(is_hex_object_name("0123456789abcdef"));
        assert!(!is_hex_object_name("--all"));
        assert!(!is_hex_object_name("HEAD"));
        assert!(!is_hex_object_name("abc"));
        assert!(!is_hex_object_name(""));
    }
}

/// Audit repro tests (backlog f479ba9a / dfd92783 / 034d39a6). Each asserts the
/// property its ticket says is violated and is `#[ignore]`d while the defect is
/// open; remove the ignore when fixed.
#[cfg(test)]
mod backlog_s07_audit {
    use super::*;

    #[test]
    #[ignore = "backlog f479ba9a: open defect, remove ignore when fixed"]
    fn backlog_f479ba9a_test_prefixed_impl_file_is_not_classified_as_test() {
        // crates/overwatch/src/test_freshness.rs is an implementation file.
        assert_eq!(
            classify_path("crates/overwatch/src/test_freshness.rs"),
            FileRole::Impl,
            "name-prefix `test_` alone made an src/ implementation file a test"
        );
    }

    #[test]
    #[ignore = "backlog dfd92783: open defect, remove ignore when fixed"]
    fn backlog_dfd92783_externalised_tests_module_credits_parent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src/foo")).unwrap();
        std::fs::write(
            tmp.path().join("src/foo.rs"),
            "pub fn f() {}\n#[cfg(test)]\nmod tests;\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("src/foo/tests.rs"),
            "#[test]\nfn t() { assert!(true); }\n",
        )
        .unwrap();
        let mut map = SpecMap::default();
        map.apply_changes(
            &[
                Change::Added("src/foo.rs".to_string()),
                Change::Added("src/foo/tests.rs".to_string()),
            ],
            "docs/specs",
            "r1",
        );
        let _ = map.mark_inline_tests(tmp.path());
        let owner = map
            .entries
            .values()
            .find(|e| e.impl_files.iter().any(|p| p == "src/foo.rs"))
            .expect("entry owning src/foo.rs");
        assert!(
            !owner.test_files.is_empty(),
            "src/foo.rs has its tests in src/foo/tests.rs but its entry reads untested: {owner:?}"
        );
    }

    // backlog 0fe96299 lives in scope.rs (`classify`); kept here so scope.rs's
    // existing proptest module is not touched.
    #[test]
    #[ignore = "backlog 0fe96299: open defect, remove ignore when fixed"]
    fn backlog_0fe96299_duplicate_changed_path_appears_once_across_hits() {
        use crate::config::Area;
        let area = |n: &str, g: &str| Area {
            name: n.into(),
            globs: vec![g.into()],
            canon: vec![],
        };
        let areas = vec![area("a0", "src/a0/**"), area("a1", "src/a1/**")];
        let changed: Vec<String> = vec![
            "src/a0/chy.rs".into(),
            "src/a1/aaa.rs".into(),
            "src/a0/chy.rs".into(),
        ];
        let (hits, _) = crate::scope::classify(&changed, &areas).unwrap();
        let all: Vec<String> = hits
            .iter()
            .flat_map(|h| h.matched_files.iter().cloned())
            .collect();
        let uniq: std::collections::HashSet<&String> = all.iter().collect();
        assert_eq!(all.len(), uniq.len(), "a file appeared twice: {all:?}");
    }

    #[test]
    #[ignore = "backlog d4b7ab1d: open defect, remove ignore when fixed"]
    fn backlog_d4b7ab1d_doc_comments_are_not_fused_mid_line() {
        // A `///` that does not start its line fuses two doc blocks: here
        // entry_matches' doc ended up glued onto AMBIGUOUS_SYMBOL_DECLS' doc.
        let fused: Vec<(usize, &str)> = include_str!("specmap.rs")
            .lines()
            .enumerate()
            .filter(|(_, l)| {
                let t = l.trim_start();
                t.starts_with("///") && t[3..].contains("`./// ")
            })
            .collect();
        assert!(fused.is_empty(), "doc comments fused mid-line: {fused:?}");
    }

    #[test]
    #[ignore = "backlog 034d39a6: open defect, remove ignore when fixed"]
    fn backlog_034d39a6_test_attr_inside_raw_string_is_not_a_test() {
        let src = "pub fn f() -> &'static str {\n    r#\"\n#[test]\nfn fake() {}\n\"#\n}\n";
        assert!(
            !has_inline_tests(src),
            "a `#[test]` line inside a raw string literal was counted as a real test"
        );
    }
}
