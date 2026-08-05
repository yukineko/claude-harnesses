//! The gate proper: source the task's done_criteria, derive its semantic
//! properties, gather the generated-code diff, and decide whether to block the
//! stop. Two modes:
//!
//!   * `inject` — block once per new diff state and inject the property checklist;
//!     the running subscription agent self-verifies its own code against each
//!     property (no API key, no extra process). Because the hook itself can't
//!     count how many properties actually hold, the first pass treats the diff as
//!     *unverified* (satisfied = 0), which is below any threshold ≥ 1, so it
//!     blocks; once the agent has addressed the checklist the same diff is
//!     allowed.
//!   * `subprocess` — run an independent checker over the properties + diff. The
//!     checker reports one `PROP <id>: PASS|FAIL` line per property; propguard
//!     counts the PASSes and blocks when that count is below `threshold`.
//!
//! The single place the numeric block threshold is enforced is
//! [`below_threshold`]: the stop is blocked iff `satisfied < threshold`.
//!
//! Fail-closed, but bounded and escapable. Environment errors that predate any
//! check (no git repo, no done_criteria, nothing checkable) always allow — the
//! gate never invents a finding. A checker that itself fails (crash / timeout /
//! unparseable output) does NOT allow silently: it blocks up to `max_attempts`
//! with a loud, escapable reason, then gives up loudly, so a broken checker can
//! never become a bypass. A truncated diff (unchecked tail) is treated the same
//! way, and so is a diff that could not be READ in full — including one where
//! only some of the reads feeding it succeeded, which is never handed onward as
//! though it were the whole change (see [`decide_diff_failed`]). A genuine
//! *tool* error still exits 0 via the panic guard in `main`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use globset::{Glob, GlobSetBuilder};
use harness_core::boundary;
use harness_core::verdict::Determination;

use crate::config::{Config, Mode};
use crate::derive::{derive_properties, source_criteria, Property};

/// **The block threshold.** The stop is blocked iff fewer than `threshold` of
/// the derived properties are satisfied. This is the one enforcement point the
/// task asks for ("閾値未満でブロックする経路"): both modes route through it.
pub fn below_threshold(satisfied: usize, threshold: usize) -> bool {
    satisfied < threshold
}

/// The `already-verified` shortcut predicate. It fires — letting the next stop
/// through without re-running the check — **only** when a prior *passing* check
/// recorded this exact `(diff, properties)` hash as `last_hash`. A below-threshold
/// (genuinely failing) subprocess check records **no** passing hash (see
/// [`decide_from_count`]), so an unfixed failing diff is never auto-allowed and is
/// re-checked on the next round. Inject mode's documented "trust-after-one-block"
/// still records the hash on its first block, so it converges as before.
pub fn already_verified(st: &crate::state::SessionState, hash: &str) -> bool {
    !st.last_hash.is_empty() && st.last_hash == hash
}

/// What the gate decided. `tag` is a short label for the JSONL log.
pub enum Decision {
    Allow {
        tag: &'static str,
        attempts: u32,
        last_hash: String,
    },
    Block {
        reason: String,
        tag: &'static str,
        files: Vec<String>,
        properties: Vec<&'static str>,
        attempts: u32,
        last_hash: String,
    },
}

/// Files that changed *and* are worth checking (match include, not exclude).
///
/// Both filter lists are operator-supplied (`config.rs` overwrites the shipped
/// defaults from `include` / `exclude` in the config file), so a pattern that
/// does not compile is reachable, not hypothetical. When either list is
/// `Undetermined` this resolves to the restrictive side — CHECK MORE, filter
/// less — and says so on stderr rather than quietly matching on the survivors.
pub fn checkable_files(cfg: &Config, changed: &[String]) -> Vec<String> {
    // Resolve each list to its own restrictive direction. Both land on "do not
    // filter", but for opposite reasons: a half-loaded `include` would drop
    // files it was supposed to select, and a half-loaded `exclude` would keep
    // excluding on patterns the operator can no longer fully see.
    let inc = resolve_filter(build_set(&cfg.include), "include");
    let exc = resolve_filter(build_set(&cfg.exclude), "exclude");
    changed
        .iter()
        .filter(|f| {
            // `None` from either side now means only "no filter configured" or
            // "the filter could not be trusted" — never "loaded, partially".
            inc.as_ref().map(|s| s.is_match(f)).unwrap_or(true)
                && !exc.as_ref().map(|s| s.is_match(f)).unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// Collapse a filter list's `Determination` to the matcher `checkable_files`
/// applies, announcing an `Undetermined` instead of swallowing it. Dropping the
/// filter is the restrictive answer here: propguard blocks on unchecked
/// properties, so checking a file it did not need to check is noise, whereas
/// skipping one it did need to check is a silent bypass.
fn resolve_filter(
    set: Determination<Option<globset::GlobSet>>,
    which: &str,
) -> Option<globset::GlobSet> {
    match set {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => {
            eprintln!(
                "propguard: WARNING the `{which}` filter list could not be compiled in full \
                 ({why}) — ignoring the whole list and checking every changed file rather than \
                 filtering on the patterns that happened to survive. Fix the pattern (see \
                 `propguard status`)."
            );
            None
        }
    }
}

/// Compile a filter list. Three answers, not two:
///
/// * `Known(None)`      — no patterns configured; there is no filter to apply.
/// * `Known(Some(set))` — every pattern compiled and reached the matcher.
/// * `Undetermined`     — at least one pattern did not compile, or the set
///   failed to build. **A partially loaded set is never returned.**
///
/// The previous form `if let Ok(glob) = Glob::new(g)` + `b.build().ok()` erased
/// both failures, and for `include` that erasure is the permissive direction:
/// each dropped pattern removes files from the checked set, and once the
/// survivors fall under `min_changed_files`, `evaluate` returns
/// `allow("no-code-changes")` — "nothing here to check" about a change set
/// propguard merely failed to match. Same shape, same fix, as
/// `blastguard::exclude::build_set` (9ed33ba6) and `blastguard::diffrisk`
/// before it; this is the third instance of that mirror gap.
fn build_set(globs: &[String]) -> Determination<Option<globset::GlobSet>> {
    let mut b = GlobSetBuilder::new();
    let mut any = false;
    for g in globs {
        match Glob::new(g) {
            Ok(glob) => {
                b.add(glob);
                any = true;
            }
            Err(e) => {
                return Determination::undetermined(format!(
                    "glob pattern {g:?} does not compile: {e}"
                ));
            }
        }
    }
    if !any {
        return Determination::Known(None);
    }
    match b.build() {
        Ok(set) => Determination::Known(Some(set)),
        Err(e) => Determination::undetermined(format!("the glob set failed to build: {e}")),
    }
}

fn hash_props(diff: &str, props: &[Property]) -> String {
    let mut h = DefaultHasher::new();
    diff.hash(&mut h);
    for p in props {
        p.id.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

fn now() -> i64 {
    chrono::Local::now().timestamp()
}

/// Effective threshold: the configured threshold, clamped to the number of
/// properties actually derived so it can never be permanently unsatisfiable.
fn effective_threshold(cfg: &Config, n_props: usize) -> usize {
    cfg.threshold.min(n_props).max(1)
}

/// Core decision. `st` is the loaded prior session state.
pub fn evaluate(cfg: &Config, root: &Path, st: &crate::state::SessionState) -> Decision {
    // 1. Source the task's done_criteria. No criteria ⇒ nothing to formalize.
    let Some(criteria) = source_criteria(cfg, root) else {
        return allow("no-criteria", st);
    };

    // 2. Derive the semantic properties (deterministic, capped 3–5).
    let props = derive_properties(&criteria, cfg.min_properties, cfg.max_properties);
    if props.is_empty() {
        return allow("no-properties", st);
    }
    let threshold = effective_threshold(cfg, props.len());

    // 3. Gather the generated-code diff to check the properties against.
    let changed = match crate::git::changed_files(root) {
        // No git scope: nothing generated to check, allow (unchanged behavior).
        crate::git::ChangeScan::NotRepo => return allow("no-git", st),
        // A git command errored inside a real repo: the changed set is
        // UNDETERMINED. Fail closed (bounded & escapable) exactly like the
        // truncation / checker-outage guards — never silently allow an empty,
        // collapsed scan (the fail-open this closes).
        crate::git::ChangeScan::Failed => {
            let prior_attempts = if now() - st.last_ts > cfg.reset_after_secs {
                0
            } else {
                st.attempts
            };
            return decide_scan_failed(cfg, prior_attempts);
        }
        crate::git::ChangeScan::Files(v) => v,
    };
    let files = checkable_files(cfg, &changed);
    if files.len() < cfg.min_changed_files {
        return allow("no-code-changes", st);
    }
    // The diff itself is three-valued. `Undetermined` means at least one read
    // feeding it did not run to a conclusion, so what we hold is either
    // nothing or a fragment — and a fragment is the more dangerous of the two,
    // because it looks like a whole diff. Neither may be judged. Fail closed
    // (bounded & escapable), exactly like the failed-scan guard above.
    let crate::git::DiffText {
        text: diff,
        truncated,
    } = match crate::git::diff_text(root, &files, cfg.max_diff_bytes) {
        Determination::Known(d) => d,
        Determination::Undetermined(why) => {
            let prior_attempts = if now() - st.last_ts > cfg.reset_after_secs {
                0
            } else {
                st.attempts
            };
            return decide_diff_failed(cfg, why.as_str(), files, prior_attempts);
        }
    };
    // Reached ONLY when every read succeeded and the combined diff really was
    // empty — e.g. the change was reverted between `changed_files` and this
    // call. It is no longer the resting place of an unread diff: that answer
    // is `Undetermined` above and blocks. There is nothing to check, so allow.
    if diff.trim().is_empty() {
        return allow("empty-diff", st);
    }

    // Attempt counter resets after an idle gap (a fresh turn).
    let prior_attempts = if now() - st.last_ts > cfg.reset_after_secs {
        0
    } else {
        st.attempts
    };

    // Truncation guard (fail closed, bounded): the tail was dropped and is
    // unchecked, so neither the checker nor the "already-verified" convergence
    // can honestly certify the whole change. Block rather than let it slip.
    if truncated {
        return decide_truncated(cfg, &props, files, prior_attempts);
    }

    let hash = hash_props(&diff, &props);

    // Same (diff, properties) we already forced a *passing* check of → already
    // verified. Only a PASS (or inject's trust-after-one-block) records the hash,
    // so a below-threshold subprocess failure does NOT short-circuit here.
    if already_verified(st, &hash) {
        return allow("already-verified", st);
    }

    match cfg.mode {
        Mode::Inject => {
            // The hook can't itself judge whether each property holds, so a new
            // diff is unverified: satisfied = 0, which is below any threshold ≥ 1.
            decide_from_count(
                cfg,
                Determination::Known(Verified {
                    satisfied: 0,
                    findings: None,
                }),
                &props,
                threshold,
                files,
                hash,
                prior_attempts,
                &criteria,
            )
        }
        Mode::Subprocess => {
            let outcome = run_checker(cfg, &criteria, &props, &diff);
            decide_from_count(
                cfg,
                outcome,
                &props,
                threshold,
                files,
                hash,
                prior_attempts,
                &criteria,
            )
        }
    }
}

/// The established count of satisfied properties — the `Known` payload of a
/// [`Determination<Verified>`]. `satisfied` is 0 in inject mode's first pass and
/// a parsed PASS count in subprocess mode; `findings` carries the checker's
/// per-property verdict text, if any.
///
/// A checker that itself failed (crash / timeout / unusable output) is **not**
/// carried here: it is a [`Determination::Undetermined`], which is never the same
/// as "checked and satisfied" and must not become a silent bypass. The
/// three-valued [`Determination`] makes that "could not check" answer impossible
/// to collapse into a permissive default (see `harness_core::verdict`).
pub struct Verified {
    pub satisfied: usize,
    pub findings: Option<String>,
}

/// Turn a `Determination<Verified>` into a `Decision`, enforcing the block
/// threshold. Split out from `evaluate` so the threshold logic is unit-testable
/// without git or a real checker subprocess. `Undetermined` (the checker could
/// not run) is matched directly rather than propagated with `require`/`?`,
/// because it carries the bounded fail-closed retry policy — not a simple
/// short-circuit.
#[allow(clippy::too_many_arguments)]
pub fn decide_from_count(
    cfg: &Config,
    outcome: Determination<Verified>,
    props: &[Property],
    threshold: usize,
    files: Vec<String>,
    hash: String,
    prior_attempts: u32,
    criteria: &str,
) -> Decision {
    match outcome {
        Determination::Undetermined(why) => {
            let e = why.as_str();
            // Fail closed but bounded: block up to max_attempts, then give up
            // loudly so a permanently broken checker can't trap the turn.
            let attempts = prior_attempts + 1;
            if attempts > cfg.max_attempts {
                eprintln!(
                    "propguard: WARNING checker still unavailable after {max} attempt(s) \
                     ({e}) — allowing the stop with properties UNVERIFIED. Fix checker_cmd \
                     (see `propguard status`) or set PROPGUARD_DISABLE=1.",
                    max = cfg.max_attempts,
                );
                return Decision::Allow {
                    tag: "checker-error-giveup",
                    attempts: 0,
                    last_hash: String::new(),
                };
            }
            // The checker never ran, so NOTHING was evaluated. Report no
            // per-property violations: attributing the full derived prop_ids
            // here would pollute the property_id-keyed fleet-correlation store
            // with unchecked ids counted as real per-property violations
            // (CA-propguard-03). Mirrors the below-threshold path, which only
            // reports ids actually established as violated.
            Decision::Block {
                reason: checker_unavailable_reason(e, attempts, cfg.max_attempts),
                tag: "checker-unavailable",
                files,
                properties: Vec::new(),
                attempts,
                last_hash: String::new(),
            }
        }
        Determination::Known(Verified {
            satisfied,
            findings,
        }) => {
            // ---- THE THRESHOLD ENFORCEMENT POINT ----
            if !below_threshold(satisfied, threshold) {
                // Enough properties hold: allow, and record the hash so the same
                // (diff, properties) is not re-checked.
                return Decision::Allow {
                    tag: "properties-satisfied",
                    attempts: 0,
                    last_hash: hash,
                };
            }
            // Below threshold → block (bounded by max_attempts).
            let attempts = prior_attempts + 1;
            if attempts > cfg.max_attempts {
                return Decision::Allow {
                    tag: "giveup",
                    attempts: 0,
                    last_hash: String::new(),
                };
            }
            let reason = block_reason(
                cfg,
                criteria,
                props,
                satisfied,
                threshold,
                &files,
                findings.as_deref(),
                attempts,
            );
            // Only record the hash as a "verified" marker when the block is
            // inject mode's trust-after-one-block: the hook can't count, so the
            // same diff is trusted next round. A subprocess check DID count and
            // found the diff failing — recording it here would let the very next
            // identical failing stop through via `already_verified` before
            // max_attempts (CA-propguard-01). Leave it empty so the checker
            // re-runs and blocks again until the properties are actually fixed.
            let last_hash = match cfg.mode {
                Mode::Inject => hash,
                Mode::Subprocess => String::new(),
            };
            // Report only the properties that are actually violated (did not
            // get an explicit PASS verdict) to overwatch's fleet-correlation
            // signal — reporting every derived property, including ones the
            // subprocess checker PASSed, would pollute that signal with
            // non-violations (CA-propguard-01). This only changes which ids
            // are recorded; the pass/block decision above is untouched.
            let violated = unsatisfied_prop_ids(props, findings.as_deref());
            Decision::Block {
                reason,
                tag: "below-threshold",
                files,
                properties: violated,
                attempts,
                last_hash,
            }
        }
    }
}

/// Which property ids are actually violated (did not receive an explicit PASS
/// verdict), for reporting to overwatch on a below-threshold block. In
/// subprocess mode `findings` carries the checker's per-property `PROP <id>:
/// PASS|FAIL` text; only ids that were NOT confirmed PASS on their own
/// anchored verdict line are considered violated (CA-propguard-01). In inject
/// mode there is no per-property verdict yet (satisfied is always 0 on the
/// first pass), so every derived property is still open and all are reported.
fn unsatisfied_prop_ids(props: &[Property], findings: Option<&str>) -> Vec<&'static str> {
    let Some(out) = findings else {
        return props.iter().map(|p| p.id).collect();
    };
    let lower = out.to_lowercase();
    props
        .iter()
        .filter(|p| {
            let id = p.id.to_lowercase();
            // This property's own verdict, from its FIRST anchored verdict line.
            // `None` means the checker emitted NO verdict line for it — it was
            // never evaluated, which is NOT the same as checked-and-failed. Only
            // a property that was checked and FAILED (`Some(false)`) is a fleet
            // violation; a never-evaluated (unchecked) property must not be
            // reported violated / pollute the fleet-correlation store, per
            // property (CA-propguard-06).
            let verdict = lower.lines().find_map(|line| verdict_for_id(line, &id));
            verdict == Some(false)
        })
        .map(|p| p.id)
        .collect()
}

/// Pure mapper: escalate an isolated checker-outage give-up to a fail-closed
/// `Block` when the outage is *systemic* (recurring across tasks/sessions),
/// per the fleet-outage-vs-isolated-flake design. Any decision other than the
/// `checker-error-giveup` `Allow` is returned unchanged for either flag value
/// — this function only ever rewrites that one specific give-up, never any
/// other Allow tag (e.g. `properties-satisfied`, `giveup`, `truncated-giveup`)
/// and never an existing Block.
///
/// `systemic_outage == false` (the common, isolated case — a checker flaked
/// on just this task/session) keeps the input `Allow` unchanged: propguard
/// must never fail-halt the whole fleet over one task's transient checker
/// error. Only a *confirmed fleet-wide* outage (the caller has already
/// checked recurrence across distinct tasks/sessions) flips to `Block`.
pub fn escalate_giveup_on_systemic(decision: Decision, systemic_outage: bool) -> Decision {
    match decision {
        Decision::Allow {
            tag: "checker-error-giveup",
            ..
        } if systemic_outage => Decision::Block {
            reason: "propguard: FLEET-WIDE checker outage confirmed (recurring across \
                     multiple tasks/sessions) — holding the stop to avoid shipping \
                     UNVERIFIED code. Fix checker_cmd (see `propguard status`) or set \
                     PROPGUARD_DISABLE=1 to bypass."
                .to_string(),
            tag: "checker-outage-systemic",
            files: vec![],
            properties: vec![],
            attempts: 0,
            last_hash: String::new(),
        },
        other => other,
    }
}

/// The three-valued front door to [`escalate_giveup_on_systemic`], taking the
/// recurrence answer as a [`Determination<bool>`] so "the violation ledger
/// could not be read" is its own answer rather than a `false`.
///
/// * `Known(false)` — checked, isolated flake → the input `Allow` is unchanged.
/// * `Known(true)`  — checked, fleet-wide outage → `Block` (delegated).
/// * `Undetermined` — the ledger could not be read / holds an undecodable line.
///   We already know propguard's checker FAILED (this path is only reached from
///   the `checker-error-giveup` arm), and we now cannot tell whether that
///   failure is fleet-wide. Two unknowns stacked must not resolve to "ship it":
///   this blocks, with its OWN tag and reason so it is never mistaken for a
///   *confirmed* systemic outage (`checker-outage-systemic`). Per CLAUDE.md §3,
///   判定不能 resolves to the restricted side.
///
/// Like its delegate, this rewrites ONLY the `checker-error-giveup` `Allow`;
/// every other decision passes through untouched on all three answers, so an
/// unreadable ledger cannot turn an unrelated `Allow` into a `Block`.
pub fn escalate_giveup_on_outage_scan(
    decision: Decision,
    outage: harness_core::verdict::Determination<bool>,
) -> Decision {
    use harness_core::verdict::Determination;
    match outage {
        Determination::Known(systemic) => escalate_giveup_on_systemic(decision, systemic),
        Determination::Undetermined(why) => match decision {
            Decision::Allow {
                tag: "checker-error-giveup",
                ..
            } => Decision::Block {
                reason: format!(
                    "propguard: the checker failed AND the fleet violation ledger could not \
                     be read ({why}), so whether this is an isolated flake or a fleet-wide \
                     outage is UNDETERMINED — holding the stop rather than shipping \
                     UNVERIFIED code on an unverifiable signal. Fix checker_cmd and the \
                     overwatch store (see `propguard status`), or set PROPGUARD_DISABLE=1 \
                     to bypass."
                ),
                tag: "checker-outage-undetermined",
                files: vec![],
                properties: vec![],
                attempts: 0,
                last_hash: String::new(),
            },
            other => other,
        },
    }
}

fn allow(tag: &'static str, st: &crate::state::SessionState) -> Decision {
    Decision::Allow {
        tag,
        attempts: 0,
        last_hash: st.last_hash.clone(),
    }
}

/// A truncated diff has an unchecked tail. Fail closed but bounded, then give up
/// loudly — same shape as reviewgate's truncation guard.
fn decide_truncated(
    cfg: &Config,
    _props: &[Property],
    files: Vec<String>,
    prior_attempts: u32,
) -> Decision {
    let attempts = prior_attempts + 1;
    if attempts > cfg.max_attempts {
        eprintln!(
            "propguard: WARNING diff still exceeds max_diff_bytes ({max_bytes} B) after \
             {max} attempt(s) — allowing the stop with the truncated tail UNCHECKED. Split the \
             change, raise max_diff_bytes, or set PROPGUARD_DISABLE=1.",
            max_bytes = cfg.max_diff_bytes,
            max = cfg.max_attempts,
        );
        return Decision::Allow {
            tag: "truncated-giveup",
            attempts: 0,
            last_hash: String::new(),
        };
    }
    // The truncated tail was never checked, so no property was actually
    // evaluated as violated. Report no per-property violations: attributing
    // the full derived prop_ids here would pollute the property_id-keyed
    // fleet-correlation store the same way CA-propguard-03 does
    // (CA-propguard-04).
    Decision::Block {
        reason: truncated_reason(cfg, &files, attempts, cfg.max_attempts),
        tag: "diff-truncated",
        files,
        properties: Vec::new(),
        attempts,
        last_hash: String::new(),
    }
}

/// A `git` command errored inside a real repo, so the changed set is
/// UNDETERMINED — there is no generated-code diff we can trust to check the
/// properties against. Fail *closed* (but bounded), modeled EXACTLY on
/// [`decide_truncated`]: block the stop with a loud, escapable reason for up to
/// `max_attempts` consecutive stops (giving a transient git error — lock
/// contention, a slow mount, a timed-out `git` — a chance to clear), then give
/// up *loudly* with a distinct tag so a persistently broken git can never trap
/// the turn. Escape hatches (`.propguard-skip`, `PROPGUARD_DISABLE=1`) stay
/// available throughout and are named in the reason (never-break-a-turn). No
/// hash is recorded (there is no diff to certify) and — like the checker-
/// unavailable / truncation blocks — NO per-property violations are attributed
/// (nothing was checked), so the fleet-correlation store isn't polluted.
fn decide_scan_failed(cfg: &Config, prior_attempts: u32) -> Decision {
    let attempts = prior_attempts + 1;
    if attempts > cfg.max_attempts {
        eprintln!(
            "propguard: WARNING git scan still failing after {max} attempt(s) — allowing the \
             stop with the change set UNDETERMINED (properties UNVERIFIED). Fix the git error \
             (see `propguard status`) or set PROPGUARD_DISABLE=1.",
            max = cfg.max_attempts,
        );
        return Decision::Allow {
            tag: "git-scan-failed-giveup",
            attempts: 0,
            last_hash: String::new(),
        };
    }
    Decision::Block {
        reason: scan_failed_reason(attempts, cfg.max_attempts),
        tag: "git-scan-failed",
        files: vec![],
        properties: Vec::new(),
        attempts,
        last_hash: String::new(),
    }
}

/// The diff could not be read in full, so there is nothing trustworthy to
/// check the properties against. Fail *closed* (but bounded), modeled exactly
/// on [`decide_scan_failed`] — the sibling one step earlier in the pipeline.
///
/// This closes backlog `87dbfbb8` and `d8e22b26`. The two shapes it catches
/// are different in how loud they are and identical in how they used to end:
///
/// * **Nothing readable.** A tracked file whose working-tree content is not
///   valid UTF-8 (and has no NUL, so `git` emits it as a textual diff) made
///   the whole `git diff` undecodable. `diff_text` returned `text: ""`, and
///   `evaluate`'s `empty-diff` arm ALLOWED the stop — while `changed_files`
///   was simultaneously answering `Files(["bad.rs"])`. The gate said "no
///   changes here" about a change it had already been told existed.
/// * **Half readable.** One sub-command readable, another not: the surviving
///   half was returned with `truncated: false`, i.e. presented as the COMPLETE
///   diff. `truncated` is the only incompleteness signal `evaluate` has, so
///   the omission was invisible and the properties were judged against a diff
///   missing a file.
///
/// `files` is carried into the block purely so the message can name what was
/// *supposed* to be checked; NO per-property violation is attributed (nothing
/// was checked, so attributing property ids would pollute the property-keyed
/// fleet-correlation store — CA-propguard-03/04) and NO hash is recorded
/// (there is no diff to certify, so `already_verified` must not arm).
fn decide_diff_failed(
    cfg: &Config,
    why: &str,
    files: Vec<String>,
    prior_attempts: u32,
) -> Decision {
    let attempts = prior_attempts + 1;
    if attempts > cfg.max_attempts {
        eprintln!(
            "propguard: WARNING the diff still could not be read in full after {max} \
             attempt(s) ({why}) — allowing the stop with the change UNOBSERVED (properties \
             UNVERIFIED). Fix the unreadable content (see `propguard status`) or set \
             PROPGUARD_DISABLE=1.",
            max = cfg.max_attempts,
        );
        return Decision::Allow {
            tag: "diff-read-failed-giveup",
            attempts: 0,
            last_hash: String::new(),
        };
    }
    Decision::Block {
        reason: diff_failed_reason(why, &files, attempts, cfg.max_attempts),
        tag: "diff-read-failed",
        files,
        properties: Vec::new(),
        attempts,
        last_hash: String::new(),
    }
}

fn diff_failed_reason(why: &str, files: &[String], attempt: u32, max: u32) -> String {
    format!(
        "🚧 propguard: 変更の diff を最後まで読めませんでした (round {attempt}/{max}).\n\n\
         `git` は変更ありと答えている ({n} files) のに、その diff を取得する読み取りの少なくとも1つが\
         最後まで完了しませんでした:\n  {why}\n\n\
         対象ファイル:\n{list}\n\
         読めなかった diff は「空の diff」でも「完全な diff」でもありません。空として扱えば未検査の変更が\
         そのまま通り、部分的に読めた分を完全な diff として checker に渡せば、欠けたファイルを見ないまま\
         PASS を出しかねません。判定不能な状態で停止を許可しないため、この停止を一時的にブロックしています。\
         {max}回連続で解消しなければ警告を出して通過を許可します (永久にはブロックしません)。\n\n\
         前に進むには次のいずれか:\n\
         - 読めない内容を解消する (典型例: 追跡対象ファイルの内容が UTF-8 として不正、`git` のエラー、\
         タイムアウト)。`propguard status` で対象 repo を確認。\n\
         - このチェックを1回だけスキップ: project root に `.propguard-skip` を作成 (理由を1行)。\n\
         - propguard を完全に無効化: 環境変数 PROPGUARD_DISABLE=1。",
        attempt = attempt,
        max = max,
        why = why,
        n = files.len(),
        list = file_list(files),
    )
}

fn scan_failed_reason(attempt: u32, max: u32) -> String {
    format!(
        "🚧 propguard: 変更内容を特定できませんでした — `git` コマンドが失敗しました (round {attempt}/{max}).\n\n\
         git repo ではあるものの `git diff` / `git status` がエラー (spawn 失敗 / 非ゼロ終了 / タイムアウト) を\
         返したため、生成コードの diff を取得できず、プロパティを検査できません。空の diff を「変更なし」と\
         解釈して無言で通過させると、未検査の変更が gate をすり抜けます。判定不能な状態で停止を許可しないため、\
         この停止を一時的にブロックしています。{max}回連続で解消しなければ警告を出して通過を許可します \
         (永久にはブロックしません)。\n\n\
         前に進むには次のいずれか:\n\
         - git のエラーを解消する (`propguard status` で対象 repo を確認)。\n\
         - このチェックを1回だけスキップ: project root に `.propguard-skip` を作成 (理由を1行)。\n\
         - propguard を完全に無効化: 環境変数 PROPGUARD_DISABLE=1。",
        attempt = attempt,
        max = max,
    )
}

fn file_list(files: &[String]) -> String {
    let mut s = String::new();
    for f in files.iter().take(40) {
        s.push_str("  ");
        s.push_str(f);
        s.push('\n');
    }
    if files.len() > 40 {
        s.push_str(&format!("  … (+{} more)\n", files.len() - 40));
    }
    s
}

fn property_list(props: &[Property]) -> String {
    let mut s = String::new();
    for (i, p) in props.iter().enumerate() {
        s.push_str(&format!(
            "  {}. [{}] {}\n     → {}\n",
            i + 1,
            p.id,
            p.title,
            p.check_hint
        ));
    }
    s
}

/// The block reason handed back to the agent when fewer than `threshold`
/// properties are (known to be) satisfied. In inject mode `findings` is None and
/// `satisfied` is 0 (the diff is unverified); in subprocess mode `findings`
/// carries the checker's per-property verdicts.
#[allow(clippy::too_many_arguments)]
fn block_reason(
    _cfg: &Config,
    criteria: &str,
    props: &[Property],
    satisfied: usize,
    threshold: usize,
    files: &[String],
    findings: Option<&str>,
    attempt: u32,
) -> String {
    let findings_block = match findings {
        Some(f) if !f.trim().is_empty() => {
            format!(
                "--- チェッカーの判定 ---\n{}\n------------------------\n\n",
                f.trim()
            )
        }
        _ => String::new(),
    };
    format!(
        "🧪 propguard: 生成コードが満たすべき semantic property が閾値に達していません \
         (round {attempt}). satisfied={satisfied} < threshold={threshold}.\n\n\
         done_criteria から導出した検査対象プロパティ:\n{props}\n\
         対象ファイル ({n} files):\n{list}\n\
         {findings}\
         各プロパティについて自分の生成コードを検証し、成り立たないものを修正してから完了してください。\
         少なくとも {threshold} 個が成り立つことを確認し、結果を簡潔に報告すること \
         (誤検知だと判断したものは理由を述べて構いません)。\n\n\
         元の done_criteria:\n  {criteria}\n\n\
         このチェックを1回だけスキップ: project root に `.propguard-skip` を作成 (理由を1行)。\
         完全に無効化: 環境変数 PROPGUARD_DISABLE=1。",
        attempt = attempt,
        satisfied = satisfied,
        threshold = threshold,
        props = property_list(props),
        n = files.len(),
        list = file_list(files),
        findings = findings_block,
        criteria = criteria.trim(),
    )
}

fn checker_unavailable_reason(err: &str, attempt: u32, max: u32) -> String {
    format!(
        "🚧 propguard: 独立プロパティチェッカーを実行できませんでした (round {attempt}/{max}).\n\n\
         checker_cmd がエラー / タイムアウト / 解析不能でした:\n  {err}\n\n\
         これは「プロパティ充足」ではありません。壊れたチェッカーを無言で通過させると gate が\
         バイパスになるため、この停止を一時的にブロックしています。{max}回連続で失敗した場合は\
         警告を出して通過を許可します (永久にはブロックしません)。\n\n\
         前に進むには次のいずれか:\n\
         - checker_cmd を修正する (`propguard status` で解決済みコマンドを確認)。\n\
         - このチェックを1回だけスキップ: project root に `.propguard-skip` を作成 (理由を1行)。\n\
         - propguard を完全に無効化: 環境変数 PROPGUARD_DISABLE=1。",
        attempt = attempt,
        max = max,
        err = err.trim(),
    )
}

fn truncated_reason(cfg: &Config, files: &[String], attempt: u32, max: u32) -> String {
    format!(
        "🚧 propguard: 変更差分が大きすぎてプロパティ検査用に切り詰められました (round {attempt}/{max}).\n\n\
         diff が max_diff_bytes ({max_bytes} B) を超えたため末尾が検査対象から欠落しています。\
         欠落分は検査されていないため、この停止を無言で許可すると未検査の変更が gate をすり抜けます。\
         {max}回連続で解消しなければ警告を出して通過を許可します (永久にはブロックしません)。\n\n\
         対象ファイル ({n} files):\n{list}\
         前に進むには次のいずれか:\n\
         - 変更を小さく分割し、それぞれが max_diff_bytes に収まるようにする。\n\
         - max_diff_bytes を引き上げる (現在 {max_bytes} B)。\n\
         - このチェックを1回だけスキップ: `.propguard-skip` を作成。完全に無効化: PROPGUARD_DISABLE=1。",
        attempt = attempt,
        max = max,
        max_bytes = cfg.max_diff_bytes,
        n = files.len(),
        list = file_list(files),
    )
}

// ---------------------------------------------------------------------------
// subprocess mode: an independent checker reports per-property PASS/FAIL.
// ---------------------------------------------------------------------------

/// Run `checker_cmd`, feeding it the properties + diff on stdin and reading a
/// `PROP <id>: PASS|FAIL` verdict per property on stdout.
fn run_checker(
    cfg: &Config,
    criteria: &str,
    props: &[Property],
    diff: &str,
) -> Determination<Verified> {
    let prompt = format!(
        "あなたは独立したプロパティ検査官です。以下の done_criteria から導出された semantic property が、\
         提示された git diff の生成コードで成り立つかを 1 つずつ判定してください。\n\n\
         done_criteria:\n{criteria}\n\n\
         プロパティ:\n{props}\n\
         各プロパティについて、次の形式で厳密に1行ずつ出力してください (他の行は無視されます):\n\
         PROP <id>: PASS   または   PROP <id>: FAIL — 理由\n\n\
         --- diff ---\n{diff}\n",
        criteria = criteria.trim(),
        props = property_list(props),
        diff = diff,
    );

    let mut cmd = build_command(&cfg.checker_cmd);
    cmd.stdin(Stdio::piped());
    // Process-group placement on spawn (so a shell-wrapped checker's
    // backgrounded grandchildren die with it on timeout — CA-propguard-004),
    // the stdin write happening off-thread before the wait starts (so a
    // checker that floods stdout before draining stdin can't deadlock this
    // call — fix-propguard-003), and the stdout-pipe-stays-open-after-exit
    // hazard (CA-propguard-005) are now all handled inside
    // `boundary::run_with_timeout_and_stdin`; this call site owns only what's
    // specific to a checker invocation: building the prompt, and parsing the
    // PASS/FAIL verdict lines out of the resulting stdout.
    let timeout = Duration::from_secs(cfg.checker_timeout_secs);
    match boundary::run_with_timeout_and_stdin(&mut cmd, timeout, Some(prompt.into_bytes())) {
        Determination::Known(out) => {
            // The exit status is the authority on whether the checker
            // COMPLETED. The verdict itself lives in the `PROP <id>:
            // PASS|FAIL` lines, never in the exit code, so a non-zero exit
            // means the checker crashed or errored mid-run — and its stdout
            // is then a partial, untrustworthy write that may name some
            // properties PASS before dying. Trusting it (the old `&&
            // out.trim().is_empty()` guard only caught the crash that ALSO
            // wrote nothing) is the same fail-open as reading a process that
            // exited 101 as "no errors": a crashed checker must be an Error,
            // which fails closed, not a parsed verdict.
            match out.stdout_on_success() {
                Determination::Known(stdout) => parse_checker_output(&stdout, props),
                Determination::Undetermined(why) => Determination::undetermined(format!(
                    "{} (its output cannot be trusted as a verdict)",
                    why.as_str()
                )),
            }
        }
        Determination::Undetermined(why) => Determination::undetermined(why.as_str().to_string()),
    }
}

/// If `line` (already lowercased) is *this* property's own verdict line — i.e.
/// it is anchored `PROP <id>[:…]` after trimming — return its verdict:
/// `Some(true)` for PASS, `Some(false)` for FAIL/anything-not-PASS. Returns
/// `None` when the line is not this property's verdict line (so a different
/// property's explanation that merely mentions this id can never win).
fn verdict_for_id(line: &str, id: &str) -> Option<bool> {
    // Must start with the `prop` keyword (after leading whitespace).
    let rest = line.trim_start().strip_prefix("prop")?;
    // A separator (whitespace) must follow the keyword before the id.
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    // The id must be the next token, on a boundary (end, ':' or whitespace) so
    // "determinism" does not match "determinism-foo".
    let after = rest.trim_start().strip_prefix(id)?;
    match after.chars().next() {
        None => {}
        Some(c) if c == ':' || c.is_whitespace() => {}
        _ => return None,
    }
    // Read the verdict from the STRUCTURED token immediately following the id
    // (after an optional ':' separator), NOT a substring search of the free-text
    // reason. A substring search misparses a PASS whose reason merely mentions
    // "fail"/"failure"/"cannot fail" as a FAIL, undercounting a satisfied
    // property (CA-propguard-05). The leading alphabetic token is the verdict;
    // anything trailing it (the human reason) is ignored.
    let token: String = after
        .trim_start_matches(|c: char| c == ':' || c.is_whitespace())
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    match token.as_str() {
        "pass" => Some(true),
        // FAIL, or any other/garbled token on this property's OWN line, fails
        // closed: the property was named but not clearly PASSed.
        _ => Some(false),
    }
}

/// Parse `PROP <id>: PASS|FAIL` lines. A property is counted satisfied only when
/// its id is explicitly reported PASS on its OWN anchored verdict line. Output
/// that mentions none of the derived property ids is unusable → Error (fail
/// closed), never silently "all pass".
pub fn parse_checker_output(out: &str, props: &[Property]) -> Determination<Verified> {
    let lower = out.to_lowercase();
    let mut satisfied = 0usize;
    let mut seen_any = false;
    for p in props {
        let id = p.id.to_lowercase();
        // Find this property's OWN verdict line and read its PASS/FAIL. The line
        // must be *anchored* to `PROP <id>` (after trimming), not merely mention
        // the id somewhere: another property's PASS explanation can name this id
        // in prose (e.g. "... this also confirms determinism holds ..."), and an
        // unanchored substring match would let that PASS override this property's
        // real FAIL verdict (CA-propguard-02).
        for line in lower.lines() {
            if let Some(verdict) = verdict_for_id(line, &id) {
                seen_any = true;
                if verdict {
                    satisfied += 1;
                }
                break;
            }
        }
    }
    if !seen_any {
        return Determination::undetermined(format!(
            "checker output named none of the {} derived properties",
            props.len()
        ));
    }
    Determination::Known(Verified {
        satisfied,
        findings: Some(out.trim().to_string()),
    })
}

fn build_command(cmdline: &str) -> Command {
    let needs_shell = cmdline.contains(|c| "|&;<>(){}$`\\\"'*?".contains(c));
    if needs_shell {
        harness_core::shell::command(cmdline)
    } else {
        let mut parts = cmdline.split_whitespace();
        let prog = parts.next().unwrap_or("claude");
        let mut c = Command::new(prog);
        c.args(parts);
        c
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use crate::derive::CATALOG;

    fn props_by_ids(ids: &[&str]) -> Vec<Property> {
        ids.iter()
            .map(|id| *CATALOG.iter().find(|p| p.id == *id).unwrap())
            .collect()
    }

    fn cfg_default() -> Config {
        Config::default() // threshold 3, max_attempts 2
    }

    // ── include/exclude filtering ──────────────────────────────────────────
    #[test]
    fn include_exclude_filtering() {
        let cfg = Config {
            include: vec!["**/*.rs".to_string()],
            exclude: vec!["**/target/**".to_string()],
            ..Config::default()
        };
        let changed = vec![
            "src/main.rs".to_string(),
            "README.md".to_string(),
            "target/x.rs".to_string(),
        ];
        assert_eq!(
            checkable_files(&cfg, &changed),
            vec!["src/main.rs".to_string()]
        );
    }

    /// A filter list that only PARTIALLY compiles must not silently shrink the
    /// checked set. `include` is the permissive direction: every pattern that
    /// fails to compile is one more file that stops being checked, and once the
    /// survivors drop below `min_changed_files`, `evaluate` returns
    /// `allow("no-code-changes")` — propguard reporting "nothing to check" about
    /// a change set it merely failed to match.
    #[test]
    fn a_partially_uncompilable_include_list_does_not_silently_narrow_the_checked_set() {
        let cfg = Config {
            // The second pattern has an unclosed character class and cannot
            // compile. The FIRST one still does, so the list loads partially.
            include: vec!["**/*.rs".to_string(), "**/*.[py".to_string()],
            exclude: vec![],
            ..Config::default()
        };
        let changed = vec!["src/main.rs".to_string(), "scripts/run.py".to_string()];
        // `run.py` is exactly what the broken pattern was meant to select. It
        // must NOT vanish just because the pattern did.
        assert_eq!(
            checkable_files(&cfg, &changed),
            changed,
            "a partially loaded include list narrowed the checked set instead of \
             resolving to the restrictive side"
        );
    }

    /// Same rule, opposite list. A partially loaded `exclude` must not keep
    /// filtering with the survivors — an exclusion the operator can no longer
    /// see is an exclusion that is not trustworthy.
    #[test]
    fn a_partially_uncompilable_exclude_list_stops_excluding_rather_than_half_excluding() {
        let cfg = Config {
            include: vec![],
            exclude: vec!["**/target/**".to_string(), "**/*.[py".to_string()],
            ..Config::default()
        };
        let changed = vec!["src/main.rs".to_string(), "target/x.rs".to_string()];
        assert_eq!(
            checkable_files(&cfg, &changed),
            changed,
            "a partially loaded exclude list kept excluding on its surviving \
             patterns; the restrictive answer is to exclude nothing"
        );
    }

    /// ANTI-VACUITY CONTROL for both tests above. A `build_set` that returned
    /// Undetermined unconditionally — or a `checkable_files` that stopped
    /// filtering altogether — would satisfy both of them while disabling the
    /// filter entirely. This one fails in that state.
    #[test]
    fn a_well_formed_filter_list_still_filters() {
        let cfg = Config {
            include: vec!["**/*.rs".to_string()],
            exclude: vec!["**/target/**".to_string()],
            ..Config::default()
        };
        let changed = vec![
            "src/main.rs".to_string(),
            "README.md".to_string(),
            "target/x.rs".to_string(),
        ];
        assert_eq!(
            checkable_files(&cfg, &changed),
            vec!["src/main.rs".to_string()],
            "a well-formed list must still filter; otherwise the two tests above \
             are satisfied by a gate that checks everything unconditionally"
        );
    }

    /// The property that was missing entirely: every pattern propguard SHIPS
    /// reaches its matcher. Without this, a typo in `default_include` /
    /// `default_exclude` fails on whichever later commit happens to exercise
    /// the path it unfiltered, not on the commit that introduces it.
    #[test]
    fn the_shipped_default_filter_lists_load_in_full() {
        let cfg = Config::default();
        for (name, globs) in [("include", &cfg.include), ("exclude", &cfg.exclude)] {
            for g in globs {
                assert!(
                    globset::Glob::new(g).is_ok(),
                    "shipped default {name} list contains a pattern that does not \
                     compile: {g:?}"
                );
            }
            assert!(
                matches!(build_set(globs), Determination::Known(Some(_))),
                "shipped default {name} list does not load in full"
            );
        }
    }

    // ── the threshold enforcement point ────────────────────────────────────

    /// At or above the threshold → ALLOW, and the hash is recorded.
    #[test]
    fn at_threshold_allows_and_records_hash() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            Determination::Known(Verified {
                satisfied: 3,
                findings: None,
            }),
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "hashabc".to_string(),
            0,
            "dc",
        );
        match d {
            Decision::Allow { tag, last_hash, .. } => {
                assert_eq!(tag, "properties-satisfied");
                assert_eq!(
                    last_hash, "hashabc",
                    "a satisfied check must record the hash"
                );
            }
            Decision::Block { .. } => panic!("satisfied >= threshold must allow"),
        }
    }

    /// Below the threshold → BLOCK, naming the properties.
    #[test]
    fn below_threshold_blocks() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            Determination::Known(Verified {
                satisfied: 1,
                findings: Some("PROP error-path: FAIL — panics".to_string()),
            }),
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "hashabc".to_string(),
            0,
            "handle errors",
        );
        match d {
            Decision::Block {
                tag,
                reason,
                properties,
                ..
            } => {
                assert_eq!(tag, "below-threshold");
                assert!(properties.contains(&"error-path"));
                assert!(reason.contains("threshold=3"));
                assert!(
                    reason.contains("PROPGUARD_DISABLE"),
                    "must name an escape hatch"
                );
            }
            Decision::Allow { .. } => panic!("satisfied < threshold must block"),
        }
    }

    /// Inject mode's first pass (satisfied = 0) is below any threshold ≥ 1 → block.
    #[test]
    fn inject_first_pass_blocks_as_unverified() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            Determination::Known(Verified {
                satisfied: 0,
                findings: None,
            }),
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "h".to_string(),
            0,
            "dc",
        );
        assert!(matches!(d, Decision::Block { .. }));
    }

    /// Bounded: after max_attempts consecutive below-threshold stops, give up.
    #[test]
    fn below_threshold_gives_up_after_max_attempts() {
        let cfg = cfg_default(); // max_attempts = 2
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            Determination::Known(Verified {
                satisfied: 0,
                findings: None,
            }),
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "h".to_string(),
            cfg.max_attempts,
            "dc",
        );
        match d {
            Decision::Allow { tag, .. } => assert_eq!(tag, "giveup"),
            Decision::Block { .. } => panic!("must give up so the turn is never trapped"),
        }
    }

    // ── checker error must not become a silent bypass ──────────────────────
    #[test]
    fn checker_error_blocks_it_does_not_allow() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            Determination::undetermined("spawn: boom"),
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "h".to_string(),
            0,
            "dc",
        );
        match d {
            Decision::Block { tag, reason, .. } => {
                assert_eq!(tag, "checker-unavailable");
                assert!(reason.contains("PROPGUARD_DISABLE"));
            }
            Decision::Allow { .. } => panic!("checker error must block (fail-closed), not allow"),
        }
    }

    // ── CA-propguard-03: a checker-unavailable Block must NOT attribute
    //    per-property violations. The checker never ran, so nothing was
    //    actually evaluated; stuffing the full derived prop_ids into
    //    `Block.properties` pollutes the property_id-keyed fleet-correlation
    //    store with unchecked ids counted as real per-property violations. ────
    #[test]
    fn checker_unavailable_block_reports_no_property_violations() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            Determination::undetermined("spawn: boom"),
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "h".to_string(),
            0,
            "dc",
        );
        match d {
            Decision::Block {
                tag, properties, ..
            } => {
                assert_eq!(tag, "checker-unavailable");
                assert!(
                    properties.is_empty(),
                    "a checker-unavailable block evaluated no property — it must not \
                     report any per-property violation to the correlation store, got {properties:?}"
                );
            }
            Decision::Allow { .. } => panic!("checker error must block (fail-closed)"),
        }
    }

    #[test]
    fn checker_error_gives_up_after_max_attempts_but_never_traps() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            Determination::undetermined("still broken"),
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "h".to_string(),
            cfg.max_attempts,
            "dc",
        );
        match d {
            Decision::Allow { tag, .. } => assert_eq!(tag, "checker-error-giveup"),
            Decision::Block { .. } => panic!("must give up after max_attempts"),
        }
    }

    // ── isolated vs systemic checker-outage escalation ─────────────────────

    fn checker_error_giveup() -> Decision {
        Decision::Allow {
            tag: "checker-error-giveup",
            attempts: 0,
            last_hash: String::new(),
        }
    }

    #[test]
    fn escalate_giveup_isolated_stays_allow() {
        // Not systemic: the isolated give-up case must pass through unchanged.
        let d = escalate_giveup_on_systemic(checker_error_giveup(), false);
        match d {
            Decision::Allow { tag, .. } => assert_eq!(tag, "checker-error-giveup"),
            Decision::Block { .. } => panic!("isolated give-up must stay Allow"),
        }
    }

    #[test]
    fn escalate_giveup_systemic_becomes_block() {
        let d = escalate_giveup_on_systemic(checker_error_giveup(), true);
        match d {
            Decision::Block {
                tag,
                reason,
                attempts,
                last_hash,
                files,
                properties,
            } => {
                assert_eq!(tag, "checker-outage-systemic");
                assert!(reason.contains("PROPGUARD_DISABLE"));
                assert!(reason.to_lowercase().contains("fleet"));
                assert_eq!(attempts, 0);
                assert!(last_hash.is_empty());
                assert!(files.is_empty());
                assert!(properties.is_empty());
            }
            Decision::Allow { .. } => panic!("systemic outage must fail-closed to Block"),
        }
    }

    #[test]
    fn escalate_giveup_non_giveup_decision_passes_through_both_flags() {
        // A normal Block (e.g. below-threshold) must not be touched by this
        // mapper regardless of the systemic flag.
        let block = || Decision::Block {
            reason: "some other block".to_string(),
            tag: "below-threshold",
            files: vec![],
            properties: vec![],
            attempts: 1,
            last_hash: String::new(),
        };
        match escalate_giveup_on_systemic(block(), true) {
            Decision::Block { tag, .. } => assert_eq!(tag, "below-threshold"),
            Decision::Allow { .. } => panic!("non-giveup decision must not be rewritten"),
        }
        match escalate_giveup_on_systemic(block(), false) {
            Decision::Block { tag, .. } => assert_eq!(tag, "below-threshold"),
            Decision::Allow { .. } => panic!("non-giveup decision must not be rewritten"),
        }

        // An Allow with a different tag (e.g. properties-satisfied) must not
        // be rewritten either, even when systemic_outage is true.
        let other_allow = || Decision::Allow {
            tag: "properties-satisfied",
            attempts: 0,
            last_hash: "h".to_string(),
        };
        match escalate_giveup_on_systemic(other_allow(), true) {
            Decision::Allow { tag, .. } => assert_eq!(tag, "properties-satisfied"),
            Decision::Block { .. } => panic!("non-giveup Allow must not be rewritten"),
        }
        match escalate_giveup_on_systemic(other_allow(), false) {
            Decision::Allow { tag, .. } => assert_eq!(tag, "properties-satisfied"),
            Decision::Block { .. } => panic!("non-giveup Allow must not be rewritten"),
        }
    }

    // ── checker output parsing ─────────────────────────────────────────────
    #[test]
    fn parse_counts_only_explicit_pass() {
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let out = "PROP error-path: PASS\nPROP output-schema: FAIL — schema changed\nPROP determinism: PASS";
        match parse_checker_output(out, &props) {
            Determination::Known(Verified { satisfied, .. }) => assert_eq!(satisfied, 2),
            Determination::Undetermined(why) => panic!("should parse: {}", why.as_str()),
        }
    }

    #[test]
    fn parse_unrelated_output_is_error_not_all_pass() {
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        // Output that names none of the property ids must fail closed, not be
        // mistaken for "everything passed".
        match parse_checker_output("looks good to me!", &props) {
            Determination::Undetermined(_) => {}
            Determination::Known(Verified { .. }) => {
                panic!("unusable checker output must be an Error (fail closed), not all-pass")
            }
        }
    }

    #[test]
    fn unspawnable_checker_is_error() {
        let cfg = Config {
            checker_cmd: "propguard-no-such-binary-xyzzy".to_string(),
            ..Config::default()
        };
        let props = props_by_ids(&["error-path"]);
        match run_checker(&cfg, "dc", &props, "diff") {
            Determination::Undetermined(_) => {}
            Determination::Known(Verified { .. }) => {
                panic!("an unspawnable checker must be an Error")
            }
        }
    }

    // ── stdin/stdout deadlock regression (fix-propguard-003) ───────────────
    //
    // A checker that writes a lot of stdout *before* draining stdin can
    // deadlock a caller that writes stdin synchronously on the main thread:
    // both the child's stdout pipe and our stdin pipe fill up (~64KB OS pipe
    // buffers) and neither side can make progress. Because that write used to
    // happen before `wait_timeout`, `checker_timeout_secs` gave zero
    // protection. This test uses a real "checker" that floods stdout without
    // reading stdin at all, with a diff large enough to fill the stdin pipe
    // buffer, and asserts `run_checker` still returns (as a timeout/error)
    // within a small configured timeout instead of hanging indefinitely.
    #[test]
    fn checker_stdout_flood_without_draining_stdin_does_not_hang() {
        // Print far more than a typical pipe buffer (~64KB) to stdout, then
        // exit, *without* ever reading stdin. If stdin were written
        // synchronously before wait_timeout, and the diff below is bigger
        // than the stdin pipe buffer too, the parent would block on
        // write_all while this child blocks on write (stdout full and
        // un-drained) — a classic two-pipe deadlock.
        let cfg = Config {
            checker_cmd: "yes X | head -c 5000000".to_string(),
            checker_timeout_secs: 2,
            ..Config::default()
        };
        let props = props_by_ids(&["error-path"]);
        // Bigger than a pipe buffer, so the old synchronous stdin write would
        // itself block once the child's stdout side backs up.
        let big_diff = "line of diff content\n".repeat(20_000);

        let start = std::time::Instant::now();
        let outcome = run_checker(&cfg, "dc", &props, &big_diff);
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(10),
            "run_checker must be bounded by checker_timeout_secs, took {elapsed:?}"
        );
        // Whatever the verdict (timeout error, or a fast exit that never
        // needed stdin at all), it must not be silently "all pass" via a
        // hang-then-succeed path; either shape is acceptable evidence the
        // call returned instead of hanging.
        match outcome {
            Determination::Undetermined(_) | Determination::Known(Verified { .. }) => {}
        }
    }

    // ── a checker that exits non-zero must not be trusted, even with output ──
    //
    // The verdict lives in the `PROP <id>: PASS|FAIL` stdout lines, never in the
    // exit code, so a non-zero exit means the checker crashed or errored
    // mid-run. Before the fix, the guard was `!success && out.trim().is_empty()`,
    // so a crash that had ALREADY written a `PASS` line before dying was parsed
    // as an authoritative verdict — the same fail-open as reading a process that
    // exited 101 as "no errors". This pins that a non-zero exit is an Error
    // (fail-closed) regardless of what was on stdout.
    #[cfg(unix)]
    #[test]
    fn nonzero_exit_with_a_pass_line_on_stdout_is_an_error_not_a_verdict() {
        let props = props_by_ids(&["error-path"]);
        let id = &props[0].id;
        // Emit a full PASS verdict for the property, then exit non-zero — a
        // partial/crashing checker that happened to write something first.
        let cfg = Config {
            checker_cmd: format!("echo 'PROP {id}: PASS'; exit 7"),
            checker_timeout_secs: 5,
            ..Config::default()
        };
        let outcome = run_checker(&cfg, "dc", &props, "diff");
        match outcome {
            Determination::Undetermined(why) => {
                let e = why.as_str();
                assert!(e.contains('7'), "reason should name the exit: {e}");
            }
            Determination::Known(Verified { satisfied, .. }) => {
                panic!(
                    "a non-zero exit must not be trusted as a verdict (got satisfied={satisfied})"
                )
            }
        }
    }

    // Guard the other side: a clean (exit 0) checker with a real verdict still
    // parses, so the fix did not turn every checker into an error.
    #[cfg(unix)]
    #[test]
    fn zero_exit_with_a_pass_line_still_parses_as_verified() {
        let props = props_by_ids(&["error-path"]);
        let id = &props[0].id;
        let cfg = Config {
            checker_cmd: format!("echo 'PROP {id}: PASS'; exit 0"),
            checker_timeout_secs: 5,
            ..Config::default()
        };
        match run_checker(&cfg, "dc", &props, "diff") {
            Determination::Known(Verified { satisfied, .. }) => assert_eq!(satisfied, 1),
            Determination::Undetermined(why) => {
                panic!("a clean checker must parse, got Error: {}", why.as_str())
            }
        }
    }

    // ── shell-wrapped timeout must kill the real grandchild (CA-propguard-004) ──
    //
    // When `checker_cmd` contains shell metacharacters, `build_command` runs it
    // via `harness_core::shell::command` (`sh -c "..."`), so the direct child
    // `run_checker` gets back is the *shell*, not the real checker. Before this
    // fix, the timeout path only called `child.kill()` on that shell — a
    // grandchild the shell backgrounds/execs never got signaled and could keep
    // running after `run_checker` returned. Assert that a shell-wrapped
    // checker_cmd which backgrounds a long-running child does not leave that
    // child alive once the timeout has fired.
    #[cfg(unix)]
    #[test]
    fn shell_wrapped_timeout_kills_the_real_backgrounded_checker() {
        let marker = std::env::temp_dir().join(format!(
            "propguard-ca004-marker-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_file(&marker);

        // `;` is a shell metacharacter, so this goes through the shell path.
        // The backgrounded `sleep 30 &` is the real long-running checker
        // process; it writes its pid to `marker` so the test can verify it
        // is actually gone (not just that `run_checker` returned) after the
        // timeout fires.
        let cfg = Config {
            checker_cmd: format!("sleep 30 & echo $! > {}; wait $!", marker.display()),
            checker_timeout_secs: 1,
            ..Config::default()
        };
        let props = props_by_ids(&["error-path"]);

        let start = std::time::Instant::now();
        let outcome = run_checker(&cfg, "dc", &props, "diff");
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(10),
            "run_checker must be bounded by checker_timeout_secs even when shell-wrapped, took {elapsed:?}"
        );
        assert!(
            matches!(outcome, Determination::Undetermined(_)),
            "a timed-out shell-wrapped checker must be reported as an Error"
        );

        // Give the OS a brief moment to actually reap the killed process
        // before we check, then confirm the grandchild `sleep` is gone.
        std::thread::sleep(Duration::from_millis(200));
        if let Ok(pid_str) = std::fs::read_to_string(&marker) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                // SAFETY: signal 0 is a pure existence/permission probe.
                let alive = unsafe { libc::kill(pid, 0) == 0 };
                assert!(
                    !alive,
                    "the backgrounded grandchild (pid {pid}) must be killed on timeout, not just the shell"
                );
            }
        }
        let _ = std::fs::remove_file(&marker);
    }

    // ── bounded stdout read after immediate-child exit (CA-propguard-005) ──
    //
    // After `wait_timeout` reports the immediate child has exited, reading
    // its stdout must not be able to hang even if a lingering process still
    // holds the pipe's write end open. This test shell-wraps a checker_cmd
    // whose immediate process exits right away, but backgrounds a child that
    // inherits the stdout fd and keeps it open well past `checker_timeout_secs`;
    // without a bounded read this would hang `run_checker` indefinitely.
    #[cfg(unix)]
    #[test]
    fn lingering_stdout_holder_does_not_hang_the_read() {
        let cfg = Config {
            // The immediate shell process exits promptly ("exit 0"), but a
            // backgrounded subshell inherits the stdout fd and sleeps well
            // beyond the timeout, keeping the pipe's write end open.
            checker_cmd: "(sleep 5 &) ; exit 0".to_string(),
            checker_timeout_secs: 1,
            ..Config::default()
        };
        let props = props_by_ids(&["error-path"]);

        let start = std::time::Instant::now();
        let outcome = run_checker(&cfg, "dc", &props, "diff");
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(4),
            "reading stdout after the immediate child exits must not hang past a reasonable \
             bound even if a lingering process keeps the pipe open, took {elapsed:?}"
        );
        match outcome {
            Determination::Undetermined(_) | Determination::Known(Verified { .. }) => {}
        }
    }

    // ── truncation guard ───────────────────────────────────────────────────
    #[test]
    fn truncated_diff_blocks_then_gives_up() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_truncated(&cfg, &props, vec!["src/x.rs".to_string()], 0);
        match d {
            Decision::Block {
                tag,
                last_hash,
                reason,
                ..
            } => {
                assert_eq!(tag, "diff-truncated");
                assert!(last_hash.is_empty(), "must not certify an unchecked tail");
                assert!(reason.contains("max_diff_bytes"));
            }
            Decision::Allow { .. } => panic!("a truncated diff must block, not silently allow"),
        }
        let g = decide_truncated(&cfg, &props, vec!["src/x.rs".to_string()], cfg.max_attempts);
        assert!(matches!(g, Decision::Allow { tag, .. } if tag == "truncated-giveup"));
    }

    // ── CA-propguard-04: a diff-truncated Block must NOT attribute
    //    per-property violations. The truncated tail was never checked, so no
    //    property was actually evaluated as violated; reporting the full
    //    prop_ids pollutes the property_id-keyed fleet-correlation store the
    //    same way CA-propguard-03 does. ──────────────────────────────────────
    #[test]
    fn truncated_block_reports_no_property_violations() {
        let cfg = cfg_default();
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_truncated(&cfg, &props, vec!["src/x.rs".to_string()], 0);
        match d {
            Decision::Block {
                tag, properties, ..
            } => {
                assert_eq!(tag, "diff-truncated");
                assert!(
                    properties.is_empty(),
                    "a diff-truncated block checked nothing (the tail was dropped) — it must \
                     not report any per-property violation to the correlation store, got {properties:?}"
                );
            }
            Decision::Allow { .. } => panic!("a truncated diff must block"),
        }
    }

    // ── a failed git scan must not become a silent allow (fail-closed) ──────
    //
    // The fail-open this fix closes: a git command errored inside a real repo,
    // the changed set collapsed to empty, and the gate read that as "no code
    // changes → allow". A `Failed` scan must BLOCK — modeled exactly on the
    // truncation / checker-outage guards: bounded, escapable, no hash, no
    // per-property violations attributed (nothing was checked).

    /// A failed git scan → BLOCK with the `git-scan-failed` tag, an escapable
    /// reason, no recorded hash, and no per-property violations.
    #[test]
    fn failed_git_scan_blocks_it_does_not_allow() {
        let cfg = cfg_default(); // max_attempts = 2
        let d = decide_scan_failed(&cfg, 0);
        match d {
            Decision::Allow { tag, .. } => {
                panic!("a failed git scan must not be silently allowed (undetermined-change bypass); got allow tag={tag}")
            }
            Decision::Block {
                tag,
                reason,
                last_hash,
                properties,
                ..
            } => {
                assert_eq!(tag, "git-scan-failed");
                assert!(
                    last_hash.is_empty(),
                    "must not certify an undetermined change set"
                );
                assert!(
                    properties.is_empty(),
                    "a git-scan-failed block checked nothing — it must not report per-property violations, got {properties:?}"
                );
                assert!(
                    reason.contains("PROPGUARD_DISABLE"),
                    "reason must name the disable escape hatch: {reason}"
                );
                assert!(
                    reason.contains(".propguard-skip"),
                    "reason must name the one-shot skip escape hatch: {reason}"
                );
            }
        }
    }

    /// Bounded, never trapped: after max_attempts consecutive failed scans we
    /// give up and allow — but via a *distinct* tag, so a persistently broken
    /// git is never mistaken for a passing check and never traps the turn.
    #[test]
    fn failed_git_scan_gives_up_after_max_attempts_but_never_traps() {
        let cfg = cfg_default(); // max_attempts = 2
        let d = decide_scan_failed(&cfg, cfg.max_attempts);
        match d {
            Decision::Allow { tag, last_hash, .. } => {
                assert_eq!(tag, "git-scan-failed-giveup");
                assert!(last_hash.is_empty());
            }
            Decision::Block { .. } => {
                panic!("must give up after max_attempts so a broken git never permanently traps the turn")
            }
        }
    }

    // ── an unread / half-read diff must not become a silent allow either ────

    /// The pure decision half of the fix: `Undetermined` from `diff_text` →
    /// BLOCK with its own tag, an escapable reason that repeats WHY, no
    /// recorded hash, and no per-property violations attributed.
    #[test]
    fn unreadable_diff_blocks_with_its_own_tag_and_says_why() {
        let cfg = cfg_default(); // max_attempts = 2
        let d = decide_diff_failed(
            &cfg,
            "stdout was not valid UTF-8",
            vec!["src/x.rs".to_string()],
            0,
        );
        match d {
            Decision::Allow { tag, .. } => panic!(
                "a diff that could not be read must not be silently allowed; got allow tag={tag}"
            ),
            Decision::Block {
                tag,
                reason,
                last_hash,
                properties,
                files,
                attempts: _,
            } => {
                assert_eq!(tag, "diff-read-failed");
                assert!(last_hash.is_empty(), "must not certify an unobserved diff");
                assert!(
                    properties.is_empty(),
                    "nothing was checked — no per-property violation may be attributed, got \
                     {properties:?}"
                );
                assert_eq!(files, vec!["src/x.rs".to_string()]);
                assert!(
                    reason.contains("stdout was not valid UTF-8"),
                    "the reason must carry the underlying cause, not just 'blocked': {reason}"
                );
                assert!(
                    reason.contains("PROPGUARD_DISABLE") && reason.contains(".propguard-skip"),
                    "the block must stay escapable: {reason}"
                );
            }
        }
    }

    /// Bounded, never trapped — same contract as the failed-scan guard: after
    /// `max_attempts` consecutive unreadable diffs we give up and allow, but
    /// under a DISTINCT tag so a give-up is never mistaken for a passing check.
    #[test]
    fn unreadable_diff_gives_up_after_max_attempts_but_never_traps() {
        let cfg = cfg_default(); // max_attempts = 2
        let d = decide_diff_failed(&cfg, "boom", vec!["src/x.rs".to_string()], cfg.max_attempts);
        match d {
            Decision::Allow { tag, last_hash, .. } => {
                assert_eq!(tag, "diff-read-failed-giveup");
                assert!(last_hash.is_empty());
            }
            Decision::Block { .. } => panic!(
                "must give up after max_attempts so a permanently unreadable diff never traps \
                 the turn"
            ),
        }
    }

    #[test]
    fn effective_threshold_is_clamped_to_property_count() {
        let cfg = Config {
            threshold: 5,
            ..Config::default()
        };
        // Only 2 properties derived → threshold can't exceed 2.
        assert_eq!(effective_threshold(&cfg, 2), 2);
    }

    #[test]
    fn below_threshold_is_the_single_comparison() {
        assert!(below_threshold(0, 3));
        assert!(below_threshold(2, 3));
        assert!(!below_threshold(3, 3));
        assert!(!below_threshold(4, 3));
    }

    // ── CA-propguard-01: a failing subprocess check must NOT arm the
    //    "already-verified" shortcut, so the next identical failing diff is
    //    re-checked (fail-closed) rather than auto-allowed. ─────────────────────
    fn state_with_hash(h: &str) -> crate::state::SessionState {
        crate::state::SessionState {
            attempts: 1,
            last_hash: h.to_string(),
            last_ts: now(),
        }
    }

    /// Subprocess mode, below threshold: the Block must not record the diff hash
    /// as a passing marker, so the SAME unfixed diff on the next round is NOT
    /// short-circuited by `already_verified` and the checker re-runs.
    #[test]
    fn subprocess_block_does_not_arm_already_verified() {
        let cfg = Config {
            mode: Mode::Subprocess,
            ..Config::default()
        };
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        // Round 1: checker says satisfied=1 (< threshold 3) => Block.
        let d = decide_from_count(
            &cfg,
            Determination::Known(Verified {
                satisfied: 1,
                findings: Some("PROP error-path: FAIL".to_string()),
            }),
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "HASH_FAIL".to_string(),
            0,
            "dc",
        );
        let recorded = match d {
            Decision::Block { tag, last_hash, .. } => {
                assert_eq!(tag, "below-threshold");
                last_hash
            }
            Decision::Allow { .. } => panic!("below threshold must block"),
        };
        // Round 2: persisted state carries whatever the Block recorded. The same
        // unfixed diff (HASH_FAIL) must NOT be treated as already-verified.
        let st = state_with_hash(&recorded);
        assert!(
            !already_verified(&st, "HASH_FAIL"),
            "a failing subprocess check must not auto-allow the next identical diff"
        );
    }

    /// Inject mode's documented "trust-after-one-block": the first (satisfied=0)
    /// block DOES record the hash, so the same diff is trusted next round.
    #[test]
    fn inject_block_arms_trust_after_one_block() {
        let cfg = Config {
            mode: Mode::Inject,
            ..Config::default()
        };
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let d = decide_from_count(
            &cfg,
            Determination::Known(Verified {
                satisfied: 0,
                findings: None,
            }),
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "HASH_INJECT".to_string(),
            0,
            "dc",
        );
        let recorded = match d {
            Decision::Block { last_hash, .. } => last_hash,
            Decision::Allow { .. } => panic!("inject first pass must block"),
        };
        let st = state_with_hash(&recorded);
        assert!(
            already_verified(&st, "HASH_INJECT"),
            "inject mode must trust the same diff after one block"
        );
    }

    // ── CA-propguard-01 (2026 audit round): a subprocess below-threshold Block
    //    must report only the UNSATISFIED properties to overwatch, not every
    //    derived property — PASSed properties polluting the fleet-correlation
    //    signal is itself a defect distinct from the hash-arming bug above. ────
    #[test]
    fn below_threshold_block_reports_only_unsatisfied_properties() {
        let cfg = Config {
            mode: Mode::Subprocess,
            ..Config::default()
        };
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let outcome = Determination::Known(Verified {
            satisfied: 2,
            findings: Some(
                "PROP error-path: PASS\n\
                 PROP output-schema: FAIL — schema changed\n\
                 PROP determinism: PASS"
                    .to_string(),
            ),
        });
        let d = decide_from_count(
            &cfg,
            outcome,
            &props,
            3,
            vec!["src/x.rs".to_string()],
            "HASH".to_string(),
            0,
            "dc",
        );
        match d {
            Decision::Block { properties, .. } => {
                assert_eq!(
                    properties,
                    vec!["output-schema"],
                    "PASSed properties must not be recorded as fleet violations"
                );
            }
            Decision::Allow { .. } => panic!("below threshold must block"),
        }
    }

    // ── CA-propguard-02: a property's verdict must be read from its OWN verdict
    //    line, not from another property's explanation that mentions its id. ─────
    #[test]
    fn parse_anchors_to_the_propertys_own_verdict_line() {
        let props = props_by_ids(&["idempotence", "determinism", "output-schema"]);
        let out = "\
PROP idempotence: PASS — this also confirms determinism holds and output-schema is untouched\n\
PROP determinism: FAIL — hidden RNG dependency\n\
PROP output-schema: PASS";
        match parse_checker_output(out, &props) {
            Determination::Known(Verified { satisfied, .. }) => {
                // idempotence PASS + output-schema PASS = 2; determinism is FAIL
                // and must never be counted satisfied via line 1's mention.
                assert_eq!(
                    satisfied, 2,
                    "determinism was reported FAIL and must not be counted satisfied"
                );
            }
            Determination::Undetermined(why) => panic!("should parse: {}", why.as_str()),
        }
    }

    // ── CA-propguard-05: a PASS verdict whose free-text reason merely mentions
    //    "fail" (e.g. "cannot fail", "no failure") must stay PASS. The verdict is
    //    the structured token after the id, not a substring of the reason. ───────
    #[test]
    fn pass_verdict_with_fail_in_reason_stays_pass() {
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        let out = "\
PROP error-path: PASS — all error branches handled, cannot fail silently\n\
PROP output-schema: PASS — no failure modes changed\n\
PROP determinism: FAIL — hidden RNG dependency";
        match parse_checker_output(out, &props) {
            Determination::Known(Verified { satisfied, .. }) => {
                // Both PASS lines mention "fail" in prose; only determinism is a
                // real FAIL. A substring parser would score 0 here.
                assert_eq!(
                    satisfied, 2,
                    "PASS verdicts whose reason mentions 'fail' must stay PASS"
                );
            }
            Determination::Undetermined(why) => panic!("should parse: {}", why.as_str()),
        }
        // And such a PASS must NOT be reported as a fleet violation.
        let violated = unsatisfied_prop_ids(&props, Some(out));
        assert_eq!(
            violated,
            vec!["determinism"],
            "a PASS whose reason mentions 'fail' must not be reported violated"
        );
    }

    // ── CA-propguard-06: a property the subprocess checker emitted NO verdict
    //    line for is NEVER-EVALUATED, not checked-and-failed — it must not be
    //    reported violated to the fleet-correlation store (per-property grain). ──
    #[test]
    fn never_evaluated_property_not_reported_violated() {
        let props = props_by_ids(&["error-path", "output-schema", "determinism"]);
        // Checker emitted verdicts only for error-path (FAIL) and output-schema
        // (PASS); determinism got NO line at all → never evaluated.
        let out = "\
PROP error-path: FAIL — panics on empty input\n\
PROP output-schema: PASS";
        let violated = unsatisfied_prop_ids(&props, Some(out));
        assert_eq!(
            violated,
            vec!["error-path"],
            "only the checked-and-FAILED property is violated; the never-evaluated \
             property must not be reported (or pollute the correlation store)"
        );
    }

    // ── backlog 87dbfbb8 / d8e22b26: a diff that could not be READ must never
    //    reach an allow, and a diff only PARTIALLY read must never be handed
    //    downstream as if it were complete. ──────────────────────────────────
    //
    // These drive `evaluate` end to end against the REAL `git` binary, because
    // the fail-open they pin is an interaction between two of its steps:
    // `changed_files` correctly answers "these files changed" and `diff_text`
    // then fails to observe them. Asserting only on `diff_text`'s own return
    // value would not show that the *decision* flipped, which is the thing that
    // was measured.
    //
    // The injected fault is a tracked file whose working-tree content is not
    // valid UTF-8 and contains no NUL byte, so `git` classifies it as TEXT and
    // emits those bytes inside a normal textual diff — which propguard's
    // bounded pipe read then cannot decode. It needs no stand-in binary: the
    // real `git` produces it.
    //
    // Every fault test below is paired with an anti-vacuity control that
    // differs ONLY in whether the bytes are decodable. Without them an
    // implementation that answered "undetermined" for every repo would pass.

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// A fresh scratch directory (mirrors `git.rs`'s test style; no `tempfile`
    /// dev-dependency). The caller removes it.
    fn scratch_dir() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "propguard-gate-diffread-test-{}-{}-{}",
            std::process::id(),
            line!(),
            n
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create scratch dir");
        p
    }

    #[track_caller]
    fn git_in(root: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .current_dir(root)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("spawn git")
            .success();
        assert!(ok, "git {args:?} failed in {}", root.display());
    }

    /// An initialised repo with `files` committed, so later edits show up as
    /// changes against HEAD.
    fn repo_with_committed(files: &[(&str, &str)]) -> std::path::PathBuf {
        let root = scratch_dir();
        git_in(&root, &["init", "-q"]);
        git_in(&root, &["config", "user.email", "t@t.com"]);
        git_in(&root, &["config", "user.name", "t"]);
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write file");
            git_in(&root, &["add", name]);
        }
        git_in(&root, &["commit", "-qm", "init"]);
        root
    }

    /// Bytes that are not valid UTF-8 and contain NO NUL, so `git` treats the
    /// file as text and puts them straight into the diff body.
    const UNDECODABLE_SOURCE: &[u8] = b"fn main() {\n    let s = \"\xff\xfe\xfd\";\n}\n";

    /// The decodable counterpart, same shape and same file, so the only
    /// difference between a fault run and its control is the byte sequence.
    const DECODABLE_SOURCE: &str = "fn main() {\n    let s = \"replaced\";\n}\n";

    fn diffread_cfg() -> Config {
        Config {
            mode: Mode::Inject,
            include: vec!["**/*.rs".to_string()],
            exclude: vec![],
            min_changed_files: 1,
            done_criteria: "the gate must never allow a stop on a diff it could not read"
                .to_string(),
            ..Config::default()
        }
    }

    fn fresh_state() -> crate::state::SessionState {
        crate::state::SessionState {
            attempts: 0,
            last_hash: String::new(),
            // Far enough in the past that the attempt counter resets.
            last_ts: 0,
        }
    }

    /// THE measured fail-open (backlog 87dbfbb8). `changed_files` answers
    /// `Files(["bad.rs"])`, `git diff` emits a textual diff propguard cannot
    /// decode, and before the fix `diff_text` returned `text: ""` /
    /// `truncated: false` so `evaluate` hit
    /// `if diff.trim().is_empty() { return allow("empty-diff") }`. An unread
    /// diff must be UNDETERMINED and block, never allow.
    #[test]
    fn a_diff_that_could_not_be_read_blocks_it_does_not_allow_as_empty() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with_committed(&[("bad.rs", "fn main() {}\n")]);
        std::fs::write(root.join("bad.rs"), UNDECODABLE_SOURCE).expect("write undecodable");

        match evaluate(&diffread_cfg(), &root, &fresh_state()) {
            Decision::Allow { tag, .. } => panic!(
                "a diff that could not be read must not be allowed as if it were empty; \
                 got allow tag={tag}"
            ),
            Decision::Block {
                tag,
                reason,
                last_hash,
                properties,
                ..
            } => {
                assert_eq!(tag, "diff-read-failed");
                assert!(
                    last_hash.is_empty(),
                    "must not certify a diff it never observed"
                );
                assert!(
                    properties.is_empty(),
                    "nothing was checked — no per-property violation may be attributed, got \
                     {properties:?}"
                );
                assert!(
                    reason.contains("PROPGUARD_DISABLE") && reason.contains(".propguard-skip"),
                    "the block must stay escapable and say why: {reason}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Anti-vacuity control #1 for the test above: the SAME repo, the SAME
    /// file, the SAME edit — only the bytes are decodable. The diff is read,
    /// so the ordinary decision (inject mode's unverified-diff block) must
    /// come back, not the undetermined one.
    #[test]
    fn a_readable_diff_still_takes_the_ordinary_path() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with_committed(&[("bad.rs", "fn main() {}\n")]);
        std::fs::write(root.join("bad.rs"), DECODABLE_SOURCE).expect("write decodable");

        match evaluate(&diffread_cfg(), &root, &fresh_state()) {
            Decision::Block { tag, files, .. } => {
                assert_eq!(
                    tag, "below-threshold",
                    "a readable diff must reach the real check, not the undetermined guard — \
                     otherwise the sibling fault test would pass merely by everything blocking"
                );
                assert_eq!(files, vec!["bad.rs".to_string()]);
            }
            Decision::Allow { tag, .. } => {
                panic!("inject mode blocks a new unverified diff; got allow tag={tag}")
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Anti-vacuity control #2, and the requirement that this fix must not
    /// break: a real repo with nothing changed still allows through the
    /// ordinary path. If the fix ever resolved "clean" to "undetermined" this
    /// is the test that catches it.
    #[test]
    fn a_genuinely_clean_repo_still_allows() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with_committed(&[("bad.rs", "fn main() {}\n")]);

        match evaluate(&diffread_cfg(), &root, &fresh_state()) {
            Decision::Allow { tag, .. } => assert_eq!(
                tag, "no-code-changes",
                "a clean repo must allow via the ordinary no-changes path"
            ),
            Decision::Block { tag, reason, .. } => {
                panic!("a genuinely clean repo must not block; got tag={tag} reason={reason}")
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// backlog d8e22b26: only PART of the diff could be read. `git diff`
    /// succeeds for the unstaged change to `a.rs` while `git diff --cached`
    /// cannot be decoded for the staged `b.rs`, and `run_diff` dropped that
    /// failure on the floor — producing a diff that mentions `a.rs`, omits
    /// `b.rs`, and carries `truncated: false`, i.e. announces itself COMPLETE.
    /// `truncated` is the gate's only incompleteness signal, so nothing
    /// downstream could tell. The decision must be the undetermined one, not a
    /// verdict reached over a diff missing a file.
    #[test]
    fn a_partially_read_diff_is_not_handed_on_as_complete() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with_committed(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
        // a.rs: an ordinary, readable, UNSTAGED edit.
        std::fs::write(root.join("a.rs"), "fn a() { let x = 1; }\n").expect("write a.rs");
        // b.rs: STAGED with undecodable bytes, so `git diff --cached` is the
        // sub-command whose output cannot be read.
        std::fs::write(root.join("b.rs"), UNDECODABLE_SOURCE).expect("write b.rs");
        git_in(&root, &["add", "b.rs"]);

        match evaluate(&diffread_cfg(), &root, &fresh_state()) {
            Decision::Allow { tag, .. } => {
                panic!("a partially-read diff must not be allowed; got allow tag={tag}")
            }
            Decision::Block { tag, last_hash, .. } => {
                assert_eq!(
                    tag, "diff-read-failed",
                    "the block must come from the incompleteness guard. A `below-threshold` \
                     block here would mean the gate judged a diff that silently omitted b.rs \
                     — the same verdict for 'checked and short' as for 'never saw half of it'"
                );
                assert!(
                    last_hash.is_empty(),
                    "an incomplete diff must never be certified into already-verified"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A checker script that PASSes every property in the catalog, so
    /// `Mode::Subprocess` reaches `properties-satisfied` — an ALLOW. Written
    /// outside the repo under test so it is not itself a changed file.
    #[cfg(unix)]
    fn all_pass_checker(dir: &Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        // `cat > /dev/null` drains the prompt so the checker never dies on a
        // full stdin pipe; then one verdict line per catalog property.
        let mut body = String::from("#!/bin/sh\ncat > /dev/null\n");
        for p in CATALOG.iter() {
            body.push_str(&format!("echo 'PROP {}: PASS'\n", p.id));
        }
        let path = dir.join("all-pass-checker.sh");
        std::fs::write(&path, body).expect("write checker");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path.to_string_lossy().into_owned()
    }

    /// The last step of backlog d8e22b26, which the original verifier recorded
    /// honestly as UNVERIFIED (not refuted) because it had no real
    /// `checker_cmd`: in `Mode::Subprocess` a partial diff would be handed to
    /// the checker, and a checker that PASSes would produce an ALLOW.
    ///
    /// This settles it by measurement in both directions. Here: the partial
    /// diff plus a checker that passes EVERYTHING must still block, because
    /// the incompleteness guard sits before the `match cfg.mode` split and the
    /// checker is never reached.
    #[cfg(unix)]
    #[test]
    fn a_partially_read_diff_blocks_in_subprocess_mode_before_the_checker_runs() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with_committed(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
        std::fs::write(root.join("a.rs"), "fn a() { let x = 1; }\n").expect("write a.rs");
        std::fs::write(root.join("b.rs"), UNDECODABLE_SOURCE).expect("write b.rs");
        git_in(&root, &["add", "b.rs"]);

        let bin = scratch_dir();
        let cfg = Config {
            mode: Mode::Subprocess,
            checker_cmd: all_pass_checker(&bin),
            ..diffread_cfg()
        };

        match evaluate(&cfg, &root, &fresh_state()) {
            Decision::Allow { tag, .. } => panic!(
                "a checker that PASSes everything must never be handed a diff missing b.rs; \
                 got allow tag={tag}"
            ),
            Decision::Block { tag, .. } => assert_eq!(tag, "diff-read-failed"),
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&bin);
    }

    /// The other direction, and the anti-vacuity partner the test above needs:
    /// with the SAME all-PASS checker and a fully readable two-sided diff,
    /// `Mode::Subprocess` really does reach `properties-satisfied` and ALLOW.
    /// So the block above is a guard that prevented a reachable allow, not an
    /// assertion about a path that could never have allowed anyway.
    #[cfg(unix)]
    #[test]
    fn subprocess_mode_really_can_allow_when_the_diff_is_fully_read() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with_committed(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
        std::fs::write(root.join("a.rs"), "fn a() { let x = 1; }\n").expect("write a.rs");
        std::fs::write(root.join("b.rs"), DECODABLE_SOURCE).expect("write b.rs");
        git_in(&root, &["add", "b.rs"]);

        let bin = scratch_dir();
        let cfg = Config {
            mode: Mode::Subprocess,
            checker_cmd: all_pass_checker(&bin),
            ..diffread_cfg()
        };

        match evaluate(&cfg, &root, &fresh_state()) {
            Decision::Allow { tag, .. } => assert_eq!(
                tag, "properties-satisfied",
                "the subprocess allow must be reachable, otherwise the sibling test guards \
                 nothing"
            ),
            Decision::Block { tag, reason, .. } => panic!(
                "an all-PASS checker over a fully readable diff must allow; got tag={tag} \
                 reason={reason}"
            ),
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&bin);
    }

    /// Anti-vacuity control for the partial-diff test: the identical two-file,
    /// staged-plus-unstaged setup with decodable bytes must still reach the
    /// ordinary decision over BOTH files.
    #[test]
    fn a_fully_read_two_sided_diff_still_takes_the_ordinary_path() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = repo_with_committed(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
        std::fs::write(root.join("a.rs"), "fn a() { let x = 1; }\n").expect("write a.rs");
        std::fs::write(root.join("b.rs"), DECODABLE_SOURCE).expect("write b.rs");
        git_in(&root, &["add", "b.rs"]);

        match evaluate(&diffread_cfg(), &root, &fresh_state()) {
            Decision::Block { tag, files, .. } => {
                assert_eq!(tag, "below-threshold");
                assert_eq!(files, vec!["a.rs".to_string(), "b.rs".to_string()]);
            }
            Decision::Allow { tag, .. } => {
                panic!("inject mode blocks a new unverified diff; got allow tag={tag}")
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
