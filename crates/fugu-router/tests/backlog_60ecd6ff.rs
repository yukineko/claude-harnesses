//! backlog 60ecd6ff: `delegation-stats --json` reports `avg_duration_secs: 0.0`
//! for a bucket that has NO duration samples. "No data" printed as zero is the
//! same class 55fb3861 fixed for cost (`avg_cost_usd` is `null` there).
//!
//! Drives the real binary against a sandboxed `$HOME`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn temp_home() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "fugu-router-60ecd6ff-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp home");
    dir
}

fn run_in(home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fugu-router"))
        .args(args)
        .env("HOME", home)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn delegation_stats_without_duration_samples_is_not_reported_as_zero() {
    let home = temp_home();
    // One flow-delegation episode with NO --duration (unmeasured).
    let (code, out, err) = run_in(
        &home,
        &[
            "record",
            "--title",
            "t",
            "--model",
            "sonnet",
            "--status",
            "verified",
            "--class",
            "flow-delegation",
            "--delegation",
            "fork",
        ],
    );
    assert_eq!(code, 0, "record failed: {out} {err}");

    let (code, out, err) = run_in(&home, &["delegation-stats", "--json"]);
    assert_eq!(code, 0, "delegation-stats failed: {err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let fork = &v["delegation"]["fork"];
    assert_eq!(fork["duration_samples"], 0, "precondition: {v}");
    assert!(
        fork["avg_duration_secs"].is_null(),
        "no duration samples must read as unmeasured (null), never 0.0 seconds: {v}"
    );
}
