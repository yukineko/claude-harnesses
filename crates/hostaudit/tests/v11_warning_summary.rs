//! v1.1 canon §1: the `warning` verdict, exit code 0 with warnings only, the
//! separate warning count in every output, one finding per (host, rule) for
//! all new rules (CONTRACT.md §12.1, §12.2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;
use serde_json::Value;

const NODESOURCE: &str =
    "deb http://archive.ubuntu.com/ubuntu jammy main\ndeb https://deb.nodesource.com/node_20.x nodistro main\n";

fn warn_host() -> std::collections::BTreeMap<String, Resp> {
    let mut h = clean_host();
    set(&mut h, W18_SRC, ok(NODESOURCE));
    h
}

fn counts(fs: &[Value]) -> (usize, usize, usize, usize) {
    (
        count_verdict(fs, "violation"),
        count_verdict(fs, "undetermined"),
        count_verdict(fs, "warning"),
        count_verdict(fs, "clean"),
    )
}

#[test]
fn clean_v11_host_has_one_finding_per_rule_and_exits_zero() {
    let mut env = Env::new();
    let o = env.audit_v11(&clean_host());
    assert_eq!(o.code, 0, "{}", o.dbg());
    let fs = o.findings();
    for r in all_rules() {
        let f = finding(&fs, "web1", r);
        let v = f["verdict"].as_str().unwrap();
        assert!(
            v == "clean" || v == "not_applicable",
            "{r} must be clean/not_applicable on the clean host: {f}"
        );
    }
    // Linux host: every WN rule is NotApplicable (canon §7).
    for r in ["WN1", "WN2", "WN3", "WN4", "WN5"] {
        assert_eq!(verdict(&fs, "web1", r), "not_applicable", "{r}");
    }
    // Nothing observed for S8/S9 on this host -> NotApplicable (canon §4).
    assert_eq!(verdict(&fs, "web1", "S8"), "not_applicable");
    assert_eq!(verdict(&fs, "web1", "S9"), "not_applicable");
    // K6 is parked: it must produce no finding at all (canon §5).
    assert!(by_rule(&fs, "K6").is_empty(), "K6 emitted: {fs:?}");
    // A clean verdict is positive evidence, not a default: S1 really is clean here.
    assert_eq!(verdict(&fs, "web1", "S1"), "clean");
}

#[test]
fn warning_only_exits_zero_and_the_warning_is_not_silent() {
    let mut env = Env::new();
    let o = env.audit_v11(&warn_host());
    let fs = o.findings();
    let s5 = finding(&fs, "web1", "S5");
    assert_eq!(s5["verdict"], "warning", "{s5}");
    assert!(
        s5["fix"].as_str().is_some_and(|x| !x.trim().is_empty()),
        "a warning must carry advice: {s5}"
    );
    assert!(evidence_text(&s5).contains("deb.nodesource.com"), "{s5}");
    // Precondition that makes exit 0 meaningful: warning is the ONLY non-clean verdict.
    assert_eq!(
        count_verdict(&fs, "violation") + count_verdict(&fs, "undetermined"),
        0,
        "{fs:?}"
    );
    assert_eq!(o.code, 0, "warnings alone must exit 0: {}", o.dbg());
}

#[test]
fn text_output_shows_warning_count_separately_from_clean() {
    let mut env = Env::new();
    let j = env.audit_v11(&warn_host());
    let (v, u, w, c) = counts(&j.findings());
    assert_eq!((v, u, w), (0, 0, 1), "fixture sanity");
    let t = env.run(&["audit"]);
    assert_eq!(t.code, 0, "{}", t.dbg());
    let want = format!("violation {v} / undetermined {u} / warning {w} / clean {c}");
    assert!(
        t.stdout.contains(&want),
        "text output lacks the summary line `{want}`:\n{}",
        t.stdout
    );
}

#[test]
fn json_output_carries_a_summary_with_a_separate_warning_count() {
    let mut env = Env::new();
    let o = env.audit_v11(&warn_host());
    let (v, u, w, c) = counts(&o.findings());
    let s = &o.json()["summary"];
    assert_eq!(s["violation"], v, "{s}");
    assert_eq!(s["undetermined"], u, "{s}");
    assert_eq!(s["warning"], w, "{s}");
    assert_eq!(s["clean"], c, "{s}");
    assert_eq!(s["warning"], 1, "{s}");
}

#[test]
fn reports_jsonl_rows_use_the_warning_verdict() {
    let mut env = Env::new();
    env.audit_v11(&warn_host());
    let rows = env.reports();
    let s5: Vec<_> = rows
        .iter()
        .filter(|r| r["rule"] == "S5" && r["target"] == "web1")
        .collect();
    assert_eq!(s5.len(), 1, "{rows:?}");
    assert_eq!(s5[0]["verdict"], "warning", "{}", s5[0]);
    assert_eq!(s5[0]["time"], NOW, "{}", s5[0]);
    assert!(s5[0]["fix"].as_str().is_some_and(|x| !x.is_empty()));
}

#[test]
fn warning_plus_violation_exits_one() {
    let mut h = warn_host();
    set(
        &mut h,
        W4,
        ok("permitrootlogin yes\npasswordauthentication no\n"),
    );
    let mut env = Env::new();
    let o = env.audit_v11(&h);
    let fs = o.findings();
    assert_eq!(verdict(&fs, "web1", "S5"), "warning");
    assert_eq!(verdict(&fs, "web1", "B1"), "violation");
    assert_eq!(o.code, 1, "{}", o.dbg());
}

#[test]
fn warning_plus_undetermined_exits_one() {
    let mut h = warn_host();
    set(&mut h, W19, ssh_down());
    let mut env = Env::new();
    let o = env.audit_v11(&h);
    let fs = o.findings();
    assert_eq!(verdict(&fs, "web1", "S5"), "warning");
    assert_eq!(verdict(&fs, "web1", "S6"), "undetermined");
    assert_eq!(o.code, 1, "{}", o.dbg());
}

#[test]
fn every_new_rule_is_reported_even_when_ssh_is_down() {
    let mut env = Env::new();
    let o = env.audit_v11(&std::collections::BTreeMap::new()); // every command -> 255
    assert_eq!(o.code, 1, "{}", o.dbg());
    let fs = o.findings();
    for r in all_rules() {
        let f = finding(&fs, "web1", r);
        assert_eq!(
            f["verdict"], "undetermined",
            "{r}: unreachable host must be Undetermined for EVERY rule: {f}"
        );
        assert!(
            f["reason"].as_str().is_some_and(|s| !s.trim().is_empty()),
            "{r}: {f}"
        );
    }
}
