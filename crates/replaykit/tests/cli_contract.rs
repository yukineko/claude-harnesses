//! CLI contract tests for replaykit. Each test drives the real binary (or reads
//! the real packaging files) in an isolated temp dir/HOME.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("replaykit-prose-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_in(args: &[&str], home: &Path, cwd: &Path) -> Out {
    let o = Command::new(env!("CARGO_BIN_EXE_replaykit"))
        .args(args)
        .env("HOME", home)
        .current_dir(cwd)
        .output()
        .expect("binary runs");
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

/// Three spans, with costs; all ok.
const SPANS: &str = concat!(
    r#"{"run_id":"r","span_id":"a","name":"n1","phase":"interpreter","status":"ok","ms":1,"cost_usd":0.1,"end_unix_ms":1}"#,
    "\n",
    r#"{"run_id":"r","span_id":"b","name":"n2","phase":"worker","status":"ok","ms":2,"cost_usd":0.2,"end_unix_ms":2}"#,
    "\n",
    r#"{"run_id":"r","span_id":"c","name":"n3","phase":"verifier","status":"verified","ms":3,"end_unix_ms":3}"#,
    "\n",
);

fn write_spans(dir: &Path) -> PathBuf {
    let p = dir.join("spans.jsonl");
    std::fs::write(&p, SPANS).unwrap();
    p
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn golden_lines(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Promote `run` with the shared SPANS into `root`.
fn promote(root: &Path, home: &Path, run: &str, extra: &[&str]) -> Out {
    let spans = write_spans(home);
    let mut args = vec![
        "promote",
        "--run",
        run,
        "--spans",
        spans.to_str().unwrap(),
        "--root",
        root.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    run_in(&args, home, home)
}

#[test]
fn claim_intro_roundtrip() {
    let home = temp_dir("intro-home");
    let root = temp_dir("intro-root");
    let o = promote(&root, &home, "my-run-2026-06-28", &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let lines = golden_lines(&root.join("evals/replay/replayed.jsonl"));
    assert_eq!(lines.len(), 1);
    let g = &lines[0];
    for k in ["id", "describe", "cmd", "assert"] {
        assert!(g.get(k).is_some(), "golden lacks {k}: {g}");
    }
    assert_eq!(g["assert"]["exit"], serde_json::json!(0));
    let cmd: Vec<String> = g["cmd"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(cmd[0], "replaykit");
    assert_eq!(cmd[1], "verify");
    // Replay the golden cmd with the real binary at cwd=root.
    let args: Vec<&str> = cmd[1..].iter().map(|s| s.as_str()).collect();
    let ok = run_in(&args, &home, &root);
    assert_eq!(ok.code, 0, "{}", ok.stderr);
    // Drift the fixture: a step becomes failed.
    let fx = root.join(&cmd[2]);
    let mut v = read_json(&fx);
    v["steps"][0]["status"] = Value::String("failed".into());
    std::fs::write(&fx, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let bad = run_in(&args, &home, &root);
    assert_eq!(bad.code, 1, "{}", bad.stderr);
}

#[test]
fn claim_promote_writes_fixture_and_dedup() {
    let home = temp_dir("pwd-home");
    let root = temp_dir("pwd-root");
    let run = "my-run-2026-06-28";
    let o = promote(&root, &home, run, &["--dataset", "replayed"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let ds = root.join("evals/replay/replayed.jsonl");
    let lines = golden_lines(&ds);
    assert_eq!(lines.len(), 1);
    let id = lines[0]["id"].as_str().unwrap().to_string();
    let fx = read_json(&root.join(format!("evals/replay/fixtures/{id}.json")));
    assert_eq!(fx["run_id"], run);
    assert_eq!(fx["steps"].as_array().unwrap().len(), 3);
    assert!(fx.get("expect").is_some());
    let o2 = promote(&root, &home, run, &["--dataset", "replayed"]);
    assert_eq!(o2.code, 0, "{}", o2.stderr);
    assert!(o2.stderr.contains("already promoted"), "{}", o2.stderr);
    assert_eq!(golden_lines(&ds).len(), 1);
}

#[test]
fn claim_extract_default_spans_path() {
    let home = temp_dir("edef-home");
    let cwd = temp_dir("edef-cwd");
    let d = home.join(".tracekit/my_run_id");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("spans.jsonl"), SPANS).unwrap();
    let o = run_in(&["extract", "--run", "my run/id"], &home, &cwd);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v: Value = serde_json::from_str(&o.stdout).unwrap();
    assert_eq!(v["steps"].as_array().unwrap().len(), 3);
    assert_eq!(v["run_id"], "my run/id");
}

#[test]
fn claim_extract_spans_override() {
    let home = temp_dir("eov-home");
    let work = temp_dir("eov-work");
    let s = write_spans(&work);
    let o = run_in(
        &["extract", "--run", "X", "--spans", s.to_str().unwrap()],
        &home,
        &work,
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v: Value = serde_json::from_str(&o.stdout).unwrap();
    assert_eq!(v["steps"].as_array().unwrap().len(), 3);
}

#[test]
fn claim_extract_out_default_stdout() {
    let home = temp_dir("eout-home");
    let work = temp_dir("eout-work");
    let s = write_spans(&work);
    let sp = s.to_str().unwrap();
    let o = run_in(&["extract", "--run", "X", "--spans", sp], &home, &work);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v: Value = serde_json::from_str(&o.stdout).unwrap();
    for k in ["run_id", "steps", "expect"] {
        assert!(v.get(k).is_some(), "missing {k}");
    }
    assert!(
        o.stdout.trim().lines().count() > 1,
        "not pretty: {}",
        o.stdout
    );
    let outf = work.join("out.json");
    let o2 = run_in(
        &[
            "extract",
            "--run",
            "X",
            "--spans",
            sp,
            "--out",
            outf.to_str().unwrap(),
        ],
        &home,
        &work,
    );
    assert_eq!(o2.code, 0, "{}", o2.stderr);
    assert_eq!(o2.stdout, "");
    assert_eq!(read_json(&outf), v);
}

fn drifted_fixture(dir: &Path, name: &str, mutate: &dyn Fn(&mut Value)) -> PathBuf {
    let mut v: Value = serde_json::from_str(
        r#"{"run_id":"drift-run","steps":[
 {"span_id":"a","name":"n","phase":"interpreter","status":"ok","ms":1,"cost_usd":0.1},
 {"span_id":"b","name":"n","phase":"worker","status":"ok","ms":1,"cost_usd":0.2}],
 "expect":{"phases":["interpreter","worker"],"max_error_count":0,"max_cost_usd":0.3}}"#,
    )
    .unwrap();
    mutate(&mut v);
    let p = dir.join(name);
    std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    p
}

#[test]
fn claim_verify_violations_to_stderr() {
    let home = temp_dir("vv-home");
    let f = drifted_fixture(&home, "f.json", &|v| {
        v["steps"][1]["status"] = "failed".into();
    });
    let o = run_in(&["verify", f.to_str().unwrap()], &home, &home);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("replay drifted (fail)"), "{}", o.stderr);
    assert!(o.stderr.contains("  - error count"), "{}", o.stderr);
    assert_eq!(o.stdout, "");
}

#[test]
fn claim_promote_paths_and_sanitized_dataset() {
    let home = temp_dir("pps-home");
    let root = temp_dir("pps-root");
    let extra = ["--evals-dir", "E", "--dataset", "a/b"];
    let o = promote(&root, &home, "some-run", &extra);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let ds = root.join("E/replay/a_b.jsonl");
    let lines = golden_lines(&ds);
    assert_eq!(lines.len(), 1);
    let id = lines[0]["id"].as_str().unwrap();
    assert!(root.join(format!("E/replay/fixtures/{id}.json")).is_file());
    let o2 = promote(&root, &home, "some-run", &extra);
    assert_eq!(o2.code, 0, "{}", o2.stderr);
    assert_eq!(golden_lines(&ds).len(), 1);
}

#[test]
fn claim_golden_cmd_relative_to_root() {
    let home = temp_dir("gcr-home");
    let root = temp_dir("gcr-root");
    assert!(root.is_absolute());
    let o = promote(&root, &home, "rel-run", &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let g = &golden_lines(&root.join("evals/replay/replayed.jsonl"))[0];
    let id = g["id"].as_str().unwrap();
    assert_eq!(
        g["cmd"],
        serde_json::json!([
            "replaykit",
            "verify",
            format!("evals/replay/fixtures/{id}.json")
        ])
    );
}

#[test]
fn claim_promote_appends_distinct_runs() {
    let home = temp_dir("pa-home");
    let root = temp_dir("pa-root");
    let ds = root.join("evals/replay/replayed.jsonl");
    let o1 = promote(&root, &home, "first-run", &[]);
    assert_eq!(o1.code, 0, "{}", o1.stderr);
    let first_bytes = std::fs::read_to_string(&ds).unwrap();
    assert_eq!(first_bytes.lines().count(), 1);
    // A different run id with a separate spans file.
    let spans2 = home.join("spans2.jsonl");
    std::fs::write(&spans2, SPANS).unwrap();
    let o2 = run_in(
        &[
            "promote",
            "--run",
            "second-run",
            "--spans",
            spans2.to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
        ],
        &home,
        &home,
    );
    assert_eq!(o2.code, 0, "{}", o2.stderr);
    let text = std::fs::read_to_string(&ds).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(format!("{}\n", lines[0]), first_bytes, "line 1 changed");
    let g1: Value = serde_json::from_str(lines[0]).unwrap();
    let g2: Value = serde_json::from_str(lines[1]).unwrap();
    assert!(g1["id"].as_str().unwrap().starts_with("first-run-"));
    assert!(g2["id"].as_str().unwrap().starts_with("second-run-"));
    assert_ne!(g1["id"], g2["id"]);
}

#[test]
fn claim_promote_defaults() {
    let home = temp_dir("pd-home");
    let cwd = temp_dir("pd-cwd");
    let s = write_spans(&home);
    let o = run_in(
        &[
            "promote",
            "--run",
            "def-run",
            "--spans",
            s.to_str().unwrap(),
        ],
        &home,
        &cwd,
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    let lines = golden_lines(&cwd.join("evals/replay/replayed.jsonl"));
    assert_eq!(lines.len(), 1);
    let id = lines[0]["id"].as_str().unwrap();
    assert!(cwd
        .join(format!("evals/replay/fixtures/{id}.json"))
        .is_file());
}

#[test]
fn claim_draft_is_noop() {
    let home = temp_dir("dr-home");
    let r1 = temp_dir("dr-r1");
    let r2 = temp_dir("dr-r2");
    let o1 = promote(&r1, &home, "same-run", &[]);
    let o2 = promote(&r2, &home, "same-run", &["--draft"]);
    assert_eq!(o1.code, 0, "{}", o1.stderr);
    assert_eq!(o2.code, 0, "{}", o2.stderr);
    let g1 = std::fs::read(r1.join("evals/replay/replayed.jsonl")).unwrap();
    let g2 = std::fs::read(r2.join("evals/replay/replayed.jsonl")).unwrap();
    assert_eq!(g1, g2);
    let id = golden_lines(&r1.join("evals/replay/replayed.jsonl"))[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let f = format!("evals/replay/fixtures/{id}.json");
    assert_eq!(
        std::fs::read(r1.join(&f)).unwrap(),
        std::fs::read(r2.join(&f)).unwrap()
    );
}

#[test]
fn claim_exit_1_regression() {
    let home = temp_dir("e1-home");
    type Mutator = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(&str, &str, Mutator)> = vec![
        (
            "phase",
            "phase set",
            Box::new(|v| v["steps"][1]["phase"] = "tool".into()),
        ),
        (
            "errors",
            "error count",
            Box::new(|v| v["steps"][0]["status"] = "failed".into()),
        ),
        (
            "cost",
            "cost",
            Box::new(|v| v["steps"][0]["cost_usd"] = 5.0.into()),
        ),
    ];
    for (name, needle, m) in &cases {
        let f = drifted_fixture(&home, &format!("{name}.json"), m);
        let o = run_in(&["verify", f.to_str().unwrap()], &home, &home);
        assert_eq!(o.code, 1, "{name}: {}", o.stderr);
        assert!(o.stderr.contains(needle), "{name}: {}", o.stderr);
    }
}

#[test]
fn claim_exit_2_harness_error() {
    let home = temp_dir("e2-home");
    let missing = home.join("nope.json");
    let o = run_in(&["verify", missing.to_str().unwrap()], &home, &home);
    assert_eq!(o.code, 2, "missing fixture: {}", o.stderr);

    let bad = home.join("bad.json");
    std::fs::write(&bad, "{ not json").unwrap();
    let o = run_in(&["verify", bad.to_str().unwrap()], &home, &home);
    assert_eq!(o.code, 2, "invalid json: {}", o.stderr);

    let notsum = home.join("notsum.json");
    std::fs::write(&notsum, r#"{"hello":"world"}"#).unwrap();
    let o = run_in(&["verify", notsum.to_str().unwrap()], &home, &home);
    assert_eq!(o.code, 2, "not a summary: {}", o.stderr);

    let o = run_in(
        &[
            "promote",
            "--run",
            "r",
            "--spans",
            missing.to_str().unwrap(),
            "--root",
            home.to_str().unwrap(),
        ],
        &home,
        &home,
    );
    assert_eq!(o.code, 2, "promote missing spans: {}", o.stderr);

    let dir = home.join("adir");
    std::fs::create_dir_all(&dir).unwrap();
    let o = run_in(
        &["extract", "--run", "r", "--spans", dir.to_str().unwrap()],
        &home,
        &home,
    );
    assert_eq!(o.code, 2, "extract spans=dir: {}", o.stderr);
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn claim_not_a_lifecycle_hook() {
    let c = crate_dir();
    assert!(!c.join("hooks").exists());
    assert!(!c.join("hooks.json").exists());
    assert!(!c.join(".claude-plugin/hooks.json").exists());
    let pj = read_json(&c.join(".claude-plugin/plugin.json"));
    assert!(pj.get("hooks").is_none(), "{pj}");
}

#[test]
fn claim_deps_no_network_no_api_key() {
    let c = crate_dir();
    let toml = std::fs::read_to_string(c.join("Cargo.toml")).unwrap();
    let mut keys = Vec::new();
    let mut in_deps = false;
    for line in toml.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t == "[dependencies]";
            continue;
        }
        if in_deps && !t.is_empty() && !t.starts_with('#') {
            if let Some((k, _)) = t.split_once('=') {
                keys.push(k.trim().to_string());
            }
        }
    }
    keys.sort();
    // ACTUAL set: harness-core is a dependency too, beyond "std + serde + clap".
    assert_eq!(keys, vec!["clap", "harness-core", "serde", "serde_json"]);
    for entry in std::fs::read_dir(c.join("src")).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().is_some_and(|e| e == "rs") {
            let s = std::fs::read_to_string(&p).unwrap();
            for bad in [
                "TcpStream",
                "UdpSocket",
                "reqwest",
                "ureq",
                "hyper",
                "API_KEY",
            ] {
                assert!(!s.contains(bad), "{} contains {bad}", p.display());
            }
        }
    }
}

#[test]
fn claim_no_ci_wiring() {
    let repo = crate_dir().join("../..");
    assert!(!repo.join(".github").exists(), ".github exists");
    let mut n = 0;
    for e in std::fs::read_dir(repo.join(".githooks")).unwrap() {
        let p = e.unwrap().path();
        if p.is_file() {
            n += 1;
            let s = std::fs::read_to_string(&p).unwrap();
            for bad in ["evalkit", "replaykit"] {
                assert!(!s.contains(bad), "{} mentions {bad}", p.display());
            }
        }
    }
    assert!(n > 0, "no hook files scanned");
}
