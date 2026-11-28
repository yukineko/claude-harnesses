#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(verifier round 12, 2026-11-28, against 4d9d37f28 / blastguard 0.2.1002):
//! reach paths of the askrules judge after the round-11 fix (a scratchpad
//! repository is `Scratchpad` only when `.git` is a real directory whose
//! `config` has no `[core] worktree` and no `[include*]`). Written by an
//! independent verifier, not the implementer (CLAUDE.md §2(a)). Black-box:
//! spawns the built binary with a sandbox HOME and a learned table whose ONLY
//! allow entry is the key `blastguard shape` prints for a benign sibling.
//!
//! Fixtures: the round-10/11 production layout (HOME is an ancestor of the
//! main tree `<home>/src/repo` and of its linked worktrees
//! `<home>/src/.harness-worktrees/{wt,wt2}`; the scratchpad is
//! `<root>/scratchpad`) in a fresh `tempfile` directory under
//! `/private/tmp/claude-askrules`.
//!
//! What these tests do NOT prove: that the learner would actually promote the
//! entry (the table is written by hand); that a session would build the
//! forged layout on its own (the fixture writes it directly); anything about
//! shapes not listed here.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000f2";
const BASE: &str = "/private/tmp/claude-askrules";

struct Env {
    _dir: tempfile::TempDir,
    _home: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    wt: PathBuf,
    main: PathBuf,
    sp: PathBuf,
}

fn git_cmd(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = git_cmd(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git(dir: &Path, args: &[&str]) {
    git_out(dir, args);
}

fn env_prod(name: &str) -> Env {
    std::fs::create_dir_all(BASE).unwrap();
    let dir = tempfile::Builder::new()
        .prefix(&format!("v12-{name}-"))
        .tempdir_in(BASE)
        .unwrap();
    let root = dir.path().to_path_buf();
    let home_dir = tempfile::Builder::new()
        .prefix("home-")
        .tempdir_in(&root)
        .unwrap();
    let home = home_dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("approvals")).unwrap();
    let main = home.join("src").join("repo");
    std::fs::create_dir_all(main.join("src")).unwrap();
    std::fs::write(main.join("src").join("lib.rs"), "x").unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["add", "src"]);
    git(&main, &["commit", "-q", "-m", "i"]);
    // Another session's unmerged branch in the main repository.
    git(&main, &["branch", "side"]);
    let wt = home.join("src").join(".harness-worktrees").join("wt");
    git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    std::fs::write(main.join("untracked_wip.rs"), "wip").unwrap();
    std::fs::create_dir_all(wt.join("build")).unwrap();
    std::fs::write(wt.join("new.rs"), "n").unwrap();
    let sp = root.join("scratchpad");
    std::fs::create_dir_all(sp.join("build")).unwrap();
    Env {
        _dir: dir,
        _home: home_dir,
        root,
        home,
        wt,
        main,
        sp,
    }
}

/// A plain scratchpad repository (the benign sibling's cwd).
fn plain_repo(at: &Path) {
    std::fs::create_dir_all(at.join("src")).unwrap();
    std::fs::write(at.join("src").join("a.rs"), "a").unwrap();
    git(at, &["init", "-q"]);
    git(at, &["add", "src"]);
    git(at, &["commit", "-q", "-m", "i"]);
    git(at, &["branch", "side"]);
    std::fs::write(at.join("new.rs"), "n").unwrap();
    std::fs::create_dir_all(at.join("build")).unwrap();
}

fn sp_repo(e: &Env) -> PathBuf {
    e.sp.join("r")
}

/// A scratchpad repository whose `.git/config` is replaced by `config`.
fn sp_config(e: &Env, name: &str, config: &str) -> PathBuf {
    let d = e.sp.join(name);
    std::fs::create_dir_all(&d).unwrap();
    git(&d, &["init", "-q"]);
    std::fs::write(d.join(".git").join("config"), config).unwrap();
    d
}

fn spawn_env(
    e: &Env,
    table: &Path,
    args: &[&str],
    stdin: &str,
    extra: &[(&str, &str)],
) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    c.args(args)
        .env("HOME", &e.home)
        .env("BLASTGUARD_BLOCK_LOG_DIR", e.root.join("blocklog"))
        .env("BLASTGUARD_RELAY_DIR", e.root.join("relay"))
        .env("BLASTGUARD_APPROVALS_DIR", e.root.join("approvals"))
        .env("BLASTGUARD_LEARNED_TABLE", table)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("BLASTGUARD_ASK_RULES")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        c.env(k, v);
    }
    let out = bounded::run_bounded(&mut c, stdin.as_bytes(), bounded::HOOK_DEADLINE);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn no_table(e: &Env) -> PathBuf {
    e.root.join("no-table.json")
}

fn decide_env(e: &Env, table: &Path, p: &Value, extra: &[(&str, &str)]) -> String {
    let (_, out, err) = spawn_env(e, table, &[], &p.to_string(), extra);
    if out.trim().is_empty() {
        return "allow".to_string();
    }
    let d: Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|x| panic!("stdout not JSON ({x}): {out:?} stderr={err:?}"));
    d["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision in {d}"))
        .to_string()
}

fn decide_with(e: &Env, table: &Path, p: &Value) -> String {
    decide_env(e, table, p, &[])
}

fn bash(cwd: &Path, command: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID,
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
}

fn shape_env(e: &Env, p: &Value, extra: &[(&str, &str)]) -> Value {
    let (code, out, err) = spawn_env(e, &no_table(e), &["shape"], &p.to_string(), extra);
    assert_eq!(code, 0, "shape failed: {out:?} {err:?}");
    serde_json::from_str(out.trim()).unwrap()
}

fn shape(e: &Env, p: &Value) -> Value {
    shape_env(e, p, &[])
}

fn table_with(e: &Env, k: &str) -> PathBuf {
    let p = e.root.join("t.json");
    let entry = json!({"shape": k, "decision": "allow", "approvals": 50, "declines": 0,
                       "sessions": 20, "samples": []});
    std::fs::write(&p, json!({"version": 1, "entries": [entry]}).to_string()).unwrap();
    p
}

fn cmd_of(p: &Value) -> String {
    p["tool_input"]["command"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// Learn `sibling` allow; `probe` must not become Allow. Returns the failure
/// text (`None` when no ride). `extra` is the hook process environment for the
/// probe only.
fn ride_env(e: &Env, sibling: &Value, probe: &Value, extra: &[(&str, &str)]) -> Option<String> {
    let before = decide_env(e, &no_table(e), probe, extra);
    assert_ne!(
        before,
        "allow",
        "precondition: probe {:?} must not be Allow without a table",
        cmd_of(probe)
    );
    let sib = shape(e, sibling);
    assert_eq!(
        sib["decision"],
        "ask",
        "precondition: sibling {:?} must Ask: {sib}",
        cmd_of(sibling)
    );
    let k = sib["shape"].as_str().unwrap().to_string();
    let t = table_with(e, &k);
    let sib_own = decide_with(e, &t, sibling);
    assert_eq!(
        sib_own,
        "allow",
        "precondition: sibling {:?} is Allow with its own entry {k}",
        cmd_of(sibling)
    );
    let after = decide_env(e, &t, probe, extra);
    (after == "allow").then(|| {
        format!(
            "probe `{}` (cwd {}) was {before} without a table but Allow once the sibling `{}` \
             (cwd {}) was learned (key {k}); probe shape {}",
            cmd_of(probe),
            probe["cwd"].as_str().unwrap_or(""),
            cmd_of(sibling),
            sibling["cwd"].as_str().unwrap_or(""),
            shape_env(e, probe, extra)
        )
    })
}

fn ride(e: &Env, sibling: &Value, probe: &Value) -> Option<String> {
    ride_env(e, sibling, probe, &[])
}

/// The work tree git uses in `dir` (canonical), or `None` when git has none.
fn toplevel(dir: &Path) -> Option<PathBuf> {
    let out = git_cmd(dir, &["rev-parse", "--show-toplevel"]);
    out.status
        .success()
        .then(|| std::fs::canonicalize(String::from_utf8_lossy(&out.stdout).trim()).unwrap())
}

// ------------------------------------------------ scratchpad config parse

/// Configs `git_config_redirects` reads as "no redirect" although git sets
/// `core.worktree` from them (each checked below with real git:
/// `rev-parse --show-toplevel` is the main tree).
fn redirect_configs(m: &str) -> Vec<(&'static str, String)> {
    let head = "[core]\n\trepositoryformatversion = 0\n\tbare = false\n";
    vec![
        // A value continued with `\` swallows the next line: git reads
        // `[x]` as part of `core.foo`, so the `worktree` line is still in
        // `[core]`; the parser switches its section to `x`.
        (
            "cont",
            format!("{head}\tfoo = a\\\n[x]\n\tworktree = {m}\n"),
        ),
        // Two section headers on one line: the parser takes only the first.
        ("twohdr", format!("{head}[x][core]worktree = {m}\n")),
        // (A UTF-8 BOM before `[core]` was a candidate: git 2.54 does NOT
        // honour that core.worktree -- measured, the work tree stayed the
        // repository itself -- so it is no reach path and is not listed.)
    ]
}

/// Round-11 rule (spec "Lines an Allow can apply to" 9): a scratchpad
/// repository is `Scratchpad` only when its config has no `core.worktree`.
/// `git_config_redirects` decides that with a line scanner whose notion of
/// the current section differs from git's in (at least) three spellings
/// (`redirect_configs`). Precondition (real git): the work tree of each is
/// the main tree. Expected: not Allow on the plain scratchpad repository's
/// key.
#[test]
fn r12_config_parser_misses_core_worktree_git_honours() {
    let e = env_prod("parse");
    plain_repo(&sp_repo(&e));
    let m = e.main.display().to_string();
    let main_real = std::fs::canonicalize(&e.main).unwrap();
    let mut bad = vec![];
    for (name, cfg) in redirect_configs(&m) {
        let d = sp_config(&e, name, &cfg);
        assert_eq!(
            toplevel(&d).as_deref(),
            Some(main_real.as_path()),
            "precondition: git's work tree in `{name}` is the main tree"
        );
        for c in ["git add -A", "git clean -fdx"] {
            if let Some(x) = ride(&e, &bash(&sp_repo(&e), c), &bash(&d, c)) {
                bad.push(format!("[{name}] {x}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "a scratchpad repository whose config git reads as core.worktree=<main tree> rides the \
         plain scratchpad key:\n{}",
        bad.join("\n")
    );
}

/// `extensions.worktreeConfig` + `.git/config.worktree` holding
/// `core.worktree`: `scratch_repo_state` reads only `.git/config`. Precondition
/// (real git): the work tree is the main tree.
#[test]
fn r12_config_worktree_file_is_not_read() {
    let e = env_prod("cfgwt");
    plain_repo(&sp_repo(&e));
    let m = e.main.display().to_string();
    let d = e.sp.join("wcfg");
    std::fs::create_dir_all(&d).unwrap();
    git(&d, &["init", "-q"]);
    git(&d, &["config", "extensions.worktreeConfig", "true"]);
    git(&d, &["config", "--worktree", "core.worktree", &m]);
    let cfg = std::fs::read_to_string(d.join(".git").join("config")).unwrap();
    assert!(
        !cfg.to_ascii_lowercase().contains("worktree ="),
        "precondition: core.worktree is not in .git/config: {cfg:?}"
    );
    assert_eq!(
        toplevel(&d),
        Some(std::fs::canonicalize(&e.main).unwrap()),
        "precondition: git's work tree is the main tree"
    );
    let mut bad = vec![];
    for c in ["git add -A", "git clean -fdx"] {
        if let Some(x) = ride(&e, &bash(&sp_repo(&e), c), &bash(&d, c)) {
            bad.push(x);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// git discovery also accepts a directory that IS a git dir (implicit bare
/// repository, `safe.bareRepository` defaults to `all`). A scratchpad
/// directory holding `HEAD`/`objects`/`refs`/`config` with `core.bare=false`
/// and `core.worktree=<main>` has no `.git` entry, so `scratch_repo_state` is
/// `NoRepo` = `Scratchpad`. Precondition (real git): `git clean -fdx` there
/// deletes the main tree's untracked work.
#[test]
fn r12_scratchpad_dir_that_is_a_git_dir_is_no_repo() {
    let h = env_prod("bareharm");
    let m = h.main.display().to_string();
    let b = h.sp.join("bare");
    std::fs::create_dir_all(&b).unwrap();
    git(&b, &["init", "-q", "--bare"]);
    git(&b, &["config", "core.bare", "false"]);
    git(&b, &["config", "core.worktree", &m]);
    assert!(!b.join(".git").exists());
    assert_eq!(
        toplevel(&b),
        Some(std::fs::canonicalize(&h.main).unwrap()),
        "precondition: git's work tree in the bare-shaped dir is the main tree"
    );
    git(&b, &["clean", "-q", "-fdx"]);
    assert!(
        !h.main.join("untracked_wip.rs").exists(),
        "precondition: git clean -fdx there deletes the main tree's untracked work"
    );

    let e = env_prod("bare");
    plain_repo(&sp_repo(&e));
    let m = e.main.display().to_string();
    let b = e.sp.join("bare");
    std::fs::create_dir_all(&b).unwrap();
    git(&b, &["init", "-q", "--bare"]);
    git(&b, &["config", "core.bare", "false"]);
    git(&b, &["config", "core.worktree", &m]);
    let mut bad = vec![];
    for c in ["git add -A", "git clean -fdx"] {
        if let Some(x) = ride(&e, &bash(&sp_repo(&e), c), &bash(&b, c)) {
            bad.push(x);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// A real `.git` DIRECTORY holding a `commondir` file is a linked-worktree
/// git dir to git: refs (branches, stash) are the common dir's. The scratchpad
/// judgement only reads `.git/config`, so it is `Plain`. Precondition (real
/// git): with no index there, `git commit -a` records the deletion of every
/// file on the MAIN tree's branch (the r11 harm). (`git branch -D side` there
/// also deletes the main repository's branch, but the detector allows `git
/// branch -D` in the main tree itself as well, so it is no ride and not
/// filed.)
#[test]
fn r12_scratchpad_git_dir_with_commondir_shares_main_refs() {
    fn forge(e: &Env) -> PathBuf {
        let d = e.sp.join("cd");
        let g = d.join(".git");
        std::fs::create_dir_all(&g).unwrap();
        let head = std::fs::read_to_string(e.main.join(".git").join("HEAD")).unwrap();
        std::fs::write(g.join("HEAD"), head).unwrap();
        std::fs::write(
            g.join("commondir"),
            format!("{}\n", e.main.join(".git").display()),
        )
        .unwrap();
        d
    }
    let h = env_prod("cdharm");
    let d = forge(&h);
    let common = git_out(&d, &["rev-parse", "--git-common-dir"]);
    assert_eq!(
        std::fs::canonicalize(d.join(common.trim())).unwrap(),
        std::fs::canonicalize(h.main.join(".git")).unwrap(),
        "precondition: git's common dir there is the main repository's"
    );
    // No index in the forged git dir = an empty index: `commit -a` records the
    // deletion of every file, on the MAIN tree's branch.
    let head0 = git_out(&h.main, &["rev-parse", "HEAD"]);
    git(&d, &["commit", "-q", "-a", "-m", "x"]);
    assert_ne!(
        head0,
        git_out(&h.main, &["rev-parse", "HEAD"]),
        "precondition: `git commit -a` there moves the main tree's branch"
    );
    let files = git_out(&h.main, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(
        !files.contains("src/lib.rs"),
        "precondition: the commit drops the main tree's files: {files:?}"
    );

    let e = env_prod("cd");
    plain_repo(&sp_repo(&e));
    let d = forge(&e);
    let mut bad = vec![];
    for c in ["git commit -a -m x", "git add -A"] {
        if let Some(x) = ride(&e, &bash(&sp_repo(&e), c), &bash(&d, c)) {
            bad.push(x);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// `scratch_repo_state` walks up past the scratchpad: a plain repository
/// ABOVE the scratchpad (its work tree holds files outside it) is `Plain` =
/// `Scratchpad`. Precondition (real git): `git commit -a` from the scratchpad
/// commits a modification of a file OUTSIDE the scratchpad into that
/// repository (`git stash -u`, which removes the outside file, is a detector
/// Deny and so not a ride).
#[test]
fn r12_repository_above_the_scratchpad_is_classed_scratchpad() {
    fn layout(name: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        std::fs::create_dir_all(BASE).unwrap();
        let t = tempfile::Builder::new()
            .prefix(&format!("v12-{name}-"))
            .tempdir_in(BASE)
            .unwrap();
        let top = t.path().join("proj");
        let sp = top.join("scratchpad");
        std::fs::create_dir_all(sp.join("r")).unwrap();
        std::fs::write(top.join("tracked.txt"), "t").unwrap();
        git(&top, &["init", "-q"]);
        git(&top, &["add", "tracked.txt"]);
        git(&top, &["commit", "-q", "-m", "i"]);
        std::fs::write(top.join("outside_wip.txt"), "w").unwrap();
        std::fs::write(sp.join("r").join("x.txt"), "x").unwrap();
        (t, top, sp)
    }
    let (_h, top, sp) = layout("aboveharm");
    std::fs::write(top.join("tracked.txt"), "another session's edit").unwrap();
    let head0 = git_out(&top, &["rev-parse", "HEAD"]);
    git(&sp, &["commit", "-q", "-a", "-m", "x"]);
    assert_ne!(
        head0,
        git_out(&top, &["rev-parse", "HEAD"]),
        "precondition: `git commit -a` in the scratchpad commits into the repository above it"
    );
    assert!(git_out(&top, &["show", "--stat", "HEAD"]).contains("tracked.txt"));

    let e = env_prod("above");
    plain_repo(&sp_repo(&e));
    let (_t, top, sp) = layout("above");
    std::fs::write(top.join("tracked.txt"), "another session's edit").unwrap();
    let mut bad = vec![];
    for c in ["git commit -a -m x", "git add -A"] {
        if let Some(x) = ride(&e, &bash(&sp_repo(&e), c), &bash(&sp, c)) {
            bad.push(x);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ own worktree config

/// Point the linked worktree `wt` at the main tree through its per-worktree
/// config (`extensions.worktreeConfig` in the common config +
/// `<main>/.git/worktrees/wt/config.worktree`).
fn redirect_wt(e: &Env) {
    let m = e.main.display().to_string();
    git(&e.main, &["config", "extensions.worktreeConfig", "true"]);
    git(&e.wt, &["config", "--worktree", "core.worktree", &m]);
}

/// `class_of_repo` returns `Worktree` for the cwd's own linked worktree
/// without reading any config. With `extensions.worktreeConfig`, git honours
/// `core.worktree` from `<main>/.git/worktrees/<wt>/config.worktree`.
/// Precondition (real git): the work tree git uses from `wt` is then the main
/// tree, and `git add -A` there stages another session's untracked main-tree
/// file into wt's index. The sibling key is learned BEFORE the config is set.
/// (`core.worktree` in the COMMON config was a candidate too: git 2.54
/// ignores it in a linked worktree -- measured, `rev-parse --show-toplevel`
/// stayed `wt` -- so only the per-worktree config is a reach path.)
#[test]
fn r12_own_worktree_class_ignores_core_worktree_in_worktree_config() {
    let h = env_prod("wtharm");
    redirect_wt(&h);
    assert_eq!(
        toplevel(&h.wt),
        Some(std::fs::canonicalize(&h.main).unwrap()),
        "precondition: git's work tree in wt is the main tree"
    );
    git(&h.wt, &["add", "-A"]);
    assert!(
        git_out(&h.wt, &["diff", "--cached", "--name-only"]).contains("untracked_wip.rs"),
        "precondition: `git add -A` in wt stages the main tree's untracked work"
    );

    let e = env_prod("wt");
    let mut keys = vec![];
    for c in ["git add -A", "git commit -a -m x"] {
        let s = shape(&e, &bash(&e.wt, c));
        assert_eq!(s["decision"], "ask", "precondition: `{c}` Asks: {s}");
        let k = s["shape"].as_str().unwrap().to_string();
        assert_eq!(
            decide_with(&e, &table_with(&e, &k), &bash(&e.wt, c)),
            "allow",
            "precondition: `{c}` in wt is Allow with its own entry {k}"
        );
        keys.push((c, k));
    }
    redirect_wt(&e);
    let mut bad = vec![];
    for (c, k) in keys {
        let p = bash(&e.wt, c);
        let d = decide_with(&e, &table_with(&e, &k), &p);
        if d == "allow" {
            bad.push(format!(
                "`{c}` in wt (work tree = main via config.worktree) is Allow with key {k}; \
                 now {}",
                shape(&e, &p)
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Detector side (no table, askrules not involved): with the same
/// per-worktree `core.worktree` redirection, `git clean -fdx` in `wt` deletes
/// the MAIN tree's untracked work (precondition below, real git), and the
/// detector answers `allow` (observed at 4d9d37f28: key `allow|s4-worktree|git
/// clean -d -f -x`). Not a duplicate of the r11 `git config core.worktree`
/// item: that one is the command that WRITES the redirection; this is a
/// destructive command judged as if wt were its own work tree.
#[test]
#[ignore = "detector, filed separately"]
fn r12_detector_clean_in_redirected_own_worktree_is_allow() {
    let h = env_prod("wtcleanharm");
    redirect_wt(&h);
    git(&h.wt, &["clean", "-q", "-fdx"]);
    assert!(
        !h.main.join("untracked_wip.rs").exists(),
        "precondition: git clean -fdx in wt deletes the main tree's untracked work"
    );
    let e = env_prod("wtclean");
    redirect_wt(&e);
    let d = decide_with(&e, &no_table(&e), &bash(&e.wt, "git clean -fdx"));
    assert_ne!(
        d, "allow",
        "`git clean -fdx` in a worktree whose config.worktree core.worktree is the main tree => {d}"
    );
}

// ------------------------------------------------ hook process environment

/// `GIT_DIR` / `GIT_WORK_TREE` in the SESSION environment (inherited by both
/// the hook and the Bash tool's shell): the shape never reads the process
/// environment, so `git add -A` in a plain scratchpad repository keeps the
/// `Scratchpad` key while git acts on the named main repository. What this
/// does NOT prove: that Claude Code passes the same environment to the hook
/// and to the Bash tool (the test sets it on the hook process only).
#[test]
fn r12_session_env_git_dir_is_ignored() {
    let e = env_prod("envgd");
    plain_repo(&sp_repo(&e));
    let m = e.main.display().to_string();
    let gd = format!("{m}/.git");
    let r = sp_repo(&e);
    let mut bad = vec![];
    for extra in [
        vec![("GIT_DIR", gd.as_str())],
        vec![("GIT_WORK_TREE", m.as_str())],
    ] {
        for c in ["git add -A", "git clean -fdx"] {
            if let Some(x) = ride_env(&e, &bash(&r, c), &bash(&r, c), &extra) {
                bad.push(format!("{extra:?}: {x}"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ code execution via git

/// Allowed git subcommands run code the scratchpad repository configures:
/// `.git/hooks/pre-commit` on `commit`, `core.fsmonitor` on `add`/`status`.
/// The shape pins neither (the round-6 reason for never allowing `bash` /
/// `sh`: the program's content may change after it was learned). Precondition
/// (real git): the hook / fsmonitor command runs and writes a marker outside
/// the scratchpad.
#[test]
fn r12_git_subcommand_runs_repo_configured_code() {
    fn arm(e: &Env, marker: &Path) -> (PathBuf, PathBuf) {
        let script = format!("#!/bin/sh\necho ran > '{}'\n", marker.display());
        let hk = e.sp.join("hk");
        plain_repo(&hk);
        let hook = hk.join(".git").join("hooks").join("pre-commit");
        std::fs::write(&hook, &script).unwrap();
        std::fs::write(hk.join("src").join("a.rs"), "changed").unwrap();
        let fm = e.sp.join("fm");
        plain_repo(&fm);
        let prog = e.sp.join("fsm.sh");
        std::fs::write(&prog, &script).unwrap();
        for p in [&hook, &prog] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(&fm, &["config", "core.fsmonitor", prog.to_str().unwrap()]);
        (hk, fm)
    }
    let h = env_prod("execharm");
    let mk = h.main.join("pwned");
    let (hk, fm) = arm(&h, &mk);
    git(&hk, &["commit", "-q", "-a", "-m", "x"]);
    assert!(mk.exists(), "precondition: the pre-commit hook ran");
    std::fs::remove_file(&mk).unwrap();
    git(&fm, &["add", "-A"]);
    assert!(mk.exists(), "precondition: core.fsmonitor ran on git add");

    let e = env_prod("exec");
    plain_repo(&sp_repo(&e));
    let (hk, fm) = arm(&e, &e.main.join("pwned"));
    let r = sp_repo(&e);
    let mut bad = vec![];
    for (c, d) in [("git commit -a -m x", &hk), ("git add -A", &fm)] {
        if let Some(x) = ride(&e, &bash(&r, c), &bash(d, c)) {
            bad.push(x);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ guards / FP measurements

/// Own-key Allows stay (task item 3), including the r11 FP (a
/// `--relative-paths` worktree), and a scratchpad repository with a benign
/// config (commented `worktree`, `core.bare = false`, a `[worktree]`
/// section) is not pushed off `Scratchpad` by the parser (measured FP side).
/// (`[core "x"] worktree = ..` -- git's `core.x.worktree`, not `core.worktree`
/// -- IS pushed to `undetermined`: a restrictive FP, recorded by
/// `probe_fp_measure`, not asserted here.)
#[test]
fn r12_guard_own_keys_stay_allow() {
    let e = env_prod("guard");
    plain_repo(&sp_repo(&e));
    let r = sp_repo(&e);
    let rel = e.home.join("src").join(".harness-worktrees").join("rel");
    git(
        &e.main,
        &[
            "worktree",
            "add",
            "-q",
            "--relative-paths",
            rel.to_str().unwrap(),
        ],
    );
    std::fs::write(rel.join("new.rs"), "n").unwrap();
    let benign = sp_config(
        &e,
        "benign",
        "[core]\n\trepositoryformatversion = 0\n\tbare = false\n\t# worktree = /x\n\
         [worktree]\n\tguessRemote = true\n",
    );
    assert_eq!(
        toplevel(&benign),
        Some(std::fs::canonicalize(&benign).unwrap())
    );
    let mut bad = vec![];
    for (cwd, c) in [
        (e.wt.clone(), "git add -A"),
        (e.sp.clone(), "rm -rf build"),
        (r.clone(), "git add -A"),
        (r.clone(), "git clean -fdx"),
        (rel.clone(), "git add -A"),
        (benign.clone(), "git add -A"),
    ] {
        let p = bash(&cwd, c);
        let s = shape(&e, &p);
        assert_eq!(s["decision"], "ask", "precondition: `{c}` Asks: {s}");
        let k = s["shape"].as_str().unwrap();
        let d = decide_with(&e, &table_with(&e, k), &p);
        if d != "allow" {
            bad.push(format!(
                "`{c}` in {} with its own entry {k} => {d}",
                cwd.display()
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Measurement helper (asserts nothing): detector decision / key of git
/// commands in a plain scratchpad repo (`R`) and in the commondir forgery
/// (`D`), and in a scratchpad below a plain repository (`A`).
#[test]
#[ignore]
fn probe_commands() {
    let e = env_prod("pc");
    plain_repo(&sp_repo(&e));
    let d = e.sp.join("cd");
    let g = d.join(".git");
    std::fs::create_dir_all(&g).unwrap();
    let head = std::fs::read_to_string(e.main.join(".git").join("HEAD")).unwrap();
    std::fs::write(g.join("HEAD"), head).unwrap();
    std::fs::write(
        g.join("commondir"),
        format!("{}\n", e.main.join(".git").display()),
    )
    .unwrap();
    let cmds = std::env::var("V12_CMDS").unwrap_or_else(|_| {
        "git branch -D side|git stash clear|git add -A|git commit --allow-empty -m x|\
         git reset --hard|git commit -a -m x|git tag -d v1|git reflog expire --all|\
         git branch -f side HEAD|git rebase side|git merge side|git checkout side"
            .to_string()
    });
    for c in cmds.split('|') {
        for (w, cwd) in [("R", sp_repo(&e)), ("D", d.clone()), ("S", e.sp.clone())] {
            let p = bash(&cwd, c);
            let s = shape(&e, &p);
            println!("{w} `{c}` => {} {}", s["decision"], s["shape"]);
        }
    }
}

/// Measurement helper (asserts nothing): FP side of the restrictive round-11
/// choices. `.git` symlinked to a real scratchpad repository, a submodule
/// inside a scratchpad repository, a linked worktree of a scratchpad repo.
#[test]
#[ignore]
fn probe_fp_measure() {
    let e = env_prod("fp");
    let r = sp_repo(&e);
    plain_repo(&r);
    let l = e.sp.join("lnk");
    std::fs::create_dir_all(&l).unwrap();
    std::os::unix::fs::symlink(r.join(".git"), l.join(".git")).unwrap();
    let sub_src = e.sp.join("subsrc");
    plain_repo(&sub_src);
    let out = Command::new("git")
        .args(["-c", "protocol.file.allow=always", "submodule", "add", "-q"])
        .arg(&sub_src)
        .arg("sub")
        .current_dir(&r)
        .output()
        .unwrap();
    println!("submodule add: {:?}", out.status);
    let rw = e.sp.join("rw");
    git(&r, &["worktree", "add", "-q", rw.to_str().unwrap()]);
    let sub = sp_config(
        &e,
        "subsec",
        "[core]\n\trepositoryformatversion = 0\n\tbare = false\n[core \"x\"]\n\tworktree = /x\n",
    );
    for (cwd, c) in [
        (sub.clone(), "git add -A"),
        (l.clone(), "git add -A"),
        (r.join("sub"), "git add -A"),
        (rw.clone(), "git add -A"),
        (r.clone(), "git add -A"),
    ] {
        let p = bash(&cwd, c);
        let s = shape(&e, &p);
        let k = s["shape"].as_str().unwrap_or("").to_string();
        let own = decide_with(&e, &table_with(&e, &k), &p);
        println!("{} `{c}` => {} own={own} {k}", cwd.display(), s["decision"]);
    }
}
