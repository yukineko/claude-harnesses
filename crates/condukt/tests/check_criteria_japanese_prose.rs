// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `condukt state check-criteria` extracting a pytest command from Japanese
//! prose done_criteria must not keep Japanese words ("が通る。scripts/test_b.py")
//! as pytest args: that makes pytest report "file not found" and the criteria
//! fail although the tests are green (false negative).
//!
//! Drives the binary BUILT FROM THIS TREE. No skip path: if python3 / pytest /
//! git is unusable the test FAILS (cannot determine is not pass).

use std::path::Path;
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

fn scrub(cmd: &mut Command) -> &mut Command {
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    cmd
}

fn git(repo: &Path, args: &[&str]) {
    let out = scrub(&mut Command::new("git"))
        .current_dir(repo)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn condukt(repo: &Path, home: &Path, args: &[&str]) -> Output {
    scrub(&mut Command::new(bin()))
        .current_dir(repo)
        .env("HOME", home)
        .env("CONDUKT_AUTONOMOUS", "0")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn condukt")
}

struct Observed {
    json: serde_json::Value,
    stdout: String,
    argv: Vec<String>,
}

fn run_case(tag: &str, criteria: &str, b_fails: bool) -> Observed {
    let py = Command::new("python3")
        .args(["-m", "pytest", "--version"])
        .output()
        .expect("python3 must be runnable (cannot determine otherwise)");
    assert!(
        py.status.success(),
        "python3 -m pytest unusable; cannot determine: {}",
        String::from_utf8_lossy(&py.stderr)
    );

    let tmp = std::env::temp_dir().join(format!("condukt-cc-ja-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let home = tmp.join("home");
    let repo = tmp.join("repo");
    std::fs::create_dir_all(home.join(".condukt/state")).unwrap();
    std::fs::create_dir_all(repo.join("scripts")).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@t.t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(
        repo.join("scripts/test_a.py"),
        "def test_ok():\n    assert True\n",
    )
    .unwrap();
    let b_body = if b_fails {
        "def test_ok():\n    assert False\n"
    } else {
        "def test_ok():\n    assert True\n"
    };
    std::fs::write(repo.join("scripts/test_b.py"), b_body).unwrap();
    git(&repo, &["add", "scripts"]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    let decomp = repo.join("decomp.json");
    let doc = serde_json::json!({
        "goal": "g",
        "tasks": [{
            "id": "t1",
            "title": "add a test",
            "touched_files": ["scripts/test_a.py", "scripts/test_b.py"],
            "deps": [],
            "class": "serial",
            "done_criteria": criteria
        }]
    });
    std::fs::write(&decomp, doc.to_string()).unwrap();

    let init = condukt(
        &repo,
        &home,
        &["state", "init", "--file", decomp.to_str().unwrap()],
    );
    let init_out = format!(
        "{}{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );
    let rid = init_out
        .lines()
        .map(str::trim)
        .rfind(|l| l.starts_with("run-"))
        .unwrap_or_else(|| panic!("state init produced no run id: {init_out}"))
        .to_string();

    let cc = condukt(
        &repo,
        &home,
        &["state", "check-criteria", "--run", &rid, "--task", "t1"],
    );
    let stdout = String::from_utf8_lossy(&cc.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&tmp);
    let json: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("check-criteria stdout not JSON ({e}): {stdout}"));
    let cmd = json
        .get("cmd")
        .filter(|c| !c.is_null())
        .or_else(|| json.get("evidence").and_then(|e| e.get("cmd")));
    let argv: Vec<String> = match cmd.and_then(|c| c.as_array()) {
        Some(a) => a
            .iter()
            .map(|x| x.as_str().expect("argv element is a string").to_string())
            .collect(),
        None => Vec::new(),
    };
    Observed { json, stdout, argv }
}

fn passed(o: &Observed) -> Option<bool> {
    o.json
        .get("passed")
        .or_else(|| o.json.get("evidence").and_then(|e| e.get("passed")))
        .and_then(|p| p.as_bool())
}

fn has_non_ascii(s: &str) -> bool {
    !s.is_ascii()
}

fn assert_no_japanese(o: &Observed) {
    assert!(!o.argv.is_empty(), "no extracted cmd in: {}", o.stdout);
    for a in &o.argv {
        assert!(
            !has_non_ascii(a),
            "Japanese prose leaked into pytest argv token {a:?}: {:?} (full: {})",
            o.argv,
            o.stdout
        );
    }
}

fn assert_paths(o: &Observed, paths: &[&str]) {
    for p in paths {
        assert!(
            o.argv.iter().any(|a| a == p),
            "argv must contain {p}, got {:?} (full: {})",
            o.argv,
            o.stdout
        );
    }
}

fn assert_passed(o: &Observed) {
    assert_eq!(
        passed(o),
        Some(true),
        "green tests must report passed=true, got: {}",
        o.stdout
    );
}

#[test]
fn case1_single_path_then_japanese_word() {
    let o = run_case("c1", "pytest scripts/test_a.py が通る", false);
    assert_no_japanese(&o);
    assert_paths(&o, &["scripts/test_a.py"]);
    assert_passed(&o);
}

#[test]
fn case2_python_m_pytest_then_japanese_word() {
    let o = run_case(
        "c2",
        "python3 -m pytest scripts/test_a.py が通ること",
        false,
    );
    assert!(
        o.argv.starts_with(&[
            "python3".to_string(),
            "-m".to_string(),
            "pytest".to_string()
        ]),
        "must keep python3 -m pytest prefix, got {:?} (full: {})",
        o.argv,
        o.stdout
    );
    assert_no_japanese(&o);
    assert_paths(&o, &["scripts/test_a.py"]);
    assert_passed(&o);
}

#[test]
fn case3_two_paths_split_by_japanese_sentence() {
    let o = run_case(
        "c3",
        "pytest scripts/test_a.py が通る。scripts/test_b.py も通る",
        false,
    );
    assert_no_japanese(&o);
    assert_paths(&o, &["scripts/test_a.py", "scripts/test_b.py"]);
    assert_passed(&o);
}

#[test]
fn case4_two_paths_joined_by_japanese_particle() {
    let o = run_case(
        "c4",
        "pytest scripts/test_a.py と scripts/test_b.py が通る",
        false,
    );
    assert_no_japanese(&o);
    assert_paths(&o, &["scripts/test_a.py", "scripts/test_b.py"]);
    assert_passed(&o);
}

/// Safety control: a genuinely failing test_b.py must never be reported as
/// passed with skip_verifier.
#[test]
fn case5_failing_test_b_is_not_passed_with_skip_verifier() {
    let o = run_case(
        "c5",
        "pytest scripts/test_a.py と scripts/test_b.py が通る",
        true,
    );
    let skip = o
        .json
        .get("skip_verifier")
        .or_else(|| o.json.get("evidence").and_then(|e| e.get("skip_verifier")))
        .and_then(|s| s.as_bool());
    assert!(
        !(passed(&o) == Some(true) && skip == Some(true)),
        "failing test_b.py must not yield passed=true + skip_verifier=true: {}",
        o.stdout
    );
}
