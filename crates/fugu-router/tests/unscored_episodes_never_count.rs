#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Episodes with `outcome: "unscored"` record usage only. `stats` and `route`
//! must give the same answer with and without them, whatever their `pass`
//! value or class.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TITLE: &str = "refactor the parser module";

fn temp_home(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "fugu-router-unscored-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join(".fugu-router")).unwrap();
    // Deterministic routing: the bandit rng seed depends on episode count.
    std::fs::write(dir.join(".fugu-router/config.toml"), "explore = false\n").unwrap();
    dir
}

fn ep(class: &str, model: &str, pass: bool, unscored: bool, ts: u64) -> String {
    let mut v = serde_json::json!({
        "ts": ts, "title": TITLE, "touched_files": ["src/parser.rs"],
        "class": class, "model": model, "role": "worker", "pass": pass,
    });
    if unscored {
        v["outcome"] = serde_json::json!("unscored");
    }
    v.to_string()
}

fn write_store(home: &Path, lines: &[String]) {
    let mut s = lines.join("\n");
    s.push('\n');
    std::fs::write(home.join(".fugu-router/episodes.jsonl"), s).unwrap();
}

fn run(home: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut child =
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_fugu-router")))
            .args(args)
            .env("HOME", home)
            .current_dir(home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn stats_models(home: &Path) -> serde_json::Value {
    let (code, out, err) = run(home, &["stats", "--json"], "");
    assert_eq!(code, 0, "stats must exit 0; stderr={err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    v["models"].clone()
}

fn route_one(home: &Path) -> (serde_json::Value, serde_json::Value) {
    let decomp = serde_json::json!({
        "goal": "g",
        "tasks": [{"id": "t1", "title": TITLE, "class": "refactor",
                   "touched_files": ["src/parser.rs"]}],
    })
    .to_string();
    let report = home.join("report.json");
    let (code, _o, err) = run(
        home,
        &["route", "--report", report.to_str().unwrap()],
        &decomp,
    );
    assert_eq!(code, 0, "route must exit 0; stderr={err}");
    let rep: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    (
        rep["t1"]["worker_model"].clone(),
        rep["t1"]["basis"].clone(),
    )
}

#[test]
fn stats_ignores_unscored_episodes() {
    let base = vec![
        ep("parallel", "sonnet", true, false, 1_700_000_001),
        ep("parallel", "sonnet", false, false, 1_700_000_002),
    ];
    let mut with = base.clone();
    with.push(ep("subagent", "sonnet", false, true, 1_700_000_010));
    with.push(ep("subagent", "sonnet", true, true, 1_700_000_011));
    with.push(ep("parallel", "sonnet", true, true, 1_700_000_012));

    let h0 = temp_home("stats-base");
    write_store(&h0, &base);
    let h1 = temp_home("stats-with");
    write_store(&h1, &with);

    let m0 = stats_models(&h0);
    let m1 = stats_models(&h1);
    assert_eq!(m0["sonnet"]["count"], 2, "baseline sanity; got {m0}");
    assert_eq!(m0["sonnet"]["passes"], 1, "baseline sanity; got {m0}");
    for k in ["count", "passes", "pass_rate"] {
        assert_eq!(
            m1["sonnet"][k], m0["sonnet"][k],
            "stats models.sonnet.{k} must not change when unscored episodes are added; with={m1} without={m0}"
        );
    }
}

#[test]
fn route_ignores_unscored_episodes() {
    let base = vec![
        ep("parallel", "haiku", false, false, 1_700_000_001),
        ep("parallel", "sonnet", true, false, 1_700_000_002),
    ];
    let mut with = base.clone();
    // Counted, haiku would be 3/4 and qualify; uncounted it is 0/1 and does not.
    for i in 0..3 {
        with.push(ep("parallel", "haiku", true, true, 1_700_000_010 + i));
    }

    let h0 = temp_home("route-base");
    write_store(&h0, &base);
    let h1 = temp_home("route-with");
    write_store(&h1, &with);

    let (w0, b0) = route_one(&h0);
    let (w1, b1) = route_one(&h1);
    assert_eq!(
        w0, "sonnet",
        "baseline sanity: failing haiku must not qualify; got {w0} ({b0})"
    );
    assert_eq!(
        (w1, b1),
        (w0, b0),
        "route must be unchanged by unscored episodes"
    );
}
