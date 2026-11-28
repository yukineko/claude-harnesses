#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (verifier round 13, 2026-11-28, against 0312210ae / blastguard 0.2.1004):
//! the round-13 design lets git decide the repository (`git rev-parse
//! --path-format=absolute --show-toplevel --git-dir --git-common-dir`,
//! `shape::git_layout`), drops scratchpad git from the learned Allow, and
//! keeps exactly two Allow-learnable places: the session's own linked
//! worktree (git dir `<common>/worktrees/<name>`, back-link `<toplevel>/.git`,
//! toplevel = the cwd's linked root) and `rm` / file tools in a scratchpad.
//! These tests attack those two places. Written by an independent verifier,
//! not the implementer (CLAUDE.md §2(a)). Black-box: spawns the built binary
//! with a sandbox HOME and a hand-written learned table that records the
//! sibling's key at the learning thresholds (3 approvals from 2 sessions, 0
//! declines).
//!
//! Fixtures: HOME is an ancestor of the main tree `<home>/src/repo` and its
//! linked worktree `<home>/src/.harness-worktrees/wt`; the scratchpad is
//! `<root>/scratchpad`, in a fresh `tempfile` directory under
//! `/private/tmp/claude-askrules`. Every harm precondition is observed with
//! real git.
//!
//! What these tests do NOT prove: that the learner would promote the entry
//! (the table is written by hand); that a session would build a forged layout
//! on its own (the fixture writes it); that Claude Code gives the hook and the
//! Bash tool the same environment.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000f3";
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
    let mut c = Command::new("git");
    c.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    bounded::run_bounded_stdin(&mut c, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE)
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

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

fn env_prod(name: &str) -> Env {
    std::fs::create_dir_all(BASE).unwrap();
    let dir = tempfile::Builder::new()
        .prefix(&format!("v13-{name}-"))
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
    let wt = home.join("src").join(".harness-worktrees").join("wt");
    git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    std::fs::write(main.join("untracked_wip.rs"), "wip").unwrap();
    std::fs::create_dir_all(wt.join("build")).unwrap();
    std::fs::create_dir_all(wt.join("target").join("debug")).unwrap();
    std::fs::write(wt.join("new.rs"), "n").unwrap();
    // fixture re-anchored 2026-11-28 (leader ruling): Scratchpad is the session's own scratchpad only
    let sp = root.join(SID).join("scratchpad");
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
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for k in blastguard::shape::GIT_REDIRECT_ENV {
        c.env_remove(k);
    }
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

fn key(s: &Value) -> String {
    s["shape"].as_str().unwrap().to_string()
}

fn class(k: &str) -> &str {
    k.split('|').nth(1).unwrap_or("")
}

/// A learned table recording `k` approved 3 times from 2 sessions, 0 declines.
fn table_recorded(e: &Env, k: &str) -> PathBuf {
    let p = e.root.join("t-recorded.json");
    let entry = json!({"shape": k, "decision": "allow", "approvals": 3, "declines": 0,
                       "sessions": 2, "samples": []});
    std::fs::write(&p, json!({"version": 1, "entries": [entry]}).to_string()).unwrap();
    p
}

fn cmd_of(p: &Value) -> String {
    p["tool_input"]["command"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// Learn `sibling`'s key (it must Ask and be Allow with its recorded key);
/// returns it.
fn learn(e: &Env, sibling: &Value) -> String {
    let s = shape(e, sibling);
    assert_eq!(
        s["decision"],
        "ask",
        "precondition: sibling `{}` Asks: {s}",
        cmd_of(sibling)
    );
    let k = key(&s);
    assert_eq!(
        decide_with(e, &table_recorded(e, &k), sibling),
        "allow",
        "precondition: sibling `{}` (cwd {}) is Allow with its recorded key {k}",
        cmd_of(sibling),
        sibling["cwd"].as_str().unwrap_or("")
    );
    k
}

/// `probe` with the recorded key `k`: `Some(failure)` when it is Allow
/// although it is not Allow without a table. A probe the detector already
/// allows (no table) is no askrules question: it is printed (`DETECTOR`)
/// and skipped.
fn rides(e: &Env, k: &str, probe: &Value) -> Option<String> {
    let before = decide_with(e, &no_table(e), probe);
    if before == "allow" {
        println!(
            "DETECTOR allow (not an askrules ride): `{}` (cwd {}) shape {}",
            cmd_of(probe),
            probe["cwd"].as_str().unwrap_or(""),
            shape(e, probe)
        );
        return None;
    }
    let after = decide_with(e, &table_recorded(e, k), probe);
    (after == "allow").then(|| {
        format!(
            "probe `{}` (cwd {}) was {before} without a table but Allow with the recorded key \
             {k}; probe shape {}",
            cmd_of(probe),
            probe["cwd"].as_str().unwrap_or(""),
            shape(e, probe)
        )
    })
}

/// git's `(toplevel, git-dir, common-dir)` from `dir`, canonical.
fn layout(dir: &Path) -> Option<(PathBuf, PathBuf, PathBuf)> {
    let out = git_cmd(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-dir",
            "--git-common-dir",
        ],
    );
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let v: Vec<PathBuf> = s.lines().map(|l| canon(Path::new(l))).collect();
    Some((v[0].clone(), v[1].clone(), v[2].clone()))
}

// ------------------------------------------- forged own-worktree root

/// Turn the main tree's `src/` into what git calls the linked worktree `wt`:
/// a `.git` FILE naming wt's admin dir, then `git worktree repair` (real git)
/// rewrites the admin dir's `gitdir` back-link to `<main>/src/.git`.
fn forge_main_src(e: &Env) -> PathBuf {
    let ms = e.main.join("src");
    std::fs::write(
        ms.join(".git"),
        format!(
            "gitdir: {}\n",
            e.main.join(".git").join("worktrees").join("wt").display()
        ),
    )
    .unwrap();
    git(&ms, &["worktree", "repair"]);
    ms
}

/// The linked-root test needs the git dir to be `<common>/worktrees/<name>`
/// and its back-link to name `<toplevel>/.git`, a real file; both are written
/// by real git (`git worktree repair`) from a `.git` file the session places
/// in a MAIN-tree subdirectory. With the session cwd there, git's answer
/// passes every check, so `<main>/src` is the "own linked worktree" and its
/// main-tree directory is classed `s4-worktree` (CLAUDE.md §8: the main tree
/// is never Allow-learnable). Precondition (real git): the layout is as
/// described, and `git add -A` + `git commit -a -m x` there (the learned
/// shapes) record the MAIN tree's `src/` content -- including its untracked
/// build output -- as a commit on wt's branch that drops wt's own
/// `src/lib.rs`. Expected: no learned Allow for those shapes in `<main>/src`.
#[test]
fn r13_back_link_repaired_to_a_main_subdir_makes_main_files_own_worktree() {
    let h = env_prod("forgeharm");
    std::fs::create_dir_all(h.main.join("src").join("build")).unwrap();
    std::fs::write(h.main.join("src").join("build").join("wip"), "w").unwrap();
    let ms = forge_main_src(&h);
    let gd = h.main.join(".git").join("worktrees").join("wt");
    assert_eq!(
        std::fs::read_to_string(gd.join("gitdir")).unwrap().trim(),
        ms.join(".git").display().to_string(),
        "precondition: git worktree repair pointed wt's back-link at <main>/src/.git"
    );
    assert_eq!(
        layout(&ms),
        Some((canon(&ms), canon(&gd), canon(&h.main.join(".git")))),
        "precondition: git's layout in <main>/src is a linked worktree rooted there"
    );
    let wt_head0 = git_out(&ms, &["rev-parse", "HEAD"]);
    git(&ms, &["add", "-A"]);
    git(&ms, &["commit", "-q", "-a", "-m", "x"]);
    let files = git_out(&ms, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert_ne!(wt_head0, git_out(&ms, &["rev-parse", "HEAD"]));
    assert!(
        files.contains("build/wip") && !files.contains("src/lib.rs"),
        "precondition: the commit on wt's branch holds the main tree's untracked build/wip \
         and drops wt's src/lib.rs: {files:?}"
    );

    let e = env_prod("forge");
    std::fs::create_dir_all(e.main.join("src").join("build")).unwrap();
    std::fs::write(e.main.join("src").join("build").join("wip"), "w").unwrap();
    let k_add = learn(&e, &bash(&e.wt, "git add -A"));
    let k_ca = learn(&e, &bash(&e.wt, "git commit -a -m x"));
    // Control (no forgery yet): `<main>/src` is the main tree, no ride.
    for (k, c) in [(&k_add, "git add -A"), (&k_ca, "git commit -a -m x")] {
        let ctl = rides(&e, k, &bash(&e.main.join("src"), c));
        assert!(ctl.is_none(), "control: unforged <main>/src rides: {ctl:?}");
    }
    let ms = forge_main_src(&e);
    let mut bad = vec![];
    for (k, c) in [(&k_add, "git add -A"), (&k_ca, "git commit -a -m x")] {
        if let Some(x) = rides(&e, k, &bash(&ms, c)) {
            bad.push(x);
        }
    }
    assert!(
        bad.is_empty(),
        "a main-tree subdirectory forged into wt's linked root is the own worktree:\n{}",
        bad.join("\n")
    );
}

/// Detector side (no table, askrules not involved) of the same forgery:
/// `rm -rf build` (the main tree's untracked build output) and `git clean
/// -fdx` in `<main>/src`. Precondition (real git): `git clean -fdx` there
/// deletes the main tree's untracked `src/build/wip` and its tracked
/// `src/lib.rs` (not in wt's index under that name). Guard: observed green
/// at 0312210ae (the detector does not Allow either).
#[test]
fn r13_detector_forged_main_subdir_root() {
    let h = env_prod("forgedetharm");
    std::fs::create_dir_all(h.main.join("src").join("build")).unwrap();
    std::fs::write(h.main.join("src").join("build").join("wip"), "w").unwrap();
    let ms = forge_main_src(&h);
    git(&ms, &["clean", "-q", "-fdx"]);
    assert!(
        !ms.join("lib.rs").exists() && !ms.join("build").exists(),
        "precondition: git clean -fdx in the forged root deletes main-tree files"
    );
    let e = env_prod("forgedet");
    std::fs::create_dir_all(e.main.join("src").join("build")).unwrap();
    std::fs::write(e.main.join("src").join("build").join("wip"), "w").unwrap();
    let ms = forge_main_src(&e);
    let mut bad = vec![];
    for c in ["rm -rf build", "git clean -fdx"] {
        let p = bash(&ms, c);
        let d = decide_with(&e, &no_table(&e), &p);
        if d == "allow" {
            bad.push(format!(
                "`{c}` in the forged root <main>/src is Allow with no table; shape {}",
                shape(&e, &p)
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------- nested trees in the own worktree

/// The class of a path is git's answer for its deepest existing DIRECTORY,
/// not for what lies below it: `rm -rf vendor` in the own worktree, where
/// `vendor/other` is ANOTHER linked worktree of the main repository (its own
/// uncommitted work). Precondition (real git): `vendor/other` is a registered
/// linked worktree with an uncommitted file. Expected: the shape class is not
/// `s4-worktree` and the call is not Allow (round 10: another session's
/// linked worktree is never an Allow-eligible target, however it is placed).
/// `rm` in wt is judged by the detector (no Ask, so no learned key exists for
/// it): the class is asserted directly and the no-table decision is checked.
#[test]
#[ignore = "RED(d5cd45de): detector + class — rm -rf of own dir holding another linked checkout is Allow"]
fn r13_rm_of_own_dir_holding_another_checkout() {
    let e = env_prod("nestwt");
    let other = e.wt.join("vendor").join("other");
    std::fs::create_dir_all(e.wt.join("vendor")).unwrap();
    git(&e.main, &["worktree", "add", "-q", other.to_str().unwrap()]);
    std::fs::write(other.join("their_wip.rs"), "w").unwrap();
    let (top, gd, _) = layout(&other).unwrap();
    assert_eq!(
        top,
        canon(&other),
        "precondition: vendor/other is its own checkout"
    );
    assert!(
        gd.starts_with(canon(&e.main.join(".git").join("worktrees"))),
        "precondition: vendor/other is a linked worktree of the main repository: {gd:?}"
    );
    assert!(
        git_out(&other, &["status", "--porcelain"]).contains("their_wip.rs"),
        "precondition: vendor/other holds uncommitted work (lost by an rm of it)"
    );
    let mut bad = vec![];
    for c in ["rm -rf vendor", "rm -rf vendor/other"] {
        let p = bash(&e.wt, c);
        let s = shape(&e, &p);
        let d = decide_with(&e, &no_table(&e), &p);
        if class(&key(&s)) == "s4-worktree" || d == "allow" {
            bad.push(format!("`{c}` (cwd wt) => {d}, shape {s}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Guards (expected green): a nested repository, a `.git` file pointing at
/// wt's own admin dir, a submodule and a symlink to the main tree inside the
/// own worktree are not the own worktree.
#[test]
fn r13_nested_repo_gitfile_submodule_symlink_in_own_worktree() {
    let e = env_prod("nest");
    let k_add = learn(&e, &bash(&e.wt, "git add -A"));
    // nested `git init`
    let nested = e.wt.join("nested");
    std::fs::create_dir_all(nested.join("d")).unwrap();
    std::fs::write(nested.join("d").join("f"), "f").unwrap();
    git(&nested, &["init", "-q"]);
    // `.git` file naming wt's own admin dir (no repair: back-link stays wt/.git)
    let gf = e.wt.join("gf");
    std::fs::create_dir_all(gf.join("d")).unwrap();
    std::fs::write(
        gf.join(".git"),
        format!(
            "gitdir: {}\n",
            e.main.join(".git").join("worktrees").join("wt").display()
        ),
    )
    .unwrap();
    assert_eq!(
        layout(&gf).unwrap().0,
        canon(&gf),
        "precondition: gf is a toplevel"
    );
    // submodule
    let subsrc = e.root.join("subsrc");
    std::fs::create_dir_all(&subsrc).unwrap();
    std::fs::write(subsrc.join("s"), "s").unwrap();
    git(&subsrc, &["init", "-q"]);
    git(&subsrc, &["add", "s"]);
    git(&subsrc, &["commit", "-q", "-m", "i"]);
    git(
        &e.wt,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            subsrc.to_str().unwrap(),
            "sub",
        ],
    );
    std::fs::write(e.wt.join("sub").join("sub_wip"), "w").unwrap();
    // symlink to the main tree
    std::os::unix::fs::symlink(&e.main, e.wt.join("lnk")).unwrap();
    let mut bad = vec![];
    // Path targets (rm in wt is detector-judged: the class is asserted).
    for c in [
        "rm -rf nested/d",
        "rm -rf gf/d",
        "rm -rf sub/sub_wip",
        "rm -rf lnk/src",
        "rm -rf build nested/d",
    ] {
        let s = shape(&e, &bash(&e.wt, c));
        println!("nest `{c}` (cwd wt) => {} {}", s["decision"], s["shape"]);
        if class(&key(&s)) == "s4-worktree" {
            bad.push(format!("`{c}` (cwd wt) is classed s4-worktree: {s}"));
        }
    }
    // Repository targets: the own `git add -A` key.
    for cwd in [
        nested.clone(),
        gf.clone(),
        e.wt.join("sub"),
        e.wt.join("lnk"),
    ] {
        if let Some(x) = rides(&e, &k_add, &bash(&cwd, "git add -A")) {
            bad.push(x);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------- the probe itself

/// `rev-parse` runs no repository-configured code: `core.fsmonitor` set in
/// the main repository's config (shared by wt) is a live program for `git
/// status` (precondition, real git), but the blastguard hook judging `git add
/// -A` in wt must not start it.
#[test]
fn r13_probe_runs_no_fsmonitor() {
    let e = env_prod("fsm");
    let marker = e.root.join("fsm-ran");
    let prog = e.root.join("fsm.sh");
    std::fs::write(
        &prog,
        format!("#!/bin/sh\necho ran >> '{}'\n", marker.display()),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(
        &e.main,
        &["config", "core.fsmonitor", prog.to_str().unwrap()],
    );
    git(&e.wt, &["status", "--porcelain"]);
    assert!(
        marker.exists(),
        "precondition: core.fsmonitor runs on git status in wt"
    );
    std::fs::remove_file(&marker).unwrap();
    for c in ["git add -A", "git commit -a -m x", "rm -rf build"] {
        let p = bash(&e.wt, c);
        let _ = shape(&e, &p);
        let _ = decide_with(&e, &no_table(&e), &p);
    }
    assert!(
        !marker.exists(),
        "the hook's git probe ran the repository's core.fsmonitor: {:?}",
        std::fs::read_to_string(&marker)
    );
}

/// `include.path` naming a FIFO makes every git command in the repository
/// block on reading config (precondition, real git: `rev-parse` is still
/// running after 3 s). The probe must time out to "not determined" (no
/// learned Allow), the hook must return, and the probe's git must not be
/// left behind (a FIFO with no reader refuses a non-blocking writer: ENXIO).
#[test]
fn r13_include_fifo_times_out_to_undetermined_and_leaves_no_git() {
    let e = env_prod("fifo");
    let k_add = learn(&e, &bash(&e.wt, "git add -A"));
    let k_ca = learn(&e, &bash(&e.wt, "git commit -a -m x"));
    let fifo = e.root.join("cfg.fifo");
    let st = Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(st.success());
    git(&e.main, &["config", "include.path", fifo.to_str().unwrap()]);
    let mut child = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&e.wt)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(3));
    let still = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();
    assert!(still, "precondition: git blocks on the include FIFO");
    let no_reader = || {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NONBLOCK (darwin 0x0004): ENXIO when no process has it open to read.
        std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(0x0004)
            .open(&fifo)
            .is_err()
    };
    assert!(
        no_reader(),
        "precondition: no reader left after killing the probe git"
    );
    let mut bad = vec![];
    for (k, c) in [(&k_add, "git add -A"), (&k_ca, "git commit -a -m x")] {
        let p = bash(&e.wt, c);
        let t0 = Instant::now();
        let d = decide_with(&e, &table_recorded(&e, k), &p);
        let el = t0.elapsed();
        println!("fifo: `{c}` => {d} in {el:?}");
        if d == "allow" {
            bad.push(format!(
                "`{c}` in wt with include.path=<fifo> (git blocks) is Allow with {k}; shape {}",
                shape(&e, &p)
            ));
        }
        if !no_reader() {
            bad.push(format!(
                "`{c}`: a git process still has the include FIFO open"
            ));
        }
    }
    // The detector's own git probe (rm in wt is detector-judged): measured.
    let t0 = Instant::now();
    let d = decide_with(&e, &no_table(&e), &bash(&e.wt, "rm -rf build"));
    println!("fifo: detector `rm -rf build` => {d} in {:?}", t0.elapsed());
    if !no_reader() {
        bad.push("`rm -rf build`: a git process still has the include FIFO open".to_string());
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------- hook-process environment

/// git environment variables NOT in `GIT_REDIRECT_ENV`: observed with real
/// git whether they move the work tree / git dir away from what `rev-parse`
/// (run without them) says, and whether the hook flags them. A variable that
/// git ignores without its companion (`GIT_CONFIG_KEY_0` without
/// `GIT_CONFIG_COUNT`) or that names no repository (`GIT_EXEC_PATH`) is no
/// reach path; this test fails only for a variable that changes git's
/// work tree / git dir AND leaves the class `s4-worktree`.
#[test]
fn r13_unlisted_git_env_that_moves_the_work_tree() {
    let e = env_prod("envx");
    let base = layout(&e.wt).unwrap();
    let m = e.main.display().to_string();
    let candidates: Vec<(&str, String)> = vec![
        ("GIT_EXEC_PATH", e.root.display().to_string()),
        ("GIT_CONFIG_KEY_0", "core.worktree".to_string()),
        ("GIT_CONFIG_VALUE_0", m.clone()),
        ("GIT_IMPLICIT_WORK_TREE", "0".to_string()),
        ("GIT_INTERNAL_SUPER_PREFIX", "x/".to_string()),
        ("GIT_SHALLOW_FILE", e.root.join("sh").display().to_string()),
        ("GIT_REPLACE_REF_BASE", "refs/x/".to_string()),
        ("XDG_CONFIG_HOME", e.root.join("xdg").display().to_string()),
    ];
    // XDG global config carrying core.worktree / core.bare.
    let xdg = e.root.join("xdg").join("git");
    std::fs::create_dir_all(&xdg).unwrap();
    std::fs::write(xdg.join("config"), format!("[core]\n\tworktree = {m}\n")).unwrap();
    let mut bad = vec![];
    for (k, v) in &candidates {
        let out = Command::new("git")
            .args([
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-dir",
                "--git-common-dir",
            ])
            .current_dir(&e.wt)
            .env(k, v)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let moved = if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).into_owned();
            let v: Vec<PathBuf> = s.lines().map(|l| canon(Path::new(l))).collect();
            (v[0].clone(), v[1].clone(), v[2].clone()) != base
        } else {
            false
        };
        let s = shape_env(&e, &bash(&e.wt, "git add -A"), &[(k, v.as_str())]);
        let cl = class(&key(&s)).to_string();
        println!(
            "env {k}={v}: git layout moved={moved} (exit {:?}) class={cl}",
            out.status.code()
        );
        if moved && cl == "s4-worktree" {
            bad.push(format!(
                "{k}={v} moves git's layout but the class stays {cl}"
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------- scratchpad rm

/// Guards (expected green): `rm` in a scratchpad through a symlink to the
/// main tree, through `..`, or on a scratchpad path that is a symlink, is not
/// `s4-scratchpad`.
#[test]
fn r13_scratchpad_rm_symlink_and_dotdot() {
    let e = env_prod("sprm");
    let k = learn(&e, &bash(&e.sp, "rm -rf build"));
    std::os::unix::fs::symlink(&e.main, e.sp.join("lnk")).unwrap();
    std::os::unix::fs::symlink(e.main.join("src"), e.sp.join("srclnk")).unwrap();
    let sp = e.sp.display().to_string();
    let mut bad = vec![];
    for c in [
        "rm -rf lnk/src".to_string(),
        "rm -rf srclnk/".to_string(),
        "rm -rf lnk/untracked_wip.rs".to_string(),
        format!("rm -rf {sp}/../home-x"),
        "rm -rf ../scratchpad/../approvals".to_string(),
        format!("rm -rf {sp}/lnk/src"),
    ] {
        if let Some(x) = rides(&e, &k, &bash(&e.sp, &c)) {
            bad.push(x);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------- FP guards / measurement

/// FP side (expected green): in the own worktree, the everyday commands stay
/// Allow with their recorded key (or are a detector Allow), and the probe is
/// far below its 2 s timeout. Measured: 20 direct `git rev-parse` runs in wt
/// and the hook's wall time per command (printed; run with --nocapture).
#[test]
fn r13_fp_own_worktree_commands_stay_allow_and_probe_is_fast() {
    let e = env_prod("fp");
    let mut max = Duration::ZERO;
    for _ in 0..20 {
        let t0 = Instant::now();
        assert!(layout(&e.wt).is_some());
        max = max.max(t0.elapsed());
    }
    println!("rev-parse in wt: max of 20 = {max:?}");
    let mut bad = vec![];
    for c in [
        "git add -A",
        "git commit -a -m x",
        "git clean -fdx",
        "rm -rf target",
        "rm -rf target/debug",
        "rm -rf build",
        "rm -rf no_such_dir",
    ] {
        let p = bash(&e.wt, c);
        let s = shape(&e, &p);
        let t0 = Instant::now();
        let d = if s["decision"] == "ask" {
            decide_with(&e, &table_recorded(&e, &key(&s)), &p)
        } else {
            s["decision"].as_str().unwrap_or("").to_string()
        };
        let el = t0.elapsed();
        println!(
            "wt `{c}` => detector {} key {} learned => {d} ({el:?})",
            s["decision"],
            key(&s)
        );
        if d != "allow" {
            bad.push(format!(
                "`{c}` in wt with its recorded key {} => {d}",
                key(&s)
            ));
        }
    }
    assert!(
        max < Duration::from_millis(500),
        "rev-parse in wt took {max:?} (timeout is 2 s)"
    );
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Measurement (asserts nothing): detector decision / key of `V13_CMDS`
/// (`|`-separated) in wt and in the scratchpad.
#[test]
#[ignore]
fn probe_commands() {
    let e = env_prod("pc");
    std::fs::write(e.wt.join("src").join("lib.rs"), "modified").unwrap();
    let cmds = std::env::var("V13_CMDS").unwrap_or_else(|_| {
        "rm -rf build|rm -f new.rs|rm -rf src|rm src/lib.rs|rm -rf new.rs build|git add -A|\
         git reset --hard|git checkout -- src|git restore src|git stash"
            .to_string()
    });
    for c in cmds.split('|') {
        for (w, cwd) in [("WT", e.wt.clone()), ("SP", e.sp.clone())] {
            let s = shape(&e, &bash(&cwd, c));
            println!("{w} `{c}` => {} {}", s["decision"], s["shape"]);
        }
    }
}

/// Measurement (asserts nothing): scratchpad `rm` on the scratchpad itself
/// and on another session's scratchpad, and `rm` of a linked worktree of the
/// main repository placed in the scratchpad.
#[test]
#[ignore]
fn probe_scratchpad_rm_scope() {
    let e = env_prod("spscope");
    let k = learn(&e, &bash(&e.sp, "rm -rf build"));
    let other = e.root.join("other-session").join("scratchpad");
    std::fs::create_dir_all(other.join("x")).unwrap();
    let wtx = e.sp.join("wtx");
    git(&e.main, &["worktree", "add", "-q", wtx.to_str().unwrap()]);
    std::fs::write(wtx.join("their_wip.rs"), "w").unwrap();
    for c in [
        format!("rm -rf {}", e.sp.display()),
        format!("rm -rf {}/x", other.display()),
        "rm -rf wtx".to_string(),
    ] {
        let p = bash(&e.sp, &c);
        let s = shape(&e, &p);
        let d = decide_with(&e, &table_recorded(&e, &k), &p);
        println!("sp `{c}` => {} {} learned => {d}", s["decision"], key(&s));
    }
}
