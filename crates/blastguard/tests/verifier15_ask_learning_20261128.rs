#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Verifier round 15 (2026-11-28, against 97d4b7709 / blastguard 0.2.1008):
//! attacks the round-14 "fixed position" rules -- `GitLayout::linked_root`
//! (the toplevel's canonical parent must EXACTLY be `<main parent>/.harness-worktrees`,
//! `<main>/.claude/worktrees` or `$HOME/.condukt/worktrees`) and
//! `is_own_scratchpad` (`/private/tmp|/tmp` + `claude-*` + one project
//! component + payload SID + `scratchpad`).
//! Written by an independent verifier, not the implementer (CLAUDE.md §2(a)).
//! Black-box: spawns the built binary with a sandbox HOME and a hand-written
//! learned table that records the sibling's key at the learning thresholds
//! (3 approvals from 2 sessions, 0 declines).
//!
//! Fixtures (the real layout): root `/private/tmp/claude-v15-*` (a `claude-`
//! component directly under `/private/tmp`), HOME `<root>/home-*`, main tree
//! `<home>/src/harness`, own worktree `<home>/src/.harness-worktrees/x`, another
//! session's worktree `<home>/src/.harness-worktrees/y` (uncommitted work),
//! scratchpad `<root>/-proj/<SID>/scratchpad`. Harm preconditions are observed
//! with real git / real rm on a twin fixture.
//!
//! What these tests do NOT prove: that the learner would promote the entry
//! (the table is written by hand); that a session would build a forged layout
//! on its own (the fixture writes it); what Claude Code puts in HOME.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000f5";

struct Env {
    _dir: tempfile::TempDir,
    _home: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    /// The HOME value passed to the hook (default: `home`).
    hook_home: Option<OsString>,
    /// The hook process's current directory (default: inherited).
    hook_cwd: Option<PathBuf>,
    main: PathBuf,
    wt: PathBuf,
    wt2: PathBuf,
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

fn new_repo(main: &Path) {
    std::fs::create_dir_all(main.join("src")).unwrap();
    std::fs::write(main.join("src").join("lib.rs"), "x").unwrap();
    git(main, &["init", "-q"]);
    git(main, &["add", "src"]);
    git(main, &["commit", "-q", "-m", "i"]);
}

fn env_prod(name: &str) -> Env {
    let dir = tempfile::Builder::new()
        .prefix(&format!("claude-v15-{name}-"))
        .tempdir_in("/private/tmp")
        .unwrap();
    let root = dir.path().to_path_buf();
    let home_dir = tempfile::Builder::new()
        .prefix("home-")
        .tempdir_in(&root)
        .unwrap();
    let home = home_dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("approvals")).unwrap();
    let main = home.join("src").join("harness");
    new_repo(&main);
    let wts = home.join("src").join(".harness-worktrees");
    let wt = wts.join("x");
    git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    std::fs::create_dir_all(wt.join("build")).unwrap();
    std::fs::write(wt.join("src").join("lib.rs"), "modified").unwrap();
    std::fs::write(wt.join("new.rs"), "n").unwrap();
    let wt2 = wts.join("y");
    git(&main, &["worktree", "add", "-q", wt2.to_str().unwrap()]);
    std::fs::write(
        wt2.join("src").join("lib.rs"),
        "other session's uncommitted edit",
    )
    .unwrap();
    std::fs::write(wt2.join("wip.rs"), "other session's untracked file").unwrap();
    let sp = root.join("-proj").join(SID).join("scratchpad");
    std::fs::create_dir_all(sp.join("build")).unwrap();
    Env {
        _dir: dir,
        _home: home_dir,
        hook_home: Some(home.clone().into_os_string()),
        hook_cwd: None,
        root,
        home,
        main,
        wt,
        wt2,
        sp,
    }
}

fn spawn(e: &Env, table: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    c.args(args)
        .env("BLASTGUARD_BLOCK_LOG_DIR", e.root.join("blocklog"))
        .env("BLASTGUARD_RELAY_DIR", e.root.join("relay"))
        .env("BLASTGUARD_APPROVALS_DIR", e.root.join("approvals"))
        .env("BLASTGUARD_LEARNED_TABLE", table)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("BLASTGUARD_ASK_RULES")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli");
    match &e.hook_home {
        Some(h) => c.env("HOME", h),
        None => c.env_remove("HOME"),
    };
    if let Some(d) = &e.hook_cwd {
        c.current_dir(d);
    }
    for k in blastguard::shape::GIT_REDIRECT_ENV {
        c.env_remove(k);
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

/// The hook's decision; empty stdout is Allow.
fn decide_with(e: &Env, table: &Path, p: &Value) -> String {
    let (_, out, err) = spawn(e, table, &[], &p.to_string());
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

fn bash(cwd: &Path, command: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID,
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
}

fn shape(e: &Env, p: &Value) -> Value {
    let (code, out, err) = spawn(e, &no_table(e), &["shape"], &p.to_string());
    assert_eq!(code, 0, "shape failed: {out:?} {err:?}");
    serde_json::from_str(out.trim()).unwrap()
}

fn key(s: &Value) -> String {
    s["shape"].as_str().unwrap_or("").to_string()
}

fn class(k: &str) -> &str {
    k.split('|').nth(1).unwrap_or("")
}

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

/// Learn `sibling`'s key (it must Ask and be Allow with its recorded key).
fn learn(e: &Env, sibling: &Value) -> String {
    let s = shape(e, sibling);
    assert_eq!(
        s["decision"],
        "ask",
        "precondition: sibling `{}` (cwd {}) Asks: {s}",
        cmd_of(sibling),
        sibling["cwd"].as_str().unwrap_or("")
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

/// `Some(failure)` when `probe` is Allow with the recorded key `k` although it
/// is not Allow without a table. A detector Allow is printed and skipped.
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

/// Own key Allow (or detector Allow) for `p`; `Some(failure)` otherwise.
fn own_key_allow(e: &Env, p: &Value) -> Option<String> {
    let s = shape(e, p);
    let d = if s["decision"] == "ask" {
        decide_with(e, &table_recorded(e, &key(&s)), p)
    } else {
        s["decision"].as_str().unwrap_or("").to_string()
    };
    (d != "allow").then(|| {
        format!(
            "`{}` (cwd {}) with its recorded key {} => {d}",
            cmd_of(p),
            p["cwd"].as_str().unwrap_or(""),
            key(&s)
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

/// Write `<dir>/.git` naming the admin dir `admin` and let real git rewrite
/// the back-link (`git worktree repair`).
fn forge(dir: &Path, admin: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
    git(dir, &["worktree", "repair"]);
}

/// A symlink outside the tempdir, removed on drop.
struct Link(PathBuf);
impl Drop for Link {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ------------------------------------------------ FP: the real layout

/// FP (expected green): the real layout keeps the own-key Allow: `git add -A`
/// / `git commit -a -m x` in the harness worktree, in Claude Code's
/// `<main>/.claude/worktrees/<n>` and in condukt's FLAT
/// `$HOME/.condukt/worktrees/run-20261122-003446-4871-t1`; `rm -rf build`
/// (relative and absolute) in the scratchpad.
#[test]
fn r15_fp_real_layout_own_keys_stay_allow() {
    let e = env_prod("fp");
    let cc = e.main.join(".claude").join("worktrees").join("n");
    std::fs::create_dir_all(cc.parent().unwrap()).unwrap();
    git(&e.main, &["worktree", "add", "-q", cc.to_str().unwrap()]);
    let ct = e
        .home
        .join(".condukt")
        .join("worktrees")
        .join("run-20261122-003446-4871-t1");
    std::fs::create_dir_all(ct.parent().unwrap()).unwrap();
    git(&e.main, &["worktree", "add", "-q", ct.to_str().unwrap()]);
    let mut bad = vec![];
    for wt in [&e.wt, &cc, &ct] {
        std::fs::write(wt.join("new2.rs"), "n").unwrap();
        for c in ["git add -A", "git commit -a -m x"] {
            if let Some(x) = own_key_allow(&e, &bash(wt, c)) {
                bad.push(x);
            }
        }
    }
    for c in [
        "rm -rf build".to_string(),
        format!("rm -rf {}/build", e.sp.display()),
    ] {
        if let Some(x) = own_key_allow(&e, &bash(&e.sp, &c)) {
            bad.push(x);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// FP (expected green): a HOME that is a symlink to the real home directory
/// (`<root>/hl -> <home>`) still places condukt's flat worktree (cwd spelled
/// by its real path) in `$HOME/.condukt/worktrees`.
#[test]
fn r15_fp_symlinked_home_keeps_condukt_worktree_own() {
    let mut e = env_prod("fphl");
    let ct = e.home.join(".condukt").join("worktrees").join("run-1-t1");
    std::fs::create_dir_all(ct.parent().unwrap()).unwrap();
    git(&e.main, &["worktree", "add", "-q", ct.to_str().unwrap()]);
    std::fs::write(ct.join("new2.rs"), "n").unwrap();
    let hl = e.root.join("hl");
    std::os::unix::fs::symlink(&e.home, &hl).unwrap();
    e.hook_home = Some(hl.clone().into_os_string());
    let mut bad = vec![];
    for c in ["git add -A", "git commit -a -m x"] {
        if let Some(x) = own_key_allow(&e, &bash(&ct, c)) {
            bad.push(x);
        }
    }
    let via = hl.join(".condukt").join("worktrees").join("run-1-t1");
    println!(
        "MEASURE cwd spelled through the HOME symlink: {}",
        shape(&e, &bash(&via, "git add -A"))
    );
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ HOME of the hook process

/// `$HOME` empty / unset / relative: container (c) is "disabled without HOME"
/// (spec item 9). A relative HOME is no determined home (CLAUDE.md §3); it
/// must not be resolved against the hook process's current directory. Probe:
/// `git add -A` in `<home>/.condukt/worktrees/run-t1` with the learned own
/// worktree key. Expected: no learned Allow for any of these HOME values.
#[test]
fn r15_home_empty_unset_or_relative_never_places_condukt_container() {
    let mut e = env_prod("relhome");
    let ct = e.home.join(".condukt").join("worktrees").join("run-t1");
    std::fs::create_dir_all(ct.parent().unwrap()).unwrap();
    git(&e.main, &["worktree", "add", "-q", ct.to_str().unwrap()]);
    std::fs::write(ct.join("new2.rs"), "n").unwrap();
    let k = learn(&e, &bash(&e.wt, "git add -A"));
    let rel = e.home.file_name().unwrap().to_os_string();
    let mut bad = vec![];
    for (label, h, cwd) in [
        ("empty", Some(OsString::new()), None),
        ("unset", None, None),
        (
            "relative, hook cwd = its parent",
            Some(rel.clone()),
            Some(e.root.clone()),
        ),
        (
            "relative `.`, hook cwd = home",
            Some(OsString::from(".")),
            Some(e.home.clone()),
        ),
    ] {
        e.hook_home = h;
        e.hook_cwd = cwd;
        let p = bash(&ct, "git add -A");
        let s = shape(&e, &p);
        println!("MEASURE HOME {label}: {s}");
        if let Some(x) = rides(&e, &k, &p) {
            bad.push(format!("HOME {label}: {x}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ forgeries inside a container

/// A directory created INSIDE the real container `<main parent>/.harness-worktrees`
/// (`f`, holding files that are no checkout's) and turned into a linked root by
/// writing `f/.git` naming ANOTHER session's admin dir (`.git/worktrees/y`) and
/// `git worktree repair`. Precondition (real git, twin fixture): git then
/// calls `f` a linked worktree rooted there, and `git add -A` + `git commit -a
/// -m x` from `f` make a commit on y's branch that holds f's `secret` and
/// drops y's tracked `src/lib.rs`. Formerly expected: no learned Allow from
/// `f` (superseded, see below). The forging steps were measured; observed at
/// 97d4b7709: Write tool `f/.git` => deny, `printf .. > f/.git` => ask, but
/// `cp ../y/.git .git` (cwd f) => allow and `git worktree repair` => allow, so
/// the whole chain needs no human once the commit key is learned.
///
/// re-anchored 2026-11-28 (user ruling 「コンテナ内なら可」)
/// Split in two. (a) here: once forged, `f` IS a real linked worktree
/// directly in the container, which the ruling makes Worktree (any session's),
/// so the learned keys DO apply there (assert inverted: Allow). (b)
/// `r15_dotgit_forging_steps_are_not_allow` keeps the forging STEPS as an
/// ignored RED (detector side, filed separately).
#[test]
fn r15_dotgit_forged_inside_harness_container() {
    let e = env_prod("cf");
    let k_add = learn(&e, &bash(&e.wt, "git add -A"));
    let k_ca = learn(&e, &bash(&e.wt, "git commit -a -m x"));
    let f = e.home.join("src").join(".harness-worktrees").join("f");
    std::fs::create_dir_all(&f).unwrap();
    std::fs::write(f.join("secret"), "s").unwrap();
    let admin_y = e.main.join(".git").join("worktrees").join("y");
    forge(&f, &admin_y);
    assert_eq!(
        layout(&f),
        Some((canon(&f), canon(&admin_y), canon(&e.main.join(".git")))),
        "precondition: git calls f a linked worktree rooted at f with y's admin dir"
    );
    let mut bad = vec![];
    for (k, c) in [(&k_add, "git add -A"), (&k_ca, "git commit -a -m x")] {
        let p = bash(&f, c);
        let d = decide_with(&e, &table_recorded(&e, k), &p);
        println!("MEASURE forged f `{c}` => {d}; shape {}", shape(&e, &p));
        if d != "allow" {
            bad.push(format!(
                "`{c}` in the forged container worktree {} with key {k} => {d} (ruling: Allow)",
                f.display()
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// (b) of the split above: the forging steps themselves. Turning a plain
/// directory in the container into another session's linked root is not an
/// ordinary edit (precondition, real git on a twin fixture: the commit then
/// lands on y's branch, holds f's `secret`, drops y's `src/lib.rs`), so none
/// of the steps -- the Write tool creating `f/.git`, `cp ../y/.git .git`
/// (cwd f), `cp <y>/.git <f>/.git`, `git worktree repair` (cwd f) -- may be
/// Allow without a table. Measured at 97d4b7709: Write => deny, `cp ../y/.git
/// .git` => allow, `git worktree repair` => allow.
#[test]
#[ignore = "RED(a80a1fea): detector — forging a linked-worktree .git is Allow; filed separately"]
fn r15_dotgit_forging_steps_are_not_allow() {
    let h = env_prod("cfharm");
    let f = h.home.join("src").join(".harness-worktrees").join("f");
    std::fs::create_dir_all(&f).unwrap();
    std::fs::write(f.join("secret"), "s").unwrap();
    let admin_y = h.main.join(".git").join("worktrees").join("y");
    forge(&f, &admin_y);
    assert_eq!(
        layout(&f),
        Some((canon(&f), canon(&admin_y), canon(&h.main.join(".git")))),
        "precondition: git calls f a linked worktree rooted at f with y's admin dir"
    );
    git(&f, &["add", "-A"]);
    git(&f, &["commit", "-q", "-a", "-m", "x"]);
    let files = git_out(&f, &["ls-tree", "-r", "--name-only", "HEAD"]);
    let branch_y = git_out(&h.main, &["log", "-1", "--format=%s", "y"]);
    assert!(
        files.lines().any(|l| l == "secret")
            && !files.lines().any(|l| l == "src/lib.rs")
            && branch_y.trim() == "x",
        "precondition: the commit lands on y's branch, holds secret, drops src/lib.rs: \
         {files:?} / y tip {branch_y:?}"
    );

    let e = env_prod("cfsteps");
    let f = e.home.join("src").join(".harness-worktrees").join("f");
    std::fs::create_dir_all(&f).unwrap();
    std::fs::write(f.join("secret"), "s").unwrap();
    let admin_y = e.main.join(".git").join("worktrees").join("y");
    let mut bad = vec![];
    // The forging steps as the agent would issue them (no table).
    let write_git = json!({
        "hook_event_name": "PreToolUse", "session_id": SID, "cwd": e.wt,
        "tool_name": "Write",
        "tool_input": {"file_path": f.join(".git"), "content": format!("gitdir: {}\n", admin_y.display())},
    });
    let d = decide_with(&e, &no_table(&e), &write_git);
    println!("MEASURE forging step Write {}/.git => {d}", f.display());
    if d == "allow" {
        bad.push(format!("Write {}/.git => allow", f.display()));
    }
    for c in [
        format!(
            "printf 'gitdir: {}\\n' > {}/.git",
            admin_y.display(),
            f.display()
        ),
        format!("cp {}/.git {}/.git", e.wt2.display(), f.display()),
        "cp ../y/.git .git".to_string(),
        "git worktree repair".to_string(),
    ] {
        let d = decide_with(&e, &no_table(&e), &bash(&f, &c));
        println!("MEASURE forging step Bash `{c}` (cwd f) => {d}");
        if d == "allow" {
            bad.push(format!("`{c}` (cwd {}) => allow", f.display()));
        }
    }
    assert!(
        bad.is_empty(),
        "forging a linked-worktree .git in the container is Allow:\n{}",
        bad.join("\n")
    );
}

/// Spec Components 1: "another session's linked worktree, however it is
/// spelled or placed, is NOT `Worktree`". The implementation's "own" is the
/// worktree the payload cwd is in, and Claude Code's Bash keeps a `cd`
/// across calls, so a cwd of another session's worktree `y` (same container)
/// makes `y` own. Precondition (real git, twin fixture): `git commit -a -m x`
/// from `y` commits y's uncommitted edit on y's branch; `git add -A` stages
/// y's untracked file. Expected (re-anchored 2026-11-28, user ruling
/// 「コンテナ内なら可」, which superseded the spec sentence above): learned
/// Allow in `y`; none in the main tree, outside the containers or in HOME.
#[test]
fn r15_cwd_in_another_sessions_worktree_in_same_container() {
    let h = env_prod("othharm");
    git(&h.wt2, &["add", "-A"]);
    let staged = git_out(&h.wt2, &["diff", "--cached", "--name-only"]);
    assert!(
        staged.lines().any(|l| l == "wip.rs"),
        "precondition: git add -A from y stages y's untracked file: {staged:?}"
    );
    git(&h.wt2, &["commit", "-q", "-a", "-m", "x"]);
    let tip = git_out(&h.wt2, &["show", "HEAD:src/lib.rs"]);
    assert_eq!(
        tip, "other session's uncommitted edit",
        "precondition: the commit holds y's uncommitted edit"
    );

    // re-anchored 2026-11-28 (user ruling 「コンテナ内なら可」)
    // Before: another session's worktree `y` in the same container must not
    // ride the own key. After: any real linked worktree directly in a fixed
    // container is Worktree, so `y` with the learned key IS Allow (asserted as
    // the FP side). Kept: the main tree, a linked worktree OUTSIDE every
    // container (`<home>/elsewhere/z`) and a plain HOME directory never ride.
    let e = env_prod("oth");
    let outside = e.home.join("elsewhere").join("z");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    git(
        &e.main,
        &["worktree", "add", "-q", outside.to_str().unwrap()],
    );
    std::fs::write(outside.join("new2.rs"), "n").unwrap();
    let hdir = e.home.join("docs");
    std::fs::create_dir_all(&hdir).unwrap();
    std::fs::write(e.main.join("new2.rs"), "n").unwrap();
    let mut bad = vec![];
    for c in ["git add -A", "git commit -a -m x"] {
        let k = learn(&e, &bash(&e.wt, c));
        let p = bash(&e.wt2, c);
        println!(
            "MEASURE cwd = other session's worktree: `{c}` => {}",
            shape(&e, &p)
        );
        let d = decide_with(&e, &table_recorded(&e, &k), &p);
        if d != "allow" {
            bad.push(format!(
                "`{c}` in another session's container worktree {} with the learned key {k} => \
                 {d} (ruling: Allow-eligible)",
                e.wt2.display()
            ));
        }
        for cwd in [&e.main, &outside, &hdir] {
            if let Some(x) = rides(&e, &k, &bash(cwd, c)) {
                bad.push(x);
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ symlinked containers

/// Containers that are symlinks: (a) `<home>/src2/.harness-worktrees ->
/// <home>/docs` (a HOME directory with the user's files), (b)
/// `<m3>/.claude/worktrees -> <m3>/src` (a directory of the main tree), (c)
/// `$HOME/.condukt/worktrees -> <main>/src`. A worktree added through each
/// link lands in the link target. Expected (guard): no learned Allow for
/// `git add -A`, `git commit -a -m x` or `rm -rf build` from the worktree,
/// spelled through the link or by its real path.
#[test]
fn r15_symlinked_containers_are_not_containers() {
    let e = env_prod("symc");
    let k_add = learn(&e, &bash(&e.wt, "git add -A"));
    let k_ca = learn(&e, &bash(&e.wt, "git commit -a -m x"));
    let sp_rm = learn(&e, &bash(&e.sp, "rm -rf build"));
    let wt_rm = {
        let s = shape(&e, &bash(&e.wt, "rm -rf build"));
        println!("MEASURE own harness worktree `rm -rf build`: {s}");
        key(&s)
    };
    // (a)
    let m2 = e.home.join("src2").join("m2");
    new_repo(&m2);
    let docs = e.home.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::os::unix::fs::symlink(&docs, e.home.join("src2").join(".harness-worktrees")).unwrap();
    let a = e.home.join("src2").join(".harness-worktrees").join("s");
    git(&m2, &["worktree", "add", "-q", a.to_str().unwrap()]);
    // (b)
    let m3 = e.home.join("src3").join("m3");
    new_repo(&m3);
    std::fs::create_dir_all(m3.join(".claude")).unwrap();
    std::os::unix::fs::symlink(m3.join("src"), m3.join(".claude").join("worktrees")).unwrap();
    let b = m3.join(".claude").join("worktrees").join("w");
    git(&m3, &["worktree", "add", "-q", b.to_str().unwrap()]);
    // (c)
    std::fs::create_dir_all(e.home.join(".condukt")).unwrap();
    std::os::unix::fs::symlink(
        e.main.join("src"),
        e.home.join(".condukt").join("worktrees"),
    )
    .unwrap();
    let c = e.home.join(".condukt").join("worktrees").join("run-t1");
    git(&e.main, &["worktree", "add", "-q", c.to_str().unwrap()]);
    let mut bad = vec![];
    for w in [&a, &b, &c] {
        let real = canon(w);
        std::fs::create_dir_all(real.join("build")).unwrap();
        std::fs::write(real.join("new2.rs"), "n").unwrap();
        for cwd in [w.clone(), real.clone()] {
            for (k, cmd) in [
                (&k_add, "git add -A"),
                (&k_ca, "git commit -a -m x"),
                (&wt_rm, "rm -rf build"),
                (&sp_rm, "rm -rf build"),
            ] {
                if k.is_empty() {
                    continue;
                }
                if let Some(x) = rides(&e, k, &bash(&cwd, cmd)) {
                    bad.push(x);
                }
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ commondir forgery

/// The own worktree's admin `commondir` rewritten to a decoy repository
/// `<home>/src/decoy/.git` (so the "main tree" would be `decoy`, whose parent's
/// `.harness-worktrees` is the real container). Precondition (real git): git
/// then reports decoy's `.git` as x's common dir. Expected (guard): the git dir
/// is no longer `<common>/worktrees/<name>`, so no learned Allow.
#[test]
fn r15_commondir_forgery_does_not_place_the_worktree() {
    let e = env_prod("cdir");
    let k_add = learn(&e, &bash(&e.wt, "git add -A"));
    let k_ca = learn(&e, &bash(&e.wt, "git commit -a -m x"));
    let decoy = e.home.join("src").join("decoy");
    new_repo(&decoy);
    let admin = e.main.join(".git").join("worktrees").join("x");
    std::fs::write(
        admin.join("commondir"),
        format!("{}\n", decoy.join(".git").display()),
    )
    .unwrap();
    let l = layout(&e.wt);
    assert_eq!(
        l.as_ref().map(|l| l.2.clone()),
        Some(canon(&decoy.join(".git"))),
        "precondition: git reports the decoy as x's common dir: {l:?}"
    );
    let mut bad = vec![];
    for (k, c) in [(&k_add, "git add -A"), (&k_ca, "git commit -a -m x")] {
        let p = bash(&e.wt, c);
        println!("MEASURE commondir forged: `{c}` => {}", shape(&e, &p));
        if let Some(x) = rides(&e, k, &p) {
            bad.push(x);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ scratchpad position

/// The `claude-*` component is not checked against the uid or the owner
/// (`scope.rs::driver_trusts_program` checks both; `shape::is_own_scratchpad`
/// only the prefix). Guards (expected green): a `claude-*` component that is
/// a SYMLINK to HOME (`/private/tmp/claude-v15lnk-* -> <home>`, so
/// `.../-proj/<SID>/scratchpad/docs` is `<home>/-proj/<SID>/scratchpad/docs`),
/// and a scratchpad directory that is itself a symlink to `<home>/docs`, give
/// no learned Allow for `rm -rf` (absolute, or relative from the linked cwd).
/// Precondition (real rm, twin): rm through the link deletes the HOME file.
#[test]
fn r15_scratchpad_components_that_are_symlinks_into_home() {
    let h = env_prod("splharm");
    let hdocs = h.home.join("docs");
    std::fs::create_dir_all(hdocs.join("d")).unwrap();
    std::fs::write(hdocs.join("d").join("keep"), "user").unwrap();
    let hsp2 = h.root.join("-proj2").join(SID).join("scratchpad");
    std::fs::create_dir_all(hsp2.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&hdocs, &hsp2).unwrap();
    let out = Command::new("rm")
        .args(["-rf", hsp2.join("d").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success() && !hdocs.join("d").exists(),
        "precondition: rm -rf through a scratchpad symlink deletes HOME's docs/d"
    );

    let e = env_prod("spl");
    let k = learn(&e, &bash(&e.sp, "rm -rf build"));
    // A claude-* component that is a symlink to HOME.
    let lnk = Link(PathBuf::from(format!("{}-lnk", e.root.display())));
    std::os::unix::fs::symlink(&e.home, &lnk.0).unwrap();
    let hsp = e.home.join("-proj").join(SID).join("scratchpad");
    std::fs::create_dir_all(hsp.join("docs")).unwrap();
    std::fs::write(hsp.join("docs").join("keep"), "user").unwrap();
    let via = lnk.0.join("-proj").join(SID).join("scratchpad");
    // A scratchpad that is itself a symlink to HOME's docs.
    let docs = e.home.join("docs");
    std::fs::create_dir_all(docs.join("d")).unwrap();
    let sp2 = e.root.join("-proj2").join(SID).join("scratchpad");
    std::fs::create_dir_all(sp2.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&docs, &sp2).unwrap();
    let mut bad = vec![];
    for (cwd, c) in [
        (e.sp.clone(), format!("rm -rf {}/docs", via.display())),
        (via.clone(), "rm -rf docs".to_string()),
        (e.sp.clone(), format!("rm -rf {}/d", sp2.display())),
        (sp2.clone(), "rm -rf d".to_string()),
    ] {
        if let Some(x) = rides(&e, &k, &bash(&cwd, &c)) {
            bad.push(x);
        }
    }
    assert!(
        hsp.join("docs").join("keep").exists() && docs.join("d").exists(),
        "fixture: the hook deleted nothing"
    );
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Measurement (FP side, printed, asserts only that the own `/private/tmp`
/// spelling stays Allow): own scratchpad `rm -rf build` with the cwd and the
/// target spelled `/tmp` / `/private/tmp` in all four combinations.
#[test]
fn r15_tmp_private_tmp_mixed_spelling_measure() {
    let e = env_prod("mix");
    let k = learn(&e, &bash(&e.sp, "rm -rf build"));
    let as_tmp = |p: &Path| PathBuf::from(p.to_str().unwrap().replacen("/private/tmp", "/tmp", 1));
    for cwd in [e.sp.clone(), as_tmp(&e.sp)] {
        for t in [e.sp.clone(), as_tmp(&e.sp)] {
            let p = bash(&cwd, &format!("rm -rf {}/build", t.display()));
            println!(
                "MEASURE cwd {} target {}: shape {} learned => {}",
                cwd.display(),
                t.display(),
                shape(&e, &p),
                decide_with(&e, &table_recorded(&e, &k), &p)
            );
        }
    }
    assert_eq!(
        decide_with(&e, &table_recorded(&e, &k), &bash(&e.sp, "rm -rf build")),
        "allow"
    );
}

// ------------------------------------------------ rules file in the real layout

/// Spec (askrules, ask_rules_toml (d)): the bundled rules file is protected
/// config, "a Write tool call to it is never Allow (the bundled path inside a
/// repo ...)". In the real layout the own worktree sits in a worktree STORAGE
/// root (`.harness-worktrees`, `$HOME/.condukt/worktrees`), where the
/// protected-path rule is lifted for the checkout's own files (user ruling
/// 2026-10-03, `exclude.rs` PROTECTED_GLOBS doc).
///
/// re-anchored 2026-11-28 (leader ruling: rules source copy follows 2026-10-03)
/// Before: expected not Allow in any of the three containers. After: the
/// source copy inside a container worktree follows ruling 2026-10-03 (Allow
/// is permitted, measured and printed, not asserted either way); the rules
/// DEPLOYED in the plugin cache (`$HOME/.claude/plugins/cache/<mkt>/blastguard/
/// <ver>/rules/ask-rules.toml`, sandbox HOME) are never Allow, with the cwd in
/// any of the three container worktrees.
#[test]
fn r15_bundled_rules_write_in_real_worktree_containers() {
    let e = env_prod("rules");
    let cc = e.main.join(".claude").join("worktrees").join("n");
    std::fs::create_dir_all(cc.parent().unwrap()).unwrap();
    git(&e.main, &["worktree", "add", "-q", cc.to_str().unwrap()]);
    let ct = e.home.join(".condukt").join("worktrees").join("run-t1");
    std::fs::create_dir_all(ct.parent().unwrap()).unwrap();
    git(&e.main, &["worktree", "add", "-q", ct.to_str().unwrap()]);
    let cached = e
        .home
        .join(".claude/plugins/cache/yukineko/blastguard/0.2.1010/rules/ask-rules.toml");
    std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
    std::fs::write(&cached, "").unwrap();
    let mut bad = vec![];
    for w in [&e.wt, &cc, &ct] {
        let f = w.join("crates/blastguard/rules/ask-rules.toml");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        let p = json!({
            "hook_event_name": "PreToolUse", "session_id": SID, "cwd": w,
            "tool_name": "Write",
            "tool_input": {"file_path": f, "content": "[[allow]]\n"},
        });
        let d = decide_with(&e, &no_table(&e), &p);
        println!("MEASURE source copy Write {} => {d}", f.display());
        let p = json!({
            "hook_event_name": "PreToolUse", "session_id": SID, "cwd": w,
            "tool_name": "Write",
            "tool_input": {"file_path": cached, "content": "[[allow]]\n"},
        });
        let d = decide_with(&e, &no_table(&e), &p);
        println!("MEASURE plugin cache Write {} => {d}", cached.display());
        if d == "allow" {
            bad.push(format!(
                "Write {} (cwd {}) => allow",
                cached.display(),
                w.display()
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ this checkout (real HOME)

/// Real-layout probe on THIS checkout (read-only: `blastguard shape` only,
/// with the real HOME): when the test runs from a linked worktree in
/// `<main parent>/.harness-worktrees`, `git add -A` there is `s4-worktree`
/// and Allow with its own key; when it runs from the main tree it is
/// `s3-main-tree`.
#[test]
fn r15_fp_this_checkout_real_layout() {
    let d = tempfile::Builder::new()
        .prefix("claude-v15-self-")
        .tempdir_in("/private/tmp")
        .unwrap();
    let repo = canon(&Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".."));
    let Some((top, gd, cd)) = layout(&repo) else {
        panic!("precondition: git answers in {}", repo.display());
    };
    let e = Env {
        root: d.path().to_path_buf(),
        home: PathBuf::new(),
        hook_home: std::env::var_os("HOME"),
        hook_cwd: None,
        main: PathBuf::new(),
        wt: PathBuf::new(),
        wt2: PathBuf::new(),
        sp: PathBuf::new(),
        _home: tempfile::tempdir().unwrap(),
        _dir: d,
    };
    std::fs::create_dir_all(e.root.join("approvals")).unwrap();
    let p = bash(&top, "git add -A");
    let s = shape(&e, &p);
    println!("MEASURE this checkout {}: {s}", top.display());
    if gd == cd {
        assert_eq!(class(&key(&s)), "s3-main-tree", "main tree: {s}");
        return;
    }
    let in_harness_container = top.parent()
        == cd
            .parent()
            .and_then(|m| m.parent())
            .map(|p| p.join(".harness-worktrees"))
            .as_deref();
    assert!(
        in_harness_container,
        "precondition: this checkout is in .harness-worktrees: {top:?} {cd:?}"
    );
    assert_eq!(class(&key(&s)), "s4-worktree", "{s}");
    assert_eq!(decide_with(&e, &table_recorded(&e, &key(&s)), &p), "allow");
}
