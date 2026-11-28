#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (detector, no learned table): three redirect shapes the detector
//! misreads. Written by an independent test author (CLAUDE.md §2(a)) from a
//! third-party verifier's observation on the fp-ask-rules branch
//! (`verifier9_ask_learning_20261128.rs`, `r9_detector_*`), re-measured here
//! against main.
//!
//! D1 (fail-open): zsh — the Bash tool's shell — reads `>!` / `>>!` / `&>!`
//! / `2>!` as the clobber forms of `>` / `>>` / `&>` / `2>`. The detector
//! (brush AST) reads `cmd >! /p` as the redirect `> !` (a file named `!` in
//! the cwd) plus the ARGUMENT `/p`, so the real write target is never judged.
//! The test first observes, with `zsh -f -c` in a scratch dir, that zsh
//! really writes the path after `>!` and creates no `!` file.
//!
//! D2 (fail-open): a redirect after `git add -A` (`2>&1`, `> /dev/null`,
//! `> log`) turns the whole-tree staging Ask into Allow. (Suspected cause:
//! the redirect words are counted as pathspecs — not verified here.)
//!
//! D3 (false positive): in the scratchpad, `rm -rf build` is Ask but the same
//! line with an output redirect (`2>&1`, `> log`) is Deny. A redirect of rm's
//! output does not change what rm deletes.
//!
//! Fixture layout (copied from the verifier): HOME is an ancestor of the main
//! tree `<home>/src/repo` and its linked worktree
//! `<home>/src/.harness-worktrees/wt`; the "scratchpad" is `<root>/scratchpad`
//! under `/private/tmp/claude-askrules`.
//!
//! DOES NOT PROVE: that any of these shapes is judged correctly once the
//! redirect is parsed right beyond the listed probes; other clobber spellings
//! (`>|`); the Write/Edit tool route; behaviour with a learned table.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000e1";
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
        .prefix(&format!("redirect-shapes-{name}-"))
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

fn decide(e: &Env, cwd: &Path, command: &str) -> String {
    let p = json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID,
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": command },
    });
    let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
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

#[test]
fn d1_zsh_bang_clobber_redirect_to_protected_path_is_not_allow() {
    let e = env("d1");
    // Precondition (observed): zsh writes the path AFTER `>!` / `>>!` and
    // creates no file named `!`.
    let zdir = e.root.join("zsh");
    std::fs::create_dir_all(&zdir).unwrap();
    let victim = zdir.join("victim");
    std::fs::write(&victim, "orig").unwrap();
    run_zsh(
        &zdir,
        &e.home,
        &format!("print -r -- clobbered >! {}", victim.display()),
    );
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap().trim(),
        "clobbered",
        "precondition: zsh `>!` writes the following path"
    );
    run_zsh(
        &zdir,
        &e.home,
        &format!("print -r -- appended >>! {}", victim.display()),
    );
    assert!(
        std::fs::read_to_string(&victim)
            .unwrap()
            .contains("appended"),
        "precondition: zsh `>>!` appends to the following path"
    );
    assert!(
        !zdir.join("!").exists(),
        "precondition: zsh `>!` creates no file named `!`"
    );

    let settings = e.home.join(".claude").join("settings.json");
    let hook = e.main.join(".githooks").join("pre-commit");
    // Controls: the plain `>` / `>>` spelling of the same writes is not Allow.
    for c in [
        format!("echo x > {}", settings.display()),
        format!("echo x >> {}", settings.display()),
        format!("echo x > {}", hook.display()),
    ] {
        assert_ne!(decide(&e, &e.wt, &c), "allow", "control: `{c}`");
    }
    let mut bad = vec![];
    for c in [
        format!("echo x >! {}", settings.display()),
        format!("echo x >>! {}", settings.display()),
        format!("echo x &>! {}", settings.display()),
        format!("echo x 2>! {}", settings.display()),
        format!("echo x >! {}", hook.display()),
    ] {
        let got = decide(&e, &e.wt, &c);
        if got == "allow" {
            bad.push(format!("`{c}` => {got}"));
        }
    }
    assert!(
        bad.is_empty(),
        "zsh clobber redirects overwrite protected config but are Allow:\n{}",
        bad.join("\n")
    );
}

#[test]
fn d2_redirect_after_git_add_all_keeps_whole_tree_ask() {
    let e = env("d2");
    assert_eq!(
        decide(&e, &e.wt, "git add -A"),
        "ask",
        "control: `git add -A`"
    );
    let mut bad = vec![];
    for c in [
        "git add -A 2>&1",
        "git add -A > /dev/null",
        "git add -A > log",
    ] {
        let got = decide(&e, &e.wt, c);
        if got == "allow" {
            bad.push(format!("`{c}` => {got}"));
        }
    }
    assert!(
        bad.is_empty(),
        "a redirect turns the whole-tree staging Ask into Allow:\n{}",
        bad.join("\n")
    );
}

#[test]
fn d3_output_redirect_does_not_turn_scratchpad_rm_ask_into_deny() {
    let e = env("d3");
    assert_eq!(
        decide(&e, &e.sp, "rm -rf build"),
        "ask",
        "control: `rm -rf build` in the scratchpad"
    );
    let mut bad = vec![];
    for c in ["rm -rf build 2>&1", "rm -rf build > log"] {
        let got = decide(&e, &e.sp, c);
        if got == "deny" {
            bad.push(format!("`{c}` => {got}"));
        }
    }
    assert!(
        bad.is_empty(),
        "an output redirect turns the scratchpad rm Ask into Deny:\n{}",
        bad.join("\n")
    );
}
