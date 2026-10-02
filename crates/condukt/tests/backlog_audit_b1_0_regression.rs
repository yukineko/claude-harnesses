// Integration test: unwrap/expect/panic are allowed (the workspace lint denies
// them for production code only).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Closure regression tests written by an independent verifier for the
//! backlog-closure audit (batch b1_0). Each test names the backlog id(s) it
//! pins. Non-ignored tests prove a closure (observed GREEN at HEAD and RED with
//! the fix construct removed). `#[ignore]`d tests pin a defect that is STILL
//! OPEN under its surviving duplicate target: they are RED today and are meant
//! to be un-ignored by whoever fixes that target.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/condukt has a repo root two levels up")
        .to_path_buf()
}

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "condukt-audit-b1-0-{tag}-{}-{}",
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create isolated dir");
    dir
}

fn condukt(dir: &Path, home: &Path, extra_path: Option<&Path>, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_condukt"));
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .stdin(Stdio::null());
    if let Some(p) = extra_path {
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![p.to_path_buf()];
        paths.extend(std::env::split_paths(&existing));
        cmd.env("PATH", std::env::join_paths(paths).unwrap());
    }
    cmd.output().expect("condukt spawns")
}

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t.t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t.t")
        .output()
        .expect("git spawns (failing rather than skipping)");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}{}",
        dir.display(),
        s(&out.stdout),
        s(&out.stderr)
    );
    s(&out.stdout)
}

fn git_repo(tag: &str) -> (PathBuf, PathBuf) {
    let base = unique_dir(tag);
    let repo = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git_ok(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
    git_ok(&repo, &["add", "seed.txt"]);
    git_ok(&repo, &["commit", "-q", "-m", "seed"]);
    (repo, home)
}

// ---------------------------------------------------------------------------
// 282fbb8d + 6a963a6a — flow SKILL AskUserQuestion count vs frozen allowlist
// ---------------------------------------------------------------------------

/// The frozen allowlist entry for flow's SKILL.md, parsed from the frozen test.
fn flow_allowlist_entry(src: &str) -> usize {
    let key = "(\"flow/skills/flow/SKILL.md\", ";
    let at = src.find(key).expect("flow entry present in ASK_ALLOWLIST");
    let rest = &src[at + key.len()..];
    let end = rest.find(')').expect("entry closes");
    rest[..end].trim().parse().expect("entry count is a number")
}

/// backlog 282fbb8d (allowlist 13 vs 15) and 6a963a6a (allowlist 10 vs 11):
/// both reported `askuserquestion_sites_match_frozen_allowlist` RED because
/// flow's SKILL.md grew `AskUserQuestion` lines the allowlist did not record.
/// Closing them requires (1) live count == allowlist, AND (2) the growth was
/// re-audited, not silently absorbed: the frozen rationale must name the audit
/// of the 11th (3214f55b) and the human ruling for the 14th/15th (ec878b2a,
/// arriving with d6ee8110).
#[test]
fn backlog_282fbb8d_6a963a6a_flow_ask_count_matches_audited_allowlist() {
    let root = repo_root();
    let frozen = std::fs::read_to_string(root.join("crates/condukt/tests/autonomy_invariant.rs"))
        .expect("frozen test readable");
    let skill = std::fs::read_to_string(root.join("crates/flow/skills/flow/SKILL.md"))
        .expect("flow SKILL readable");
    let live = skill
        .lines()
        .filter(|l| l.contains("AskUserQuestion"))
        .count();
    let allowed = flow_allowlist_entry(&frozen);
    assert_eq!(
        live, allowed,
        "282fbb8d/6a963a6a: flow SKILL.md has {live} AskUserQuestion line(s), the frozen \
         allowlist records {allowed}"
    );
    for (needle, why) in [
        (
            "3214f55b",
            "6a963a6a: the 11th site's re-audit must be recorded",
        ),
        (
            "ec878b2a",
            "282fbb8d: the 14th/15th sites' human ruling must be recorded",
        ),
        (
            "d6ee8110",
            "282fbb8d: the commit that added the 14th/15th must be named",
        ),
    ] {
        assert!(frozen.contains(needle), "{why} (missing `{needle}`)");
    }
}

// ---------------------------------------------------------------------------
// 866ab9f2 — absent decomposition: durable verify + named claim-release exit
// ---------------------------------------------------------------------------

fn tdd_bin_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let bin_path = profile_dir.join("tdd");
    if !bin_path.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let status = Command::new(&cargo)
            .args(["build", "-p", "tdd", "--bin", "tdd"])
            .status()
            .expect("spawn cargo build -p tdd");
        assert!(status.success(), "`cargo build -p tdd` failed");
    }
    assert!(bin_path.exists(), "tdd binary at {}", bin_path.display());
    profile_dir
}

fn find_named(root: &Path, name: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            find_named(&p, name, out);
        } else if p.file_name().and_then(|x| x.to_str()) == Some(name) {
            out.push(p);
        }
    }
}

/// backlog 866ab9f2: with the run's decomposition file ABSENT (ENOENT), a
/// `kind:fix` task with no proofs is promoted to `verified` (absence is an
/// observation, so the F->P gate does not apply), and `state set` then exits
/// NON-ZERO with the named `claim release NOT PERFORMED` diagnostic — the ruled
/// contract of backlog 9a4fb884 (commit 306cf008), i.e. the "RED" the item
/// reported was the spec, not a regression.
#[test]
fn backlog_866ab9f2_absent_decomposition_verifies_durably_and_names_the_release() {
    let dir = unique_dir("866ab9f2");
    let home = unique_dir("866ab9f2-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("audit-866ab9f2", "t-fix");
    let dec = serde_json::json!({
        "goal": "g",
        "tasks": [{"id": task, "title": "demo", "kind": "fix",
                   "reproduction_tests": "tests/repro.rs::reproduces"}]
    })
    .to_string();
    let dec_path = dir.join("decomposition.json");
    std::fs::write(&dec_path, dec).unwrap();
    let out = condukt(
        &dir,
        &home,
        Some(&tdd),
        &[
            "state",
            "init",
            "--run",
            run,
            "--file",
            dec_path.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "state init: {}", s(&out.stderr));

    let mut hits = Vec::new();
    find_named(
        &home.join(".condukt/state"),
        &format!("{run}.decomposition.json"),
        &mut hits,
    );
    assert_eq!(hits.len(), 1, "one persisted decomposition: {hits:?}");
    std::fs::remove_file(&hits[0]).unwrap();

    let out = condukt(
        &dir,
        &home,
        Some(&tdd),
        &[
            "state", "set", "--run", run, "--task", task, "--status", "verified",
        ],
    );
    let (code, err) = (out.status.code().unwrap_or(-1), s(&out.stderr));
    assert!(
        !err.contains("refusing to verify"),
        "866ab9f2: an ABSENT decomposition must not be refused by the F->P gate: {err}"
    );
    let show = condukt(&dir, &home, Some(&tdd), &["state", "show", "--run", run]);
    assert!(show.status.success(), "state show: {}", s(&show.stderr));
    let v: serde_json::Value = serde_json::from_slice(&show.stdout).expect("show is JSON");
    let status = v["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == task)
        .map(|t| t["status"].as_str().unwrap().to_string());
    assert_eq!(
        status.as_deref(),
        Some("verified"),
        "866ab9f2: the promotion must have LANDED durably: {err}"
    );
    assert_ne!(
        code, 0,
        "866ab9f2: an Undetermined claim release must exit non-zero (ruling 9a4fb884): {err}"
    );
    assert!(
        err.contains("claim release NOT PERFORMED"),
        "866ab9f2: the non-zero exit must be the NAMED claim-release diagnostic: {err}"
    );
}

// ---------------------------------------------------------------------------
// STILL-OPEN duplicate pins (ignored; RED today)
// ---------------------------------------------------------------------------

/// backlog 43393ce2 == 4708069b: run-state is addressed by
/// `repo_root(cwd)`, which for a LINKED WORKTREE is the worktree itself, so a
/// run created from a worktree is invisible to `state list` from the main tree.
/// RED while the split exists.
#[test]
#[ignore = "pins OPEN backlog 4708069b/43393ce2 (run-state split per checkout); un-ignore when fixed"]
fn backlog_43393ce2_4708069b_run_created_in_worktree_is_listed_from_main_tree() {
    let (repo, home) = git_repo("43393ce2");
    let wt = repo.parent().unwrap().join("wt");
    git_ok(
        &repo,
        &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()],
    );
    let dec_path = wt.join("decomposition.json");
    std::fs::write(
        &dec_path,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"x"}]}"#,
    )
    .unwrap();
    let run = "audit-43393ce2";
    let out = condukt(
        &wt,
        &home,
        None,
        &[
            "state",
            "init",
            "--run",
            run,
            "--file",
            dec_path.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "state init in worktree: {}",
        s(&out.stderr)
    );
    let from_wt = s(&condukt(&wt, &home, None, &["state", "list"]).stdout);
    assert!(
        from_wt.contains(run),
        "control: listed from the worktree: {from_wt}"
    );
    let from_main = s(&condukt(&repo, &home, None, &["state", "list"]).stdout);
    assert!(
        from_main.contains(run),
        "43393ce2/4708069b: a run created in a linked worktree is invisible from the main \
         tree's `state list` (got: {from_main:?})"
    );
}

/// backlog c10bbb8e == 752e006a: `worktree create --run R --branch R/topic`
/// silently produces `R/R/topic`. RED while the double prefix exists.
#[test]
#[ignore = "pins OPEN backlog 752e006a/c10bbb8e (run prefix doubled into --branch); un-ignore when fixed"]
fn backlog_c10bbb8e_752e006a_run_qualified_branch_is_not_double_prefixed() {
    let (repo, home) = git_repo("c10bbb8e");
    let run = "flow-34385c9e";
    let out = condukt(
        &repo,
        &home,
        None,
        &[
            "worktree",
            "create",
            "--run",
            run,
            "--topic",
            "circuit-stateless",
            "--branch",
            "flow-34385c9e/circuit-stateless",
        ],
    );
    let branches = git_ok(&repo, &["branch", "--list", "--format=%(refname:short)"]);
    assert!(
        !branches.contains("flow-34385c9e/flow-34385c9e/"),
        "c10bbb8e/752e006a: the run id was doubled into the branch name. create exit={:?} \
         stderr={} branches:\n{branches}",
        out.status.code(),
        s(&out.stderr)
    );
}

/// backlog 057fa13c + 0f0425d5 == e494a8a3: the intermittently RED
/// `every_invocation_is_journaled_even_when_nothing_is_recorded` resolves its
/// ledger through `Config::load()` — the REAL state dir — rather than a
/// test-private one, so it shares state with everything else on the machine
/// (e494a8a3 step 2: "pin HOME and state dir"). RED while the test body still
/// loads the live config.
#[test]
#[ignore = "pins OPEN backlog e494a8a3/057fa13c/0f0425d5 (diffrisk ledger test not isolated); un-ignore when fixed"]
fn backlog_057fa13c_0f0425d5_e494a8a3_diffrisk_journal_test_pins_its_state_dir() {
    let src =
        std::fs::read_to_string(repo_root().join("crates/condukt/src/diffrisk_record.rs")).unwrap();
    let at = src
        .find("fn every_invocation_is_journaled_even_when_nothing_is_recorded")
        .expect("the flaky test still exists under its name");
    let body = &src[at..];
    let body = &body[..body.find("\n    }\n").unwrap_or(body.len())];
    assert!(
        !body.contains("Config::load()"),
        "057fa13c/0f0425d5/e494a8a3: the diffrisk journaling test still resolves its ledger \
         via Config::load() (the live state dir), so concurrent tests/sessions share it"
    );
}

/// backlog b0934634 == 7100bb4e: the dead-session worktree GC
/// (`condukt worktree reconcile --remove`) exists but nothing drives it. RED
/// while no hook, skill or git hook invokes it.
#[test]
#[ignore = "pins OPEN backlog 7100bb4e/b0934634 (worktree GC not auto-driven); un-ignore when fixed"]
fn backlog_b0934634_7100bb4e_worktree_gc_is_driven_automatically() {
    let root = repo_root();
    let mut files = Vec::new();
    fn walk(p: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(p) else {
            return;
        };
        for e in rd.flatten() {
            let q = e.path();
            if q.is_dir() {
                walk(&q, out);
            } else {
                out.push(q);
            }
        }
    }
    for c in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        walk(&c.path().join("hooks"), &mut files);
        walk(&c.path().join("skills"), &mut files);
    }
    walk(&root.join(".githooks"), &mut files);
    let driven: Vec<_> = files
        .iter()
        .filter(|f| {
            std::fs::read_to_string(f)
                .map(|t| t.contains("worktree reconcile") && t.contains("--remove"))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        !driven.is_empty(),
        "b0934634/7100bb4e: no hook/skill/githook invokes `condukt worktree reconcile --remove`"
    );
}
