// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Isolation tests for the merge proof contract (backlog 7768aeaf,
//! `specs/condukt-merge-proof-contract-7768aeaf.toml` R1-R8, user rulings
//! 2026-11-20 rounds 1-5).
//!
//! Every refusal test asserts the three observable parts of a refusal:
//! non-zero exit, `git rev-parse main` unchanged, and stderr carrying the exact
//! token `merge-proof-contract(<item>)` for the item the fixture violates.
//!
//! Isolation: a fixture that records real proofs commits `t.sh` and the mode
//! file with the content `tdd` runs on BEFORE `tdd green`, so GREEN's
//! `tree_identity.tree` equals the branch tip's commit tree (h holds) and the
//! script prints full libtest `test result:` lines (`counts.determined` is
//! true). `assert_isolated` checks both on every GREEN slot before the merge,
//! so only the targeted item can refuse.
//!
//! DOES NOT PROVE: the wording around the token; that a hold (runtime-overlap)
//! record is not created on refusal (only the merge-conflict record is
//! observed); `proof dir が proof_dir_candidates の 1 候補に定まらない` and
//! `branch tip の tree が計算できない` (not driven); behaviour with a tdd
//! config that moves `proof_dir`; concurrency.
//!
//! Written by an independent test author, not the implementer.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const RUN: &str = "run-7768aeaf";
const SESSION: &str = "sess-7768aeaf-contract";

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

fn git_raw(dir: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_raw(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn condukt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    common::prime_if_running(args, home, SESSION);
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
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
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

fn show(what: &str, out: &Output) {
    eprintln!(
        "{what} exit={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── fixture ─────────────────────────────────────────────────────────────────

struct Spec {
    /// `kind` of task `t1` in the run state (`None` = the key is omitted).
    kind: Option<&'static str>,
    /// Worktree topic; the branch is `<prefix>/<RUN>/<topic>` and its
    /// commits carry `Condukt-Task: <RUN>/<topic>`. tdd proofs use it as the
    /// task id.
    topic: &'static str,
    /// Edit `base.txt` on both main and the branch so a trial merge conflicts.
    conflict: bool,
    /// Record the worktree on task `t1` via `state set --status running`.
    set_running: bool,
}

const FIX: Spec = Spec {
    kind: Some("fix"),
    topic: "t1",
    conflict: false,
    set_running: true,
};

struct Fx {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    wt: PathBuf,
    branch: String,
    task: String,
}

impl Fx {
    fn trailer(&self) -> String {
        format!("Condukt-Task: {RUN}/{}", self.task)
    }

    fn commit(&self, paths: &[&str], msg: &str) {
        let mut add = vec!["add", "-f", "--"];
        add.extend_from_slice(paths);
        git(&self.wt, &add);
        git(
            &self.wt,
            &["commit", "-q", "-m", msg, "--trailer", &self.trailer()],
        );
    }

    fn main_sha(&self) -> String {
        git(&self.repo, &["rev-parse", "main"])
    }

    fn tip_tree(&self) -> String {
        git(
            &self.repo,
            &["rev-parse", &format!("{}^{{tree}}", self.branch)],
        )
    }
}

fn setup(spec: Spec) -> Fx {
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

    let mut task = serde_json::json!({
        "id": "t1", "title": "fix it", "reproduction_tests": "sh t.sh"
    });
    if let Some(k) = spec.kind {
        task["kind"] = serde_json::Value::String(k.to_string());
    }
    let dec = tmp.path().join("dec.json");
    std::fs::write(
        &dec,
        serde_json::json!({"goal": "g", "tasks": [task]}).to_string(),
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
                "worktree", "create", "--run", RUN, "--topic", spec.topic, "--branch", spec.topic,
            ],
        ),
        "worktree create",
    ));
    if spec.set_running {
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
    }
    let branch = git(&wt, &["branch", "--show-current"]);
    let f = Fx {
        _tmp: tmp,
        repo,
        home,
        wt,
        branch,
        task: spec.topic.to_string(),
    };
    std::fs::write(f.wt.join("impl.txt"), "impl\n").unwrap();
    let mut paths = vec!["impl.txt"];
    if spec.conflict {
        std::fs::write(f.wt.join("base.txt"), "branch version\n").unwrap();
        paths.push("base.txt");
    }
    f.commit(&paths, "implement");
    if spec.conflict {
        std::fs::write(f.repo.join("base.txt"), "main version\n").unwrap();
        git(&f.repo, &["add", "base.txt"]);
        git(&f.repo, &["commit", "-q", "-m", "main edit"]);
    }
    f
}

// ── real tdd proofs ─────────────────────────────────────────────────────────

/// A libtest-shaped script whose behaviour is chosen by the content of
/// `mode_file`. Every `test result:` line carries all three counts, so tdd
/// records `counts.determined: true` whenever one is printed.
fn script(mode_file: &str) -> String {
    format!(
        "mode=none; [ -f {mode_file} ] && mode=$(cat {mode_file})
case $mode in
  none) echo 'test m::a ... ok'; echo 'test m::b ... FAILED'; echo 'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out'; exit 1;;
  good) echo 'test m::a ... ok'; echo 'test m::b ... ok'; echo 'test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'; exit 0;;
  lost) echo 'test m::a ... ok'; echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'; exit 0;;
  nocompile) echo 'error: could not compile'; exit 1;;
  zero) echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'; exit 0;;
esac
"
    )
}

fn tdd(f: &Fx, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new(tdd_bin_dir().join("tdd")))
        .args(args)
        .current_dir(&f.wt)
        .env("HOME", &f.home)
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .expect("tdd");
    assert!(
        out.status.success(),
        "tdd {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// One test command: `sh <script>` reading `<mode_file>`; RED is taken with
/// mode `red`, GREEN (when `Some`) with mode `green`.
struct Cmd {
    script: &'static str,
    mode_file: &'static str,
    red: &'static str,
    green: Option<&'static str>,
}

const T_SH: &str = "t.sh";
const MODE: &str = "mode.txt";

fn single(red: &'static str, green: &'static str) -> [Cmd; 1] {
    [Cmd {
        script: T_SH,
        mode_file: MODE,
        red,
        green: Some(green),
    }]
}

/// Commit every script + mode file (RED modes), `tdd red` each command, then
/// commit the GREEN modes and `tdd green` each command that has one. Every
/// file tdd runs on is committed before GREEN, so GREEN's tree is the tip's.
fn record(f: &Fx, cmds: &[Cmd]) {
    let mut paths = Vec::new();
    for c in cmds {
        std::fs::write(f.wt.join(c.script), script(c.mode_file)).unwrap();
        std::fs::write(f.wt.join(c.mode_file), c.red).unwrap();
        paths.push(c.script);
        paths.push(c.mode_file);
    }
    f.commit(&paths, "add tests");
    for c in cmds {
        tdd(
            f,
            &[
                "red",
                "--task",
                &f.task,
                "--cmd",
                &format!("sh {}", c.script),
            ],
        );
    }
    let mut green_paths = Vec::new();
    for c in cmds {
        if let Some(g) = c.green {
            std::fs::write(f.wt.join(c.mode_file), g).unwrap();
            green_paths.push(c.mode_file);
        }
    }
    if !green_paths.is_empty() {
        f.commit(&green_paths, "make it pass");
    }
    for c in cmds {
        if c.green.is_some() {
            tdd(
                f,
                &[
                    "green",
                    "--task",
                    &f.task,
                    "--cmd",
                    &format!("sh {}", c.script),
                ],
            );
        }
    }
}

fn proof_dir(f: &Fx) -> PathBuf {
    f.wt.join(".tdd")
}

/// Every proof file of `kind` for the fixture's task in the worktree.
fn slots(f: &Fx, kind: &str) -> Vec<PathBuf> {
    let prefix = format!("{}.", f.task);
    let suffix = format!(".{kind}.json");
    let mut v: Vec<PathBuf> = std::fs::read_dir(proof_dir(f))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().to_string();
            n.starts_with(&prefix) && n.ends_with(&suffix)
        })
        .collect();
    v.sort();
    v
}

fn read_json(p: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap())
        .unwrap_or_else(|e| panic!("{} not JSON: {e}", p.display()))
}

/// Fixture precondition: every GREEN slot was taken on the tip's tree, has
/// determined counts, and records RED's failed set — so (h) holds and only
/// the item a test targets can refuse.
fn assert_isolated(f: &Fx, expect_greens: usize) {
    let greens = slots(f, "green");
    assert_eq!(
        greens.len(),
        expect_greens,
        "fixture: GREEN slots {greens:?}"
    );
    let tip = f.tip_tree();
    for g in &greens {
        let v = read_json(g);
        assert_eq!(
            v["tree_identity"]["tree"].as_str(),
            Some(tip.as_str()),
            "fixture precondition: GREEN {} was taken on the branch tip tree {tip}: {v}",
            g.display()
        );
        assert_eq!(
            v["counts"]["determined"].as_bool(),
            Some(true),
            "fixture precondition: GREEN {} has determined counts: {v}",
            g.display()
        );
        assert!(
            v["red_failed_tests"].is_array(),
            "fixture precondition: GREEN {} records red_failed_tests: {v}",
            g.display()
        );
    }
}

// ── merge / assertions ──────────────────────────────────────────────────────

fn merge_with(f: &Fx, extra: &[&str]) -> Output {
    let mut args = vec!["worktree", "merge", "--branch", f.branch.as_str()];
    args.extend_from_slice(extra);
    let out = condukt(&f.repo, &f.home, &args);
    show(&format!("merge {extra:?}"), &out);
    out
}

fn token(item: &str) -> String {
    format!("merge-proof-contract({item})")
}

fn assert_refused(f: &Fx, extra: &[&str], item: &str, why: &str) {
    let before = f.main_sha();
    let out = merge_with(f, extra);
    let after = f.main_sha();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success() && after == before && stderr.contains(&token(item)),
        "{why}: expected non-zero exit, main unchanged and stderr containing {:?}; \
         exit={:?} main {before} -> {after}; stdout={} stderr={stderr}",
        token(item),
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
    );
}

fn assert_task_merge_refused(f: &Fx, item: &str, why: &str) {
    let task = f.task.clone();
    assert_refused(f, &["--run", RUN, "--task", &task], item, why);
}

fn assert_merged(f: &Fx, extra: &[&str], why: &str) {
    let tip = git(&f.repo, &["rev-parse", &f.branch]);
    let out = merge_with(f, extra);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success()
            && stdout.contains("merged '")
            && !stderr.contains("merge-proof-contract("),
        "{why}: expected a completed merge; exit={:?} stdout={stdout} stderr={stderr}",
        out.status.code()
    );
    assert!(
        git_raw(&f.repo, &["merge-base", "--is-ancestor", &tip, "main"])
            .status
            .success(),
        "{why}: main must contain the branch tip {tip}"
    );
}

fn assert_task_merge_succeeds(f: &Fx, why: &str) {
    let task = f.task.clone();
    assert_merged(f, &["--run", RUN, "--task", &task], why);
}

/// Every file under `dir` (recursive) whose name is `name`.
fn find_named(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd {
        let p = e.unwrap().path();
        if p.is_dir() {
            find_named(&p, name, out);
        } else if p.file_name().map(|n| n == name).unwrap_or(false) {
            out.push(p);
        }
    }
}

/// `<run dir>/<RUN>.json` under the sandboxed `$HOME/.condukt`.
fn run_state_file(f: &Fx) -> PathBuf {
    let mut found = Vec::new();
    find_named(&f.home.join(".condukt"), &format!("{RUN}.json"), &mut found);
    assert_eq!(found.len(), 1, "fixture: one run state file: {found:?}");
    found.remove(0)
}

/// `archive_dir` of proof_archive.rs: `<run dir>/<run>.tdd-proofs/<task>`,
/// proofs in its `.tdd/`.
fn archive_proof_dir(f: &Fx) -> PathBuf {
    run_state_file(f)
        .parent()
        .unwrap()
        .join(format!("{RUN}.tdd-proofs"))
        .join(&f.task)
        .join(".tdd")
}

fn merge_conflict_records(f: &Fx) -> Vec<PathBuf> {
    let mut found = Vec::new();
    find_named(
        &f.home.join(".overwatch"),
        "merge_conflicts.jsonl",
        &mut found,
    );
    found.retain(|p| {
        std::fs::read_to_string(p)
            .map(|t| !t.trim().is_empty())
            .unwrap_or(true)
    });
    found
}


// ── RED: abandon + re-create leaves the discarded attempt's archive ────────

/// Attempt 1 records real proofs, is abandoned (`state abandon --force`) and
/// its worktree removed (which archives its proofs). Attempt 2 re-creates the
/// task id's worktree and records NEW real proofs. The merge must not be
/// refused for disagreeing with the discarded attempt's archive.
///
/// Readings: (a) defect: abandon/re-create should invalidate or version the
/// archive, else every retry is unmergeable; (b) intended: fail-closed on any
/// proof disagreement, the operator clears the stale archive (R5 test
/// `r5_worktree_and_archive_disagree_is_undetermined_proof` pins disagreement
/// as a refusal). This test pins (a) for the abandon/re-create lifecycle only.
#[test]
#[ignore = "RED(20c08a70): merge refused undetermined-proof because the archive holds the abandoned attempt's proofs"]
fn abandon_then_recreate_task_merges_with_new_proofs() {
    let f = setup(FIX);
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    ok(
        &condukt(
            &f.repo,
            &f.home,
            &[
                "state",
                "abandon",
                "--run",
                RUN,
                "--task",
                "t1",
                "--force",
                "abandoned for retry",
            ],
        ),
        "state abandon",
    );
    ok(
        &condukt(
            &f.repo,
            &f.home,
            &[
                "worktree",
                "remove",
                "--path",
                f.wt.to_str().unwrap(),
                "--branch",
                &f.branch,
            ],
        ),
        "worktree remove",
    );
    assert!(
        archive_proof_dir(&f).exists(),
        "fixture: abandoned attempt's proofs are archived"
    );
    let wt2 = PathBuf::from(ok(
        &condukt(
            &f.repo,
            &f.home,
            &[
                "worktree", "create", "--run", RUN, "--topic", "t1", "--branch", "t1",
            ],
        ),
        "worktree create 2",
    ));
    ok(
        &condukt(
            &f.repo,
            &f.home,
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
                wt2.to_str().unwrap(),
            ],
        ),
        "state set running 2",
    );
    let f2 = Fx {
        _tmp: tempfile::tempdir().unwrap(),
        repo: f.repo.clone(),
        home: f.home.clone(),
        branch: git(&wt2, &["branch", "--show-current"]),
        wt: wt2,
        task: "t1".to_string(),
    };
    std::fs::write(f2.wt.join("impl2.txt"), "impl attempt 2\n").unwrap();
    f2.commit(&["impl2.txt"], "implement 2");
    record(&f2, &single("none", "good"));
    assert_isolated(&f2, 1);
    assert_task_merge_succeeds(&f2, "retry after abandon");
}
