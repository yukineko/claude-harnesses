// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Observed 2026-11-17, condukt run-20261116-020031-33040 task migrate-path:
//! the worker ran plain `cargo test` instead of `tdd red` / `tdd green`, so no
//! `.tdd/*.red.json` / `*.green.json` existed. `condukt worktree merge` still
//! merged the task into main; the gap surfaced only afterwards, when
//! `condukt state set --status verified` refused with "no tdd proof files
//! (*.red.json / *.green.json) found".
//!
//! Reading (A, asserted): a task whose decomposition kind requires the F->P
//! oracle (`fix` / `feature`) must not reach main without proofs; the merge
//! refuses (non-zero, main untouched). Reading (B, intended): the oracle is
//! enforced only at `state set verified`, and a merge before verification is
//! by design allowed. Under (B) the failure mode here is that unverifiable
//! work is already on main when the refusal comes; this test fails under (B)
//! and a human ruling is needed to keep or drop it.
//!
//! Controls: (1) `state check-oracle` reports the proof-less task as invalid
//! (so the fixture task really requires the oracle); (2) with valid proofs the
//! same merge succeeds.
//!
//! DOES NOT PROVE: which layer should refuse (merge vs a pre-merge command);
//! proof validity beyond what `tdd oracle` accepts; behaviour for non
//! fix/feature tasks; the real `tdd red`/`tdd green` outputs (proof JSON is
//! hand-written in the shape the oracle reads).
//!
//! Written by an independent test author, not the implementer.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn tdd_bin_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("profile dir")
        .to_path_buf();
    let bin_path = profile_dir.join("tdd");
    if !bin_path.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let mut cmd = Command::new(&cargo);
        cmd.args(["build", "-p", "tdd", "--bin", "tdd"]);
        if profile_dir.file_name().and_then(|s| s.to_str()) == Some("release") {
            cmd.arg("--release");
        }
        assert!(cmd.status().expect("cargo build -p tdd").success());
    }
    assert!(bin_path.exists(), "no tdd at {}", bin_path.display());
    profile_dir
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn condukt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    common::prime_if_running(args, home, "sess-nomerge-proofs");
    let mut paths = vec![tdd_bin_dir()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("CONDUKT_TESTSCOPE_LEDGER", common::testscope_ledger(home))
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("CLAUDE_CODE_SESSION_ID", "sess-nomerge-proofs")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("CONDUKT_DISABLE")
        .stdin(Stdio::null())
        .output()
        .expect("condukt")
}

fn ok(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

const RUN: &str = "run-nomerge-proofs";

struct Fx {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    wt: PathBuf,
    branch: String,
}

/// Repo on main; run with one `fix` task `t1`; its worktree holds one commit
/// carrying the `Condukt-Task: <run>/t1` trailer. No proofs yet.
fn setup() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t.t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    let dec = repo.join("dec.json");
    std::fs::write(
        &dec,
        serde_json::json!({"goal": "g", "tasks": [
            {"id": "t1", "title": "fix it", "kind": "fix",
             "reproduction_tests": "cargo test -p demo"}
        ]})
        .to_string(),
    )
    .unwrap();
    ok(
        &condukt(
            &repo,
            &home,
            &[
                "state",
                "init",
                "--run",
                RUN,
                "--file",
                dec.to_str().unwrap(),
            ],
        ),
        "state init",
    );
    let wt = PathBuf::from(ok(
        &condukt(
            &repo,
            &home,
            &[
                "worktree", "create", "--run", RUN, "--topic", "t1", "--branch", "t1",
            ],
        ),
        "worktree create",
    ));
    ok(
        &condukt(
            &repo,
            &home,
            &[
                "state",
                "set",
                "--run",
                RUN,
                "--task",
                "t1",
                "--status",
                "running",
                "--worktree",
                wt.to_str().unwrap(),
            ],
        ),
        "state set running",
    );
    std::fs::write(wt.join("impl.txt"), "impl\n").unwrap();
    git(&wt, &["add", "impl.txt"]);
    git(
        &wt,
        &[
            "commit",
            "-q",
            "-m",
            "implement",
            "--trailer",
            &format!("Condukt-Task: {RUN}/t1"),
        ],
    );
    let branch = git(&wt, &["branch", "--show-current"]);
    Fx {
        _tmp: tmp,
        repo,
        home,
        wt,
        branch,
    }
}

fn write_valid_proofs(wt: &Path) {
    let d = wt.join(".tdd");
    std::fs::create_dir_all(&d).unwrap();
    for (kind, passed, code) in [("red", false, 101), ("green", true, 0)] {
        std::fs::write(
            d.join(format!("t1.{kind}.json")),
            serde_json::json!({
                "task": "t1", "phase": kind, "cmd": "cargo test -p demo",
                "passed": passed, "exit_code": code,
                "ts": "2026-11-17T00:00:00+09:00",
                "output_tail": "x", "author": "a"
            })
            .to_string(),
        )
        .unwrap();
    }
}

fn check_oracle(f: &Fx) -> serde_json::Value {
    let out = condukt(
        &f.repo,
        &f.home,
        &["state", "check-oracle", "--run", RUN, "--task", "t1"],
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "check-oracle not JSON ({e}): stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn merge(f: &Fx) -> Output {
    let out = condukt(
        &f.repo,
        &f.home,
        &[
            "worktree", "merge", "--branch", &f.branch, "--run", RUN, "--task", "t1",
        ],
    );
    eprintln!(
        "merge exit={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
#[ignore = "RED worktree merge lands a fix task that has no tdd proofs"]
fn merge_of_fix_task_without_tdd_proofs_is_refused() {
    let f = setup();
    assert!(!f.wt.join(".tdd").exists(), "fixture: no proofs");
    let v = check_oracle(&f);
    assert_ne!(
        v.get("valid_fp_oracle").and_then(|x| x.as_bool()),
        Some(true),
        "precondition: a proof-less fix task has no valid oracle: {v}"
    );
    let before = git(&f.repo, &["rev-parse", "main"]);
    let out = merge(&f);
    let after = git(&f.repo, &["rev-parse", "main"]);
    assert!(
        !out.status.success() && after == before,
        "merge of a fix task with NO tdd proofs must be refused and leave main untouched; \
         exit={:?} main {before} -> {after}; stdout={} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---- Proof-content merge gates (user ruling relayed 2026-11-17) -----------
//
// Status at HEAD: `worktree::merge` reads no proof at all, so each property
// below is unenforced. (i) is the test above. (f) (g) (h) drive REAL
// `tdd red` / `tdd green` against a script printing libtest-style
// `test NAME ... ok|FAILED|ignored` lines. The per-test proof schema is NOT
// pinned here: the tests only observe whether the merge is refused.
//   (f) green's passed set must be a superset of red's failed set
//   (g) green's pass count must be > 0
//   (h) the tree green was taken on must equal the tree being merged
// DOES NOT PROVE: which layer refuses; that tdd parses libtest lines (a fix
// may choose another capture); the control only shows the good path merges.

const SCRIPT: &str = "mode=none; [ -f mode.txt ] && mode=$(cat mode.txt)
case $mode in
  none) echo 'test m::a ... ok'; echo 'test m::b ... FAILED'; echo 'test result: FAILED. 1 passed; 1 failed'; exit 1;;
  good) echo 'test m::a ... ok'; echo 'test m::b ... ok'; echo 'test result: ok. 2 passed; 0 failed'; exit 0;;
  lost) echo 'test m::a ... ok'; echo 'test result: ok. 1 passed; 0 failed'; exit 0;;
  nocompile) echo 'error: could not compile'; exit 1;;
  zero) echo 'test result: ok. 0 passed; 0 failed; 0 ignored'; exit 0;;
esac
";

fn tdd_in(f: &Fx, args: &[&str]) -> Output {
    let out = harness_core::git_env::scrub(&mut Command::new(tdd_bin_dir().join("tdd")))
        .args(args)
        .current_dir(&f.wt)
        .env("HOME", &f.home)
        .env("CLAUDE_CODE_SESSION_ID", "sess-nomerge-proofs")
        .stdin(Stdio::null())
        .output()
        .expect("tdd");
    assert!(
        out.status.success(),
        "tdd {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Record real RED (mode `red_mode`) then GREEN (mode `green_mode`) in the
/// task worktree with the same command.
fn record_real_proofs(f: &Fx, red_mode: &str, green_mode: &str) {
    std::fs::write(f.wt.join("t.sh"), SCRIPT).unwrap();
    let cmd = "sh t.sh";
    std::fs::write(f.wt.join("mode.txt"), red_mode).unwrap();
    tdd_in(f, &["red", "--task", "t1", "--cmd", cmd]);
    std::fs::write(f.wt.join("mode.txt"), green_mode).unwrap();
    tdd_in(f, &["green", "--task", "t1", "--cmd", cmd]);
}

fn assert_refused(f: &Fx, why: &str) {
    let before = git(&f.repo, &["rev-parse", "main"]);
    let out = merge(f);
    let after = git(&f.repo, &["rev-parse", "main"]);
    assert!(
        !out.status.success() && after == before,
        "{why}; exit={:?} main {before} -> {after}; stdout={} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Control: real proofs, green passes everything red failed, same tree.
#[test]
fn control_merge_with_real_good_proofs_succeeds() {
    let f = setup();
    record_real_proofs(&f, "none", "good");
    let out = merge(&f);
    assert!(
        out.status.success(),
        "control: merge with real good proofs must succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "RED merge ignores that a test failed at RED is absent from GREEN's passed set (f)"]
fn merge_refused_when_green_passed_set_misses_a_red_failure() {
    let f = setup();
    record_real_proofs(&f, "none", "lost");
    assert_refused(
        &f,
        "(f) m::b failed at RED and is not among GREEN's passed tests (only m::a), \
         so the merge must be refused",
    );
}

#[test]
#[ignore = "RED merge ignores a GREEN proof with zero passed tests (g)"]
fn merge_refused_when_green_pass_count_is_zero() {
    let f = setup();
    record_real_proofs(&f, "nocompile", "zero");
    assert_refused(
        &f,
        "(g) GREEN exited 0 having run 0 tests ('0 passed'), so the merge must be refused",
    );
}

#[test]
#[ignore = "RED merge ignores that GREEN was taken on a different tree than the one merged (h)"]
fn merge_refused_when_tree_changed_after_green() {
    let f = setup();
    record_real_proofs(&f, "none", "good");
    std::fs::write(f.wt.join("late.txt"), "changed after GREEN\n").unwrap();
    git(&f.wt, &["add", "late.txt"]);
    git(
        &f.wt,
        &[
            "commit",
            "-q",
            "-m",
            "late change",
            "--trailer",
            &format!("Condukt-Task: {RUN}/t1"),
        ],
    );
    assert_refused(
        &f,
        "(h) a commit was added after GREEN was recorded, so GREEN's tree is not the \
         merged tree and the merge must be refused",
    );
}

/// Control: same fixture with valid proofs merges.
#[test]
fn control_merge_with_valid_tdd_proofs_succeeds() {
    let f = setup();
    write_valid_proofs(&f.wt);
    let v = check_oracle(&f);
    assert_eq!(
        v.get("valid_fp_oracle").and_then(|x| x.as_bool()),
        Some(true),
        "control precondition: proofs form a valid oracle: {v}"
    );
    let out = merge(&f);
    assert!(
        out.status.success(),
        "control: merge with valid proofs must succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
