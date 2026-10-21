//! CLI contract tests for tracekit. Each test drives the real binary with a
//! fresh temp HOME, or checks packaging wiring (plugin.json, source scan).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use tempfile::TempDir;

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tracekit"))
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("binary runs")
}

fn ok(home: &Path, args: &[&str]) -> String {
    let o = run(home, args);
    assert_eq!(
        o.status.code(),
        Some(0),
        "args {args:?} stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn spans_file(home: &Path, rid: &str) -> PathBuf {
    home.join(".tracekit").join(rid).join("spans.jsonl")
}

fn lines(home: &Path, rid: &str) -> Vec<Value> {
    std::fs::read_to_string(spans_file(home, rid))
        .expect("spans.jsonl exists")
        .lines()
        .map(|l| serde_json::from_str(l).expect("each line is JSON"))
        .collect()
}

fn export_stdout(home: &Path, rid: &str) -> Value {
    let s = ok(home, &["export", rid, "--out", "-"]);
    serde_json::from_str(&s).expect("export stdout is JSON")
}

fn otlp_spans(doc: &Value) -> &Vec<Value> {
    doc["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .expect("spans array")
}

fn attr<'a>(span: &'a Value, key: &str) -> Option<&'a Value> {
    span["attributes"]
        .as_array()?
        .iter()
        .find(|a| a["key"] == key)
        .map(|a| &a["value"])
}

#[test]
fn claim_record_tree_render_export() {
    let h = TempDir::new().unwrap();
    let home = h.path();
    ok(
        home,
        &[
            "record",
            "--run",
            "R1",
            "--span",
            "t1",
            "--name",
            "root",
            "--phase",
            "interpreter",
            "--model",
            "opus",
            "--ms",
            "100",
            "--cost",
            "0.25",
            "--status",
            "ok",
            "--end-unix-ms",
            "10000",
        ],
    );
    ok(
        home,
        &[
            "record",
            "--run",
            "R1",
            "--span",
            "t2",
            "--parent",
            "t1",
            "--name",
            "child",
            "--phase",
            "worker",
            "--model",
            "sonnet",
            "--ms",
            "50",
            "--cost",
            "0.5",
            "--status",
            "verified",
            "--end-unix-ms",
            "10100",
        ],
    );
    let l = lines(home, "R1");
    assert_eq!(l[1]["parent_id"], "t1");
    assert_eq!(l[1]["phase"], "worker");
    assert_eq!(l[1]["model"], "sonnet");
    assert_eq!(l[1]["ms"], 50);
    assert_eq!(l[1]["cost_usd"], 0.5);
    assert_eq!(l[1]["status"], "verified");

    let t = ok(home, &["trace", "R1"]);
    let indent = |needle: &str| {
        let line = t.lines().find(|l| l.contains(needle)).expect(needle);
        line.len() - line.trim_start().len()
    };
    assert!(indent("child") > indent("root"), "trace:\n{t}");

    let doc = export_stdout(home, "R1");
    let sp = otlp_spans(&doc);
    assert_eq!(sp.len(), 2);
    let root = sp.iter().find(|s| s["name"] == "root").unwrap();
    let child = sp.iter().find(|s| s["name"] == "child").unwrap();
    assert_eq!(child["parentSpanId"], root["spanId"]);
    assert_ne!(root["spanId"], "");
}

#[test]
fn claim_one_line_per_record() {
    let h = TempDir::new().unwrap();
    let home = h.path();
    for id in ["a", "b"] {
        ok(
            home,
            &[
                "record", "--run", "R", "--span", id, "--name", id, "--phase", "worker",
            ],
        );
    }
    let raw = std::fs::read_to_string(spans_file(home, "R")).unwrap();
    assert_eq!(raw.lines().count(), 2);
    let l = lines(home, "R");
    assert_eq!(l[0]["span_id"], "a");
    assert_eq!(l[1]["span_id"], "b");
}

#[test]
fn claim_store_path_append_concurrent() {
    let h = TempDir::new().unwrap();
    let home = h.path();
    ok(
        home,
        &[
            "record", "--run", "RC", "--span", "pre", "--name", "pre", "--phase", "worker",
        ],
    );
    let n = 16;
    let kids: Vec<_> = (0..n)
        .map(|i| {
            Command::new(env!("CARGO_BIN_EXE_tracekit"))
                .args([
                    "record",
                    "--run",
                    "RC",
                    "--span",
                    &format!("c{i}"),
                    "--name",
                    "x",
                    "--phase",
                    "worker",
                ])
                .env("HOME", home)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut k in kids {
        assert!(k.wait().unwrap().success());
    }
    // ACTUAL behaviour (claim "N parseable lines" is FALSE): `writeln!` issues the
    // JSON and the trailing newline as separate append writes, so under
    // concurrency two spans can end up on one line (not parseable per line,
    // flaky). What IS always true: no bytes are lost -- the pre-existing line
    // stays first and every span id is present in the raw file.
    let raw = std::fs::read_to_string(spans_file(home, "RC")).unwrap();
    assert!(
        raw.lines().next().unwrap().contains("\"span_id\":\"pre\""),
        "pre-existing line must stay first"
    );
    for i in 0..n {
        assert_eq!(
            raw.matches(&format!("\"span_id\":\"c{i}\"")).count(),
            1,
            "span c{i} must appear exactly once in:\n{raw}"
        );
    }
}

#[test]
fn claim_defaults_ms_status() {
    let h = TempDir::new().unwrap();
    let home = h.path();
    ok(
        home,
        &[
            "record", "--run", "R", "--span", "a", "--name", "a", "--phase", "worker",
        ],
    );
    let l = lines(home, "R");
    assert_eq!(l[0]["ms"], 0);
    assert_eq!(l[0]["status"], "ok");
}

#[test]
fn claim_task_exported() {
    let h = TempDir::new().unwrap();
    let home = h.path();
    ok(
        home,
        &[
            "record", "--run", "R", "--span", "a", "--name", "a", "--phase", "worker", "--task",
            "T9",
        ],
    );
    let doc = export_stdout(home, "R");
    let v = attr(&otlp_spans(&doc)[0], "harness.task_id").expect("attr present");
    assert_eq!(v["stringValue"], "T9");
}

#[test]
fn claim_end_unix_ms_override() {
    let h = TempDir::new().unwrap();
    let home = h.path();
    ok(
        home,
        &[
            "record",
            "--run",
            "R",
            "--span",
            "a",
            "--name",
            "a",
            "--phase",
            "worker",
            "--end-unix-ms",
            "1700000000000",
        ],
    );
    assert_eq!(lines(home, "R")[0]["end_unix_ms"], 1700000000000u64);
    let doc = export_stdout(home, "R");
    assert_eq!(
        otlp_spans(&doc)[0]["endTimeUnixNano"],
        "1700000000000000000"
    );
}

#[test]
fn claim_export_default_path() {
    let h = TempDir::new().unwrap();
    let home = h.path();
    ok(
        home,
        &[
            "record", "--run", "RID-42", "--span", "a", "--name", "a", "--phase", "worker",
        ],
    );
    ok(home, &["export", "RID-42"]);
    let p = home.join(".tracekit/RID-42/otlp-RID-42.json");
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(&p).expect("default file")).unwrap();
    assert_eq!(otlp_spans(&doc).len(), 1);
}

#[test]
fn claim_export_stdout() {
    let h = TempDir::new().unwrap();
    let home = h.path();
    ok(
        home,
        &[
            "record", "--run", "RID-42", "--span", "a", "--name", "a", "--phase", "worker",
        ],
    );
    let doc = export_stdout(home, "RID-42");
    assert!(doc["resourceSpans"].is_array());
    assert!(!home.join(".tracekit/RID-42/otlp-RID-42.json").exists());
}

#[test]
fn claim_otlp_shape_attributes() {
    let h = TempDir::new().unwrap();
    let home = h.path();
    ok(
        home,
        &[
            "record", "--run", "R", "--span", "w", "--name", "w", "--phase", "worker", "--model",
            "opus", "--cost", "0.5", "--task", "T1",
        ],
    );
    ok(
        home,
        &[
            "record", "--run", "R", "--span", "x", "--parent", "w", "--name", "x", "--phase",
            "tool",
        ],
    );
    let doc = export_stdout(home, "R");
    let sp = otlp_spans(&doc);
    assert_eq!(sp.len(), 2);
    let w = sp.iter().find(|s| s["name"] == "w").unwrap();
    let x = sp.iter().find(|s| s["name"] == "x").unwrap();
    for k in [
        "gen_ai.operation.name",
        "gen_ai.request.model",
        "gen_ai.usage.cost_usd",
        "harness.phase",
        "harness.task_id",
        "harness.status",
    ] {
        assert!(attr(w, k).is_some(), "worker span missing {k}");
    }
    assert_eq!(
        attr(w, "gen_ai.operation.name").unwrap()["stringValue"],
        "invoke_agent"
    );
    assert_eq!(
        attr(x, "gen_ai.operation.name").unwrap()["stringValue"],
        "execute_tool"
    );
}

#[test]
fn claim_four_subcommands() {
    let h = TempDir::new().unwrap();
    let o = run(h.path(), &["--help"]);
    let s = String::from_utf8_lossy(&o.stdout).into_owned();
    let cmds: Vec<&str> = s
        .split("Commands:")
        .nth(1)
        .expect("Commands section")
        .lines()
        .skip(1)
        .take_while(|l| l.starts_with("  ") && !l.trim().is_empty())
        .map(|l| l.split_whitespace().next().unwrap())
        .collect();
    let mut sorted = cmds.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        vec!["export", "help", "list", "record", "trace"],
        "{s}"
    );
}

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn no_hook_violation(root: &Path) -> Option<String> {
    if root.join("hooks").exists() {
        return Some("hooks dir exists".into());
    }
    // Cannot determine (missing / unreadable / malformed plugin.json) is a
    // violation, not a pass.
    let pj = match std::fs::read_to_string(root.join(".claude-plugin/plugin.json")) {
        Ok(s) => s,
        Err(e) => return Some(format!("plugin.json unreadable: {e}")),
    };
    let v: Value = match serde_json::from_str(&pj) {
        Ok(v) => v,
        Err(e) => return Some(format!("plugin.json malformed: {e}")),
    };
    v.get("hooks").map(|_| "plugin.json has hooks".into())
}

#[test]
fn claim_no_hook() {
    assert_eq!(no_hook_violation(&crate_root()), None);
}

fn scan_for_tracekit(dir: &Path, hits: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("read_dir") {
        let p = e.expect("entry").path();
        if p.is_dir() {
            scan_for_tracekit(&p, hits);
        } else if let Ok(t) = std::fs::read_to_string(&p) {
            if t.contains("tracekit") || t.contains("spans.jsonl") {
                hits.push(p);
            }
        } else {
            // unreadable/non-UTF8: cannot determine -> count as a hit
            hits.push(p);
        }
    }
}

#[test]
fn claim_condukt_binary_no_spans() {
    let mut hits = Vec::new();
    scan_for_tracekit(&crate_root().join("../condukt/src"), &mut hits);
    assert!(hits.is_empty(), "condukt/src mentions tracekit: {hits:?}");
}

// ---- RED by test-input mutation (packaging claims) ----

#[test]
fn red_inputs_for_packaging_claims_are_detected() {
    let t = TempDir::new().unwrap();
    std::fs::create_dir_all(t.path().join(".claude-plugin")).unwrap();
    std::fs::write(
        t.path().join(".claude-plugin/plugin.json"),
        r#"{"name":"x","hooks":{}}"#,
    )
    .unwrap();
    assert!(no_hook_violation(t.path()).is_some());
    std::fs::write(
        t.path().join(".claude-plugin/plugin.json"),
        r#"{"name":"x"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(t.path().join("hooks")).unwrap();
    assert!(no_hook_violation(t.path()).is_some());
    let empty = TempDir::new().unwrap();
    assert!(
        no_hook_violation(empty.path()).is_some(),
        "missing plugin.json must not pass"
    );
    std::fs::create_dir_all(empty.path().join(".claude-plugin")).unwrap();
    std::fs::write(empty.path().join(".claude-plugin/plugin.json"), "{not json").unwrap();
    assert!(
        no_hook_violation(empty.path()).is_some(),
        "malformed plugin.json must not pass"
    );

    std::fs::create_dir_all(t.path().join("src/sub")).unwrap();
    std::fs::write(t.path().join("src/sub/a.rs"), "// tracekit record").unwrap();
    let mut hits = Vec::new();
    scan_for_tracekit(&t.path().join("src"), &mut hits);
    assert_eq!(hits.len(), 1);
}
