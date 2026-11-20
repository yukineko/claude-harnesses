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

// ── R8 control ──────────────────────────────────────────────────────────────

/// R8: valid real proofs, scripts committed before GREEN -> merges.
#[test]
fn control_valid_real_proofs_merge() {
    let f = setup(FIX);
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    assert_task_merge_succeeds(&f, "R8 valid real proofs");
}

// ── R1 ──────────────────────────────────────────────────────────────────────

/// R1 (i): a fix task with no proof file at all.
#[test]
fn r1_no_proofs_is_refused_i() {
    let f = setup(FIX);
    assert!(!proof_dir(&f).exists(), "fixture: no proofs");
    assert_task_merge_refused(&f, "i", "R1 no proofs");
}

/// R1 missing-green: the only command has a RED slot and no GREEN slot.
#[test]
fn r1_red_without_green_is_refused_missing_green() {
    let f = setup(FIX);
    record(
        &f,
        &[Cmd {
            script: T_SH,
            mode_file: MODE,
            red: "none",
            green: None,
        }],
    );
    assert!(slots(&f, "green").is_empty() && slots(&f, "red").len() == 1);
    assert_task_merge_refused(&f, "missing-green", "R1 RED slot without GREEN");
}

/// R1 missing-green: command 1 is RED+GREEN, command 2 only RED.
#[test]
fn r1_second_command_red_without_green_is_refused_missing_green() {
    let f = setup(FIX);
    record(
        &f,
        &[
            Cmd {
                script: T_SH,
                mode_file: MODE,
                red: "none",
                green: Some("good"),
            },
            Cmd {
                script: "u.sh",
                mode_file: "mode2.txt",
                red: "none",
                green: None,
            },
        ],
    );
    assert_isolated(&f, 1);
    assert_eq!(slots(&f, "red").len(), 2, "fixture: two RED slots");
    assert_task_merge_refused(&f, "missing-green", "R1 2nd command has no GREEN");
}

/// R1 control: a GREEN slot whose RED file is gone is not by itself refused.
#[test]
fn r1_control_green_without_red_is_not_refused() {
    let f = setup(FIX);
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    for r in slots(&f, "red") {
        std::fs::remove_file(r).unwrap();
    }
    assert!(slots(&f, "red").is_empty());
    assert_task_merge_succeeds(&f, "R1 GREEN without RED is not a refusal reason");
}

// ── R2 (f) ──────────────────────────────────────────────────────────────────

/// R2 (f): m::b failed at RED and is not passed at GREEN.
#[test]
fn r2_green_misses_red_failure_is_refused_f() {
    let f = setup(FIX);
    record(&f, &single("none", "lost"));
    assert_isolated(&f, 1);
    assert_task_merge_refused(&f, "f", "R2 red(none)/green(lost)");
}

/// R2 (f): two commands; only the 2nd (hashed) GREEN slot violates (f).
#[test]
fn r2_second_green_slot_violating_f_is_refused() {
    let f = setup(FIX);
    record(
        &f,
        &[
            Cmd {
                script: T_SH,
                mode_file: MODE,
                red: "none",
                green: Some("good"),
            },
            Cmd {
                script: "u.sh",
                mode_file: "mode2.txt",
                red: "none",
                green: Some("lost"),
            },
        ],
    );
    assert_isolated(&f, 2);
    assert_eq!(
        read_json(&proof_dir(&f).join("t1.green.json"))["cmd"].as_str(),
        Some("sh t.sh"),
        "fixture: the first (passing) command holds the legacy-named slot"
    );
    assert_task_merge_refused(&f, "f", "R2 only the hashed GREEN slot violates (f)");
}

// ── R3 (g) ──────────────────────────────────────────────────────────────────

/// R3 (g): GREEN ran zero tests (determined count 0).
#[test]
fn r3_green_zero_passed_is_refused_g() {
    let f = setup(FIX);
    record(&f, &single("nocompile", "zero"));
    assert_isolated(&f, 1);
    let g = read_json(&slots(&f, "green")[0]);
    assert_eq!(g["counts"]["passed"].as_u64(), Some(0), "fixture: {g}");
    assert_task_merge_refused(&f, "g", "R3 red(nocompile)/green(zero)");
}

// ── R4 (h) ──────────────────────────────────────────────────────────────────

/// R4 (h): a trailered commit after GREEN changes the tip tree.
#[test]
fn r4_commit_after_green_is_refused_h() {
    let f = setup(FIX);
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    std::fs::write(f.wt.join("late.txt"), "changed after GREEN\n").unwrap();
    f.commit(&["late.txt"], "late change");
    assert_task_merge_refused(&f, "h", "R4 commit after GREEN");
}

/// R4: committing only `.tdd/` after GREEN keeps (h) (proof dir excluded).
#[test]
fn r4_control_committing_only_proof_dir_after_green_merges() {
    let f = setup(FIX);
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    f.commit(&[".tdd"], "commit proofs");
    assert_ne!(
        f.tip_tree(),
        read_json(&slots(&f, "green")[0])["tree_identity"]["tree"]
            .as_str()
            .unwrap(),
        "fixture: the raw tip tree now differs only by .tdd/"
    );
    assert_task_merge_succeeds(&f, "R4 .tdd-only commit after GREEN");
}

// ── R5 undetermined-proof ───────────────────────────────────────────────────

/// Valid real proofs, then `mutate` the legacy GREEN slot's JSON.
fn undetermined_proof_case(why: &str, mutate: impl FnOnce(&mut serde_json::Value)) {
    let f = setup(FIX);
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    let p = proof_dir(&f).join("t1.green.json");
    let mut v = read_json(&p);
    mutate(&mut v);
    std::fs::write(&p, v.to_string()).unwrap();
    assert_task_merge_refused(&f, "undetermined-proof", why);
}

#[test]
fn r5_red_failed_tests_null_is_undetermined_proof() {
    undetermined_proof_case("R5 red_failed_tests null", |v| {
        v["red_failed_tests"] = serde_json::Value::Null;
    });
}

#[test]
fn r5_red_failed_tests_missing_is_undetermined_proof() {
    undetermined_proof_case("R5 red_failed_tests missing", |v| {
        v.as_object_mut().unwrap().remove("red_failed_tests");
    });
}

#[test]
fn r5_counts_missing_is_undetermined_proof() {
    undetermined_proof_case("R5 counts missing", |v| {
        v.as_object_mut().unwrap().remove("counts");
    });
}

#[test]
fn r5_counts_not_determined_is_undetermined_proof() {
    undetermined_proof_case("R5 counts.determined false", |v| {
        v["counts"] = serde_json::json!({"determined": false, "reason": "x"});
    });
}

#[test]
fn r5_tree_identity_null_is_undetermined_proof() {
    undetermined_proof_case("R5 tree_identity null", |v| {
        v["tree_identity"] = serde_json::Value::Null;
    });
}

#[test]
fn r5_tree_identity_missing_is_undetermined_proof() {
    undetermined_proof_case("R5 tree_identity missing", |v| {
        v.as_object_mut().unwrap().remove("tree_identity");
    });
}

#[test]
fn r5_tree_null_is_undetermined_proof() {
    undetermined_proof_case("R5 tree_identity.tree null", |v| {
        v["tree_identity"]["tree"] = serde_json::Value::Null;
    });
}

#[test]
fn r5_tree_missing_is_undetermined_proof() {
    undetermined_proof_case("R5 tree_identity.tree missing", |v| {
        v["tree_identity"].as_object_mut().unwrap().remove("tree");
    });
}

/// R5: a GREEN slot that does not parse.
#[test]
fn r5_unparsable_green_is_undetermined_proof() {
    let f = setup(FIX);
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    std::fs::write(proof_dir(&f).join("t1.green.json"), "{ not json").unwrap();
    assert_task_merge_refused(&f, "undetermined-proof", "R5 unparsable GREEN");
}

/// R5: old-layout hand-written proofs (the `write_valid_proofs` shape of
/// backlog_20261117_merge_without_tdd_proofs.rs) carry no per-test fields.
#[test]
fn r5_old_layout_hand_written_proofs_are_undetermined_proof() {
    let f = setup(FIX);
    let d = proof_dir(&f);
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
    assert_task_merge_refused(&f, "undetermined-proof", "R5 old-layout proofs");
}

/// Restores the proof dir's mode so the tempdir can be removed.
struct Unlock(PathBuf);
impl Drop for Unlock {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// R5: the proof dir cannot be listed (slot_files read error).
#[test]
fn r5_unreadable_proof_dir_is_undetermined_proof() {
    use std::os::unix::fs::PermissionsExt;
    let f = setup(FIX);
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    let d = proof_dir(&f);
    let _unlock = Unlock(d.clone());
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        std::fs::read_dir(&d).is_err(),
        "fixture precondition: the proof dir is unreadable (not running as root)"
    );
    assert_task_merge_refused(&f, "undetermined-proof", "R5 unreadable proof dir");
}

/// Copy the worktree proofs into the archive dir; `edit` the GREEN copy.
fn archive_copy(f: &Fx, edit: bool) {
    let a = archive_proof_dir(f);
    std::fs::create_dir_all(&a).unwrap();
    for p in slots(f, "red").into_iter().chain(slots(f, "green")) {
        let name = p.file_name().unwrap().to_os_string();
        let mut text = std::fs::read_to_string(&p).unwrap();
        if edit && name.to_string_lossy().ends_with(".green.json") {
            let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
            v["output_tail"] = serde_json::Value::String("a different run".to_string());
            text = v.to_string();
        }
        std::fs::write(a.join(name), text).unwrap();
    }
}

/// R5: worktree and archive both hold the GREEN proof with different content.
#[test]
fn r5_worktree_and_archive_disagree_is_undetermined_proof() {
    let f = setup(FIX);
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    archive_copy(&f, true);
    assert_task_merge_refused(
        &f,
        "undetermined-proof",
        "R5 worktree and archive proofs differ",
    );
}

/// Control for the case above: an identical archive copy is not a conflict.
#[test]
fn r5_control_identical_archive_copy_merges() {
    let f = setup(FIX);
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    archive_copy(&f, false);
    assert_task_merge_succeeds(&f, "R5 identical archive copy");
}

// ── R5 undetermined-task ────────────────────────────────────────────────────

/// R5: the named task (`ghost`, its branch's recorded identity) is absent
/// from run state. Proofs for it are valid, so only the task can refuse.
#[test]
fn r5_task_absent_from_run_state_is_undetermined_task() {
    let f = setup(Spec {
        topic: "ghost",
        set_running: false,
        ..FIX
    });
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    assert_task_merge_refused(&f, "undetermined-task", "R5 task not in run state");
}

/// R5: the run state file does not parse.
#[test]
fn r5_unreadable_run_state_is_undetermined_task() {
    let f = setup(FIX);
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    std::fs::write(run_state_file(&f), "{ corrupt").unwrap();
    assert_task_merge_refused(&f, "undetermined-task", "R5 corrupt run state");
}

/// R5: the task has no `kind`.
#[test]
fn r5_task_without_kind_is_undetermined_task() {
    let f = setup(Spec {
        kind: None,
        set_running: false,
        ..FIX
    });
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    assert_task_merge_refused(&f, "undetermined-task", "R5 task without kind");
}

// ── R6 ──────────────────────────────────────────────────────────────────────

/// R6: a contract violation that would also conflict is refused with the
/// token before the trial merge, and no merge-conflict record is written.
#[test]
fn r6_violating_and_conflicting_branch_is_refused_before_conflict_record() {
    let f = setup(Spec {
        conflict: true,
        ..FIX
    });
    assert_task_merge_refused(&f, "i", "R6 violating + conflicting");
    let recs = merge_conflict_records(&f);
    assert!(
        recs.is_empty(),
        "R6: a contract refusal must not record a merge conflict: {recs:?}"
    );
    assert!(
        !git_raw(&f.repo, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
            .status
            .success(),
        "R6: no merge in progress after the refusal"
    );
}

/// R6: no --run/--task; the branch's conduktTask names a proof-less fix task.
#[test]
fn r6_merge_without_flags_resolves_task_from_record_i() {
    let f = setup(FIX);
    assert_refused(&f, &[], "i", "R6 conduktTask -> proof-less fix task");
}

fn unset_task_record(f: &Fx) {
    let key = format!("branch.{}.conduktTask", f.branch);
    git(&f.repo, &["config", "--local", "--unset", &key]);
    assert!(
        !git_raw(&f.repo, &["config", "--local", "--get", &key])
            .status
            .success(),
        "fixture: conduktTask record removed"
    );
}

/// R6 control: no conduktTask record and no flags -> the contract is not
/// checked (the base record is kept, so the other guards still pass).
#[test]
fn r6_control_no_task_record_is_not_checked() {
    let f = setup(FIX);
    unset_task_record(&f);
    assert_merged(&f, &[], "R6 branch with no conduktTask record");
}

/// R6 control: kind=chore without proofs merges.
#[test]
fn r6_control_chore_without_proofs_merges() {
    let f = setup(Spec {
        kind: Some("chore"),
        ..FIX
    });
    assert_task_merge_succeeds(&f, "R6 chore without proofs");
}

/// R6: proofs exist only as committed `.tdd/` at the branch tip.
#[test]
fn r6_proofs_only_committed_at_tip_is_refused_i() {
    let f = setup(FIX);
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    f.commit(&[".tdd"], "commit proofs");
    std::fs::remove_dir_all(proof_dir(&f)).unwrap();
    assert!(git(&f.repo, &["ls-tree", "-r", "--name-only", &f.branch]).contains(".tdd/"));
    assert_task_merge_refused(&f, "i", "R6 committed .tdd/ is not a proof source");
}

// ── R7 resolve-merge ────────────────────────────────────────────────────────

/// `$HOME/.overwatch/<project key>/overwatch/` (one project in the sandbox).
fn store_dir(f: &Fx) -> PathBuf {
    let mut found = Vec::new();
    find_named(
        &f.home.join(".overwatch"),
        "merge_conflicts.jsonl",
        &mut found,
    );
    assert_eq!(found.len(), 1, "expected one overwatch store: {found:?}");
    found.remove(0).parent().unwrap().to_path_buf()
}

/// Conflicting fix task with valid real proofs; the merge is recorded as a
/// conflict (valid proofs pass the contract) and `choice` is recorded as the
/// human resolution. Returns the fixture and the conflict id.
fn held(extra: &[&str], choice: &str) -> (Fx, String) {
    let f = setup(Spec {
        conflict: true,
        ..FIX
    });
    record(&f, &single("none", "good"));
    assert_isolated(&f, 1);
    let out = merge_with(&f, extra);
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("CONFLICT"),
        "fixture precondition: the merge is recorded as a conflict; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(store_dir(&f).join("merge_conflicts.jsonl")).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "expected one conflict entry: {text:?}");
    let id = serde_json::from_str::<serde_json::Value>(lines[0]).unwrap()["conflict_id"]
        .as_str()
        .unwrap()
        .to_string();
    use std::io::Write;
    let line = serde_json::json!({
        "conflict_id": id, "choice": choice, "decided_by": "human",
        "note": null, "ts": 1_700_000_000_i64,
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store_dir(&f).join("merge_conflict_resolutions.jsonl"))
        .unwrap();
    writeln!(file, "{line}").unwrap();
    (f, id)
}

fn held_task_named(choice: &str) -> (Fx, String) {
    held(&["--run", RUN, "--task", "t1"], choice)
}

fn resolve(f: &Fx, id: &str) -> Output {
    let out = condukt(&f.repo, &f.home, &["worktree", "resolve-merge", "--id", id]);
    show("resolve-merge", &out);
    out
}

fn assert_resolve_refused(f: &Fx, id: &str, item: &str, why: &str) {
    let before = git(&f.repo, &["rev-parse", "HEAD"]);
    let main_before = f.main_sha();
    let out = resolve(f, id);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success() && stderr.contains(&token(item)),
        "{why}: expected non-zero exit and {:?} on stderr; exit={:?} stderr={stderr}",
        token(item),
        out.status.code()
    );
    assert_eq!(f.main_sha(), main_before, "{why}: main must not move");
    assert_eq!(
        git(&f.repo, &["rev-parse", "HEAD"]),
        before,
        "{why}: HEAD must equal head_before"
    );
    assert!(
        !git_raw(&f.repo, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
            .status
            .success(),
        "{why}: no merge may be left in progress"
    );
    assert!(
        git_raw(&f.repo, &["diff", "--cached", "--quiet"])
            .status
            .success(),
        "{why}: nothing may be left staged"
    );
}

/// R7 Theirs: a trailered commit after GREEN (h is compared with the tip at
/// resolve time) -> refused before the merge.
#[test]
fn r7_resolve_theirs_with_violation_is_refused_h() {
    let (f, id) = held_task_named("theirs");
    std::fs::write(f.wt.join("late.txt"), "changed after GREEN\n").unwrap();
    f.commit(&["late.txt"], "late change");
    assert_resolve_refused(&f, &id, "h", "R7 Theirs after a post-GREEN commit");
}

/// R7 Manual, clean-mergeable path: the branch now matches main's
/// `base.txt` (so `git merge` is clean) and its proofs are gone -> refused,
/// rolled back, nothing in progress.
#[test]
fn r7_resolve_manual_clean_with_violation_is_refused_and_rolled_back() {
    let (f, id) = held_task_named("manual");
    std::fs::write(f.wt.join("base.txt"), "main version\n").unwrap();
    f.commit(&["base.txt"], "resolve on the branch");
    std::fs::remove_dir_all(proof_dir(&f)).unwrap();
    assert_resolve_refused(&f, &id, "i", "R7 clean Manual without proofs");
}

/// R7 control: Ours is not refused by the contract.
#[test]
fn r7_control_resolve_ours_is_not_refused() {
    let (f, id) = held_task_named("ours");
    std::fs::remove_dir_all(proof_dir(&f)).unwrap();
    let out = resolve(&f, &id);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success() && !stderr.contains("merge-proof-contract("),
        "R7 Ours must not be refused by the contract; stderr={stderr}"
    );
}

/// R7 control: Manual that still conflicts (ManualPending) is not refused.
#[test]
fn r7_control_resolve_manual_pending_is_not_refused() {
    let (f, id) = held_task_named("manual");
    std::fs::remove_dir_all(proof_dir(&f)).unwrap();
    let out = resolve(&f, &id);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success()
            && String::from_utf8_lossy(&out.stdout).contains("manual '")
            && !stderr.contains("merge-proof-contract("),
        "R7 ManualPending must not be refused by the contract; stderr={stderr}"
    );
}

/// R7 control: a branch with no conduktTask record is not checked.
#[test]
fn r7_control_resolve_without_task_record_is_not_checked() {
    let (f, id) = held(&[], "theirs");
    unset_task_record(&f);
    std::fs::remove_dir_all(proof_dir(&f)).unwrap();
    let out = resolve(&f, &id);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success() && !stderr.contains("merge-proof-contract("),
        "R7 no conduktTask record: contract not checked; stderr={stderr}"
    );
}
