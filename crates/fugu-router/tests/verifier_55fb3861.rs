//! Independent verifier e2e for backlog 55fb3861 (written by the verifier, not
//! the worker). Drives the REAL `fugu-router` binary against an isolated temp
//! HOME and asserts on the on-disk store and on `stats --json` / `suggest`.
//!
//! Contract under test (task 55fb3861 done_criteria 2 and 3):
//! - `record` without `--cost` stores an UNMEASURED cost (no `cost_usd` key)
//!   and `stats` never averages it in as $0;
//! - a measured cost is stored and averaged;
//! - a legacy stored `cost_usd: 0.0` reads back as unmeasured (mirrors
//!   `duration_secs`), and an explicit `--cost 0` is written as 0.0 but also
//!   reads back as unmeasured;
//! - a negative / non-finite `--cost` exits non-zero and writes nothing;
//! - `policy::decide` (explore = false) no longer treats unmeasured passes as
//!   free, and an all-measured / none-measured history decides as before.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn vhome(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "fugu-v55fb3861-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join(".fugu-router")).expect("mk temp home");
    dir
}

fn fr(home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fugu-router"))
        .args(args)
        .env("HOME", home)
        .env_remove("FUGU_ROUTER_MODE")
        .stdin(Stdio::null())
        .output()
        .expect("fugu-router runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn store(home: &Path) -> PathBuf {
    home.join(".fugu-router").join("episodes.jsonl")
}

fn rec(home: &Path, title: &str, model: &str, cost: Option<&str>) {
    let mut a = vec![
        "record", "--title", title, "--model", model, "--status", "verified",
    ];
    let flag;
    if let Some(c) = cost {
        flag = format!("--cost={c}");
        a.push(&flag);
    }
    let (code, o, e) = fr(home, &a);
    assert_eq!(code, 0, "record {title} {cost:?} must succeed: {o} {e}");
}

fn raw_line(home: &Path, title: &str) -> serde_json::Value {
    std::fs::read_to_string(store(home))
        .expect("store exists")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("json line"))
        .find(|v| v["title"] == title)
        .unwrap_or_else(|| panic!("no stored line titled {title}"))
}

fn stats(home: &Path) -> serde_json::Value {
    let (code, o, e) = fr(home, &["stats", "--json"]);
    assert_eq!(code, 0, "stats --json: {e}");
    serde_json::from_str(&o).unwrap_or_else(|err| panic!("stats json {err}: {o}"))
}

#[test]
fn v_unmeasured_is_not_stored_or_averaged_as_zero() {
    let h = vhome("unmeasured");
    rec(&h, "alpha", "opus", None);
    rec(&h, "beta", "opus", Some("6.0"));
    let alpha = raw_line(&h, "alpha");
    assert!(
        alpha.get("cost_usd").is_none() || alpha["cost_usd"].is_null(),
        "omitted --cost must not persist a cost: {alpha}"
    );
    let v = stats(&h);
    assert_eq!(v["models"]["opus"]["count"], 2, "{v}");
    assert_eq!(
        v["models"]["opus"]["avg_cost_usd"].as_f64(),
        Some(6.0),
        "unmeasured averaged in as $0 would give 3.0: {v}"
    );
}

#[test]
fn v_measured_costs_are_averaged() {
    let h = vhome("measured");
    rec(&h, "m1", "haiku", Some("1.5"));
    rec(&h, "m2", "haiku", Some("0.5"));
    assert_eq!(raw_line(&h, "m1")["cost_usd"].as_f64(), Some(1.5));
    let v = stats(&h);
    assert_eq!(
        v["models"]["haiku"]["avg_cost_usd"].as_f64(),
        Some(1.0),
        "{v}"
    );
}

#[test]
fn v_legacy_zero_and_explicit_zero_read_back_as_unmeasured() {
    let h = vhome("legacy");
    std::fs::write(
        store(&h),
        concat!(
            r#"{"ts":1,"title":"old","touched_files":[],"class":"parallel","model":"sonnet","role":"worker","pass":true,"cost_usd":0.0}"#,
            "\n",
            r#"{"ts":2,"title":"old2","touched_files":[],"class":"parallel","model":"sonnet","role":"worker","pass":true,"cost_usd":5.0}"#,
            "\n"
        ),
    )
    .unwrap();
    // Explicit `--cost 0`: the worker claims it is written as 0.0 but reads
    // back as unmeasured (same rule as duration_secs).
    rec(&h, "zero", "sonnet", Some("0"));
    assert_eq!(
        raw_line(&h, "zero")["cost_usd"].as_f64(),
        Some(0.0),
        "explicit --cost 0 is persisted as given"
    );
    let v = stats(&h);
    assert_eq!(v["models"]["sonnet"]["count"], 3, "{v}");
    assert_eq!(
        v["models"]["sonnet"]["avg_cost_usd"].as_f64(),
        Some(5.0),
        "legacy 0.0 and explicit 0 must both read as unmeasured: {v}"
    );
}

#[test]
fn v_negative_or_non_finite_cost_rejected_and_nothing_written() {
    let h = vhome("neg");
    for bad in ["-0.01", "-3", "NaN", "-inf", "inf"] {
        let flag = format!("--cost={bad}");
        let (code, o, e) = fr(
            &h,
            &[
                "record", "--title", "bad", "--model", "opus", "--status", "verified", &flag,
            ],
        );
        assert_ne!(code, 0, "--cost={bad} must exit non-zero: {o} {e}");
        let written = std::fs::read_to_string(store(&h)).unwrap_or_default();
        assert!(
            written.trim().is_empty(),
            "--cost={bad} wrote an episode: {written}"
        );
    }
}

fn decide_home(tag: &str) -> PathBuf {
    let h = vhome(tag);
    std::fs::write(
        h.join(".fugu-router").join("config.toml"),
        "explore = false\nk = 20\nsim_threshold = 0.05\nmin_samples = 1\npass_threshold = 0.6\n",
    )
    .unwrap();
    h
}

fn suggest(h: &Path) -> String {
    let (code, o, e) = fr(h, &["suggest", "refactor the widget parser module"]);
    assert_eq!(code, 0, "suggest must not fail: {o} {e}");
    assert!(
        o.contains("cheapest qualifying tier"),
        "expected policy::decide (explore=false) learned path: {o}"
    );
    o
}

/// haiku: one measured $3 pass + two unmeasured passes; sonnet: three $1
/// passes. Counting unmeasured as free gives haiku $1/pass and haiku wins the
/// tie; the measured cost/pass is $3 so sonnet must win.
#[test]
fn v_decide_does_not_treat_unmeasured_passes_as_free() {
    let h = decide_home("decide-mixed");
    let t = "refactor the widget parser module";
    rec(&h, t, "haiku", Some("3.0"));
    rec(&h, t, "haiku", None);
    rec(&h, t, "haiku", None);
    for _ in 0..3 {
        rec(&h, t, "sonnet", Some("1.0"));
    }
    let o = suggest(&h);
    assert!(o.starts_with("worker=sonnet"), "{o}");
}

/// Regression guard: an all-measured history decides by cost/pass exactly as
/// before (cheaper-per-pass tier wins).
#[test]
fn v_decide_all_measured_unchanged() {
    let h = decide_home("decide-all");
    let t = "refactor the widget parser module";
    for _ in 0..3 {
        rec(&h, t, "haiku", Some("2.0"));
        rec(&h, t, "sonnet", Some("1.0"));
    }
    let o = suggest(&h);
    assert!(o.starts_with("worker=sonnet"), "{o}");
}

/// No tier measured at all: no panic / NaN, falls back to cheapest-first.
#[test]
fn v_decide_nothing_measured_is_cheapest_first() {
    let h = decide_home("decide-none");
    let t = "refactor the widget parser module";
    for _ in 0..3 {
        rec(&h, t, "haiku", None);
        rec(&h, t, "sonnet", None);
    }
    let o = suggest(&h);
    assert!(o.starts_with("worker=haiku"), "{o}");
    assert!(!o.contains("NaN") && !o.contains("inf"), "{o}");
}
