//! Auto-reconcile fix commits with unresolved review findings
//! (`overwatch reconcile-fixed`).
//!
//! Context: findings recorded via `record-finding` only leave the
//! `review-queue` when a human runs `record-disposition`. If nobody
//! remembers to run it after the fix commit lands, the finding sits "open"
//! forever even though the underlying issue is long fixed (see the
//! Continuous-Audit review-queue stale-backlog incident, 2026-07-17: 18
//! findings were all already fixed in source with passing tests, but none
//! had been dispositioned). This module closes that gap deterministically:
//! scan a range of git commit messages for finding-id references
//! (`CA-<crate>-<NNN>`, the convention already used by `bridge.rs` /
//! `continuous-audit.sh`) and auto-record a CONFIRMED disposition for any
//! referenced finding that is on the store but not yet disposed.
//!
//! # The two ledgers this joins, and what an unreadable one means (t3)
//!
//! The decision needs BOTH the findings ledger (is this id a real finding?) and
//! the disposition ledger (is it already closed?). Reading either as an empty
//! set does not just lose a statistic:
//!
//! * an empty FINDINGS set means every referenced id is "unknown" → nothing is
//!   reconciled, while the report says "0 finding(s) reconciled" — a sentence a
//!   human reads as "nothing needed doing";
//! * an empty DISPOSITION set means every finding looks undisposed → the same
//!   finding is disposed again, and `append_disposition`'s own dedup cannot
//!   catch it (it re-reads the same unreadable ledger), so the metrics that are
//!   computed from this ledger get skewed by duplicates.
//!
//! So both are read tri-state, and an `Undetermined` answer means this run
//! writes NOTHING, says so, and exits 3 — never "0 reconciled".
use crate::disposition::{Disposition, DispositionVerdict};
use crate::review_queue::SourceHealth;
use crate::store;
use anyhow::Result;
use harness_core::verdict::Determination;
use regex::Regex;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// One commit under consideration: its hash (used for disposition
/// provenance) and full message text (subject + body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRef {
    pub hash: String,
    pub message: String,
}

/// Extract every `CA-<crate>-<NNN>`-shaped finding-id referenced in `text`,
/// in first-seen order, de-duplicated. Pure (no I/O, no clock).
pub fn extract_finding_ids(text: &str) -> Vec<String> {
    #[expect(
        clippy::expect_used,
        reason = "compile-time literal pattern with no runtime input; Regex::new can \
                  only fail if this line is edited wrong, which is a build-time bug. \
                  Returning an empty Vec instead would read downstream as 'this commit \
                  references no finding' — the fail-open direction for reconcile."
    )]
    let re = Regex::new(r"CA-[A-Za-z0-9_-]+-[0-9]+").expect("static finding-id pattern is valid");
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for m in re.find_iter(text) {
        let id = m.as_str().to_string();
        if seen.insert(id.clone()) {
            out.push(id);
        }
    }
    out
}

/// Compute the new dispositions `reconcile-fixed` should write: walk
/// `commits` in order, and for the FIRST commit that references a
/// finding-id which is (a) present in `known_finding_ids` (actually recorded
/// via `record-finding`) and (b) NOT already in `already_disposed`
/// (idempotent: a disposition already on the ledger — whether from a manual
/// `record-disposition` or an earlier `reconcile-fixed` run — always wins
/// and is never overwritten or duplicated), emit one CONFIRMED `Disposition`
/// whose `reviewer` records provenance as `auto-reconcile(commit <hash>)`.
/// A finding-id referenced by multiple commits within this same call is
/// disposed only once (first commit wins). Pure: no I/O, no clock (`now` is
/// caller-supplied).
pub fn compute_reconcile_dispositions(
    commits: &[CommitRef],
    known_finding_ids: &BTreeSet<String>,
    already_disposed: &BTreeSet<String>,
    now: i64,
) -> Vec<Disposition> {
    let mut resolved_this_run: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for commit in commits {
        for fid in extract_finding_ids(&commit.message) {
            if !known_finding_ids.contains(&fid) {
                continue;
            }
            if already_disposed.contains(&fid) || !resolved_this_run.insert(fid.clone()) {
                continue;
            }
            out.push(Disposition::new(
                fid,
                DispositionVerdict::Confirmed,
                format!("auto-reconcile(commit {})", commit.hash),
                now,
            ));
        }
    }
    out
}

/// Which commits `reconcile-fixed` should scan.
#[derive(Debug, Clone)]
pub enum ReconcileRange {
    /// `<ref>..HEAD` — every commit since `ref` (exclusive).
    SinceRef(String),
    /// A raw git revision range/expression, passed through to `git log` as-is.
    Range(String),
    /// The most recent `n` commits on the current branch.
    LastN(usize),
}

/// Run `git log` over `range` and parse it into `CommitRef`s. Fail-soft: ANY
/// failure (git not installed, cwd not a repo, non-zero exit, non-utf8
/// output) yields an empty vec rather than an `Err` — a git-level failure
/// means "0 commits reconciled", never a panic or a propagated error, so
/// `reconcile-fixed` always exits 0.
fn git_log_commits(cwd: &Path, range: &ReconcileRange) -> Vec<CommitRef> {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .arg("log")
        .arg("--pretty=format:%H%x1f%B%x1e");
    match range {
        ReconcileRange::SinceRef(r) => {
            cmd.arg(format!("{r}..HEAD"));
        }
        ReconcileRange::Range(r) => {
            cmd.arg(r);
        }
        ReconcileRange::LastN(n) => {
            cmd.arg(format!("-n{n}"));
        }
    }
    let Ok(out) = cmd.output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let Ok(text) = String::from_utf8(out.stdout) else {
        return Vec::new();
    };
    parse_git_log(&text)
}

/// Parse `git log --pretty=format:%H%x1f%B%x1e` output (`\x1f`-separated
/// hash/message, `\x1e`-terminated records — chosen so commit message
/// content can never be confused with the delimiters) into `CommitRef`s.
/// Pure and total: malformed/empty records are skipped rather than
/// panicking.
fn parse_git_log(text: &str) -> Vec<CommitRef> {
    text.split('\u{1e}')
        .filter_map(|rec| {
            let rec = rec.trim_matches('\n');
            if rec.is_empty() {
                return None;
            }
            let mut parts = rec.splitn(2, '\u{1f}');
            let hash = parts.next()?.to_string();
            let message = parts.next().unwrap_or("").to_string();
            if hash.is_empty() {
                return None;
            }
            Some(CommitRef { hash, message })
        })
        .collect()
}

/// Read the two ledgers the reconcile decision joins, tri-state.
///
/// Returns `None` when EITHER could not be read in full (each is announced on
/// stderr and named in `undetermined`), because the join is meaningless without
/// both — see the module note above for what each collapse would do.
fn reconcile_inputs(
    cwd: &Path,
    undetermined: &mut Vec<&'static str>,
) -> Option<(BTreeSet<String>, BTreeSet<String>)> {
    let known_finding_ids = match store::scan_review_findings_all(cwd) {
        Ok(Determination::Known(rows)) => Some(
            rows.into_iter()
                .map(|f| f.finding_id)
                .collect::<BTreeSet<String>>(),
        ),
        Ok(Determination::Undetermined(why)) => {
            eprintln!(
                "overwatch reconcile-fixed: WARNING — the review-findings history (hot store \
                 plus archive) could not be read or held an undecodable line ({why}); NO \
                 finding was reconciled. This is NOT a report that nothing needed \
                 reconciling."
            );
            undetermined.push("review_findings.jsonl");
            None
        }
        Err(e) => {
            eprintln!(
                "overwatch reconcile-fixed: WARNING — the review-findings history could not \
                 be located ({e}); NO finding was reconciled."
            );
            undetermined.push("review_findings.jsonl");
            None
        }
    };
    let already_disposed = match store::scan_dispositions(cwd) {
        Ok(Determination::Known(rows)) => Some(
            rows.into_iter()
                .map(|d| d.finding_id)
                .collect::<BTreeSet<String>>(),
        ),
        Ok(Determination::Undetermined(why)) => {
            eprintln!(
                "overwatch reconcile-fixed: WARNING — the disposition ledger could not be \
                 read or held an undecodable line ({why}); NO finding was reconciled. \
                 Proceeding would re-dispose findings that are already closed."
            );
            undetermined.push("dispositions.jsonl");
            None
        }
        Err(e) => {
            eprintln!(
                "overwatch reconcile-fixed: WARNING — the disposition ledger could not be \
                 located ({e}); NO finding was reconciled."
            );
            undetermined.push("dispositions.jsonl");
            None
        }
    };
    match (known_finding_ids, already_disposed) {
        (Some(known), Some(disposed)) => Some((known, disposed)),
        _ => None,
    }
}

/// Count findings that already have a landed fix commit (referenced within
/// `range`) but no disposition yet — the `review-metrics` early-warning
/// signal for the same "fix landed, nobody ran record-disposition" gap
/// `reconcile-fixed` closes (recurs every round `reconcile-fixed` isn't run
/// over, e.g. because it scans too short a range). Pure recomputation of
/// `compute_reconcile_dispositions`'s output size — never writes.
///
/// Tri-state: an unreadable ledger is `Undetermined`, NOT `Known(0)`. A zero
/// here is rendered as "no stale finding" (the warning line is suppressed), so
/// collapsing the two would turn a broken store into an all-clear.
pub fn stale_undisposed_count(cwd: &Path, range: ReconcileRange) -> Determination<usize> {
    let commits = git_log_commits(cwd, &range);
    let mut undetermined: Vec<&'static str> = Vec::new();
    match reconcile_inputs(cwd, &mut undetermined) {
        Some((known_finding_ids, already_disposed)) => Determination::known(
            compute_reconcile_dispositions(&commits, &known_finding_ids, &already_disposed, 0)
                .len(),
        ),
        None => Determination::undetermined(format!(
            "the stale-undisposed count joins ledgers that could not be read: {}",
            undetermined.join(", ")
        )),
    }
}

/// CLI entry point for `overwatch reconcile-fixed`.
///
/// Fail-soft on GIT: a git failure degrades to "0 commits scanned", which the
/// output states as the count it is. NOT fail-soft on the STORE: an unreadable
/// findings or disposition ledger writes nothing, is announced, and returns
/// [`SourceHealth::SomeUndetermined`] (exit 3) rather than reporting
/// "0 finding(s) reconciled".
pub fn run(range: ReconcileRange, dry_run: bool, json: bool) -> Result<SourceHealth> {
    let cwd = std::env::current_dir()?;
    let now = store::now();

    let commits = git_log_commits(&cwd, &range);
    let mut undetermined: Vec<&'static str> = Vec::new();
    let (known_finding_ids, already_disposed) = match reconcile_inputs(&cwd, &mut undetermined) {
        Some(inputs) => inputs,
        None => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "commits_scanned": commits.len(),
                        // Not "reconciled: []" alone: with a non-empty
                        // `undetermined_sources` the empty list means "could
                        // not tell", not "nothing to do".
                        "reconciled": Vec::<String>::new(),
                        "dry_run": dry_run,
                        "undetermined_sources": undetermined,
                    }))?
                );
            } else {
                println!(
                    "reconcile-fixed: scanned {} commit(s), NOTHING was reconciled — {} could \
                     not be read (this is NOT a report that no finding needed reconciling)",
                    commits.len(),
                    undetermined.join(", ")
                );
            }
            return Ok(SourceHealth::SomeUndetermined);
        }
    };

    let new_dispositions =
        compute_reconcile_dispositions(&commits, &known_finding_ids, &already_disposed, now);

    if !dry_run {
        for d in &new_dispositions {
            // Fail-soft: a single store-write failure must not abort the
            // batch or the command (matches `disposition_cli::record`).
            if let Err(e) = store::append_disposition(&cwd, d) {
                eprintln!(
                    "overwatch: WARNING could not record auto-reconcile disposition for {}: {e}",
                    d.finding_id
                );
            }
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "commits_scanned": commits.len(),
                "reconciled": new_dispositions.iter().map(|d| d.finding_id.clone()).collect::<Vec<_>>(),
                "dry_run": dry_run,
                // Always present so the key's absence never has to be
                // interpreted; empty here means both ledgers were read.
                "undetermined_sources": Vec::<String>::new(),
            }))?
        );
        return Ok(SourceHealth::AllRead);
    }

    if new_dispositions.is_empty() {
        println!(
            "reconcile-fixed: scanned {} commit(s), 0 finding(s) reconciled",
            commits.len()
        );
    } else {
        let verb = if dry_run {
            "would reconcile"
        } else {
            "reconciled"
        };
        println!(
            "reconcile-fixed: scanned {} commit(s), {verb} {} finding(s):",
            commits.len(),
            new_dispositions.len()
        );
        for d in &new_dispositions {
            println!("  {} -> confirmed ({})", d.finding_id, d.reviewer);
        }
    }
    Ok(SourceHealth::AllRead)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_finding_ids_finds_ca_ids_in_commit_message() {
        let msg = "fix(overwatch): resolve CA-overwatch-004 leak\n\nAlso touches CA-blastguard-01.";
        assert_eq!(
            extract_finding_ids(msg),
            vec![
                "CA-overwatch-004".to_string(),
                "CA-blastguard-01".to_string()
            ]
        );
    }

    #[test]
    fn extract_finding_ids_dedupes_repeated_ids() {
        let msg = "CA-overwatch-004 fixed. See also CA-overwatch-004 in the test.";
        assert_eq!(
            extract_finding_ids(msg),
            vec!["CA-overwatch-004".to_string()]
        );
    }

    #[test]
    fn extract_finding_ids_empty_when_no_match() {
        assert!(extract_finding_ids("just a normal commit message").is_empty());
    }

    #[test]
    fn compute_reconcile_dispositions_confirms_known_undisposed_finding() {
        let commits = vec![CommitRef {
            hash: "abc123".to_string(),
            message: "fix: resolve CA-overwatch-001".to_string(),
        }];
        let known: BTreeSet<String> = ["CA-overwatch-001".to_string()].into_iter().collect();
        let disposed = BTreeSet::new();
        let out = compute_reconcile_dispositions(&commits, &known, &disposed, 1000);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].finding_id, "CA-overwatch-001");
        assert_eq!(out[0].verdict, DispositionVerdict::Confirmed);
        assert_eq!(out[0].reviewer, "auto-reconcile(commit abc123)");
        assert_eq!(out[0].resolved_ts, 1000);
    }

    #[test]
    fn compute_reconcile_dispositions_skips_unknown_finding_id() {
        let commits = vec![CommitRef {
            hash: "abc123".to_string(),
            message: "fix: resolve CA-overwatch-999".to_string(),
        }];
        let known: BTreeSet<String> = BTreeSet::new();
        let disposed = BTreeSet::new();
        assert!(compute_reconcile_dispositions(&commits, &known, &disposed, 1000).is_empty());
    }

    #[test]
    fn compute_reconcile_dispositions_skips_already_disposed_finding() {
        let commits = vec![CommitRef {
            hash: "abc123".to_string(),
            message: "fix: resolve CA-overwatch-001".to_string(),
        }];
        let known: BTreeSet<String> = ["CA-overwatch-001".to_string()].into_iter().collect();
        let disposed: BTreeSet<String> = ["CA-overwatch-001".to_string()].into_iter().collect();
        assert!(compute_reconcile_dispositions(&commits, &known, &disposed, 1000).is_empty());
    }

    #[test]
    fn compute_reconcile_dispositions_is_idempotent_across_multiple_referencing_commits() {
        let commits = vec![
            CommitRef {
                hash: "aaa".to_string(),
                message: "wip: touch CA-overwatch-001".to_string(),
            },
            CommitRef {
                hash: "bbb".to_string(),
                message: "fix: actually resolve CA-overwatch-001".to_string(),
            },
        ];
        let known: BTreeSet<String> = ["CA-overwatch-001".to_string()].into_iter().collect();
        let disposed = BTreeSet::new();
        let out = compute_reconcile_dispositions(&commits, &known, &disposed, 1000);
        assert_eq!(
            out.len(),
            1,
            "the same finding-id must be disposed only once"
        );
        assert_eq!(out[0].reviewer, "auto-reconcile(commit aaa)");
    }

    #[test]
    fn parse_git_log_round_trips_hash_and_multiline_message() {
        let raw = "deadbeef\u{1f}fix: resolve CA-x-1\n\nbody line\u{1e}\ncafefeed\u{1f}chore: unrelated\u{1e}";
        let commits = parse_git_log(raw);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "deadbeef");
        assert!(commits[0].message.contains("CA-x-1"));
        assert_eq!(commits[1].hash, "cafefeed");
    }
}
