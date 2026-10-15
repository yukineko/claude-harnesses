//! Reproduction tests for evalkit's prose claims. Each `claim_<id>` drives the
//! real binary (or reads the repo tree) and asserts the specific outcome the
//! README states.

use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn fresh_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("evalkit-prose-{}-{tag}-{id}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
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

/// A root with `evals/x.jsonl` holding `cases`.
fn root_with(cases: &str) -> PathBuf {
    let r = fresh_dir("root");
    std::fs::create_dir_all(r.join("evals")).unwrap();
    std::fs::write(r.join("evals/x.jsonl"), cases).unwrap();
    r
}

fn run_root(cases: &str, extra: &[&str]) -> Out {
    let r = root_with(cases);
    let mut args = vec!["run", "--root", s(&r)];
    args.extend_from_slice(extra);
    let cwd = fresh_dir("cwd");
    let o = ev_in(&args, &cwd);
    std::fs::remove_dir_all(&r).ok();
    std::fs::remove_dir_all(&cwd).ok();
    o
}

const PASS: &str = r#"{"id":"p","cmd":["printf","%s","HELLO"],"assert":{"contains":["HELLO"]}}"#;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn script(dir: &Path, name: &str, out: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\nprintf {out}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn claim_run_exit_on_regress() {
    let ok = run_root(PASS, &[]);
    assert_eq!(ok.code, 0, "{}{}", ok.stdout, ok.stderr);
    let bad = r#"{"id":"r","cmd":["printf","%s","HELLO"],"assert":{"contains":["NOPE"]}}"#;
    let o = run_root(&format!("{PASS}\n{bad}"), &[]);
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
}

#[test]
fn claim_exit_codes_run() {
    assert_eq!(run_root(PASS, &[]).code, 0);
    let bad = r#"{"id":"b","cmd":["printf","%s","X"],"assert":{"contains":["NOPE"]}}"#;
    assert_eq!(run_root(bad, &[]).code, 1);
    // empty eval dir -> 2
    let r = fresh_dir("empty");
    std::fs::create_dir_all(r.join("evals")).unwrap();
    let o = ev_in(&["run", "--root", s(&r)], &r);
    assert_eq!(o.code, 2, "{}{}", o.stdout, o.stderr);
    assert!(o.stderr.contains("no cases found"), "{}", o.stderr);
    // nonexistent dir -> 2
    let o = ev_in(&["run", "--root", s(&r), "--dir", "nonexistent"], &r);
    assert_eq!(o.code, 2, "{}{}", o.stdout, o.stderr);
    assert!(o.stderr.contains("reading eval dir"), "{}", o.stderr);
    // unreadable .jsonl (mode 000) -> 2
    let f = r.join("evals/u.jsonl");
    std::fs::write(&f, PASS).unwrap();
    // a readable, valid sibling so only the unreadable file can explain exit 2
    std::fs::write(r.join("evals/ok.jsonl"), PASS).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read_to_string(&f).is_ok(); // root would defeat mode 000
    let o = ev_in(&["run", "--root", s(&r)], &r);
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!readable, "needs a non-root user so mode 000 is unreadable");
    assert_eq!(o.code, 2, "{}{}", o.stdout, o.stderr);
    assert!(o.stderr.contains("u.jsonl"), "{}", o.stderr);
    std::fs::remove_dir_all(&r).ok();
}

#[test]
fn claim_id_required() {
    for line in [
        r#"{"id":"","cmd":["true"],"assert":{}}"#,
        r#"{"cmd":["true"],"assert":{}}"#,
    ] {
        let o = run_root(line, &[]);
        assert_eq!(o.code, 2, "{line}: {}{}", o.stdout, o.stderr);
    }
}

#[test]
fn claim_describe_label() {
    let plain = r#"{"id":"abc","cmd":["printf","%s","X"],"assert":{"contains":["X"]}}"#;
    let o = run_root(plain, &["--json"]);
    assert_eq!(o.code, 0);
    let v: Value = serde_json::from_str(&o.stdout).unwrap();
    assert_eq!(v["cases"][0]["case"], "abc");
    let desc = r#"{"id":"abc","describe":"the label","cmd":["printf","%s","X"],"assert":{"contains":["X"]}}"#;
    let o = run_root(desc, &["--json"]);
    assert_eq!(o.code, 0);
    let v: Value = serde_json::from_str(&o.stdout).unwrap();
    assert_eq!(v["cases"][0]["case"], "abc — the label");
}

#[test]
fn claim_file_subject_rel_root() {
    let r = root_with(
        r#"{"id":"hit","file":"sub/a.txt","assert":{"contains":["MARK"]}}
{"id":"miss","file":"sub/a.txt","assert":{"contains":["NOPE"]}}"#,
    );
    std::fs::create_dir_all(r.join("sub")).unwrap();
    std::fs::write(r.join("sub/a.txt"), "xx MARK yy").unwrap();
    let cwd = fresh_dir("cwd");
    let o = ev_in(&["run", "--root", s(&r), "--json"], &cwd);
    let v: Value = serde_json::from_str(&o.stdout).unwrap();
    assert_eq!(o.code, 1);
    assert_eq!(v["cases"][0]["case"], "hit");
    assert_eq!(v["cases"][0]["pass"], true);
    assert_eq!(v["cases"][1]["case"], "miss");
    assert_eq!(v["cases"][1]["pass"], false);
    std::fs::remove_dir_all(&r).ok();
    std::fs::remove_dir_all(&cwd).ok();
}

#[test]
fn claim_cmd_subject_stdout() {
    let line = r#"{"id":"c","cmd":["sh","-c","printf '%s-%s' \"$0\" \"$1\"","ARGA","ARGB"],"assert":{"exit":0,"contains":["ARGA-ARGB"]}}"#;
    let o = run_root(line, &[]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let neg = line.replace("ARGA-ARGB\"", "ARGB-ARGA\"");
    assert_eq!(run_root(&neg, &[]).code, 1);
}

#[test]
fn claim_stdin_piped() {
    let line = r#"{"id":"c","cmd":["cat"],"stdin":"PIPED","assert":{"contains":["PIPED"]}}"#;
    let o = run_root(line, &[]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
}

#[test]
fn claim_stdout_only() {
    let line =
        r#"{"id":"c","cmd":["sh","-c","echo ERRMARK >&2"],"assert":{"contains":["ERRMARK"]}}"#;
    let o = run_root(line, &[]);
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stdout.contains("missing required substring"),
        "{}",
        o.stdout
    );
}

#[test]
fn claim_acquire_fail() {
    let missing = r#"{"id":"m","file":"no/such/file.txt","assert":{}}"#;
    let o = run_root(missing, &[]);
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stdout.contains("could not acquire subject"),
        "{}",
        o.stdout
    );
    let nospawn = r#"{"id":"n","cmd":["no-such-prog-xyz"],"assert":{}}"#;
    let o = run_root(nospawn, &[]);
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stdout.contains("could not acquire subject"),
        "{}",
        o.stdout
    );
}

#[test]
fn claim_dir_default_evals() {
    let r = root_with(PASS);
    std::fs::create_dir_all(r.join("other")).unwrap();
    std::fs::write(
        r.join("other/o.jsonl"),
        r#"{"id":"in-other","cmd":["true"],"assert":{}}"#,
    )
    .unwrap();
    let o = ev_in(&["run", "--root", s(&r)], &r);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("ok   p"), "{}", o.stdout);
    assert!(!o.stdout.contains("in-other"), "{}", o.stdout);
    std::fs::remove_dir_all(&r).ok();
}

#[test]
fn claim_root_anchors() {
    let r = fresh_dir("anch");
    std::fs::create_dir_all(r.join("evals")).unwrap();
    std::fs::create_dir_all(r.join("bins")).unwrap();
    std::fs::write(r.join("data.txt"), "ROOTDATA").unwrap();
    script(&r.join("bins"), "evkroot", "BINFOUND");
    let rs = s(&r).to_string();
    std::fs::write(
        r.join("evals/x.jsonl"),
        format!(
            r#"{{"id":"f","file":"data.txt","assert":{{"contains":["ROOTDATA"]}}}}
{{"id":"b","cmd":["evkroot"],"assert":{{"contains":["BINFOUND"]}}}}
{{"id":"w","cmd":["pwd"],"assert":{{"contains":["{rs}"]}}}}"#
        ),
    )
    .unwrap();
    // (a) no --root: cwd = R
    let o = ev_in(&["run", "--bin-dir", "bins"], &r);
    assert_eq!(o.code, 0, "(a) {}{}", o.stdout, o.stderr);
    // (b)(c)(d) --root R from an unrelated cwd, relative --bin-dir
    let other = fresh_dir("other");
    let o = ev_in(&["run", "--root", &rs, "--bin-dir", "bins"], &other);
    assert_eq!(o.code, 0, "(b-d) {}{}", o.stdout, o.stderr);
    // control: without --bin-dir the script is not found
    let o = ev_in(&["run", "--root", &rs], &other);
    assert_eq!(o.code, 1, "no bin-dir -> evkroot not found: {}", o.stdout);
    std::fs::remove_dir_all(&r).ok();
    std::fs::remove_dir_all(&other).ok();
}

#[test]
fn claim_parse_errors_exit2() {
    let valid = r#"
// comment

{"id":"p","cmd":["true"],"assert":{}}

// another
"#;
    assert_eq!(run_root(valid, &[]).code, 0);
    for bad in [
        "{not json}",
        r#"{"id":"","cmd":["true"],"assert":{}}"#,
        r#"{"id":"both","file":"a","cmd":["true"],"assert":{}}"#,
        r#"{"id":"neither","assert":{}}"#,
    ] {
        let doc = format!("{valid}\n{bad}\n");
        let o = run_root(&doc, &[]);
        assert_eq!(o.code, 2, "{bad}: {}{}", o.stdout, o.stderr);
    }
}

#[test]
fn claim_list_plain() {
    let r = fresh_dir("list");
    std::fs::create_dir_all(r.join("evals")).unwrap();
    std::fs::write(
        r.join("evals/x.jsonl"),
        r#"{"id":"mk","describe":"marker","cmd":["touch","MARKER"],"assert":{}}"#,
    )
    .unwrap();
    let plain = ev_in(&["list", "--root", s(&r)], &r);
    let withflags = ev_in(
        &[
            "list",
            "--json",
            "--root",
            s(&r),
            "--bin-dir",
            "x",
            "--dir",
            "evals",
        ],
        &r,
    );
    assert_eq!(plain.code, 0, "{}", plain.stderr);
    assert_eq!(withflags.code, 0, "{}", withflags.stderr);
    assert_eq!(plain.stdout, withflags.stdout);
    assert!(serde_json::from_str::<Value>(&withflags.stdout).is_err());
    let line = withflags.stdout.lines().next().unwrap();
    let (path, label) = line.split_once('\t').unwrap();
    assert!(path.ends_with("x.jsonl"), "{line}");
    assert_eq!(label, "mk — marker");
    assert!(!r.join("MARKER").exists(), "list must not run cases");
    std::fs::remove_dir_all(&r).ok();
}

#[test]
fn claim_bin_dir_path() {
    let r = fresh_dir("bindir");
    std::fs::create_dir_all(r.join("evals")).unwrap();
    let bins = fresh_dir("bins");
    script(&bins, "evkprobe", "PROBED");
    script(&bins, "echo", "SHADOWED");
    std::fs::write(
        r.join("evals/x.jsonl"),
        r#"{"id":"probe","cmd":["evkprobe"],"assert":{"contains":["PROBED"]}}"#,
    )
    .unwrap();
    let with = ev_in(&["run", "--root", s(&r), "--bin-dir", s(&bins)], &r);
    assert_eq!(with.code, 0, "{}{}", with.stdout, with.stderr);
    let without = ev_in(&["run", "--root", s(&r)], &r);
    assert_eq!(without.code, 1, "{}{}", without.stdout, without.stderr);
    assert!(without.stdout.contains("could not acquire subject"));
    // shadowing: a same-named program earlier on PATH loses to DIR's
    std::fs::write(
        r.join("evals/x.jsonl"),
        r#"{"id":"shadow","cmd":["echo","hi"],"assert":{"contains":["SHADOWED"]}}"#,
    )
    .unwrap();
    let o = ev_in(&["run", "--root", s(&r), "--bin-dir", s(&bins)], &r);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    std::fs::remove_dir_all(&r).ok();
    std::fs::remove_dir_all(&bins).ok();
}

#[test]
fn claim_draft_reported() {
    let doc = format!("{{\"id\":\"d1\",\"draft\":true}}\n{PASS}\n");
    let o = run_root(&doc, &[]);
    assert!(
        o.stdout.contains("skip d1 (draft — assertion pending)"),
        "{}",
        o.stdout
    );
    assert!(o.stdout.contains("1 draft(s) skipped"), "{}", o.stdout);
    assert!(o.stdout.contains("1/2 passed, 0 failed"), "{}", o.stdout);
}

/// Run `evalkit run --json` over `cases`, save the output to a file in `dir`.
fn run_json_to(cases: &str, dir: &Path, name: &str) -> PathBuf {
    let r = root_with(cases);
    let o = ev_in(&["run", "--root", s(&r), "--json"], dir);
    assert!(o.code == 0 || o.code == 1, "{}{}", o.stdout, o.stderr);
    let p = dir.join(name);
    std::fs::write(&p, &o.stdout).unwrap();
    std::fs::remove_dir_all(&r).ok();
    p
}

fn case_line(id: &str, describe: &str, want: &str) -> String {
    format!(
        r#"{{"id":"{id}","describe":"{describe}","cmd":["printf","%s","HAVE"],"assert":{{"contains":["{want}"]}}}}"#
    )
}

#[test]
fn claim_canary_label_keys() {
    let d = fresh_dir("canary");
    // describe-edit => one dropped + one added
    let a = run_json_to(&case_line("k", "old", "HAVE"), &d, "a.json");
    let b = run_json_to(&case_line("k", "new", "HAVE"), &d, "b.json");
    let o = ev_in(
        &["canary", "--baseline", s(&a), "--current", s(&b), "--json"],
        &d,
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v: Value = serde_json::from_str(&o.stdout).unwrap();
    assert_eq!(v["added"], serde_json::json!(["k — new"]));
    assert_eq!(v["dropped"], serde_json::json!(["k — old"]));
    // regression and fix
    let base = format!(
        "{}\n{}",
        case_line("reg", "", "HAVE"),
        case_line("fix", "", "NOPE")
    );
    let cur = format!(
        "{}\n{}",
        case_line("reg", "", "NOPE"),
        case_line("fix", "", "HAVE")
    );
    let a = run_json_to(&base, &d, "a2.json");
    let b = run_json_to(&cur, &d, "b2.json");
    let o = ev_in(
        &["canary", "--baseline", s(&a), "--current", s(&b), "--json"],
        &d,
    );
    let v: Value = serde_json::from_str(&o.stdout).unwrap();
    assert_eq!(v["regressions"], serde_json::json!(["reg"]));
    assert_eq!(v["fixes"], serde_json::json!(["fix"]));
    // human output: pass-rate before -> after with delta
    let o = ev_in(&["canary", "--baseline", s(&a), "--current", s(&b)], &d);
    assert!(
        o.stdout.contains("pass-rate 50.0% → 50.0% (+0.0 pts)"),
        "{}",
        o.stdout
    );
    let c = run_json_to(&case_line("reg", "", "HAVE"), &d, "c.json");
    let o = ev_in(&["canary", "--baseline", s(&a), "--current", s(&c)], &d);
    assert!(
        o.stdout.contains("pass-rate 50.0% → 100.0% (+50.0 pts)"),
        "{}",
        o.stdout
    );
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn claim_canary_dropped_bucket() {
    let d = fresh_dir("dropped");
    let base = format!(
        "{}\n{}",
        case_line("keep", "", "HAVE"),
        case_line("gone", "", "HAVE")
    );
    let a = run_json_to(&base, &d, "a.json");
    let b = run_json_to(&case_line("keep", "", "HAVE"), &d, "b.json");
    let o = ev_in(
        &["canary", "--baseline", s(&a), "--current", s(&b), "--json"],
        &d,
    );
    let v: Value = serde_json::from_str(&o.stdout).unwrap();
    assert_eq!(v["dropped"], serde_json::json!(["gone"]));
    let o = ev_in(&["canary", "--baseline", s(&a), "--current", s(&b)], &d);
    assert!(
        o.stdout.contains("1 dropped (gone from current):") && o.stdout.contains("- gone"),
        "{}",
        o.stdout
    );
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn claim_canary_exit2() {
    let d = fresh_dir("c2");
    let good = run_json_to(&case_line("k", "", "HAVE"), &d, "g.json");
    let missing = d.join("nonexistent.json");
    let o = ev_in(
        &["canary", "--baseline", s(&missing), "--current", s(&good)],
        &d,
    );
    assert_eq!(o.code, 2, "{}{}", o.stdout, o.stderr);
    let bad = d.join("bad.json");
    std::fs::write(&bad, "{ not json").unwrap();
    let o = ev_in(
        &["canary", "--baseline", s(&bad), "--current", s(&good)],
        &d,
    );
    assert_eq!(o.code, 2, "{}{}", o.stdout, o.stderr);
    let o = ev_in(
        &["canary", "--baseline", s(&good), "--current", s(&bad)],
        &d,
    );
    assert_eq!(o.code, 2, "{}{}", o.stdout, o.stderr);
    std::fs::remove_dir_all(&d).ok();
}

fn read_cases(rel: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(repo_root().join(rel)).unwrap();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn claim_evals_files() {
    let skill = read_cases("evals/skill-invariants.jsonl");
    assert!(!skill.is_empty());
    for c in skill.iter().filter(|c| c["draft"] != true) {
        let f = c["file"].as_str().unwrap_or_else(|| panic!("no file: {c}"));
        assert!(f.ends_with("SKILL.md"), "{c}");
    }
    let cli = read_cases("evals/cli-contracts.jsonl");
    assert!(!cli.is_empty());
    for c in &cli {
        assert!(c["cmd"].is_array() && c.get("file").is_none(), "{c}");
    }
    let replay = read_cases("evals/replay/replayed.jsonl");
    assert!(!replay.is_empty());
    for c in &replay {
        let cmd: Vec<&str> = c["cmd"]
            .as_array()
            .unwrap_or_else(|| panic!("not cmd: {c}"))
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(cmd[0], "replaykit", "{c}");
        assert_eq!(cmd[1], "verify", "{c}");
        let fx = cmd
            .iter()
            .find(|a| a.starts_with("evals/replay/fixtures/"))
            .unwrap_or_else(|| panic!("no fixture arg: {c}"));
        assert!(repo_root().join(fx).is_file(), "missing {fx}");
    }
}

#[test]
fn claim_no_ci_no_hook() {
    let root = repo_root();
    assert!(!root.join(".github/workflows/eval.yml").exists());
    let wf = root.join(".github/workflows");
    if wf.is_dir() {
        let n = std::fs::read_dir(&wf)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                matches!(
                    e.path().extension().and_then(|x| x.to_str()),
                    Some("yml") | Some("yaml")
                )
            })
            .count();
        assert_eq!(n, 0, "workflow files present");
    }
    let me = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(!me.join("hooks").exists());
    let plugin: Value = serde_json::from_str(
        &std::fs::read_to_string(me.join(".claude-plugin/plugin.json")).unwrap(),
    )
    .unwrap();
    assert!(plugin.get("hooks").is_none(), "{plugin}");
}

/// Markdown mentions in running prose are not invocations; inside a fenced code
/// block (or in any non-markdown file: hooks, scripts) they are.
fn invokes(p: &Path, text: &str, needle: &str) -> bool {
    if p.extension().and_then(|x| x.to_str()) != Some("md") {
        return text.contains(needle);
    }
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if fenced && line.contains(needle) {
            return true;
        }
    }
    false
}

fn walk(dir: &Path, hits: &mut Vec<PathBuf>, needle: &str) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        let name = e.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        if p.is_dir() {
            walk(&p, hits, needle);
        } else if let Ok(t) = std::fs::read_to_string(&p) {
            if invokes(&p, &t, needle) {
                hits.push(p);
            }
        }
    }
}

#[test]
fn claim_nothing_runs_it() {
    let root = repo_root();
    let needle = "evalkit run";
    let mut hits = Vec::new();
    walk(&root.join(".githooks"), &mut hits, needle);
    walk(&root.join("crates/flow"), &mut hits, needle);
    walk(&root.join("crates/condukt"), &mut hits, needle);
    for e in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        walk(&e.path().join("hooks"), &mut hits, needle);
    }
    assert!(hits.is_empty(), "invokers found: {hits:?}");
    // the documented invocation is accepted by the binary
    let r = root_with(PASS);
    let cwd = fresh_dir("cwd");
    let o = ev_in(
        &["run", "--bin-dir", "target/release", "--root", s(&r)],
        &cwd,
    );
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    std::fs::remove_dir_all(&r).ok();
    std::fs::remove_dir_all(&cwd).ok();
}
