#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(verifier round 9, 2026-11-28, against 8f4342bfe): reach paths of the
//! askrules judge after the round-8 fixes (cwd tree classed by git kind,
//! unquoted `<` refused, cp/mv into a directory undetermined). Written by an
//! independent verifier, not the implementer (CLAUDE.md §2(a)). Black-box:
//! spawns the built binary with a sandbox HOME and a learned table whose ONLY
//! allow entry is the key `blastguard shape` prints for the probe itself or
//! for a benign sibling.
//!
//! Fixtures: the round-8 production layout (HOME is an ancestor of the main
//! tree `<home>/src/repo` and of its linked worktree
//! `<home>/src/.harness-worktrees/wt`; the scratchpad is `<root>/scratchpad`)
//! in a fresh `tempfile` directory under `/private/tmp/claude-askrules`.
//!
//! What these tests do NOT prove: that the learner would actually promote the
//! entry; that the harm happens at run time beyond the zsh precondition each
//! test observes; anything about shapes not listed here.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000d9";
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

/// Production layout (see the module docs).
fn env_prod(name: &str) -> Env {
    std::fs::create_dir_all(BASE).unwrap();
    let dir = tempfile::Builder::new()
        .prefix(&format!("v9-{name}-"))
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
    let sp = root.join("scratchpad");
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

fn fixtures(e: &Env) {
    let claude = e.home.join(".claude");
    std::fs::create_dir_all(claude.join("hooks")).unwrap();
    std::fs::write(claude.join("settings.json"), "{}").unwrap();
    std::fs::write(e.main.join("notes"), "untracked").unwrap();
    std::fs::create_dir_all(e.main.join("build")).unwrap();
    std::fs::create_dir_all(e.wt.join("build")).unwrap();
    std::fs::write(e.wt.join("build").join("o"), "x").unwrap();
    std::fs::create_dir_all(e.sp.join("build")).unwrap();
    std::fs::write(e.sp.join("payload"), "payload").unwrap();
    std::fs::write(e.wt.join("payload"), "payload").unwrap();
    // A directory whose CHILD named like the copied file is a symlink into
    // the main tree: `cp <f> <dir>/` writes `<dir>/<basename f>` through it.
    std::fs::create_dir_all(e.sp.join("into")).unwrap();
    std::os::unix::fs::symlink(
        e.main.join("src").join("lib.rs"),
        e.sp.join("into").join("payload"),
    )
    .unwrap();
    // Claude Code's own worktree layout: a linked worktree INSIDE the main
    // tree, `<main>/.claude/worktrees/<name>`, holding uncommitted work.
    let w2 = e.main.join(".claude").join("worktrees").join("w2");
    std::fs::create_dir_all(w2.parent().unwrap()).unwrap();
    git(&e.main, &["worktree", "add", "-q", w2.to_str().unwrap()]);
    std::fs::write(w2.join("wip.rs"), "uncommitted").unwrap();
    std::fs::create_dir_all(e.main.join("ign")).unwrap();
    // `~/.claude` contents that are not on the protected list: transcripts
    // and memory, the plugin tree, the user CLAUDE.md.
    let mem = claude.join("projects").join("p").join("memory");
    std::fs::create_dir_all(&mem).unwrap();
    std::fs::write(mem.join("MEMORY.md"), "memory").unwrap();
    std::fs::write(claude.join("projects").join("p").join("s.jsonl"), "{}").unwrap();
    std::fs::write(claude.join("CLAUDE.md"), "user rules").unwrap();
    let cache_bin = claude
        .join("plugins")
        .join("cache")
        .join("m")
        .join("p")
        .join("1")
        .join("bin");
    std::fs::create_dir_all(&cache_bin).unwrap();
    std::fs::write(cache_bin.join("p"), "bin").unwrap();
    std::fs::create_dir_all(e.wt.join("into")).unwrap();
    std::os::unix::fs::symlink(
        claude.join("settings.json"),
        e.wt.join("into").join("payload"),
    )
    .unwrap();
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

fn write_tool(cwd: &Path, path: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID,
        "cwd": cwd,
        "tool_name": "Write",
        "tool_input": { "file_path": path, "content": "x" },
    })
}

fn shape(e: &Env, p: &Value) -> Value {
    let (code, out, err) = spawn(e, &no_table(e), &["shape"], &p.to_string());
    assert_eq!(code, 0, "shape failed: {out:?} {err:?}");
    serde_json::from_str(out.trim()).unwrap()
}

fn table_with(e: &Env, k: &str) -> PathBuf {
    let p = e.root.join("t.json");
    let entry = json!({"shape": k, "decision": "allow", "approvals": 50, "declines": 0,
                       "sessions": 20, "samples": []});
    std::fs::write(&p, json!({"version": 1, "entries": [entry]}).to_string()).unwrap();
    p
}

#[allow(dead_code)]
fn show(p: &Value) -> String {
    match p["tool_name"].as_str() {
        Some("Bash") => p["tool_input"]["command"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        Some(t) => format!(
            "{t}({})",
            p["tool_input"]["file_path"].as_str().unwrap_or("")
        ),
        None => String::new(),
    }
}

/// Learn `sibling` allow; `probe` must not become Allow. (Kept from round 8
/// for the next round; no round-9 ride was found.)
#[allow(dead_code)]
fn assert_no_ride(e: &Env, sibling: &Value, probe: &Value) {
    let before = decide_with(e, &no_table(e), probe);
    assert_ne!(
        before,
        "allow",
        "precondition: probe {:?} must not be Allow without a table",
        show(probe)
    );
    let sib = shape(e, sibling);
    assert_eq!(
        sib["decision"],
        "ask",
        "precondition: sibling {:?} must Ask: {sib}",
        show(sibling)
    );
    let k = sib["shape"].as_str().unwrap().to_string();
    let t = table_with(e, &k);
    assert_eq!(
        decide_with(e, &t, sibling),
        "allow",
        "control: sibling {:?} with its own allow entry {k}",
        show(sibling)
    );
    let after = decide_with(e, &t, probe);
    assert_ne!(
        after,
        "allow",
        "probe {:?} was {before} without a table but Allow once the sibling {:?} was learned; \
         learned key {k}, probe shape {}",
        show(probe),
        show(sibling),
        shape(e, probe)
    );
}

/// The benign sibling of the `rm -rf` rides below: removing a stale condukt
/// worktree directory from a linked worktree. Observed at 7b58a1b5f: Ask,
/// key `scoped-destructive-confined|s4-worktree|rm -f -r`.
#[allow(dead_code)]
fn condukt_cleanup(e: &Env) -> Value {
    let stale = e.home.join(".condukt").join("worktrees").join("stale");
    bash(&e.wt, &format!("rm -rf {}", stale.display()))
}

fn class_of_key(k: &str) -> &str {
    k.split('|').nth(1).unwrap_or("")
}

const ELIGIBLE: &[&str] = &["s4-none", "s4-worktree", "s4-scratchpad"];

/// Measurement helper (asserts nothing). `<where> <cmd>`: where = W / M / S /
/// P / C (cwd = worktree / main tree / scratchpad / `<home>/src` /
/// `~/.claude`); prefix F = a Write tool call whose file_path is `<cmd>`.
#[test]
#[ignore]
fn probe_dump() {
    let e = env_prod("probe");
    fixtures(&e);
    let probes = std::env::var("V9_PROBES").unwrap_or_else(|_| PROBES.to_string());
    for line in probes.lines().filter(|l| !l.trim().is_empty()) {
        let (where_, cmd) = line.split_once(' ').unwrap();
        let cmd = cmd
            .replace("$SP", e.sp.to_str().unwrap())
            .replace("$WT", e.wt.to_str().unwrap())
            .replace("$MAIN", e.main.to_str().unwrap())
            .replace("$HOMEDIR", e.home.to_str().unwrap());
        let w = where_.trim_start_matches('F');
        let src = e.home.join("src");
        let dotclaude = e.home.join(".claude");
        let w2 = e.main.join(".claude").join("worktrees").join("w2");
        let cwd = match w {
            "X" => &w2,
            "M" => &e.main,
            "S" => &e.sp,
            "P" => &src,
            "C" => &dotclaude,
            _ => &e.wt,
        };
        let p = if where_.starts_with('F') {
            write_tool(cwd, &cmd)
        } else {
            bash(cwd, &cmd)
        };
        let s = shape(&e, &p);
        let d = decide_with(&e, &no_table(&e), &p);
        let k = s["shape"].as_str().unwrap_or("").to_string();
        let own = decide_with(&e, &table_with(&e, &k), &p);
        println!("{where_} {cmd:<70} => {d:<5} own={own:<5} {k}");
    }
}

const PROBES: &str = r#"
W git add -A > /dev/null
W git add -A 2>&1
W git add -A >> $WT/log
W git add -A 2> $WT/log
W git add -A > log
S rm -rf build 2>&1
S rm -rf build > /dev/null
S rm -rf build > log
X git add -A
X rm -rf $MAIN/src
X rm -rf $MAIN/notes
FX $MAIN/src/lib.rs
FX $MAIN/notes
X rm -rf $MAIN/build
X rm -rf ../../../build
S rm -rf build > $SP/log
S rm -rf build 2> $SP/log
S rm -rf build >> $SP/log
W git add -A > $SP/log
W git add -A > $WT/log
S cp payload into
S cp -t into payload
S cp -r payload into
S cp -f payload into
W git clean -fdx
W git clean -fdx >! $HOMEDIR/.claude/settings.json
W git reset --hard
W git reset --hard >! $HOMEDIR/.claude/settings.json
W git branch -D other
W git branch -D other >! $HOMEDIR/.claude/settings.json
W git stash drop
W git stash drop >! $HOMEDIR/.claude/settings.json
W git checkout -f
W git checkout -f >! $HOMEDIR/.claude/settings.json
W git restore .
W git restore . >! $MAIN/src/lib.rs
W rm -rf build
W rm -rf build >! $MAIN/src/lib.rs
W git status >! $HOMEDIR/.claude/settings.json
W echo x >! $HOMEDIR/.claude/settings.json
W echo x >>! $HOMEDIR/.claude/settings.json
W echo x &>! $HOMEDIR/.claude/settings.json
W echo x >&! $HOMEDIR/.claude/settings.json
W echo x >! $MAIN/.githooks/pre-commit
W echo x > $HOMEDIR/.claude/settings.json
W git add -A >! $MAIN/src/lib.rs
W git add -A >! $HOMEDIR/.claude/settings.json
W git add -A
W git add -A $MAIN/src/lib.rs
S rm -rf build >! $MAIN/src/lib.rs
W git commit -m x >! $MAIN/src/lib.rs
W git commit -m x
W rm -rf $WT/build/.
W mkdir $MAIN/newdir
W mkdir $HOMEDIR/.claude/settings.local.json
"#;

/// Measurement helper (asserts nothing): the structural gate on zsh forms.
#[test]
#[ignore]
fn static_dump() {
    for c in [
        "git add -A >! /x",
        "git add -A >>! /x",
        "git add -A >>| /x",
        "git add -A >&! /x",
        "git add -A &>! /x",
        "git add -A &>| /x",
        "git add -A 2>! /x",
        "git add -A >| /x",
        "git add -A >(true)",
        "git add -A > >(true)",
        "git add -A =(true)",
        "git add -A x=(true)",
        "git add -A ~root/x",
        "git add -A a~b",
        "git add -A ^x",
        "git add -A x#",
        "git add -A &!",
        "git add -A &|",
        "git add -A !",
        "git add -A x!",
        "git add -A %1",
        "git add -A -- x",
    ] {
        println!(
            "{c:<28} static={}",
            blastguard::shape::is_single_static_command(c)
        );
    }
}

fn run_zsh(dir: &Path, home: &Path, script: &str) -> String {
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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ------------------------------------------------ zsh `>!` clobber redirect

/// Condition 2 ("every redirect target is a static literal" — of the
/// redirect the SHELL performs): zsh (the Bash tool's shell) reads `>!`,
/// `>>!`, `&>!`, `2>!` as the clobber forms of `>`, `>>`, `&>`, `2>` (no
/// option needed). The brush AST reads `cmd >! /p` as the redirect `> !`
/// (a file named `!` in the cwd) plus the ARGUMENT `/p`. So the real write
/// target `/p` is never classed as a target, and the shape is classed by
/// `<cwd>/!`. Observed at 8f4342bfe (cwd = linked worktree):
/// `git add -A >! <main>/src/lib.rs` -> `allow|s4-worktree|git add -A`,
/// `git status >! <home>/.claude/settings.json` -> `allow|s4-worktree|git status`;
/// `is_single_static_command` is `true` for all four forms. (No ride was
/// found: every probe the detector Asks for without the redirect becomes
/// Allow/Deny with the extra operand; the detector side is
/// `r9_detector_zsh_bang_clobber_overwrites_protected_config`.)
#[test]
fn r9_zsh_bang_clobber_redirect_is_not_static_and_not_a_placed_class() {
    let e = env_prod("zshbang");
    fixtures(&e);
    // Precondition (observed, not assumed): zsh writes the path AFTER `>!`
    // and creates no file named `!`.
    let victim = e.root.join("victim");
    std::fs::write(&victim, "orig").unwrap();
    run_zsh(
        &e.sp,
        &e.home,
        &format!("print -r -- clobbered >! {}", victim.display()),
    );
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap().trim(),
        "clobbered",
        "precondition: zsh `>!` writes the following path"
    );
    assert!(
        !e.sp.join("!").exists(),
        "precondition: zsh `>!` creates no file named `!`"
    );
    let mut bad = vec![];
    for c in [
        "git add -A >! /x",
        "git add -A >>! /x",
        "git add -A &>! /x",
        "git add -A 2>! /x",
    ] {
        if blastguard::shape::is_single_static_command(c) {
            bad.push(format!("`{c}` passes the structural gate"));
        }
    }
    let settings = e.home.join(".claude").join("settings.json");
    for probe in [
        format!("git add -A >! {}/src/lib.rs", e.main.display()),
        format!("git status >! {}", settings.display()),
        format!("git commit -m x >>! {}/src/lib.rs", e.main.display()),
    ] {
        let s = shape(&e, &bash(&e.wt, &probe));
        let k = s["shape"].as_str().unwrap();
        if ELIGIBLE.contains(&class_of_key(k)) {
            bad.push(format!("`{probe}` => {} {k}", s["decision"]));
        }
    }
    assert!(
        bad.is_empty(),
        "a zsh clobber redirect `>!` is read as a redirect to `./!` plus an argument, so \
         its real target (main tree / ~/.claude/settings.json) is not classed:\n{}",
        bad.join("\n")
    );
}

// ------------------------------------------------ `cp f DIR` with a RELATIVE DIR

/// Condition 5 / the round-8 fix "cp/mv into a directory is undetermined":
/// `detect::ast::shape::is_existing_dir(p)` is `std::fs::metadata(p)` on the
/// spelling as resolved by `resolve_at`, which leaves a relative operand
/// relative when no `cd` applies (`eval::cwd::resolve`, `Cwd::Hook`). The
/// metadata call then reads it against the HOOK PROCESS's cwd, not the
/// payload cwd, so `cp payload into` (no trailing slash) run in the
/// scratchpad does not see that `into` is a directory. `into/payload` is a
/// symlink into the main tree; cp writes through it (precondition below,
/// observed). Observed at 8f4342bfe: `allow|s4-scratchpad|cp`,
/// `allow|s4-scratchpad|cp -t`, `allow|s4-scratchpad|cp -f`. (The absolute
/// spellings and `into/` are `s4-undetermined`; the detector Allows these
/// lines without a table, so no ride: this is the class property.)
#[test]
fn r9_cp_into_relative_dir_through_symlinked_child_is_not_a_placed_class() {
    let e = env_prod("cprel");
    fixtures(&e);
    let mut bad = vec![];
    for probe in [
        "cp payload into",
        "cp -t into payload",
        "cp -f payload into",
    ] {
        let s = shape(&e, &bash(&e.sp, probe));
        let k = s["shape"].as_str().unwrap();
        if ELIGIBLE.contains(&class_of_key(k)) {
            bad.push(format!(
                "`{probe}` (cwd scratchpad) => {} {k}",
                s["decision"]
            ));
        }
    }
    // Precondition (observed): the copy lands in the MAIN tree's tracked file.
    let st = Command::new("cp")
        .args(["payload", "into"])
        .current_dir(&e.sp)
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(
        std::fs::read_to_string(e.main.join("src").join("lib.rs")).unwrap(),
        "payload",
        "precondition: `cp payload into` writes through into/payload into the main tree"
    );
    assert!(
        bad.is_empty(),
        "a copy into a RELATIVE directory whose child is a symlink into the main tree is \
         classed as an Allow-eligible place:\n{}",
        bad.join("\n")
    );
}

// ------------------------------------------------ detector (not askrules)

/// DETECTOR, outside askrules (no table): the zsh clobber redirect `>!`
/// overwrites protected config with a plain Allow. Control: `>` to the same
/// path is Ask (`protected-path`). Precondition (zsh writes the path after
/// `>!`) is observed in
/// `r9_zsh_bang_clobber_redirect_is_not_static_and_not_a_placed_class`.
/// Observed at 8f4342bfe: all three probes `allow` (`allow|s4-worktree|echo ?`).
#[test]
#[ignore = "detector, filed separately"]
fn r9_detector_zsh_bang_clobber_overwrites_protected_config() {
    let e = env_prod("detbang");
    fixtures(&e);
    let settings = e.home.join(".claude").join("settings.json");
    let control = format!("echo x > {}", settings.display());
    assert_ne!(
        decide_with(&e, &no_table(&e), &bash(&e.wt, &control)),
        "allow",
        "control: `{control}`"
    );
    let mut bad = vec![];
    for c in [
        format!("echo x >! {}", settings.display()),
        format!("echo x >>! {}", settings.display()),
        format!("echo x >! {}/.githooks/pre-commit", e.main.display()),
    ] {
        let got = decide_with(&e, &no_table(&e), &bash(&e.wt, &c));
        if got == "allow" {
            bad.push(format!("`{c}` => {got}"));
        }
    }
    assert!(
        bad.is_empty(),
        "these overwrite protected config under zsh but are Allow:\n{}",
        bad.join("\n")
    );
}

/// DETECTOR, outside askrules (no table): `detect::git_stage::verdict` takes
/// the raw segment's trailing words as pathspecs, so a redirect after
/// `git add -A` (`2>&1`, `> /dev/null`, `> log`) is read as a pathspec and the
/// whole-tree-staging Ask becomes Allow. git still stages the whole tree.
/// Control: `git add -A` is Ask (`git-whole-tree-stage`). Observed at 8f4342bfe.
#[test]
#[ignore = "detector, filed separately"]
fn r9_detector_redirect_drops_git_add_sweep_ask() {
    let e = env_prod("detstage");
    fixtures(&e);
    assert_eq!(
        decide_with(&e, &no_table(&e), &bash(&e.wt, "git add -A")),
        "ask",
        "control: `git add -A`"
    );
    let mut bad = vec![];
    for c in [
        "git add -A 2>&1",
        "git add -A > /dev/null",
        "git add -A > log",
    ] {
        let got = decide_with(&e, &no_table(&e), &bash(&e.wt, c));
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

/// DETECTOR false positive, outside askrules (no table): `rm -rf build` in
/// the scratchpad is Ask (`scoped-destructive-confined`, a bounded question),
/// but the same line with an output redirect that writes nothing outside the
/// scratchpad (`2>&1`, `> log`) is Deny (`rm-recursive`). A redirect of rm's
/// output does not change what rm deletes. Observed at 8f4342bfe.
#[test]
#[ignore = "detector, filed separately"]
fn r9_detector_redirect_turns_scratchpad_rm_ask_into_deny() {
    let e = env_prod("detrmfp");
    fixtures(&e);
    assert_eq!(
        decide_with(&e, &no_table(&e), &bash(&e.sp, "rm -rf build")),
        "ask",
        "control: `rm -rf build` in the scratchpad"
    );
    let mut bad = vec![];
    for c in ["rm -rf build 2>&1", "rm -rf build > log"] {
        let got = decide_with(&e, &no_table(&e), &bash(&e.sp, c));
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

// ------------------------------------------------ guards (must stay green)

/// Guard for the false-positive side (task item 3): own-key Allow still
/// holds for the linked-worktree `git add -A`, the scratchpad `rm -rf build`
/// (relative and absolute), and from Claude Code's nested linked worktree
/// `<main>/.claude/worktrees/w2`. Observed GREEN at 8f4342bfe.
#[test]
fn r9_guard_own_key_allows_stay() {
    let e = env_prod("guard");
    fixtures(&e);
    let w2 = e.main.join(".claude").join("worktrees").join("w2");
    for (cwd, c) in [
        (&e.wt, "git add -A".to_string()),
        (&w2, "git add -A".to_string()),
        (&e.sp, "rm -rf build".to_string()),
        (&e.sp, format!("rm -rf {}/build", e.sp.display())),
    ] {
        let p = bash(cwd, &c);
        let s = shape(&e, &p);
        assert_eq!(s["decision"], "ask", "precondition: `{c}` Asks: {s}");
        let k = s["shape"].as_str().unwrap();
        assert_eq!(
            decide_with(&e, &table_with(&e, k), &p),
            "allow",
            "`{c}` with its own allow entry {k}"
        );
    }
}

// ------------------------------------------------ class `none` for a real write

/// Spec Components: `None` is "command has no write target". `mkdir` and
/// `touch` are in `askrules::builtin::MODELLED_TOOLS` (`Model::Plain`, "the
/// verb says everything that matters"), but `judge::table_b::extract` has no
/// row for them, so `shape_facts_in` lists no target and the class is
/// `none` (Allow-eligible) wherever they write: the MAIN tree (never
/// Allow-eligible, CLAUDE.md §8) or `~/.claude` (a directory at
/// `settings.local.json` makes that settings file unwritable). Observed at
/// 8f4342bfe: `mkdir <main>/newdir` -> `allow|s4-none|mkdir`,
/// `mkdir <home>/.claude/settings.local.json` -> `allow|s4-none|mkdir`. (The
/// detector Allows these lines without a table: class property, no ride.)
#[test]
fn r9_mkdir_touch_targets_are_classed_not_none() {
    let e = env_prod("mkdir");
    fixtures(&e);
    let m = e.main.display();
    let h = e.home.display();
    let mut bad = vec![];
    for probe in [
        format!("mkdir {m}/newdir"),
        format!("mkdir -p {m}/src/new"),
        format!("touch {m}/src/new.rs"),
        format!("mkdir {h}/.claude/settings.local.json"),
    ] {
        let s = shape(&e, &bash(&e.wt, &probe));
        let k = s["shape"].as_str().unwrap();
        if ELIGIBLE.contains(&class_of_key(k)) {
            bad.push(format!("`{probe}` => {} {k}", s["decision"]));
        }
    }
    assert!(
        bad.is_empty(),
        "a mkdir / touch that writes into the main tree or ~/.claude is classed as an \
         Allow-eligible place:\n{}",
        bad.join("\n")
    );
}

/// Spec Components: `Worktree` is "inside a git linked worktree ... or under
/// `~/.condukt/worktrees/...`". `shape::class_of_path_spelled` returns
/// `Worktree` for any spelling that CONTAINS `/.harness-worktrees/`, so the
/// worktree CONTAINER itself, spelled with a trailing `/`, `/.` or `//`, is
/// `s4-worktree` (the container without the slash is `s4-home`). Observed at
/// 8f4342bfe: `rm -rf <home>/src/.harness-worktrees/` ->
/// `rm-recursive|s4-worktree|rm -f -r` (the detector Denies the rm; the
/// detector-side `mv` of the container is filed separately).
#[test]
fn r9_worktree_container_is_not_a_worktree_class() {
    let e = env_prod("container");
    fixtures(&e);
    let c = e.home.join("src").join(".harness-worktrees");
    let c = c.display();
    let mut bad = vec![];
    for probe in [
        format!("rm -rf {c}/"),
        format!("rm -rf {c}/."),
        format!("rm -rf {c}//"),
    ] {
        let s = shape(&e, &bash(&e.wt, &probe));
        let k = s["shape"].as_str().unwrap();
        if ELIGIBLE.contains(&class_of_key(k)) {
            bad.push(format!("`{probe}` => {} {k}", s["decision"]));
        }
    }
    assert!(
        bad.is_empty(),
        "the worktree container (every session's worktree) is classed as one \
         worktree:\n{}",
        bad.join("\n")
    );
}
