//! Canary diff: compare two `evalkit run --json` result sets — a *baseline*
//! (e.g. SKILL.md before an edit) against a *current* run — and surface what
//! moved. It is the pure diff layer behind the promptfoo-style side-by-side:
//! replay the same golden dataset across two prompt versions and classify every
//! case as a regression, fix, added, dropped, or unchanged.
//!
//! The classification core (`diff`) is pure over two case-maps so it is fully
//! unit-testable without touching the filesystem — mirroring how `run.rs` keeps
//! `check_assert` pure.
//!
//! Exit-code policy (fail-closed by default):
//! - `1` when a golden's verdict was lost: a regression (pass→fail), a dropped
//!   golden (in baseline, gone from current), or a demotion (pass→draft or
//!   fail→draft — turning a failing case into a draft hides the failure). This
//!   is the default; `--report-only` is the explicit opt-in that turns it into
//!   exit `0`.
//! - `2` harness error OR "cannot determine": an unreadable / unparseable
//!   report, or a side with no verdict-carrying case (empty or all-draft). That
//!   is never `0`, and `--report-only` does not soften it — "nothing was
//!   checked" is not "nothing regressed".
//! - `0` otherwise.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::json;

/// One case's outcome distilled from an `evalkit run --json` `cases[]` entry.
/// A `skipped` case (a draft) is neither a pass nor a fail; `pass` is then false.
#[derive(Clone, Copy)]
pub struct CaseResult {
    pub pass: bool,
    pub skipped: bool,
}

/// The result of diffing a baseline against a current run. Case labels are kept
/// per bucket (sorted) so the report can name exactly what changed; `unchanged`
/// is only a count since listing every steady case adds noise.
pub struct Diff {
    pub baseline_pass_rate: f64,
    pub current_pass_rate: f64,
    /// baseline.pass && current.fail (neither skipped) — the gating bucket.
    pub regressions: Vec<String>,
    /// baseline.fail && current.pass (neither skipped).
    pub fixes: Vec<String>,
    /// Present only in the current run.
    pub added: Vec<String>,
    /// Present only in the baseline run. Gates: a golden that vanished is a
    /// lost check, not a neutral change.
    pub dropped: Vec<String>,
    /// !baseline.skipped && current.skipped — a passing OR failing golden turned
    /// into a draft. Gates: demoting a case to draft hides it (and, for a
    /// failing case, its failure) from the run exactly like deleting it.
    pub demoted: Vec<String>,
    /// Count of cases present on both sides with no gating or fix transition
    /// (pass→pass, fail→fail, skip→pass, skip→fail, skip→skip).
    pub unchanged: usize,
    /// Cases on each side that carry a verdict (not skipped). Zero on either
    /// side means the comparison cannot be determined.
    pub baseline_verdicts: usize,
    pub current_verdicts: usize,
}

impl Diff {
    pub fn delta(&self) -> f64 {
        self.current_pass_rate - self.baseline_pass_rate
    }

    /// True when a golden's verdict was lost: regression, dropped, or demoted.
    pub fn lost_coverage(&self) -> bool {
        !self.regressions.is_empty() || !self.dropped.is_empty() || !self.demoted.is_empty()
    }

    /// True when either side has no verdict-carrying case (empty or all-draft):
    /// the diff compared nothing, so it cannot vouch for anything.
    pub fn undetermined(&self) -> bool {
        self.baseline_verdicts == 0 || self.current_verdicts == 0
    }
}

/// Pass-rate over a case-map: passed / (total - skipped). A skipped draft is
/// excluded from the denominator; an all-skipped (or empty) map → 0.0 (no
/// divide-by-zero).
fn pass_rate(cases: &BTreeMap<String, CaseResult>) -> f64 {
    let skipped = cases.values().filter(|c| c.skipped).count();
    let denom = cases.len() - skipped;
    if denom == 0 {
        return 0.0;
    }
    let passed = cases.values().filter(|c| c.pass).count();
    passed as f64 / denom as f64
}

/// Pure classification core: diff two case-maps keyed by `case` label.
///
/// A `regression` is *strictly* `baseline.pass == true && current.pass == false`
/// with neither side skipped. A `fix` is the mirror (baseline fail → current
/// pass). A verdict-carrying baseline case (pass or fail) that is skipped in
/// current is a demotion, its own bucket: a draft carries no verdict, so it is
/// not a pass→fail regression, but it gates like one (see `execute`). Skip→X
/// transitions (skip→pass, skip→fail, skip→skip) fall through to `unchanged`.
pub fn diff(
    baseline: &BTreeMap<String, CaseResult>,
    current: &BTreeMap<String, CaseResult>,
) -> Diff {
    let mut regressions = Vec::new();
    let mut fixes = Vec::new();
    let mut added = Vec::new();
    let mut dropped = Vec::new();
    let mut demoted = Vec::new();
    let mut unchanged = 0usize;

    // Union of labels; BTreeMap iteration is sorted, so each bucket stays sorted.
    let mut labels: Vec<&String> = baseline.keys().chain(current.keys()).collect();
    labels.sort();
    labels.dedup();

    for label in labels {
        match (baseline.get(label), current.get(label)) {
            (Some(b), Some(c)) => {
                // A real pass/fail flip requires both sides to carry a verdict.
                let b_fail = !b.pass && !b.skipped;
                let c_fail = !c.pass && !c.skipped;
                if b.pass && c_fail {
                    regressions.push(label.clone());
                } else if !b.skipped && c.skipped {
                    demoted.push(label.clone());
                } else if b_fail && c.pass {
                    fixes.push(label.clone());
                } else {
                    unchanged += 1;
                }
            }
            (None, Some(_)) => added.push(label.clone()),
            (Some(_), None) => dropped.push(label.clone()),
            (None, None) => unreachable!("label came from one of the two maps"),
        }
    }

    Diff {
        baseline_pass_rate: pass_rate(baseline),
        current_pass_rate: pass_rate(current),
        regressions,
        fixes,
        added,
        dropped,
        demoted,
        unchanged,
        baseline_verdicts: verdicts(baseline),
        current_verdicts: verdicts(current),
    }
}

/// Number of cases that carry a pass/fail verdict (not skipped drafts).
fn verdicts(cases: &BTreeMap<String, CaseResult>) -> usize {
    cases.values().filter(|c| !c.skipped).count()
}

/// Parse one `evalkit run --json` report into a case-map keyed by `case` label.
/// Missing `pass`/`skipped` default to false so a partial report still diffs.
fn parse_report(path: &Path) -> Result<BTreeMap<String, CaseResult>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let cases = value
        .get("cases")
        .and_then(|c| c.as_array())
        .with_context(|| {
            format!(
                "{}: no `cases` array (not an `evalkit run --json` output?)",
                path.display()
            )
        })?;
    let mut map = BTreeMap::new();
    for c in cases {
        let label = c.get("case").and_then(|l| l.as_str()).with_context(|| {
            format!(
                "{}: a case entry is missing its `case` label",
                path.display()
            )
        })?;
        let pass = c.get("pass").and_then(|p| p.as_bool()).unwrap_or(false);
        let skipped = c.get("skipped").and_then(|s| s.as_bool()).unwrap_or(false);
        map.insert(label.to_string(), CaseResult { pass, skipped });
    }
    Ok(map)
}

/// Read+parse the two reports, diff them, print a human report or `--json`
/// summary, and return the exit code: `2` on any harness error (unreadable /
/// unparseable file) or when either side has no verdict-carrying case (empty or
/// all-draft), regardless of `report_only`; else `1` when a golden's verdict
/// was lost (regression / dropped / demoted) unless `report_only`; else `0`.
pub fn execute(baseline: PathBuf, current: PathBuf, json_out: bool, report_only: bool) -> i32 {
    let (base_map, cur_map) = match (parse_report(&baseline), parse_report(&current)) {
        (Ok(b), Ok(c)) => (b, c),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("evalkit: {e:#}");
            return 2;
        }
    };
    let d = diff(&base_map, &cur_map);

    if json_out {
        report_json(&d);
    } else {
        report_human(&d);
    }

    if d.undetermined() {
        eprintln!(
            "evalkit: canary cannot determine: no verdict-carrying cases (baseline {}, current {}); an empty or all-draft run checks nothing",
            d.baseline_verdicts, d.current_verdicts
        );
        return 2;
    }
    if d.lost_coverage() && !report_only {
        1
    } else {
        0
    }
}

fn report_human(d: &Diff) {
    let before = d.baseline_pass_rate * 100.0;
    let after = d.current_pass_rate * 100.0;
    let delta = d.delta() * 100.0;
    println!("evalkit canary: pass-rate {before:.1}% → {after:.1}% ({delta:+.1} pts)\n");

    if d.regressions.is_empty() {
        println!("  no regressions");
    } else {
        println!("  !! {} REGRESSION(S) (pass → fail):", d.regressions.len());
        for label in &d.regressions {
            println!("     !! {label}");
        }
    }
    print_bucket("fixes (fail → pass)", &d.fixes);
    print_bucket("added (new in current)", &d.added);
    print_bucket("dropped (gone from current)", &d.dropped);
    print_bucket("demoted (pass/fail → draft)", &d.demoted);
    println!("\n  {} unchanged", d.unchanged);
}

fn print_bucket(title: &str, labels: &[String]) {
    if labels.is_empty() {
        return;
    }
    println!("  {} {}:", labels.len(), title);
    for label in labels {
        println!("     - {label}");
    }
}

fn report_json(d: &Diff) {
    println!(
        "{}",
        json!({
            "baseline_pass_rate": d.baseline_pass_rate,
            "current_pass_rate": d.current_pass_rate,
            "delta": d.delta(),
            "regressions": d.regressions,
            "fixes": d.fixes,
            "added": d.added,
            "dropped": d.dropped,
            "demoted": d.demoted,
        })
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, bool, bool)]) -> BTreeMap<String, CaseResult> {
        entries
            .iter()
            .map(|(label, pass, skipped)| {
                (
                    label.to_string(),
                    CaseResult {
                        pass: *pass,
                        skipped: *skipped,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn pass_to_fail_is_a_regression() {
        let b = map(&[("a", true, false)]);
        let c = map(&[("a", false, false)]);
        let d = diff(&b, &c);
        assert_eq!(d.regressions, vec!["a".to_string()]);
        assert!(d.fixes.is_empty());
    }

    #[test]
    fn fail_to_pass_is_a_fix_and_does_not_gate() {
        let b = map(&[("a", false, false)]);
        let c = map(&[("a", true, false)]);
        let d = diff(&b, &c);
        assert_eq!(d.fixes, vec!["a".to_string()]);
        assert!(d.regressions.is_empty());
        // A fix alone never drives exit 1: it loses no passing golden.
        assert!(!d.lost_coverage());
    }

    #[test]
    fn added_and_dropped_detected() {
        let b = map(&[("only_base", true, false)]);
        let c = map(&[("only_cur", true, false)]);
        let d = diff(&b, &c);
        assert_eq!(d.added, vec!["only_cur".to_string()]);
        assert_eq!(d.dropped, vec!["only_base".to_string()]);
        assert!(d.regressions.is_empty());
    }

    #[test]
    fn skipped_transitions_are_not_regressions() {
        // Neither pass → skip nor skip → fail is a pass/fail verdict flip, so
        // neither is a regression. pass → skip is a demotion (its own gating
        // bucket); skip → fail stays unchanged.
        let b = map(&[("a", true, false), ("b", false, true)]);
        let c = map(&[("a", false, true), ("b", false, false)]);
        let d = diff(&b, &c);
        assert!(d.regressions.is_empty(), "{:?}", d.regressions);
        assert!(d.fixes.is_empty(), "{:?}", d.fixes);
        assert_eq!(d.demoted, vec!["a".to_string()]);
        assert!(d.lost_coverage());
        assert_eq!(d.unchanged, 1);
    }

    #[test]
    fn dropped_and_demoted_count_as_lost_coverage() {
        let b = map(&[("keep", true, false), ("gone", true, false)]);
        let c = map(&[("keep", true, false)]);
        let d = diff(&b, &c);
        assert_eq!(d.dropped, vec!["gone".to_string()]);
        assert!(d.lost_coverage());

        // A failing case turned draft hides its failure: demoted, gates.
        let b = map(&[("keep", true, false), ("f", false, false)]);
        let c = map(&[("keep", true, false), ("f", false, true)]);
        let d = diff(&b, &c);
        assert_eq!(d.demoted, vec!["f".to_string()]);
        assert!(d.lost_coverage());

        // A draft turned passing or failing loses nothing.
        let b = map(&[
            ("keep", true, false),
            ("s", false, true),
            ("t", false, true),
        ]);
        let c = map(&[
            ("keep", true, false),
            ("s", true, false),
            ("t", false, false),
        ]);
        let d = diff(&b, &c);
        assert!(!d.lost_coverage());
        assert_eq!(d.unchanged, 3);
    }

    #[test]
    fn all_draft_or_empty_side_is_undetermined() {
        let pass = map(&[("a", true, false)]);
        let draft = map(&[("a", false, true)]);
        let empty: BTreeMap<String, CaseResult> = BTreeMap::new();
        assert!(diff(&pass, &draft).undetermined());
        assert!(diff(&draft, &pass).undetermined());
        assert!(diff(&pass, &empty).undetermined());
        assert!(!diff(&pass, &pass).undetermined());
    }

    #[test]
    fn pass_rate_math_and_divide_by_zero() {
        // 2 of 3 non-skipped pass → 2/3; one skipped excluded from denominator.
        let m = map(&[
            ("a", true, false),
            ("b", true, false),
            ("c", false, false),
            ("d", false, true),
        ]);
        assert!((pass_rate(&m) - 2.0 / 3.0).abs() < 1e-9);

        // All skipped → denominator 0 → 0.0, no panic.
        let all_skip = map(&[("a", false, true)]);
        assert_eq!(pass_rate(&all_skip), 0.0);

        // Empty map → 0.0.
        let empty: BTreeMap<String, CaseResult> = BTreeMap::new();
        assert_eq!(pass_rate(&empty), 0.0);
    }

    #[test]
    fn delta_is_current_minus_baseline() {
        let b = map(&[("a", true, false), ("x", false, false)]); // 0.5
        let c = map(&[("a", true, false), ("x", true, false)]); // 1.0
        let d = diff(&b, &c);
        assert!((d.delta() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn default_gates_report_only_exit_policy() {
        // Build two tiny reports on disk and exercise the real exit-code policy.
        let dir = std::env::temp_dir().join(format!("evalkit-canary-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("base.json");
        let cur = dir.join("cur.json");
        std::fs::write(
            &base,
            r#"{"total":1,"passed":1,"failed":0,"skipped":0,"cases":[{"case":"a","pass":true,"skipped":false,"failures":[]}]}"#,
        )
        .unwrap();
        std::fs::write(
            &cur,
            r#"{"total":1,"passed":0,"failed":1,"skipped":0,"cases":[{"case":"a","pass":false,"skipped":false,"failures":["boom"]}]}"#,
        )
        .unwrap();

        // A regression exists: by default it gates → exit 1 (json and human).
        assert_eq!(execute(base.clone(), cur.clone(), true, false), 1);
        assert_eq!(execute(base.clone(), cur.clone(), false, false), 1);
        // --report-only is the explicit opt-in that makes it informational.
        assert_eq!(execute(base.clone(), cur.clone(), false, true), 0);
        // Diffing a report against itself: no regression → exit 0 by default.
        assert_eq!(execute(base.clone(), base.clone(), false, false), 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unreadable_file_is_harness_error_exit_2() {
        let missing = PathBuf::from("/no/such/evalkit/report.json");
        assert_eq!(execute(missing.clone(), missing, false, true), 2);
    }
}
