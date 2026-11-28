#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(verifier round 5, 2026-11-28, against a5d3788b7): reach paths of the
//! askrules judge (`crates/blastguard/src/askrules/`) and the `s4-` shapes.
//! Written by an independent verifier, not the implementer (CLAUDE.md §2(a)).
//! Black-box: spawns the built binary with a sandbox HOME, a learned table
//! whose ONLY allow entry is the key `blastguard shape` prints for a benign
//! sibling, and (where named) a human rules file via `BLASTGUARD_ASK_RULES`.
//!
//! Every fixture lives in a fresh `tempfile` directory under
//! `/private/tmp/claude-askrules` (a `$TMPDIR` under `/var/folders` is classed
//! `system`); a directory named `scratchpad` below it is classed
//! `s4-scratchpad`, and a linked git worktree is created per test. HOME is a
//! tempdir under cargo's target tmp dir (outside every temp root, like a
//! production HOME), except in the tests marked CONDITIONAL, which put HOME
//! inside /private/tmp because their reach exists only there.
//!
//! Each test first checks its preconditions (the probe is NOT Allow without the
//! table; the sibling Asks; the sibling and the probe share one key, or the
//! rule file is the only difference), so a GREEN cannot come from a drifted
//! fixture.
//!
//! What these tests do NOT prove: that the learner would actually promote the
//! sibling (>=3 approvals in >=2 sessions); that the probe's harm happens at
//! run time (nothing is executed); anything about shapes not listed here.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000d5";
const BASE: &str = "/private/tmp/claude-askrules";

struct Env {
    _dir: tempfile::TempDir,
    _home: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    wt: PathBuf,
    main: PathBuf,
    /// `<root>/scratchpad` (classed `s4-scratchpad`).
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
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} in {}", dir.display());
}

/// The default sandbox: HOME is NOT under /private/tmp. A HOME inside a temp
/// root changes the detector's verdict for `rm -rf` (observed: a scratchpad /
/// worktree `rm -rf` is an Ask with HOME under /private/tmp/claude-askrules,
/// an Allow with HOME outside every temp root; see `probe_home_location`).
/// A production HOME (/Users/<u>) is outside every temp root, so the sandbox
/// HOME lives in cargo's target tmp dir.
fn env(name: &str) -> Env {
    env_with(name, false)
}

/// The sandbox with HOME INSIDE the temp root (`<root>/home`, under
/// /private/tmp). Only the tests that say so use it; their reach exists only
/// in that configuration.
fn env_home_in_tmp(name: &str) -> Env {
    env_with(name, true)
}

fn env_with(name: &str, home_in_tmp: bool) -> Env {
    std::fs::create_dir_all(BASE).unwrap();
    let dir = tempfile::Builder::new()
        .prefix(&format!("v5-{name}-"))
        .tempdir_in(BASE)
        .unwrap();
    let root = dir.path().to_path_buf();
    let home_dir = if home_in_tmp {
        tempfile::Builder::new()
            .prefix("home-")
            .tempdir_in(&root)
            .unwrap()
    } else {
        tempfile::Builder::new()
            .prefix(&format!("v5-home-{name}-"))
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .unwrap()
    };
    let home = home_dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("approvals")).unwrap();
    let main = root.join("repo");
    std::fs::create_dir_all(main.join("src")).unwrap();
    std::fs::write(main.join("src").join("lib.rs"), "x").unwrap();
    git(&main, &["init", "-q"]);
    git(&main, &["add", "src"]);
    git(&main, &["commit", "-q", "-m", "i"]);
    let wt = root.join("wt");
    git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    // fixture re-anchored 2026-11-28 (leader ruling): Scratchpad is the session's own scratchpad only
    let sp = root.join(SID).join("scratchpad");
    std::fs::create_dir_all(&sp).unwrap();
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

fn spawn(
    e: &Env,
    table: &Path,
    rules: Option<&Path>,
    args: &[&str],
    stdin: &str,
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
    if let Some(r) = rules {
        c.env("BLASTGUARD_ASK_RULES", r);
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

fn decide_with(e: &Env, table: &Path, rules: Option<&Path>, p: &Value) -> String {
    let (_, out, err) = spawn(e, table, rules, &[], &p.to_string());
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

fn shape_with(e: &Env, rules: Option<&Path>, p: &Value) -> Value {
    let (code, out, err) = spawn(e, &no_table(e), rules, &["shape"], &p.to_string());
    assert_eq!(code, 0, "shape failed: {out:?} {err:?}");
    serde_json::from_str(out.trim()).unwrap()
}

fn shape(e: &Env, p: &Value) -> Value {
    shape_with(e, None, p)
}

fn key(e: &Env, p: &Value) -> String {
    shape(e, p)["shape"].as_str().unwrap().to_string()
}

fn table_with(e: &Env, k: &str) -> PathBuf {
    let p = e.root.join("t.json");
    let entry = json!({"shape": k, "decision": "allow", "approvals": 50, "declines": 0,
                       "sessions": 20, "samples": []});
    std::fs::write(&p, json!({"version": 1, "entries": [entry]}).to_string()).unwrap();
    p
}

fn cmd_of(p: &Value) -> &str {
    p["tool_input"]["command"].as_str().unwrap_or("")
}

/// Learn `sibling` allow; `probe` must not become Allow. `rules`: the human
/// rules file both runs use.
fn assert_no_ride(e: &Env, rules: Option<&Path>, sibling: &Value, probe: &Value) {
    let before = decide_with(e, &no_table(e), rules, probe);
    assert_ne!(
        before,
        "allow",
        "precondition: probe {:?} must not be Allow without a table",
        cmd_of(probe)
    );
    let sib = shape_with(e, rules, sibling);
    assert_eq!(
        sib["decision"],
        "ask",
        "precondition: sibling {:?} must Ask: {sib}",
        cmd_of(sibling)
    );
    let k = sib["shape"].as_str().unwrap().to_string();
    let t = table_with(e, &k);
    // Positive control: the entry is live for the sibling itself.
    assert_eq!(
        decide_with(e, &t, rules, sibling),
        "allow",
        "control: sibling {:?} with its own allow entry {k}",
        cmd_of(sibling)
    );
    let after = decide_with(e, &t, rules, probe);
    assert_ne!(
        after,
        "allow",
        "probe {:?} was {before} without a table but Allow once the sibling {:?} was learned; \
         learned key {k}, probe shape {}",
        cmd_of(probe),
        cmd_of(sibling),
        shape_with(e, rules, probe)
    );
}

// ------------------------------------------------------- symlinks in a placed class

/// CONDITIONAL (HOME inside /private/tmp only): `rm -rf <scratchpad>/dlink/sub`
/// where `dlink -> $HOME/docs`: rm follows the directory symlink in the middle
/// of the path and deletes `$HOME/docs/sub`. The shape is classed by the
/// spelling (`s4-scratchpad`), so it shares the key of a plain scratchpad
/// `rm -rf` (observed: both `scoped-destructive-confined|s4-scratchpad|rm -f
/// -r`). With HOME outside every temp root the detector Denies the probe
/// (`rm-recursive`) and Allows the sibling, so there is no reach there; the
/// class label is spelling-based in both configurations.
#[test]
fn r5_symlinked_dir_in_scratchpad_rides_scratchpad_rm_allow_into_home() {
    let e = env_home_in_tmp("symhome");
    let docs = e.home.join("docs");
    untracked(&docs.join("sub"));
    std::os::unix::fs::symlink(&docs, e.sp.join("dlink")).unwrap();
    let sibling = bash(&e.wt, &format!("rm -rf {}/x", e.sp.display()));
    let probe = bash(&e.wt, &format!("rm -rf {}/dlink/sub", e.sp.display()));
    assert_no_ride(&e, None, &sibling, &probe);
}

/// An untracked directory with content (not recoverable from git).
fn untracked(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("data.txt"), "not in git").unwrap();
}

/// CONDITIONAL (HOME inside /private/tmp only): `rm -rf
/// <scratchpad>/clink/hooks` where `clink -> $HOME/.claude`: deletes
/// `~/.claude/hooks` (protected config). Observed: the detector Asks
/// (`scoped-destructive-confined`, NOT a protected-path rule) with shape
/// `scoped-destructive-confined|s4-scratchpad|rm -f -r`, the key of a plain
/// scratchpad `rm -rf`. With HOME outside every temp root the detector Denies
/// it (`rm-recursive`).
#[test]
fn r5_symlinked_dir_in_scratchpad_rides_scratchpad_rm_allow_into_protected_config() {
    let e = env_home_in_tmp("symprot");
    let claude = e.home.join(".claude");
    untracked(&claude.join("hooks"));
    std::fs::write(claude.join("settings.json"), "{}").unwrap();
    std::os::unix::fs::symlink(&claude, e.sp.join("clink")).unwrap();
    let sibling = bash(&e.wt, &format!("rm -rf {}/x", e.sp.display()));
    for target in ["hooks", "settings.json"] {
        let probe = bash(&e.wt, &format!("rm -rf {}/clink/{target}", e.sp.display()));
        assert_no_ride(&e, None, &sibling, &probe);
    }
}

/// GUARD (observed GREEN at a5d3788b7, kept so a change is seen): `rm -rf
/// mlink/notes` in a linked worktree where `mlink -> <main tree>` would delete
/// the main working tree's untracked `notes`. Its shape is classed
/// `s4-worktree` by spelling, but the detector itself Denies it
/// (`rm-recursive-unrecoverable`), so the learned entry is never consulted.
/// (`rm -rf mlink/<absent>` is an Ask with the worktree key; nothing exists
/// to delete, so it is not counted as a reach.) Run with HOME inside
/// /private/tmp, the only configuration where the sibling Asks.
#[test]
fn r5_symlink_to_main_tree_in_worktree_does_not_ride_worktree_rm_allow() {
    let e = env_home_in_tmp("symmain");
    std::os::unix::fs::symlink(&e.main, e.wt.join("mlink")).unwrap();
    // The sibling's target does not exist (an existing untracked one is a
    // detector Deny, an existing tracked / empty one an Allow: only the
    // absent target Asks).
    untracked(&e.main.join("notes"));
    let sibling = bash(&e.wt, "rm -rf build");
    let probe = bash(&e.wt, "rm -rf mlink/notes");
    assert_no_ride(&e, None, &sibling, &probe);
}

// ------------------------------------------------------- unpinned program via env

/// `PATH=<dir>:$PATH git add -A` runs `<dir>/git` (any program), but the
/// assignment is not in the key: it shares `git add -A` with the plain
/// command (observed `git-whole-tree-stage|s4-worktree|git add -A`).
#[test]
fn r5_path_prefix_assignment_rides_git_add_allow() {
    let e = env("pathgit");
    let bin = e.sp.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("git"), "#!/bin/sh\necho any program\n").unwrap();
    let sibling = bash(&e.wt, "git add -A");
    let probe = bash(&e.wt, &format!("PATH={}:$PATH git add -A", bin.display()));
    assert_no_ride(&e, None, &sibling, &probe);
}

/// `EDITOR=<prog> git commit -a` runs `<prog>` on the message file; the
/// assignment is not in the key (`git commit -a`), and `EDITOR` is not one of
/// the `GIT_*` overrides the shape refuses.
#[test]
fn r5_editor_prefix_assignment_rides_git_commit_allow() {
    let e = env("editor");
    let sibling = bash(&e.wt, "git commit -a");
    let probe = bash(
        &e.wt,
        &format!("EDITOR={}/evil git commit -a", e.sp.display()),
    );
    assert_no_ride(&e, None, &sibling, &probe);
}

/// `HOME=<dir> git add -A` makes git read `<dir>/.gitconfig`, whose
/// `core.fsmonitor` is a program git runs; the assignment is not in the key
/// (observed `git-whole-tree-stage|s4-worktree|git add -A`), while
/// `GIT_CONFIG_GLOBAL=` (same effect) is refused with `?`.
#[test]
fn r5_home_prefix_assignment_rides_git_add_allow() {
    let e = env("homegit");
    let fh = e.sp.join("fh");
    std::fs::create_dir_all(&fh).unwrap();
    std::fs::write(
        fh.join(".gitconfig"),
        format!("[core]\n\tfsmonitor = {}/evil\n", e.sp.display()),
    )
    .unwrap();
    let sibling = bash(&e.wt, "git add -A");
    let probe = bash(&e.wt, &format!("HOME={} git add -A", fh.display()));
    assert_no_ride(&e, None, &sibling, &probe);
}

// ------------------------------------------------------- deny regex evaded

fn write_rules(e: &Env, text: &str) -> PathBuf {
    let p = e.root.join("rules").join("ask-rules.toml");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
    p
}

const DENY_GIT_ADD_A: &str = r#"
[[deny]]
reason = "never stage the whole tree"
[deny.match]
command_regex = '\bgit\s+add\s+-A\b'
"#;

/// A human `[[deny]] command_regex` for `git add -A` beats the learned allow
/// for the plain spelling, but `git ad''d -A` (same argv, same shape
/// `git add -A`) is not matched by the raw-text regex and rides the learned
/// allow: "deny beats allow" depends on the spelling.
#[test]
fn r5_deny_command_regex_evaded_by_quoting_while_learned_allow_matches() {
    let e = env("denyq");
    let rules = write_rules(&e, DENY_GIT_ADD_A);
    let plain = bash(&e.wt, "git add -A");
    let quoted = bash(&e.wt, "git ad''d -A");
    let k = key(&e, &plain);
    assert_eq!(
        k,
        key(&e, &quoted),
        "precondition: both spellings share one shape"
    );
    let t = table_with(&e, &k);
    assert_eq!(
        decide_with(&e, &t, Some(&rules), &plain),
        "deny",
        "control: the deny regex beats the learned allow for the plain spelling"
    );
    let got = decide_with(&e, &t, Some(&rules), &quoted);
    assert_ne!(
        got, "allow",
        "`git ad''d -A` (argv identical to `git add -A`) evades the human deny regex \
         and is Allowed by the learned entry {k}"
    );
}

// ------------------------------------------------------- defective rule files

/// Control and probe for a defective human deny rule. Uncompilable regex: the
/// spec says it counts as matched (deny). Wrong type for the same key: the
/// condition is dropped, the rule has no readable key left and is skipped;
/// the learned allow for the shape then applies.
#[test]
fn r5_deny_rule_with_wrong_typed_regex_is_dropped_and_learned_allow_applies() {
    let e = env("denytype");
    let p = bash(&e.wt, "git add -A");
    let t = table_with(&e, &key(&e, &p));
    let broken_rx = write_rules(
        &e,
        "[[deny]]\nreason = \"x\"\n[deny.match]\ncommand_regex = '('\n",
    );
    assert_eq!(
        decide_with(&e, &t, Some(&broken_rx), &p),
        "deny",
        "control: an uncompilable deny regex counts as matched"
    );
    let wrong_type = write_rules(
        &e,
        "[[deny]]\nreason = \"x\"\n[deny.match]\ncommand_regex = 1\n",
    );
    let got = decide_with(&e, &t, Some(&wrong_type), &p);
    assert_ne!(
        got, "allow",
        "a deny rule whose condition cannot be read is judgement-unable (CLAUDE.md §3) but \
         is silently dropped, and the learned allow applies"
    );
}

/// A human rules file with a valid deny for `git add -A` and a TOML syntax
/// error elsewhere: "no rule of this file is usable", so the deny is gone and
/// the learned allow applies to the very shape the file denies.
#[test]
fn r5_unparsable_rules_file_drops_its_denies_and_learned_allow_applies() {
    let e = env("denyparse");
    let p = bash(&e.wt, "git add -A");
    let t = table_with(&e, &key(&e, &p));
    let good = write_rules(&e, DENY_GIT_ADD_A);
    assert_eq!(
        decide_with(&e, &t, Some(&good), &p),
        "deny",
        "control: the parsable file denies"
    );
    let broken = write_rules(
        &e,
        &format!("{DENY_GIT_ADD_A}\n[[deny]\nreason = \"typo\"\n"),
    );
    let got = decide_with(&e, &t, Some(&broken), &p);
    assert_ne!(
        got, "allow",
        "an unparsable rules file (judgement-unable) dropped its deny for this shape and the \
         learned allow applies"
    );
}

// ------------------------------------------------------- the override target

/// `BLASTGUARD_ASK_RULES=<scratchpad>/r/ask-rules.toml`: the file itself is
/// protected (control: `rm -f` / `mv` of the file are not Allow), but
/// removing or renaming its DIRECTORY (`rm -rf <scratchpad>/r`, `mv
/// <scratchpad>/r <scratchpad>/r2`) makes the override path missing ("a
/// missing path = no human rules"): every human deny is gone, the same
/// self-grant as editing the file. Observed: both plain Allow with no table
/// (`allow|s4-scratchpad|rm -f -r`, `allow|s4-scratchpad|mv`).
/// `is_ask_rules_override` compares the exact file path only
/// (crates/blastguard/src/exclude.rs), and `holds_protected_paths` does not
/// consult it.
#[test]
fn r5_removing_or_renaming_the_rules_override_directory_is_not_allow() {
    let e = env("ovr");
    let dir = e.sp.join("r");
    std::fs::create_dir_all(&dir).unwrap();
    let rules = dir.join("ask-rules.toml");
    std::fs::write(&rules, DENY_GIT_ADD_A).unwrap();
    for c in [
        format!("rm -f {}", rules.display()),
        format!("mv {} {}/x", rules.display(), e.sp.display()),
    ] {
        assert_ne!(
            decide_with(&e, &no_table(&e), Some(&rules), &bash(&e.wt, &c)),
            "allow",
            "control: `{c}` on the override file itself is protected"
        );
    }
    let mut bad = vec![];
    for c in [
        format!("rm -rf {}", dir.display()),
        format!("mv {} {}/r2", dir.display(), e.sp.display()),
    ] {
        let got = decide_with(&e, &no_table(&e), Some(&rules), &bash(&e.wt, &c));
        if got == "allow" {
            bad.push(c);
        }
    }
    assert!(
        bad.is_empty(),
        "removing / renaming the directory of the BLASTGUARD_ASK_RULES file removes every \
         human rule, but is Allow: {bad:?}"
    );
}

// ------------------------------------------------------- measurement only

/// Measurement: the same scratchpad `rm -rf` with HOME under `$TMPDIR` vs
/// under `/private/tmp/claude-askrules`.
#[test]
#[ignore]
fn probe_home_location() {
    let e = env("homeloc");
    untracked(&e.sp.join("build"));
    let cmd = format!("rm -rf {}/build", e.sp.display());
    let tmp_home = std::env::temp_dir().join(format!("v5-home-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_home).unwrap();
    for (label, home) in [
        ("tmpdir-home", tmp_home.clone()),
        ("askrules-home", e.home.clone()),
    ] {
        for cwd in ["/tmp/x", e.wt.to_str().unwrap()] {
            let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
            c.args(["shape"])
                .env("HOME", &home)
                .env("BLASTGUARD_LEARNED_TABLE", no_table(&e))
                .env("BLASTGUARD_APPROVALS_DIR", e.root.join("approvals"))
                .env_remove("CLAUDE_PROJECT_DIR")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let p = bash(Path::new(cwd), &cmd);
            let out =
                bounded::run_bounded(&mut c, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
            println!(
                "{label} cwd={cwd} => {}",
                String::from_utf8_lossy(&out.stdout).trim()
            );
        }
    }
}

/// Measurement helper (not a test of a property): prints verdict and shape of
/// every line of `$BASE/probe-cmds.txt` (`W <cmd>` / `M <cmd>` in the linked
/// worktree / main tree, `T:<Tool> <json input>`). `$ROOT` and `__WT__` are
/// substituted.
#[test]
#[ignore]
fn probe_dump() {
    let e = env("probe");
    std::os::unix::fs::symlink(&e.main, e.wt.join("mlink")).unwrap();
    untracked(&e.main.join("notes"));
    let claude = e.home.join(".claude");
    std::fs::create_dir_all(claude.join("hooks")).unwrap();
    std::fs::write(claude.join("settings.json"), "{}").unwrap();
    std::os::unix::fs::symlink(&claude, e.sp.join("clink")).unwrap();
    let rdir = e.sp.join("r");
    std::fs::create_dir_all(&rdir).unwrap();
    let rules = rdir.join("ask-rules.toml");
    std::fs::write(&rules, "").unwrap();
    std::os::unix::fs::symlink(&rules, e.sp.join("rlink")).unwrap();
    std::os::unix::fs::symlink(&rdir, e.sp.join("rdir")).unwrap();
    untracked(&e.home.join("docs").join("sub"));
    std::os::unix::fs::symlink(e.home.join("docs"), e.sp.join("dlink")).unwrap();
    let Ok(text) = std::fs::read_to_string(format!("{BASE}/probe-cmds.txt")) else {
        return;
    };
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (where_, cmd) = line.split_once(' ').unwrap();
        let cmd = &cmd
            .replace("\\n", "\n")
            .replace("$ROOT", e.root.to_str().unwrap())
            .replace("__WT__", e.wt.to_str().unwrap());
        let other;
        let cwd = if where_ == "M" {
            &e.main
        } else if let Some(c) = where_.strip_prefix("C:") {
            other = PathBuf::from(c);
            &other
        } else {
            &e.wt
        };
        let p = if let Some(tool) = where_.strip_prefix("T:") {
            json!({
                "hook_event_name": "PreToolUse",
                "session_id": SID,
                "cwd": e.wt,
                "tool_name": tool,
                "tool_input": serde_json::from_str::<Value>(cmd).unwrap(),
            })
        } else {
            bash(cwd, cmd)
        };
        let s = shape_with(&e, Some(&rules), &p);
        let d = decide_with(&e, &no_table(&e), Some(&rules), &p);
        println!("{where_} {cmd:<60} => {d:<5} {}", s["shape"]);
    }
}
