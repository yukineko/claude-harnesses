#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verifier (CLAUDE.md §2(a)) for the redirect-shape fixes of
//! backlog ffa05737 (D1 zsh `>!` clobber, D2 `git add -A 2>&1`) and 0e79c339
//! (D3 `rm -rf build 2>&1`). Written by an agent that did not implement them.
//!
//! Ruling 2026-11-28: a redirect does not change the rm verdict.
//!
//! Every zsh behaviour a test relies on is first OBSERVED with `zsh -f -c`
//! in a scratch dir (the Bash tool's shell is zsh).
//!
//! DOES NOT PROVE: the Write/Edit tool route; behaviour with a learned table;
//! any redirect spelling not listed here; that Ask is the *right* level where
//! only "not Allow" is asserted.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000e2";
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

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} in {}", dir.display());
}

fn env(name: &str) -> Env {
    std::fs::create_dir_all(BASE).unwrap();
    let dir = tempfile::Builder::new()
        .prefix(&format!("redirect-verifier-{name}-"))
        .tempdir_in(BASE)
        .unwrap();
    let root = dir.path().to_path_buf();
    let home_dir = tempfile::Builder::new()
        .prefix("home-")
        .tempdir_in(&root)
        .unwrap();
    let home = home_dir.path().to_path_buf();
    let main = home.join("src").join("repo");
    std::fs::create_dir_all(main.join("src")).unwrap();
    std::fs::create_dir_all(main.join(".githooks")).unwrap();
    std::fs::write(main.join("src").join("lib.rs"), "x").unwrap();
    std::fs::write(main.join(".githooks").join("pre-commit"), "#!/bin/sh\n").unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["add", "src", ".githooks"]);
    git(&main, &["commit", "-q", "-m", "i"]);
    let wt = home.join("src").join(".harness-worktrees").join("wt");
    git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    let sp = root.join("scratchpad");
    std::fs::create_dir_all(sp.join("build")).unwrap();
    std::fs::create_dir_all(sp.join("build dir")).unwrap();
    std::fs::create_dir_all(wt.join("build")).unwrap();
    let claude = home.join(".claude");
    std::fs::create_dir_all(claude.join("hooks")).unwrap();
    std::fs::write(claude.join("settings.json"), "{}").unwrap();
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

fn decide_with(bin: &Path, e: &Env, cwd: &Path, command: &str) -> String {
    let p = json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID,
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": command },
    });
    let mut c = Command::new(bin);
    c.env("HOME", &e.home)
        .env("BLASTGUARD_BLOCK_LOG_DIR", e.root.join("blocklog"))
        .env("BLASTGUARD_RELAY_DIR", e.root.join("relay"))
        .env("BLASTGUARD_APPROVALS_DIR", e.root.join("approvals"))
        .env("BLASTGUARD_LEARNED_TABLE", e.root.join("no-table.json"))
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("BLASTGUARD_ASK_RULES")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut c, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(0), "hook must exit 0: {err}");
    if s.trim().is_empty() {
        return "allow".to_string();
    }
    let d: Value = serde_json::from_str(s.trim())
        .unwrap_or_else(|x| panic!("stdout not JSON ({x}): {s:?} stderr={err:?}"));
    d["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision in {d}"))
        .to_string()
}

fn decide(e: &Env, cwd: &Path, command: &str) -> String {
    decide_with(Path::new(env!("CARGO_BIN_EXE_blastguard")), e, cwd, command)
}

fn rank(d: &str) -> u8 {
    match d {
        "allow" => 0,
        "ask" => 1,
        "deny" => 2,
        other => panic!("unknown decision {other:?}"),
    }
}

fn run_zsh(dir: &Path, home: &Path, script: &str) {
    let mut c = Command::new("zsh");
    c.args(["-f", "-c", script])
        .current_dir(dir)
        .env("HOME", home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut c, b"", bounded::TOOL_DEADLINE);
    assert!(
        out.status.success(),
        "zsh {script:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Observe that zsh writes `victim` (the path after `op`) and creates no
/// file named `!` / `|` / `&` in the cwd.
fn zsh_writes_following_path(e: &Env, op: &str, attached: bool) {
    let tag: String = op.bytes().map(|b| format!("{b:02x}")).collect();
    let zdir = e.root.join(format!("zsh-{tag}-{attached}"));
    let _ = std::fs::create_dir_all(&zdir);
    let victim = zdir.join("victim");
    std::fs::write(&victim, "orig\n").unwrap();
    let sep = if attached { "" } else { " " };
    run_zsh(
        &zdir,
        &e.home,
        &format!("print -r -- WROTE {op}{sep}{}", victim.display()),
    );
    let got = std::fs::read_to_string(&victim).unwrap();
    assert!(
        got.contains("WROTE"),
        "precondition: zsh `{op}` writes the following path, got {got:?}"
    );
    for stray in ["!", "|", "&"] {
        assert!(
            !zdir.join(stray).exists(),
            "precondition: zsh `{op}` creates no file named `{stray}`"
        );
    }
}

fn settings(e: &Env) -> String {
    e.home
        .join(".claude")
        .join("settings.json")
        .display()
        .to_string()
}

// ---------------------------------------------------------------- D1 shapes

#[test]
fn v_bar_clobber_to_protected_is_not_allow() {
    let e = env("bar");
    zsh_writes_following_path(&e, ">|", false);
    let s = settings(&e);
    for c in [
        format!("echo x >| {s}"),
        format!("echo x >|{s}"),
        format!("echo x 2>| {s}"),
    ] {
        assert_ne!(decide(&e, &e.wt, &c), "allow", "`{c}`");
    }
}

#[test]
fn v_amp_append_bang_to_protected_is_not_allow() {
    let e = env("ampappend");
    zsh_writes_following_path(&e, "&>>!", false);
    let s = settings(&e);
    let c = format!("echo x &>>! {s}");
    assert_ne!(decide(&e, &e.wt, &c), "allow", "`{c}`");
}

#[test]
fn v_bang_attached_target_is_not_allow() {
    let e = env("bangattached");
    zsh_writes_following_path(&e, ">!", true);
    let s = settings(&e);
    for c in [
        format!("echo x >!{s}"),
        format!("echo x >>!{s}"),
        "echo x >!~/.claude/settings.json".to_string(),
    ] {
        assert_ne!(decide(&e, &e.wt, &c), "allow", "`{c}`");
    }
}

#[test]
fn v_bang_undeterminable_target_is_at_least_ask() {
    let e = env("bangvar");
    for c in [
        "echo x >!\"$X\"",
        "echo x >! \"$X\"",
        "echo x >!$X",
        "echo x >! $(echo f)",
    ] {
        assert_ne!(decide(&e, &e.wt, c), "allow", "`{c}`");
    }
}

#[test]
fn v_space_then_bang_word_is_not_over_asked() {
    // zsh: `> !foo` (space between) is the plain `>` with the file `!foo`.
    let e = env("spacebang");
    let zdir = e.root.join("zsh-spacebang");
    std::fs::create_dir_all(&zdir).unwrap();
    run_zsh(&zdir, &e.home, "print -r -- x > !foo");
    assert!(
        zdir.join("!foo").exists() && !zdir.join("foo").exists(),
        "precondition: zsh `> !foo` writes the file `!foo`"
    );
    let plain = decide(&e, &e.wt, "echo x > foo");
    let got = decide(&e, &e.wt, "echo x > !foo");
    assert_eq!(
        got, plain,
        "`echo x > !foo` should be judged like `echo x > foo` (plain = {plain})"
    );
}

// ---------------------------------------------------------------- D2 shapes

#[test]
fn v_git_commit_with_redirect_matches_plain_commit() {
    let e = env("commit");
    let plain = decide(&e, &e.wt, "git commit -m x");
    for c in [
        "git commit -m x 2>&1",
        "git commit -m x > /dev/null",
        "git commit -m x 2>/dev/null",
    ] {
        assert_eq!(decide(&e, &e.wt, c), plain, "`{c}` vs plain {plain}");
    }
}

#[test]
fn v_git_add_all_clobber_to_protected_keeps_both_verdicts() {
    let e = env("addclobber");
    let s = settings(&e);
    let add = decide(&e, &e.wt, "git add -A");
    let write = decide(&e, &e.wt, &format!("echo x >! {s}"));
    assert_eq!(add, "ask", "control: `git add -A`");
    assert_ne!(write, "allow", "control: `echo x >! settings`");
    let want = if rank(&add) >= rank(&write) {
        add
    } else {
        write
    };
    let c = format!("git add -A >! {s}");
    assert_eq!(decide(&e, &e.wt, &c), want, "`{c}`");
}

#[test]
fn v_git_add_all_piped_to_tee_is_not_allow() {
    let e = env("addtee");
    for c in ["git add -A 2>&1 | tee log", "git add -A >log 2>&1"] {
        assert_ne!(decide(&e, &e.wt, c), "allow", "`{c}`");
    }
}

#[test]
fn v_git_add_path_with_redirect_matches_plain() {
    let e = env("addpath");
    let plain = decide(&e, &e.wt, "git add src");
    for c in ["git add src 2>&1", "git add src > /dev/null"] {
        assert_eq!(decide(&e, &e.wt, c), plain, "`{c}` vs plain {plain}");
    }
}

// ---------------------------------------------------------------- D3 shapes

#[test]
fn v_rm_quoted_operand_with_redirect_matches_plain() {
    let e = env("rmquoted");
    let plain = decide(&e, &e.sp, "rm -rf \"build dir\"");
    let got = decide(&e, &e.sp, "rm -rf \"build dir\" 2>&1");
    assert_eq!(got, plain, "quoted operand + `2>&1` vs plain {plain}");
}

#[test]
fn v_rm_build_with_redirects_matches_plain() {
    let e = env("rmbuild");
    let mut bad = vec![];
    for cwd in [&e.sp, &e.wt] {
        let plain = decide(&e, cwd, "rm -rf build");
        for c in [
            "rm -rf build 2>/dev/null",
            "rm -rf build > log",
            "rm -rf build 2>&1",
            "rm -rf build &>/dev/null",
            "rm -rf build >/dev/null 2>&1",
        ] {
            let got = decide(&e, cwd, c);
            if got != plain {
                bad.push(format!("{}: `{c}` => {got}, plain {plain}", cwd.display()));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn v_rm_system_and_protected_with_redirect_stay_deny() {
    let e = env("rmdeny");
    let mut bad = vec![];
    for c in [
        "rm -rf /etc".to_string(),
        "rm -rf /etc 2>/dev/null".to_string(),
        "rm -rf /etc > log".to_string(),
        "rm -rf 2>/dev/null /etc".to_string(),
        "rm -rf ~/.claude".to_string(),
        "rm -rf ~/.claude 2>&1".to_string(),
        format!("rm -rf {}/.claude 2>&1", e.home.display()),
        "rm -rf ~/.claude &>/dev/null".to_string(),
    ] {
        let got = decide(&e, &e.wt, &c);
        if got != "deny" {
            bad.push(format!("`{c}` => {got}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------- new escapes (verifier)

/// zsh append/clobber spellings the scanners do not read: `>&!`, `>>&`,
/// `>>&!`, `>>|`. Each is observed to write the following path first.
#[test]
fn red_zsh_amp_and_bar_append_forms_to_protected_are_not_allow() {
    let e = env("ampforms");
    for op in [">&!", ">>&", ">>&!", ">>|"] {
        zsh_writes_following_path(&e, op, false);
    }
    let s = settings(&e);
    let hook = e
        .main
        .join(".githooks")
        .join("pre-commit")
        .display()
        .to_string();
    let mut bad = vec![];
    for t in [&s, &hook] {
        for op in [">&!", ">>&", ">>&!", ">>|"] {
            let c = format!("echo x {op} {t}");
            let got = decide(&e, &e.wt, &c);
            if got == "allow" {
                bad.push(format!("`{c}` => {got}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "zsh redirect spellings overwrite protected config but are Allow:\n{}",
        bad.join("\n")
    );
}

/// `rm -rf ~/.claude>log`: the shell deletes `~/.claude` (the `>log` is a
/// redirect glued to the operand). Observed in zsh on a scratch dir first.
#[test]
fn v_rm_glued_redirect_operand_stays_deny() {
    let e = env("rmglued");
    let zdir = e.root.join("zsh-glued");
    std::fs::create_dir_all(zdir.join("victim")).unwrap();
    run_zsh(&zdir, &e.home, "rm -rf victim>log");
    assert!(
        !zdir.join("victim").exists(),
        "precondition: zsh `rm -rf victim>log` deletes victim"
    );
    let mut bad = vec![];
    for c in [
        "rm -rf ~/.claude>log",
        "rm -rf ~/.claude>/dev/null",
        "rm -rf /etc>log",
        "rm -rf /etc 2>&1>log",
    ] {
        let got = decide(&e, &e.wt, c);
        if got != "deny" {
            bad.push(format!("`{c}` => {got}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// bash reading of `>!`: `rm -rf build >! ~/.claude` in bash is `> !` (file
/// `!`) plus the OPERAND `~/.claude`, which bash deletes. D1 keeps the bash
/// reading for redirect targets; the D3 stripper drops `>!` AND the next
/// word, so the bash operand is no longer judged. Under zsh the same line
/// only fails to open the directory `~/.claude` for writing.
#[test]
fn red_rm_bash_reading_of_bang_operand_stays_deny() {
    let e = env("rmbashbang");
    // Precondition (observed): bash deletes the word after `>!`.
    let bdir = e.root.join("bash-bang");
    std::fs::create_dir_all(bdir.join("victim")).unwrap();
    let mut c = Command::new("bash");
    c.args(["--norc", "--noprofile", "-c", "rm -rf build >! victim"])
        .current_dir(&bdir)
        .env("HOME", &e.home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut c, b"", bounded::TOOL_DEADLINE);
    assert!(out.status.success(), "bash precondition run failed");
    assert!(
        !bdir.join("victim").exists() && bdir.join("!").exists(),
        "precondition: bash reads `>! victim` as `> !` plus the operand victim"
    );
    let mut bad = vec![];
    for c in ["rm -rf build >! ~/.claude", "rm -rf build >! /etc"] {
        let got = decide(&e, &e.sp, c);
        if got != "deny" {
            bad.push(format!("`{c}` => {got}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// --------------------------------------------- false-positive comparison

/// D2 false positive: `git add src>log` is `git add src` with stdout to
/// `log` (observed in zsh: the file `log` is created, `src>log` is no
/// pathspec). 0.2.1012 judged it Allow like `git add src`; the fix Asks.
#[test]
fn red_git_add_glued_redirect_matches_plain() {
    let e = env("addglued");
    let zdir = e.root.join("zsh-addglued");
    std::fs::create_dir_all(&zdir).unwrap();
    run_zsh(&zdir, &e.home, "print -r -- src>log");
    assert_eq!(
        std::fs::read_to_string(zdir.join("log")).unwrap().trim(),
        "src",
        "precondition: zsh reads `src>log` as the word `src` plus `>log`"
    );
    let plain = decide(&e, &e.wt, "git add src");
    for c in ["git add src>log", "git add src 2>&1>/dev/null"] {
        assert_eq!(decide(&e, &e.wt, c), plain, "`{c}` vs plain {plain}");
    }
}

/// Compares this build with a baseline binary (the deployed one) on the same
/// fixture. Run with
/// `BLASTGUARD_BASELINE_BIN=<path> cargo test ... -- --ignored --nocapture fp_`.
/// Prints every difference; fails on a line that became STRICTER, except the
/// intended fail-open closures. Measured against 0.2.1012 on 2026-11-28: the
/// only unintended stricter line is `git add src>log` (allow -> ask).
#[test]
#[ignore = "RED observed 2026-11-28 vs 0.2.1012 (`git add src>log` allow -> ask); needs BLASTGUARD_BASELINE_BIN"]
fn fp_compare_against_baseline_binary() {
    let Some(base) = std::env::var_os("BLASTGUARD_BASELINE_BIN") else {
        panic!("BLASTGUARD_BASELINE_BIN not set");
    };
    let base = PathBuf::from(base);
    let e = env("fp");
    let s = settings(&e);
    let cases: Vec<(PathBuf, String)> = [
        (&e.wt, "echo x > foo".to_string()),
        (&e.wt, "echo x > !foo".to_string()),
        (&e.wt, "echo x >! foo".to_string()),
        (&e.wt, "echo x >!foo".to_string()),
        (&e.wt, "echo x >> !foo".to_string()),
        (&e.wt, "echo x >| foo".to_string()),
        (&e.wt, "echo x 2>! /dev/null".to_string()),
        (&e.wt, "echo x >! /dev/null".to_string()),
        (&e.wt, "echo 'a > !b'".to_string()),
        (&e.wt, "git add -A".to_string()),
        (&e.wt, "git add src".to_string()),
        (&e.wt, "git add src 2>&1".to_string()),
        (&e.wt, "git add src > /dev/null".to_string()),
        (&e.wt, "git add src>log".to_string()),
        (&e.wt, "git add 'a>b'".to_string()),
        (&e.wt, "git add src/lib.rs 2>/dev/null".to_string()),
        (&e.wt, "git add src && git commit -m x 2>&1".to_string()),
        (&e.wt, "git commit -m x 2>&1".to_string()),
        (&e.wt, "git commit -m 'a > b'".to_string()),
        (&e.wt, "rm -rf build".to_string()),
        (&e.wt, "rm -rf build 2>&1".to_string()),
        (&e.wt, "rm -rf build > log".to_string()),
        (&e.sp, "rm -rf build".to_string()),
        (&e.sp, "rm -rf build 2>&1".to_string()),
        (&e.sp, "rm -rf build 2>/dev/null".to_string()),
        (&e.sp, "rm -rf build > log".to_string()),
        (&e.sp, "rm -rf \"build dir\" 2>&1".to_string()),
        (&e.sp, "rm -rf 'build dir' > log".to_string()),
        (&e.sp, "rm -f build/x 2>/dev/null".to_string()),
        (&e.sp, "rm -rf build >! ~/.claude".to_string()),
        (&e.wt, format!("echo x >! {s}")),
        (&e.wt, format!("echo x > {s}")),
        (&e.wt, format!("git add -A >! {s}")),
        (&e.wt, "git add -A 2>&1 | tee log".to_string()),
        (&e.wt, "rm -rf ~/.claude>log".to_string()),
        (&e.wt, format!("echo x >>& {s}")),
        (&e.wt, format!("echo x >>| {s}")),
    ]
    .into_iter()
    .map(|(p, c)| (p.clone(), c))
    .collect();
    // Fail-open closures the fixes are MEANT to make stricter (not FPs).
    let intended = [
        format!("echo x >! {s}"),
        format!("git add -A >! {s}"),
        "git add -A 2>&1 | tee log".to_string(),
    ];
    let mut stricter = vec![];
    for (cwd, c) in &cases {
        let new = decide(&e, cwd, c);
        let old = decide_with(&base, &e, cwd, c);
        let tag = if cwd == &e.sp { "sp" } else { "wt" };
        println!("[{tag}] {old:>5} -> {new:<5} {c}");
        if rank(&new) > rank(&old) && !intended.contains(c) {
            stricter.push(format!("[{tag}] `{c}`: {old} -> {new}"));
        }
    }
    assert!(
        stricter.is_empty(),
        "stricter than baseline:\n{}",
        stricter.join("\n")
    );
}
