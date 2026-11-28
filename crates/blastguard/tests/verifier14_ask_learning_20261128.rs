#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Verifier round 14 (2026-11-28, against bbae037d1 / blastguard 0.2.1006):
//! attacks the round-13 fixes -- `GitLayout::linked_root` (a toplevel equal
//! to / inside / containing the main tree, or a common dir that is not
//! `<x>/.git`, is no linked root) and the Scratchpad class (only a path whose
//! component right before `scratchpad` is the payload `session_id`).
//! Written by an independent verifier, not the implementer (CLAUDE.md §2(a)).
//! Black-box: spawns the built binary with a sandbox HOME and a hand-written
//! learned table that records the sibling's key at the learning thresholds
//! (3 approvals from 2 sessions, 0 declines).
//!
//! Fixtures (the production layout): HOME is `<root>/home-*`, the main tree
//! `<home>/src/harness`, its linked worktree `<home>/src/.harness-worktrees/x`,
//! the scratchpad `<root>/-proj/<SID>/scratchpad`, all in a fresh `tempfile`
//! directory under `/private/tmp/claude-askrules`. Harm preconditions are
//! observed with real git / real rm.
//!
//! What these tests do NOT prove: that the learner would promote the entry
//! (the table is written by hand); that a session would build a forged layout
//! on its own (the fixture writes it); that Claude Code sends exactly these
//! `session_id` strings.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000f4";
const OTHER: &str = "5d0c1f0e-aaaa-4bbb-8ccc-00000000aaaa";
const BASE: &str = "/private/tmp/claude-askrules";

struct Env {
    _dir: tempfile::TempDir,
    _home: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    wt: PathBuf,
    main: PathBuf,
    /// `<root>/-proj` (holds `<sid>/scratchpad` of every session).
    proj: PathBuf,
    /// `<proj>/<SID>/scratchpad`.
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
        .prefix(&format!("v14-{name}-"))
        .tempdir_in(BASE)
        .unwrap();
    let root = dir.path().to_path_buf();
    let home_dir = tempfile::Builder::new()
        .prefix("home-")
        .tempdir_in(&root)
        .unwrap();
    let home = home_dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("approvals")).unwrap();
    let main = home.join("src").join("harness");
    std::fs::create_dir_all(main.join("src")).unwrap();
    std::fs::write(main.join("src").join("lib.rs"), "x").unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["add", "src"]);
    git(&main, &["commit", "-q", "-m", "i"]);
    let wt = home.join("src").join(".harness-worktrees").join("x");
    git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    std::fs::create_dir_all(wt.join("build")).unwrap();
    std::fs::write(wt.join("src").join("lib.rs"), "modified").unwrap();
    std::fs::write(wt.join("new.rs"), "n").unwrap();
    let proj = root.join("-proj");
    let sp = proj.join(SID).join("scratchpad");
    std::fs::create_dir_all(sp.join("build")).unwrap();
    Env {
        _dir: dir,
        _home: home_dir,
        root,
        home,
        wt,
        main,
        proj,
        sp,
    }
}

fn spawn(e: &Env, table: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
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

/// The hook's decision; empty stdout is Allow (also on a non-zero exit, which
/// Claude Code treats as a non-blocking hook error: the tool runs).
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

/// A Bash payload; `sid: None` leaves the `session_id` key out.
fn bash_sid(cwd: &Path, command: &str, sid: Option<&str>) -> Value {
    let mut v = json!({
        "hook_event_name": "PreToolUse",
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": command },
    });
    if let Some(s) = sid {
        v["session_id"] = json!(s);
    }
    v
}

fn bash(cwd: &Path, command: &str) -> Value {
    bash_sid(cwd, command, Some(SID))
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

/// Learn `sibling`'s key (it must Ask and be Allow with its recorded key).
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
/// although it is not Allow without a table. A detector Allow (no table) is
/// no askrules question: printed and skipped.
fn rides(e: &Env, k: &str, probe: &Value) -> Option<String> {
    let before = decide_with(e, &no_table(e), probe);
    if before == "allow" {
        println!(
            "DETECTOR allow (not an askrules ride): `{}` (cwd {}) sid {} shape {}",
            cmd_of(probe),
            probe["cwd"].as_str().unwrap_or(""),
            probe["session_id"],
            shape(e, probe)
        );
        return None;
    }
    let after = decide_with(e, &table_recorded(e, k), probe);
    (after == "allow").then(|| {
        format!(
            "probe `{}` (cwd {}, session_id {}) was {before} without a table but Allow with \
             the recorded key {k}; probe shape {}",
            cmd_of(probe),
            probe["cwd"].as_str().unwrap_or(""),
            probe["session_id"],
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

fn rm_rf(p: &Path) {
    let mut c = Command::new("rm");
    c.args(["-rf", p.to_str().unwrap()]);
    let out = bounded::run_bounded_stdin(&mut c, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(out.status.success(), "rm -rf {}", p.display());
}

// ------------------------------------------------ session_id comparison

/// The payload `session_id` decides which scratchpad is the session's own.
/// A `session_id` that differs from the path component in case, by
/// surrounding white space, is `..` / `.` / empty / another session's id, or
/// is missing, never makes `<proj>/<SID>/scratchpad` Scratchpad: the learned
/// `rm -rf build` key (recorded with the right id) is not Allow for it, and
/// the shape class is not `s4-scratchpad`. Also the path spelled with the
/// upper-cased id together with an upper-cased payload id (the same directory
/// on a case-insensitive macOS volume).
#[test]
fn r14_session_id_variants_never_make_own_scratchpad() {
    let e = env_prod("sid");
    let k = learn(&e, &bash(&e.sp, "rm -rf build"));
    assert_eq!(
        class(&k),
        "s4-scratchpad",
        "control: own scratchpad key {k}"
    );
    let upper = SID.to_uppercase();
    let padded_l = format!(" {SID}");
    let padded_r = format!("{SID} ");
    let padded_n = format!("{SID}\n");
    let mut bad = vec![];
    let variants: Vec<Option<&str>> = vec![
        Some(upper.as_str()),
        Some(padded_l.as_str()),
        Some(padded_r.as_str()),
        Some(padded_n.as_str()),
        Some(".."),
        Some("."),
        Some(""),
        Some(OTHER),
        Some("scratchpad"),
        None,
    ];
    for sid in variants {
        for c in [
            "rm -rf build".to_string(),
            format!("rm -rf {}/build", e.sp.display()),
        ] {
            let p = bash_sid(&e.sp, &c, sid);
            let s = shape(&e, &p);
            if class(&key(&s)) == "s4-scratchpad" {
                bad.push(format!("session_id {sid:?}: `{c}` classed {}", key(&s)));
            }
            if let Some(x) = rides(&e, &k, &p) {
                bad.push(x);
            }
        }
    }
    // Upper-cased spelling + upper-cased payload id.
    let up_sp = e.proj.join(&upper).join("scratchpad");
    let c = format!("rm -rf {}/build", up_sp.display());
    for cwd in [&e.sp, &up_sp] {
        let p = bash_sid(cwd, &c, Some(&upper));
        let s = shape(&e, &p);
        if class(&key(&s)) == "s4-scratchpad" {
            bad.push(format!(
                "upper spelling+id: `{c}` (cwd {}) classed {}",
                cwd.display(),
                key(&s)
            ));
        }
        if let Some(x) = rides(&e, &k, &p) {
            bad.push(x);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ symlinks around the scratchpad

/// A symlink in the own scratchpad that points at ANOTHER session's
/// scratchpad (`<sp>/olink -> <proj>/<OTHER>/scratchpad`), and an own
/// session-id directory that is itself a symlink to another session's
/// directory (`<proj>/<SID3> -> <proj>/<OTHER>`, payload id SID3). The
/// spelling names the own scratchpad, rm deletes the other session's files.
/// Precondition (real rm): `rm -rf <sp>/olink/data` deletes
/// `<OTHER>/scratchpad/data`. Expected: no learned Allow.
#[test]
fn r14_symlink_from_own_scratchpad_into_another_sessions_scratchpad() {
    let h = env_prod("symharm");
    let hother = h.proj.join(OTHER).join("scratchpad");
    std::fs::create_dir_all(hother.join("data")).unwrap();
    std::fs::write(hother.join("data").join("keep"), "theirs").unwrap();
    std::os::unix::fs::symlink(&hother, h.sp.join("olink")).unwrap();
    rm_rf(&h.sp.join("olink").join("data"));
    assert!(
        !hother.join("data").exists(),
        "precondition: rm -rf through the own-scratchpad symlink deletes the other session's data"
    );

    let e = env_prod("sym");
    let other = e.proj.join(OTHER).join("scratchpad");
    std::fs::create_dir_all(other.join("data")).unwrap();
    std::fs::write(other.join("data").join("keep"), "theirs").unwrap();
    std::os::unix::fs::symlink(&other, e.sp.join("olink")).unwrap();
    let sid3 = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000f5";
    std::os::unix::fs::symlink(e.proj.join(OTHER), e.proj.join(sid3)).unwrap();
    // An alias path to the OWN scratchpad (FP side, measured only).
    std::os::unix::fs::symlink(e.proj.join(SID), e.root.join("alias")).unwrap();
    let k = learn(&e, &bash(&e.sp, "rm -rf build"));
    let mut bad = vec![];
    for (cwd, c, sid) in [
        (e.sp.clone(), "rm -rf olink/data".to_string(), SID),
        (
            e.sp.clone(),
            format!("rm -rf {}/olink/data", e.sp.display()),
            SID,
        ),
        (
            e.proj.join(sid3).join("scratchpad"),
            "rm -rf data".to_string(),
            sid3,
        ),
        (
            e.sp.clone(),
            format!("rm -rf {}/{sid3}/scratchpad/data", e.proj.display()),
            sid3,
        ),
    ] {
        let p = bash_sid(&cwd, &c, Some(sid));
        if let Some(x) = rides(&e, &k, &p) {
            bad.push(x);
        }
    }
    let alias = bash(
        &e.sp,
        &format!("rm -rf {}/alias/scratchpad/build", e.root.display()),
    );
    println!(
        "MEASURE alias path to own scratchpad: shape {} learned => {}",
        shape(&e, &alias),
        decide_with(&e, &table_recorded(&e, &k), &alias)
    );
    assert!(
        other.join("data").join("keep").exists(),
        "fixture: the other session's data is untouched by the hook"
    );
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Spec Components 1 (round 14): "another session's scratchpad ... is
/// `Undetermined`". `is_own_scratchpad` accepts ANY `<SID>/scratchpad` pair
/// in the path, so a directory chain `<SID>/scratchpad` created INSIDE
/// another session's scratchpad (`<proj>/<OTHER>/scratchpad/n/<SID>/scratchpad`,
/// real directories, no symlink) is classed Scratchpad and rides the learned
/// own-scratchpad `rm -rf` key. Harm shown: only content under that nested
/// chain is deleted (the chain names this session's id); no deletion of the
/// other session's own files outside the chain was found (a symlink on the
/// way is Undetermined). This is a spec <-> implementation contradiction.
#[test]
fn r14_own_sid_chain_nested_in_another_sessions_scratchpad_is_not_own() {
    let e = env_prod("nest");
    let other = e.proj.join(OTHER).join("scratchpad");
    let nested = other.join("n").join(SID).join("scratchpad");
    std::fs::create_dir_all(nested.join("x")).unwrap();
    let k = learn(&e, &bash(&e.sp, "rm -rf build"));
    let mut bad = vec![];
    for (cwd, c) in [
        (e.sp.clone(), format!("rm -rf {}/x", nested.display())),
        (nested.clone(), "rm -rf x".to_string()),
    ] {
        let p = bash(&cwd, &c);
        let s = shape(&e, &p);
        if class(&key(&s)) == "s4-scratchpad" {
            bad.push(format!(
                "`{c}` (cwd {}) inside another session's scratchpad is classed {}",
                cwd.display(),
                key(&s)
            ));
        }
        if let Some(x) = rides(&e, &k, &p) {
            bad.push(x);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// `/tmp` vs `/private/tmp`: `/tmp` is a symlink to `/private/tmp` on macOS.
/// A `/tmp`-spelled own-scratchpad path is a symlinked spelling, so it is not
/// Allow-learnable (`spelling_is_real`). Guard: the learned key never makes a
/// `/tmp` spelling Allow into ANOTHER session's scratchpad; the own
/// scratchpad's `/tmp` spelling is measured (printed) for the FP side.
#[test]
fn r14_tmp_vs_private_tmp_spelling() {
    let e = env_prod("tmp");
    assert!(
        canon(Path::new("/tmp")) == Path::new("/private/tmp"),
        "precondition: /tmp is /private/tmp"
    );
    let other = e.proj.join(OTHER).join("scratchpad");
    std::fs::create_dir_all(other.join("data")).unwrap();
    let k = learn(&e, &bash(&e.sp, "rm -rf build"));
    let as_tmp = |p: &Path| PathBuf::from(p.to_str().unwrap().replacen("/private/tmp", "/tmp", 1));
    let mut bad = vec![];
    for (cwd, c) in [
        (
            as_tmp(&e.sp),
            format!("rm -rf {}/data", as_tmp(&other).display()),
        ),
        (as_tmp(&other), "rm -rf data".to_string()),
    ] {
        if let Some(x) = rides(&e, &k, &bash(&cwd, &c)) {
            bad.push(x);
        }
    }
    for (cwd, c) in [
        (as_tmp(&e.sp), "rm -rf build".to_string()),
        (
            e.sp.clone(),
            format!("rm -rf {}/build", as_tmp(&e.sp).display()),
        ),
    ] {
        let p = bash(&cwd, &c);
        println!(
            "MEASURE /tmp spelling of the own scratchpad: `{c}` (cwd {}) shape {} learned => {}",
            cwd.display(),
            shape(&e, &p),
            decide_with(&e, &table_recorded(&e, &k), &p)
        );
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ linked_root: FP side

/// FP (expected green): the production layout -- main `<home>/src/harness`,
/// worktree `<home>/src/.harness-worktrees/x` (a SIBLING of the main tree),
/// scratchpad `<proj>/<SID>/scratchpad` under /private/tmp/claude-* -- keeps
/// the own-key Allow for `git add -A` / `git commit -a -m x` in the worktree
/// and `rm -rf build` in the scratchpad (relative and absolute).
#[test]
fn r14_fp_production_layout_own_keys_stay_allow() {
    let e = env_prod("fp");
    let mut bad = vec![];
    for (cwd, c) in [
        (&e.wt, "git add -A".to_string()),
        (&e.wt, "git commit -a -m x".to_string()),
        (&e.sp, "rm -rf build".to_string()),
        (&e.sp, format!("rm -rf {}/build", e.sp.display())),
    ] {
        let p = bash(cwd, &c);
        let s = shape(&e, &p);
        let d = if s["decision"] == "ask" {
            decide_with(&e, &table_recorded(&e, &key(&s)), &p)
        } else {
            s["decision"].as_str().unwrap_or("").to_string()
        };
        if d != "allow" {
            bad.push(format!(
                "`{c}` (cwd {}) with its recorded key {} => {d}",
                cwd.display(),
                key(&s)
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// FP (expected green): linked worktrees OUTSIDE the main tree in other
/// places are the own worktree from their cwd: `<root>/elsewhere/w3` (not
/// under HOME's src at all) and `<home>/src/harness-wt` (a sibling whose name
/// has the main tree's name as a string prefix -- a component comparison,
/// not a string prefix). `git add -A` there is `s4-worktree` and Allow with
/// the key learned in `.harness-worktrees/x`.
#[test]
fn r14_fp_linked_worktrees_outside_main_stay_own() {
    let e = env_prod("fpout");
    let w3 = e.root.join("elsewhere").join("w3");
    std::fs::create_dir_all(w3.parent().unwrap()).unwrap();
    git(&e.main, &["worktree", "add", "-q", w3.to_str().unwrap()]);
    let pre = e.home.join("src").join("harness-wt");
    git(&e.main, &["worktree", "add", "-q", pre.to_str().unwrap()]);
    let k = learn(&e, &bash(&e.wt, "git add -A"));
    assert_eq!(class(&k), "s4-worktree", "control: {k}");
    let mut bad = vec![];
    for cwd in [&w3, &pre] {
        std::fs::write(cwd.join("new.rs"), "n").unwrap();
        let p = bash(cwd, "git add -A");
        let s = shape(&e, &p);
        let d = decide_with(&e, &table_recorded(&e, &k), &p);
        if key(&s) != k || d != "allow" {
            bad.push(format!(
                "`git add -A` in {} => key {} learned {d} (expected {k} / allow)",
                cwd.display(),
                key(&s)
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// FP (RED at bbae037d1): Claude Code's own worktree layout puts linked
/// worktrees INSIDE the main tree, `<main>/.claude/worktrees/<name>` (the
/// layout v9/v10 `r*_guard_own_key_allows_stay` use, observed GREEN at
/// 8f4342bfe). The round-14 `linked_root` rule "a toplevel inside the main
/// tree is never a linked checkout" makes every such worktree
/// `s4-undetermined`: `git add -A` / `git commit -a -m x` there never reach
/// the learned own-worktree Allow. Precondition (real git): git reports a
/// linked worktree rooted at w2 with git dir `<main>/.git/worktrees/w2`.
#[test]
fn r14_fp_claude_code_nested_worktree_stays_own() {
    let e = env_prod("fpnest");
    let w2 = e.main.join(".claude").join("worktrees").join("w2");
    std::fs::create_dir_all(w2.parent().unwrap()).unwrap();
    git(&e.main, &["worktree", "add", "-q", w2.to_str().unwrap()]);
    std::fs::write(w2.join("new.rs"), "n").unwrap();
    assert_eq!(
        layout(&w2),
        Some((
            canon(&w2),
            canon(&e.main.join(".git").join("worktrees").join("w2")),
            canon(&e.main.join(".git"))
        )),
        "precondition: git calls w2 a linked worktree rooted at w2"
    );
    let mut bad = vec![];
    for c in ["git add -A", "git commit -a -m x"] {
        let k = learn(&e, &bash(&e.wt, c));
        let p = bash(&w2, c);
        let s = shape(&e, &p);
        let d = decide_with(&e, &table_recorded(&e, &k), &p);
        if key(&s) != k || d != "allow" {
            bad.push(format!(
                "`{c}` in Claude Code's worktree {} => key {} learned {d} (own key {k} is allow)",
                w2.display(),
                key(&s)
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ linked_root: forged roots

/// Write `<dir>/.git` naming wt's admin dir and let real git rewrite the
/// back-link (`git worktree repair`).
fn forge(e: &Env, dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(".git"),
        format!(
            "gitdir: {}\n",
            e.main.join(".git").join("worktrees").join("x").display()
        ),
    )
    .unwrap();
    git(dir, &["worktree", "repair"]);
}

/// A toplevel CONTAINING the main tree: `<home>/src/.git` forged to wt's
/// admin dir. Precondition (real git): git calls `<home>/src` a linked
/// worktree rooted there, and `git add -A` + `git commit -a -m x` there make
/// a commit on wt's branch that drops wt's `src/lib.rs`. Guard (expected
/// green): no learned Allow from `<home>/src` or the main tree below it.
#[test]
fn r14_forged_root_containing_the_main_tree() {
    let h = env_prod("ancharm");
    let top = h.home.join("src");
    forge(&h, &top);
    let gd = h.main.join(".git").join("worktrees").join("x");
    assert_eq!(
        layout(&top),
        Some((canon(&top), canon(&gd), canon(&h.main.join(".git")))),
        "precondition: git's layout in <home>/src is a linked worktree rooted there"
    );
    git(&top, &["add", "-A"]);
    git(&top, &["commit", "-q", "-a", "-m", "x"]);
    let files = git_out(&top, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(
        !files.lines().any(|l| l == "src/lib.rs"),
        "precondition: the commit on wt's branch drops wt's src/lib.rs: {files:?}"
    );

    let e = env_prod("anc");
    let k_add = learn(&e, &bash(&e.wt, "git add -A"));
    let k_ca = learn(&e, &bash(&e.wt, "git commit -a -m x"));
    let top = e.home.join("src");
    forge(&e, &top);
    let mut bad = vec![];
    for (k, c) in [(&k_add, "git add -A"), (&k_ca, "git commit -a -m x")] {
        for cwd in [top.clone(), e.main.clone(), e.main.join("src")] {
            if let Some(x) = rides(&e, k, &bash(&cwd, c)) {
                bad.push(x);
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The round-13 forgery (r13_back_link_repaired_to_a_main_subdir...) moved
/// OUTSIDE the main tree: `<home>/notes/.git` forged to wt's admin dir.
/// `linked_root`'s new rule only excludes toplevels related to the main tree,
/// so `<home>/notes` (a HOME directory, spec: `Home`, never Allow-eligible)
/// is the cwd's own linked worktree. Precondition (real git): `git add -A` +
/// `git commit -a -m x` there make a commit on wt's branch that holds HOME's
/// `notes/secret` content and drops wt's `src/lib.rs` -- the same harm the
/// round-13 RED named, with HOME files instead of main-tree files.
/// Expected: no learned Allow from `<home>/notes`.
#[test]
fn r14_forged_root_outside_the_main_tree_in_home() {
    let h = env_prod("homeharm");
    let notes = h.home.join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join("secret"), "s").unwrap();
    forge(&h, &notes);
    let gd = h.main.join(".git").join("worktrees").join("x");
    assert_eq!(
        layout(&notes),
        Some((canon(&notes), canon(&gd), canon(&h.main.join(".git")))),
        "precondition: git's layout in <home>/notes is a linked worktree rooted there"
    );
    git(&notes, &["add", "-A"]);
    git(&notes, &["commit", "-q", "-a", "-m", "x"]);
    let files = git_out(&notes, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(
        files.lines().any(|l| l == "secret") && !files.lines().any(|l| l == "src/lib.rs"),
        "precondition: the commit on wt's branch holds HOME's secret and drops src/lib.rs: {files:?}"
    );

    let e = env_prod("home");
    let notes = e.home.join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join("secret"), "s").unwrap();
    let k_add = learn(&e, &bash(&e.wt, "git add -A"));
    let k_ca = learn(&e, &bash(&e.wt, "git commit -a -m x"));
    // Control (no forgery yet): `<home>/notes` is no git tree, no ride.
    for (k, c) in [(&k_add, "git add -A"), (&k_ca, "git commit -a -m x")] {
        let ctl = rides(&e, k, &bash(&notes, c));
        assert!(
            ctl.is_none(),
            "control: unforged <home>/notes rides: {ctl:?}"
        );
    }
    forge(&e, &notes);
    let mut bad = vec![];
    for (k, c) in [(&k_add, "git add -A"), (&k_ca, "git commit -a -m x")] {
        if let Some(x) = rides(&e, k, &bash(&notes, c)) {
            bad.push(x);
        }
    }
    assert!(
        bad.is_empty(),
        "a HOME directory forged into wt's linked root is the own worktree:\n{}",
        bad.join("\n")
    );
}

/// A common dir that is not `<x>/.git` (a bare repository `<root>/bare.git`
/// with a linked worktree `<root>/bwt`): the spec says the main tree is not
/// determined, so `bwt` is not `s4-worktree`. Precondition (real git): git
/// reports bwt as a linked worktree whose common dir is `bare.git`. Guard
/// (expected green): no learned Allow; the class is printed for the FP side.
#[test]
fn r14_bare_common_dir_is_not_own_worktree() {
    let e = env_prod("bare");
    let bare = e.root.join("bare.git");
    git(
        &e.root,
        &[
            "clone",
            "-q",
            "--bare",
            e.main.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let bwt = e.root.join("bwt");
    git(&bare, &["worktree", "add", "-q", bwt.to_str().unwrap()]);
    std::fs::write(bwt.join("new.rs"), "n").unwrap();
    let l = layout(&bwt).expect("precondition: git answers in bwt");
    assert_eq!(
        l.2,
        canon(&bare),
        "precondition: bwt's common dir is the bare repo"
    );
    assert_eq!(l.0, canon(&bwt), "precondition: bwt is its own toplevel");
    let mut bad = vec![];
    for c in ["git add -A", "git commit -a -m x"] {
        let k = learn(&e, &bash(&e.wt, c));
        let p = bash(&bwt, c);
        let s = shape(&e, &p);
        println!(
            "MEASURE bare-repo worktree `{c}` => {} {}",
            s["decision"],
            key(&s)
        );
        if class(&key(&s)) == "s4-worktree" {
            bad.push(format!("`{c}` in bare-repo worktree classed {}", key(&s)));
        }
        if let Some(x) = rides(&e, &k, &p) {
            bad.push(x);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
