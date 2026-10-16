//! Contract tests for the fail-closed `evalkit canary` / `evalkit run` gate
//! (user ruling): canary gates by default (`--report-only` is the opt-out),
//! dropped goldens and pass→draft demotions gate, all-draft / empty runs are
//! "cannot determine" and resolve to exit 2, and `--fail-on-regression` is gone.
//!
//! Written independently of the implementation; drives the real binary.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn fresh_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!(
        "evalkit-canary-gate-{}-{tag}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    fn dump(&self) -> String {
        format!(
            "exit={}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.code, self.stdout, self.stderr
        )
    }
}

fn ev_in(args: &[&str], cwd: &Path) -> Out {
    let o = Command::new(env!("CARGO_BIN_EXE_evalkit"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// One case entry in an `evalkit run --json` report.
fn case(label: &str, pass: bool, skipped: bool) -> String {
    let failures = if pass || skipped { "[]" } else { r#"["boom"]"# };
    format!(r#"{{"case":"{label}","pass":{pass},"skipped":{skipped},"failures":{failures}}}"#)
}

/// A full report with consistent totals.
fn report(cases: &[(&str, bool, bool)]) -> String {
    let total = cases.len();
    let skipped = cases.iter().filter(|c| c.2).count();
    let passed = cases.iter().filter(|c| c.1 && !c.2).count();
    let failed = total - skipped - passed;
    let body: Vec<String> = cases.iter().map(|(l, p, sk)| case(l, *p, *sk)).collect();
    format!(
        r#"{{"total":{total},"passed":{passed},"failed":{failed},"skipped":{skipped},"cases":[{}]}}"#,
        body.join(",")
    )
}

/// Write baseline/current reports into a fresh dir and run `canary` with `extra`.
fn canary(baseline: &str, current: &str, extra: &[&str]) -> Out {
    let d = fresh_dir("canary");
    let b = d.join("base.json");
    let c = d.join("cur.json");
    std::fs::write(&b, baseline).unwrap();
    std::fs::write(&c, current).unwrap();
    let mut args = vec!["canary", "--baseline", s(&b), "--current", s(&c)];
    args.extend_from_slice(extra);
    let o = ev_in(&args, &d);
    std::fs::remove_dir_all(&d).ok();
    o
}

fn regression_pair() -> (String, String) {
    (
        report(&[("a", true, false), ("b", true, false)]),
        report(&[("a", false, false), ("b", true, false)]),
    )
}

#[test]
fn canary_default_exits_1_on_regression() {
    let (b, c) = regression_pair();
    let o = canary(&b, &c, &[]);
    assert_eq!(
        o.code,
        1,
        "pass→fail regression must gate by default\n{}",
        o.dump()
    );
}

#[test]
fn canary_default_exits_1_on_regression_from_real_run_reports() {
    // Same contract, but the reports come from the real `run --json` output.
    let r = fresh_dir("runroot");
    std::fs::create_dir_all(r.join("evals")).unwrap();
    let good = r#"{"id":"g","cmd":["printf","%s","HELLO"],"assert":{"contains":["HELLO"]}}"#;
    let keep = r#"{"id":"k","cmd":["printf","%s","HELLO"],"assert":{"contains":["HELLO"]}}"#;
    std::fs::write(r.join("evals/x.jsonl"), format!("{good}\n{keep}\n")).unwrap();
    let base = ev_in(&["run", "--root", s(&r), "--json"], &r);
    assert_eq!(base.code, 0, "baseline run must pass\n{}", base.dump());
    let bad = r#"{"id":"g","cmd":["printf","%s","HELLO"],"assert":{"contains":["NOPE"]}}"#;
    std::fs::write(r.join("evals/x.jsonl"), format!("{bad}\n{keep}\n")).unwrap();
    let cur = ev_in(&["run", "--root", s(&r), "--json"], &r);
    assert_eq!(cur.code, 1, "current run must fail\n{}", cur.dump());
    std::fs::remove_dir_all(&r).ok();

    let o = canary(&base.stdout, &cur.stdout, &[]);
    assert_eq!(
        o.code,
        1,
        "pass→fail regression (real run reports) must gate by default\n{}",
        o.dump()
    );
}

#[test]
fn canary_report_only_exits_0_on_regression() {
    let (b, c) = regression_pair();
    let o = canary(&b, &c, &["--report-only"]);
    assert_eq!(
        o.code,
        0,
        "--report-only must make the regression informational\n{}",
        o.dump()
    );
}

#[test]
fn canary_dropped_golden_exits_1() {
    let b = report(&[("a", true, false), ("gone", true, false)]);
    let c = report(&[("a", true, false)]);
    let o = canary(&b, &c, &[]);
    assert_eq!(
        o.code,
        1,
        "a passing golden dropped from current must gate by default\n{}",
        o.dump()
    );
}

#[test]
fn canary_pass_to_draft_exits_1_and_reports_demoted() {
    // `keep` stays a passing non-draft case on both sides, so current is not all-draft.
    let b = report(&[("keep", true, false), ("demo", true, false)]);
    let c = report(&[("keep", true, false), ("demo", false, true)]);

    let o = canary(&b, &c, &[]);
    assert_eq!(
        o.code,
        1,
        "pass→draft demotion must gate by default\n{}",
        o.dump()
    );

    let j = canary(&b, &c, &["--json"]);
    assert_eq!(
        j.code,
        1,
        "pass→draft demotion must gate with --json too\n{}",
        j.dump()
    );
    let v: Value = serde_json::from_str(j.stdout.trim())
        .unwrap_or_else(|e| panic!("--json stdout is not JSON ({e})\n{}", j.dump()));
    let demoted = v
        .get("demoted")
        .and_then(|d| d.as_array())
        .unwrap_or_else(|| panic!("--json output lacks a `demoted` array\n{}", j.dump()));
    assert!(
        demoted.iter().any(|x| x.as_str() == Some("demo")),
        "`demoted` must name the pass→draft case `demo`\n{}",
        j.dump()
    );
    assert!(
        !demoted.iter().any(|x| x.as_str() == Some("keep")),
        "`demoted` must not name the steady case `keep`\n{}",
        j.dump()
    );
}

#[test]
fn canary_all_draft_current_exits_2() {
    let b = report(&[("a", true, false), ("b", true, false)]);
    let c = report(&[("a", false, true), ("b", false, true)]);
    let o = canary(&b, &c, &[]);
    assert_eq!(
        o.code,
        2,
        "all-draft current is undeterminable → exit 2\n{}",
        o.dump()
    );
    let o = canary(&b, &c, &["--report-only"]);
    assert_eq!(
        o.code,
        2,
        "all-draft current must exit 2 even with --report-only\n{}",
        o.dump()
    );
}

#[test]
fn canary_empty_current_exits_2() {
    let b = report(&[("a", true, false)]);
    let c = r#"{"total":0,"passed":0,"failed":0,"skipped":0,"cases":[]}"#;
    let o = canary(&b, c, &[]);
    assert_eq!(
        o.code,
        2,
        "empty current (`cases: []`) is undeterminable → exit 2\n{}",
        o.dump()
    );
}

#[test]
fn run_all_draft_exits_2() {
    let r = fresh_dir("draftroot");
    std::fs::create_dir_all(r.join("evals")).unwrap();
    std::fs::write(r.join("evals/x.jsonl"), "{\"id\":\"d\",\"draft\":true}\n").unwrap();
    let o = ev_in(&["run", "--root", s(&r)], &r);
    std::fs::remove_dir_all(&r).ok();
    assert_eq!(
        o.code,
        2,
        "a run with only draft cases has nothing runnable → exit 2\n{}",
        o.dump()
    );
    assert!(
        o.stderr.contains("no runnable cases"),
        "stderr must say `no runnable cases`\n{}",
        o.dump()
    );
}

#[test]
fn canary_clean_diff_exits_0() {
    let b = report(&[("a", true, false), ("b", true, false)]);
    let o = canary(&b, &b, &[]);
    assert_eq!(
        o.code,
        0,
        "identical all-passing reports must not gate\n{}",
        o.dump()
    );
}

#[test]
fn canary_rejects_removed_fail_on_regression_flag() {
    let (b, c) = regression_pair();
    let o = canary(&b, &c, &["--fail-on-regression"]);
    assert_eq!(
        o.code,
        2,
        "--fail-on-regression is removed; clap must reject it (exit 2)\n{}",
        o.dump()
    );
    assert!(
        o.stderr.contains("--fail-on-regression"),
        "stderr must name the rejected flag\n{}",
        o.dump()
    );
}

fn fail_to_draft_pair() -> (String, String) {
    // `keep` stays a passing non-draft case on both sides, so current is not all-draft.
    (
        report(&[("keep", true, false), ("f", false, false)]),
        report(&[("keep", true, false), ("f", false, true)]),
    )
}

#[test]
fn canary_fail_to_draft_exits_1_and_reports_demoted() {
    let (b, c) = fail_to_draft_pair();

    // --json first, so the demoted-array contract is observed even if the exit
    // contract would also fail.
    let j = canary(&b, &c, &["--json"]);
    let v: Value = serde_json::from_str(j.stdout.trim())
        .unwrap_or_else(|e| panic!("--json stdout is not JSON ({e})\n{}", j.dump()));
    let demoted = v
        .get("demoted")
        .and_then(|d| d.as_array())
        .unwrap_or_else(|| panic!("--json output lacks a `demoted` array\n{}", j.dump()));
    assert!(
        demoted.iter().any(|x| x.as_str() == Some("f")),
        "`demoted` must name the fail→draft case `f`\n{}",
        j.dump()
    );
    assert!(
        !demoted.iter().any(|x| x.as_str() == Some("keep")),
        "`demoted` must not name the steady case `keep`\n{}",
        j.dump()
    );

    let o = canary(&b, &c, &[]);
    assert_eq!(
        o.code,
        1,
        "fail→draft demotion must gate by default\n{}",
        o.dump()
    );
}

#[test]
fn canary_fail_to_draft_report_only_exits_0() {
    let (b, c) = fail_to_draft_pair();
    let o = canary(&b, &c, &["--report-only"]);
    assert_eq!(
        o.code,
        0,
        "--report-only must make the fail→draft demotion informational\n{}",
        o.dump()
    );
}
