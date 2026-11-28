#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Verifier round 16 (2026-11-28, against 514947932 / blastguard 0.2.1010):
//! attacks `shape::containing_linked_root` (user ruling 2026-11-28
//! 「コンテナ内なら可」: ANY real linked worktree directly in a fixed container is
//! `Worktree`, the cwd need not be in it), the non-absolute-HOME refusal of
//! container (c), the `/tmp` -> `/private/tmp` spelling alias, and the leader
//! ruling on rules files (plugin cache / `~/.claude` never Allow).
//! Written by an independent verifier, not the implementer (CLAUDE.md §2(a)).
//! Black-box: spawns the built binary with a sandbox HOME and a hand-written
//! learned table that records the probe's sibling key at the learning
//! thresholds (3 approvals from 2 sessions, 0 declines).
//!
//! Fixtures (the real layout): root `/private/tmp/claude-v16-*`, HOME
//! `<root>/home-*`, main tree `<home>/src/harness`, own worktree
//! `<home>/src/.harness-worktrees/x`, another session's worktree
//! `<home>/src/.harness-worktrees/y` (uncommitted work), scratchpad
//! `<root>/-proj/<SID>/scratchpad`. Harm preconditions are observed with real
//! git / real rm on a twin fixture.
//!
//! What these tests do NOT prove: that the learner would promote the entry
//! (the table is written by hand); realpath TOCTOU (out of scope); behaviour
//! on a host where `/private/tmp` is not reached through a `/tmp` symlink
//! (out of scope); what Claude Code puts in HOME.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000f6";
const OTHER_SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000a6";
/// Classes an Allow rule may match only when the target is placed: a probe
/// that must not be placed must never be one of these.
const PLACED: &[&str] = &["s4-none", "s4-worktree", "s4-scratchpad"];

struct Env {
    _dir: tempfile::TempDir,
    _home: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    hook_home: Option<OsString>,
    hook_cwd: Option<PathBuf>,
    main: PathBuf,
    wts: PathBuf,
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
        .prefix(&format!("claude-v16-{name}-"))
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
    std::fs::create_dir_all(wt2.join("build")).unwrap();
    std::fs::write(main.join("new.rs"), "main untracked").unwrap();
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
        wts,
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

fn write_tool(cwd: &Path, path: &Path) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID,
        "cwd": cwd,
        "tool_name": "Write",
        "tool_input": { "file_path": path, "content": "[[rule]]\n" },
    })
}

fn edit_tool(cwd: &Path, path: &Path) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID,
        "cwd": cwd,
        "tool_name": "Edit",
        "tool_input": { "file_path": path, "old_string": "a", "new_string": "b" },
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

fn describe(p: &Value) -> String {
    let ti = &p["tool_input"];
    let what = ti["command"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{} {}",
                p["tool_name"].as_str().unwrap_or(""),
                ti["file_path"].as_str().unwrap_or("")
            )
        });
    format!("`{what}` (cwd {})", p["cwd"].as_str().unwrap_or(""))
}

/// Learn `sibling`'s key (it must Ask and be Allow with its recorded key).
fn learn(e: &Env, sibling: &Value) -> String {
    let s = shape(e, sibling);
    assert_eq!(
        s["decision"],
        "ask",
        "precondition: sibling {} Asks: {s}",
        describe(sibling)
    );
    let k = key(&s);
    assert_eq!(
        decide_with(e, &table_recorded(e, &k), sibling),
        "allow",
        "precondition: sibling {} is Allow with its recorded key {k}",
        describe(sibling)
    );
    k
}

/// `Some(failure)` when `probe` is Allow with the recorded key `k` although it
/// is not Allow without a table. A detector Allow is printed and skipped.
fn rides(e: &Env, k: &str, probe: &Value) -> Option<String> {
    let before = decide_with(e, &no_table(e), probe);
    if before == "allow" {
        println!(
            "DETECTOR allow (not an askrules ride): {} shape {}",
            describe(probe),
            shape(e, probe)
        );
        return None;
    }
    let after = decide_with(e, &table_recorded(e, k), probe);
    (after == "allow").then(|| {
        format!(
            "probe {} was {before} without a table but Allow with the recorded key {k}; probe \
             shape {}",
            describe(probe),
            shape(e, probe)
        )
    })
}

/// `Some(failure)` when the askrules side may place `probe`: its class is a
/// placed one (whatever the detector decides), or it Asks and is Allow with
/// its OWN key. A detector Allow is printed (`DETECTOR allow`), not judged
/// here: the detector is unchanged by the diff under test (see
/// `r16_detector_allows_seen_in_round16`).
fn never_placed(e: &Env, probe: &Value) -> Option<String> {
    let s = shape(e, probe);
    let k = key(&s);
    let d = s["decision"].as_str().unwrap_or("").to_string();
    println!("MEASURE {} => {d} {k}", describe(probe));
    if PLACED.contains(&class(&k)) {
        return Some(format!("{} is placed: {k} ({d})", describe(probe)));
    }
    if d == "allow" {
        println!(
            "DETECTOR allow (not an askrules ride): {} {k}",
            describe(probe)
        );
        return None;
    }
    if d != "ask" {
        return None;
    }
    let after = decide_with(e, &table_recorded(e, &k), probe);
    (after == "allow").then(|| format!("{} is Allow with its own key {k}", describe(probe)))
}

/// Own key Allow (or detector Allow) for `p`; `Some(failure)` otherwise.
fn own_key_allow(e: &Env, p: &Value) -> Option<String> {
    let s = shape(e, p);
    let d = if s["decision"] == "ask" {
        decide_with(e, &table_recorded(e, &key(&s)), p)
    } else {
        s["decision"].as_str().unwrap_or("").to_string()
    };
    println!("MEASURE FP {} => {d} ({})", describe(p), key(&s));
    (d != "allow").then(|| format!("{} with its recorded key {} => {d}", describe(p), key(&s)))
}

/// `/private/tmp/...` spelled `/tmp/...`.
fn as_tmp(p: &Path) -> PathBuf {
    PathBuf::from(p.to_str().unwrap().replacen("/private/tmp/", "/tmp/", 1))
}

/// A symlink outside the tempdir, removed on drop.
struct Link(PathBuf);
impl Drop for Link {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ------------------------------------------------ containing_linked_root

/// The checkout ROOT of a container worktree and the CONTAINER itself are not
/// `Worktree` (spec Components 1: "only inside an individual checkout").
/// Precondition (real rm, twin): `rm -rf <y>/` deletes y's uncommitted and
/// untracked work. Probes from cwd = own worktree x and from cwd = main:
/// `rm -rf <y>`, `rm -rf <y>/`, `rm -rf <x>`, `rm -rf <container>`, `rm -rf
/// <container>/`. Expected: never placed, never Allow with the own key (the
/// detector Allows measured for these are in
/// `r16_detector_allows_seen_in_round16`).
#[test]
fn r16_container_worktree_root_and_container_are_never_placed() {
    let h = env_prod("rootharm");
    let out = Command::new("rm")
        .args(["-rf", &format!("{}/", h.wt2.display())])
        .output()
        .unwrap();
    assert!(
        out.status.success() && !h.wt2.join("wip.rs").exists(),
        "precondition: rm -rf <y>/ deletes y's untracked work"
    );

    let e = env_prod("root");
    let y = e.wt2.display().to_string();
    let x = e.wt.display().to_string();
    let c = e.wts.display().to_string();
    let mut bad = vec![];
    for cwd in [&e.wt, &e.main] {
        for cmd in [
            format!("rm -rf {y}"),
            format!("rm -rf {y}/"),
            format!("rm -rf {x}"),
            format!("rm -rf {c}"),
            format!("rm -rf {c}/"),
        ] {
            if let Some(f) = never_placed(&e, &bash(cwd, &cmd)) {
                bad.push(f);
            }
        }
    }
    assert!(
        e.wt2.join("wip.rs").exists(),
        "fixture: the hook deleted nothing"
    );
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Symlinks onto a container worktree: `<home>/lnk -> <y>` (outside the
/// container), `<container>/lnk2 -> <y>` (an entry IN the container that is a
/// link), and `<x>/m -> <main>` (a link inside the own worktree onto the main
/// tree). Probes: `rm -rf` of a child through each link, of the link spelled
/// with a trailing `/`, and `git add -A` with the cwd spelled through each
/// link. Expected: never placed; no ride of the own `git add -A` key.
/// Precondition (real rm, twin): `rm -rf <x>/m/src` deletes the MAIN tree's
/// tracked `src`.
#[test]
fn r16_symlinks_onto_container_worktrees_are_never_placed() {
    let h = env_prod("lnkharm");
    std::os::unix::fs::symlink(&h.main, h.wt.join("m")).unwrap();
    let out = Command::new("rm")
        .args(["-rf", h.wt.join("m").join("src").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success() && !h.main.join("src").exists(),
        "precondition: rm -rf <x>/m/src deletes the main tree's src"
    );

    let e = env_prod("lnk");
    let k_add = learn(&e, &bash(&e.wt, "git add -A"));
    let l1 = e.home.join("lnk");
    std::os::unix::fs::symlink(&e.wt2, &l1).unwrap();
    let l2 = e.wts.join("lnk2");
    std::os::unix::fs::symlink(&e.wt2, &l2).unwrap();
    let l3 = e.wt.join("m");
    std::os::unix::fs::symlink(&e.main, &l3).unwrap();
    let mut bad = vec![];
    for l in [&l1, &l2, &l3] {
        let ls = l.display().to_string();
        for cmd in [
            format!("rm -rf {ls}/src"),
            format!("rm -rf {ls}/build"),
            format!("rm -rf {ls}/"),
        ] {
            if let Some(f) = never_placed(&e, &bash(&e.wt, &cmd)) {
                bad.push(f);
            }
        }
        if let Some(f) = never_placed(&e, &write_tool(&e.wt, &l.join("src").join("lib.rs"))) {
            bad.push(f);
        }
        if let Some(f) = rides(&e, &k_add, &bash(l, "git add -A")) {
            bad.push(f);
        }
    }
    assert!(
        e.main.join("src").exists() && e.wt2.join("wip.rs").exists(),
        "fixture: the hook deleted nothing"
    );
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The main tree spelled THROUGH a container worktree with `..`:
/// `<x>/../../harness/src` is `<main>/src` (precondition: canonicalises
/// there). Probes (cwd x): `rm -rf <x>/../../harness/src`, `rm -rf
/// ../../harness/src`, the Write and Edit tools on
/// `<x>/../../harness/src/lib.rs`, `git -C <x>/../../harness add -A`.
/// Expected: never placed; `git -C ...` does not ride the own `git add -A` key.
#[test]
fn r16_main_tree_spelled_through_a_worktree_with_dotdot() {
    let e = env_prod("dotdot");
    let k_add = learn(&e, &bash(&e.wt, "git add -A"));
    let via = e.wt.join("..").join("..").join("harness");
    assert_eq!(
        canon(&via.join("src")),
        canon(&e.main.join("src")),
        "precondition: the spelling reaches the main tree"
    );
    let v = via.display().to_string();
    let mut bad = vec![];
    for p in [
        bash(&e.wt, &format!("rm -rf {v}/src")),
        bash(&e.wt, "rm -rf ../../harness/src"),
        write_tool(&e.wt, &via.join("src").join("lib.rs")),
        edit_tool(&e.wt, &via.join("src").join("lib.rs")),
    ] {
        if let Some(f) = never_placed(&e, &p) {
            bad.push(f);
        }
    }
    if let Some(f) = rides(&e, &k_add, &bash(&e.wt, &format!("git -C {v} add -A"))) {
        bad.push(f);
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// cwd = the MAIN tree and the git repository named with `-C`: `git -C <x>
/// add -A` acts on the container worktree x (ruling: Allow-eligible; measured
/// and printed, not asserted), while from cwd = x, `git -C <main> add -A` and
/// from cwd = main a plain `git add -A` act on the main tree. Precondition
/// (real git, twin): `git -C <x> add -A` from main stages only x's files and
/// leaves the main index empty. Expected: the main-tree probes never ride the
/// own `git add -A` key, and a path target in the main tree from cwd = main is
/// never placed.
#[test]
fn r16_git_dash_c_between_main_and_container_worktree() {
    let h = env_prod("gcharm");
    let out = Command::new("git")
        .args(["-C", h.wt.to_str().unwrap(), "add", "-A"])
        .current_dir(&h.main)
        .output()
        .unwrap();
    assert!(out.status.success());
    let main_staged = git_out(&h.main, &["diff", "--cached", "--name-only"]);
    let wt_staged = git_out(&h.wt, &["diff", "--cached", "--name-only"]);
    assert!(
        main_staged.trim().is_empty() && wt_staged.lines().any(|l| l == "new.rs"),
        "precondition: -C <x> stages x only: main {main_staged:?} x {wt_staged:?}"
    );

    let e = env_prod("gc");
    let k_add = learn(&e, &bash(&e.wt, "git add -A"));
    let x = e.wt.display().to_string();
    let m = e.main.display().to_string();
    let p = bash(&e.main, &format!("git -C {x} add -A"));
    println!(
        "MEASURE cwd main `git -C <x> add -A`: shape {} no table {} with own x key {}",
        shape(&e, &p),
        decide_with(&e, &no_table(&e), &p),
        decide_with(&e, &table_recorded(&e, &k_add), &p)
    );
    let mut bad = vec![];
    for p in [
        bash(&e.wt, &format!("git -C {m} add -A")),
        bash(&e.main, "git add -A"),
        bash(&e.main, "git commit -a -m x"),
    ] {
        if let Some(f) = rides(&e, &k_add, &p) {
            bad.push(f);
        }
    }
    for p in [
        bash(&e.main, "rm -rf src"),
        bash(&e.main, &format!("rm -rf {m}/src")),
    ] {
        if let Some(f) = never_placed(&e, &p) {
            bad.push(f);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The `.git` LINK FILE of another session's container worktree is git
/// metadata, not a checked-out file: rewriting it redirects y onto another
/// admin dir (precondition, real git on a twin: after writing `<y>/.git` =
/// `gitdir: <admin x>`, git in y reports x's branch), removing it detaches y.
/// Probes (cwd x): the Write tool on `<y>/.git`, `rm -f <y>/.git`, `rm -rf
/// <y>/.git`. Expected: never placed (the same family as the forging steps of
/// `r15_dotgit_forging_steps_are_not_allow`).
#[test]
fn r16_dotgit_link_file_of_a_container_worktree_is_never_placed() {
    let h = env_prod("dgharm");
    let admin_x = h.main.join(".git").join("worktrees").join("x");
    std::fs::write(
        h.wt2.join(".git"),
        format!("gitdir: {}\n", admin_x.display()),
    )
    .unwrap();
    let br = git_out(&h.wt2, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(br.trim(), "x", "precondition: y now runs on x's branch");

    let e = env_prod("dg");
    let y = e.wt2.display().to_string();
    let mut bad = vec![];
    for p in [
        write_tool(&e.wt, &e.wt2.join(".git")),
        bash(&e.wt, &format!("rm -f {y}/.git")),
        bash(&e.wt, &format!("rm -rf {y}/.git")),
    ] {
        if let Some(f) = never_placed(&e, &p) {
            bad.push(f);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Measurement only (ruling question, nothing asserted): `git worktree remove
/// --force <y>` from cwd x removes another session's worktree with its
/// uncommitted work; its repository class under the new ruling.
#[test]
fn r16_measure_worktree_remove_of_another_session() {
    let e = env_prod("wtrm");
    let y = e.wt2.display().to_string();
    for c in [
        format!("git worktree remove --force {y}"),
        format!("git worktree remove {y}"),
    ] {
        let p = bash(&e.wt, &c);
        let s = shape(&e, &p);
        println!(
            "MEASURE `{c}` (cwd x): shape {s} with own key => {}",
            decide_with(&e, &table_recorded(&e, &key(&s)), &p)
        );
    }
}

/// Detector side (no table), observed at 514947932 by the class tests above:
/// these are Allow although the harm is real (precondition, real rm on a
/// twin): (1) `rm -rf <y>` / `rm -rf <y>/` -- another session's whole
/// container worktree (root; its uncommitted and untracked work), from cwd x;
/// (2) `rm -rf <x>/m/src` and the Write tool on `<x>/m/src/lib.rs` where
/// `<x>/m -> <main>` (the MAIN tree's tracked files through a link in the own
/// worktree); (3) `rm -rf ../../harness/src` and the Write tool on
/// `<x>/../../harness/src/lib.rs` from cwd x (the main tree through `..`,
/// CLAUDE.md §8). Expected: not Allow. Not caused by the diff under test
/// (`shape.rs` classes feed only the askrules judge).
#[test]
#[ignore = "RED: detector — rm/Write of another container worktree root or the main tree via symlink/.. from a worktree is Allow; filed separately"]
fn r16_detector_allows_seen_in_round16() {
    let h = env_prod("detharm");
    std::os::unix::fs::symlink(&h.main, h.wt.join("m")).unwrap();
    let out = Command::new("rm")
        .args(["-rf", h.wt.join("m").join("src").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success() && !h.main.join("src").exists(),
        "precondition: rm -rf <x>/m/src deletes the main tree's src"
    );
    let out = Command::new("rm")
        .args(["-rf", h.wt2.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success() && !h.wt2.exists(),
        "precondition: rm -rf <y> deletes another session's worktree"
    );

    let e = env_prod("det");
    std::os::unix::fs::symlink(&e.main, e.wt.join("m")).unwrap();
    let y = e.wt2.display().to_string();
    let m = e.wt.join("m");
    let via = e.wt.join("..").join("..").join("harness");
    let mut bad = vec![];
    for p in [
        bash(&e.wt, &format!("rm -rf {y}")),
        bash(&e.wt, &format!("rm -rf {y}/")),
        bash(&e.wt, &format!("rm -rf {}/src", m.display())),
        write_tool(&e.wt, &m.join("src").join("lib.rs")),
        bash(&e.wt, "rm -rf ../../harness/src"),
        write_tool(&e.wt, &via.join("src").join("lib.rs")),
    ] {
        let d = decide_with(&e, &no_table(&e), &p);
        println!("MEASURE detector {} => {d}", describe(&p));
        if d == "allow" {
            bad.push(format!("{} => allow", describe(&p)));
        }
    }
    assert!(
        e.main.join("src").exists() && e.wt2.exists(),
        "fixture: the hook deleted nothing"
    );
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ `/tmp` alias

/// `/tmp` spellings: (1) ANOTHER session's scratchpad spelled `/tmp/...`
/// (absolute target from the own scratchpad, and as the cwd with a relative
/// target); (2) a `claude-*` component that is a SYMLINK to HOME, spelled
/// through `/tmp` (`/tmp/claude-v16lnk-* -> <home>`); (3) a scratchpad-shaped
/// path under `$TMPDIR` (`/var/folders/...`, spelled `/var/...` and
/// `/private/var/...`: `$TMPDIR` is NOT a root, and `/var` is not the alias).
/// Precondition (real rm, twin): rm through (2) deletes the HOME file.
/// Expected: no ride of the own scratchpad `rm -rf build` key.
#[test]
fn r16_tmp_alias_other_sid_symlinked_claude_dir_and_var() {
    let h = env_prod("tmpharm");
    let hsp = h.home.join("-proj").join(SID).join("scratchpad");
    std::fs::create_dir_all(hsp.join("docs")).unwrap();
    std::fs::write(hsp.join("docs").join("keep"), "user").unwrap();
    let hl = Link(PathBuf::from(format!("{}-lnk", h.root.display())));
    std::os::unix::fs::symlink(&h.home, &hl.0).unwrap();
    let through = as_tmp(&hl.0).join("-proj").join(SID).join("scratchpad");
    let out = Command::new("rm")
        .args(["-rf", through.join("docs").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success() && !hsp.join("docs").exists(),
        "precondition: rm through /tmp/<claude-link> deletes HOME's file"
    );

    let e = env_prod("tmp");
    let k = learn(&e, &bash(&e.sp, "rm -rf build"));
    let other = e.root.join("-proj").join(OTHER_SID).join("scratchpad");
    std::fs::create_dir_all(other.join("build")).unwrap();
    let lnk = Link(PathBuf::from(format!("{}-lnk", e.root.display())));
    std::os::unix::fs::symlink(&e.home, &lnk.0).unwrap();
    let hsp = e.home.join("-proj").join(SID).join("scratchpad");
    std::fs::create_dir_all(hsp.join("docs")).unwrap();
    std::fs::write(hsp.join("docs").join("keep"), "user").unwrap();
    let via = as_tmp(&lnk.0).join("-proj").join(SID).join("scratchpad");
    let tv = tempfile::Builder::new()
        .prefix("claude-v16var-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let vsp = tv.path().join("-proj").join(SID).join("scratchpad");
    std::fs::create_dir_all(vsp.join("build")).unwrap();
    let vreal = canon(&vsp);
    let mut probes = vec![
        bash(&e.sp, &format!("rm -rf {}/build", as_tmp(&other).display())),
        bash(&as_tmp(&other), "rm -rf build"),
        bash(&e.sp, &format!("rm -rf {}/docs", via.display())),
        bash(&via, "rm -rf docs"),
        bash(&e.sp, &format!("rm -rf {}/build", vreal.display())),
        bash(&vreal, "rm -rf build"),
    ];
    if vsp != vreal {
        probes.push(bash(&e.sp, &format!("rm -rf {}/build", vsp.display())));
        probes.push(bash(&vsp, "rm -rf build"));
    }
    let mut bad = vec![];
    for p in probes {
        if let Some(f) = rides(&e, &k, &p) {
            bad.push(f);
        }
    }
    assert!(
        hsp.join("docs").join("keep").exists() && other.join("build").exists(),
        "fixture: the hook deleted nothing"
    );
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ HOME of the hook process

/// HOME values for container (c) `$HOME/.condukt/worktrees`: empty, relative,
/// the literal `~` (with `<root>/~ -> <home>` and the hook cwd = root, so a
/// cwd-relative resolution WOULD land on the real home) must not place the
/// condukt worktree (no ride of the own `git add -A` key); a HOME with a
/// trailing `/`, and a symlinked HOME with a trailing `/`, are the same home
/// and keep the learned Allow (FP side).
#[test]
fn r16_home_spellings_for_the_condukt_container() {
    let mut e = env_prod("home");
    let ct = e.home.join(".condukt").join("worktrees").join("run-t1");
    std::fs::create_dir_all(ct.parent().unwrap()).unwrap();
    git(&e.main, &["worktree", "add", "-q", ct.to_str().unwrap()]);
    std::fs::write(ct.join("new2.rs"), "n").unwrap();
    let k = learn(&e, &bash(&e.wt, "git add -A"));
    let tilde = e.root.join("~");
    std::os::unix::fs::symlink(&e.home, &tilde).unwrap();
    assert_eq!(
        canon(&tilde),
        canon(&e.home),
        "precondition: <root>/~ is home"
    );
    let hl = e.root.join("hl");
    std::os::unix::fs::symlink(&e.home, &hl).unwrap();
    let mut bad = vec![];
    for (label, h, cwd) in [
        ("empty", OsString::new(), None),
        (
            "tilde, hook cwd = root",
            OsString::from("~"),
            Some(e.root.clone()),
        ),
        (
            "relative name, hook cwd = root",
            e.home.file_name().unwrap().to_os_string(),
            Some(e.root.clone()),
        ),
        (
            "relative `./.`, hook cwd = home",
            OsString::from("./."),
            Some(e.home.clone()),
        ),
    ] {
        e.hook_home = Some(h);
        e.hook_cwd = cwd;
        let p = bash(&ct, "git add -A");
        println!("MEASURE HOME {label}: {}", shape(&e, &p));
        if let Some(x) = rides(&e, &k, &p) {
            bad.push(format!("HOME {label}: {x}"));
        }
    }
    e.hook_cwd = None;
    for (label, h) in [
        ("trailing slash", format!("{}/", e.home.display())),
        ("symlink with trailing slash", format!("{}/", hl.display())),
    ] {
        e.hook_home = Some(OsString::from(h));
        let p = bash(&ct, "git add -A");
        let d = decide_with(&e, &table_recorded(&e, &k), &p);
        println!("MEASURE HOME {label}: {} => {d}", shape(&e, &p));
        if d != "allow" {
            bad.push(format!(
                "FP: HOME {label}: own condukt worktree `git add -A` => {d}"
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ rules files (leader ruling)

/// Leader ruling 2026-11-28: the rules DEPLOYED in the plugin cache
/// (`$HOME/.claude/plugins/cache/<mkt>/blastguard/<ver>/rules/ask-rules.toml`)
/// are never Allow, also when a HOME-class Write key is learned (`Home` is not
/// a never-Allow class). Probes: the Write and Edit tools on the cached file,
/// `printf x > <cached>`, `rm -rf <cache>/.../rules`. Siblings: the same tool
/// on `<home>/notes.txt` (learned when it Asks). Expected: none Allow.
#[test]
fn r16_plugin_cache_rules_never_allow_even_with_a_home_key() {
    let e = env_prod("cache");
    let rules = e
        .home
        .join(".claude/plugins/cache/yukineko/blastguard/0.2.1010/rules");
    std::fs::create_dir_all(&rules).unwrap();
    let cached = rules.join("ask-rules.toml");
    std::fs::write(&cached, "a").unwrap();
    let notes = e.home.join("notes.txt");
    std::fs::write(&notes, "a").unwrap();
    let mut bad = vec![];
    for (sib, probe) in [
        (write_tool(&e.wt, &notes), write_tool(&e.wt, &cached)),
        (edit_tool(&e.wt, &notes), edit_tool(&e.wt, &cached)),
        (
            bash(&e.wt, &format!("printf x > {}", notes.display())),
            bash(&e.wt, &format!("printf x > {}", cached.display())),
        ),
        (
            bash(&e.wt, &format!("rm -rf {}/docs", e.home.display())),
            bash(&e.wt, &format!("rm -rf {}", rules.display())),
        ),
    ] {
        let before = decide_with(&e, &no_table(&e), &probe);
        if before == "allow" {
            bad.push(format!("{} is a detector Allow", describe(&probe)));
            continue;
        }
        let s = shape(&e, &sib);
        println!(
            "MEASURE sibling {} => {s}; probe {} => {}",
            describe(&sib),
            describe(&probe),
            shape(&e, &probe)
        );
        let mut keys = vec![key(&shape(&e, &probe))];
        if s["decision"] == "ask" {
            keys.push(key(&s));
        }
        for k in keys {
            if decide_with(&e, &table_recorded(&e, &k), &probe) == "allow" {
                bad.push(format!("{} is Allow with key {k}", describe(&probe)));
            }
        }
    }
    assert_eq!(std::fs::read_to_string(&cached).unwrap(), "a", "fixture");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ FP: the real layout

/// FP (expected green): with the learned own keys, the real layout is Allow:
/// `git add -A` / `git commit -a -m x` in the own worktree x AND in another
/// session's worktree y (ruling 「コンテナ内なら可」); `rm -rf <y>/build` from
/// cwd x and `rm -rf <x>/build` from cwd = main (the cwd need not be in the
/// worktree); the own scratchpad `rm -rf build` with the cwd and the target
/// spelled `/tmp` and `/private/tmp` in all combinations.
#[test]
fn r16_fp_real_layout_learned_allow() {
    let e = env_prod("fp");
    std::fs::write(e.wt2.join("new2.rs"), "n").unwrap();
    let mut bad = vec![];
    for w in [&e.wt, &e.wt2] {
        for c in ["git add -A", "git commit -a -m x"] {
            if let Some(x) = own_key_allow(&e, &bash(w, c)) {
                bad.push(x);
            }
        }
    }
    for p in [
        bash(&e.wt, &format!("rm -rf {}/build", e.wt2.display())),
        bash(&e.main, &format!("rm -rf {}/build", e.wt.display())),
    ] {
        if let Some(x) = own_key_allow(&e, &p) {
            bad.push(x);
        }
    }
    for cwd in [e.sp.clone(), as_tmp(&e.sp)] {
        if let Some(x) = own_key_allow(&e, &bash(&cwd, "rm -rf build")) {
            bad.push(x);
        }
        for t in [e.sp.clone(), as_tmp(&e.sp)] {
            std::fs::create_dir_all(e.sp.join("build")).unwrap();
            let p = bash(&cwd, &format!("rm -rf {}/build", t.display()));
            if let Some(x) = own_key_allow(&e, &p) {
                bad.push(x);
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
