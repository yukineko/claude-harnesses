//! backlog 55fb3861: an episode whose cost was never measured must be stored
//! as UNMEASURED and kept out of every cost aggregate, never stored or
//! averaged as `cost_usd: 0.0` ("this task was free").
//!
//! Before the fix `record --cost` defaulted to `0.0` and `Episode.cost_usd`
//! was a bare `f64`, so omitting `--cost` (what condukt now does for an
//! unresolved cost) still wrote `"cost_usd":0.0` and `stats` averaged it in.
//! These tests observe the real binary and the real on-disk store, so they
//! check what fugu-router actually PERSISTS, not just what a caller passed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn temp_home(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("fugu-router-cost-{tag}-{pid}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp home");
    dir
}

fn run_in(home: &Path, args: &[&str]) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_fugu-router");
    let mut child = Command::new(bin)
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"");
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `record` one sonnet episode; `extra` is appended verbatim. Panics on a
/// non-zero exit so a rejected record cannot pass silently.
fn record(home: &Path, title: &str, extra: &[&str]) {
    let mut args = vec![
        "record", "--title", title, "--model", "sonnet", "--status", "verified",
    ];
    args.extend_from_slice(extra);
    let (code, out, err) = run_in(home, &args);
    assert_eq!(code, 0, "record `{title}` failed: {out} {err}");
}

fn store_lines(home: &Path) -> Vec<serde_json::Value> {
    let p = home.join(".fugu-router").join("episodes.jsonl");
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{} must exist: {e}", p.display()))
        .lines()
        .map(|l| serde_json::from_str(l).expect("episode line is JSON"))
        .collect()
}

fn stats_json(home: &Path, sub: &str) -> serde_json::Value {
    let (code, out, err) = run_in(home, &[sub, "--json"]);
    assert_eq!(code, 0, "{sub} --json failed: {err}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("{sub} json: {e}: {out}"))
}

#[test]
fn record_without_cost_stores_no_cost_and_stats_does_not_average_it() {
    let home = temp_home("stats");
    record(&home, "measured", &["--cost", "2.0"]);
    record(&home, "unmeasured", &[]);

    let lines = store_lines(&home);
    let unmeasured = lines
        .iter()
        .find(|e| e["title"] == "unmeasured")
        .expect("unmeasured episode stored");
    assert!(
        unmeasured.get("cost_usd").is_none(),
        "an omitted --cost must be stored as unmeasured (no cost_usd key), got: {unmeasured}"
    );
    let measured = lines
        .iter()
        .find(|e| e["title"] == "measured")
        .expect("measured episode stored");
    assert_eq!(measured["cost_usd"].as_f64(), Some(2.0));

    let v = stats_json(&home, "stats");
    let sonnet = &v["models"]["sonnet"];
    assert_eq!(sonnet["count"], 2, "both episodes still count: {v}");
    assert_eq!(
        sonnet["avg_cost_usd"].as_f64(),
        Some(2.0),
        "the unmeasured episode must not be averaged in as 0 (would give 1.0): {v}"
    );
    assert_eq!(sonnet["cost_samples"], 1, "{v}");
    assert_eq!(v["cost_coverage"]["recorded"], 1, "{v}");
    assert_eq!(v["cost_coverage"]["total"], 2, "{v}");
}

#[test]
fn stats_reports_no_average_rather_than_zero_when_nothing_is_measured() {
    let home = temp_home("none");
    record(&home, "unmeasured", &[]);
    let v = stats_json(&home, "stats");
    assert!(
        v["models"]["sonnet"]["avg_cost_usd"].is_null(),
        "no measured cost → no average (null), never $0: {v}"
    );
    assert_eq!(v["cost_coverage"]["recorded"], 0, "{v}");
}

#[test]
fn delegation_stats_averages_cost_over_measured_episodes_only() {
    let home = temp_home("deleg");
    let deleg = ["--class", "flow-delegation", "--delegation", "fork"];
    let mut with_cost = deleg.to_vec();
    with_cost.extend_from_slice(&["--cost", "3.0"]);
    record(&home, "f-measured", &with_cost);
    record(&home, "f-unmeasured", &deleg);

    let v = stats_json(&home, "delegation-stats");
    let fork = &v["delegation"]["fork"];
    assert_eq!(fork["count"], 2, "{v}");
    assert_eq!(fork["cost_samples"], 1, "{v}");
    assert_eq!(
        fork["avg_cost_usd"].as_f64(),
        Some(3.0),
        "the unmeasured fork episode must not be averaged in as 0 (would give 1.5): {v}"
    );
}

#[test]
fn record_rejects_negative_or_non_finite_cost() {
    let home = temp_home("reject");
    // `--cost=<v>` form so a negative value reaches the validation instead of
    // being rejected by clap as an unknown `-1` flag.
    for bad in ["-1", "-0.5", "NaN", "inf"] {
        let flag = format!("--cost={bad}");
        let (code, _o, _e) = run_in(
            &home,
            &[
                "record", "--title", "z", "--model", "sonnet", "--status", "verified", &flag,
            ],
        );
        assert_ne!(code, 0, "--cost {bad} must be rejected");
    }
    let p = home.join(".fugu-router").join("episodes.jsonl");
    assert!(
        std::fs::read_to_string(&p).unwrap_or_default().is_empty(),
        "a rejected --cost must not write an episode"
    );
}

/// Legacy store lines were written by the old writer, which always serialized
/// `cost_usd` and wrote `0.0` for an unmeasured task. Exactly like
/// `duration_secs`, a present `0.0` reads back as unmeasured.
#[test]
fn legacy_zero_cost_line_reads_back_as_unmeasured() {
    let home = temp_home("legacy");
    let dir = home.join(".fugu-router");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("episodes.jsonl"),
        concat!(
            r#"{"ts":1,"title":"legacy","touched_files":[],"class":"parallel","model":"sonnet","role":"worker","pass":true,"cost_usd":0.0}"#,
            "\n",
            r#"{"ts":2,"title":"real","touched_files":[],"class":"parallel","model":"sonnet","role":"worker","pass":true,"cost_usd":4.0}"#,
            "\n"
        ),
    )
    .unwrap();
    let v = stats_json(&home, "stats");
    assert_eq!(
        v["models"]["sonnet"]["avg_cost_usd"].as_f64(),
        Some(4.0),
        "legacy 0.0 = unmeasured, so the average is 4.0 not 2.0: {v}"
    );
    assert_eq!(v["cost_coverage"]["recorded"], 1, "{v}");
    assert_eq!(v["cost_coverage"]["total"], 2, "{v}");
}
