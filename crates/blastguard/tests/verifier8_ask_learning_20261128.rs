#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(verifier round 8, 2026-11-28, against 7b58a1b5f): reach paths of the
//! askrules judge after the round-7 fixes (bare command word, no zsh `=word`,
//! no symlink component). Written by an independent verifier, not the
//! implementer (CLAUDE.md §2(a)). Black-box: spawns the built binary with a
//! sandbox HOME, a learned table whose ONLY allow entry is the key
//! `blastguard shape` prints for a benign sibling.
//!
//! Fixtures live in a fresh `tempfile` directory under
//! `/private/tmp/claude-askrules`, production layout: HOME is an ancestor of
//! the main tree `<home>/src/repo` and of its linked worktree
//! `<home>/src/.harness-worktrees/wt`; the scratchpad is `<root>/scratchpad`.
//!
//! What these tests do NOT prove: that the learner would actually promote the
//! sibling; that the probe's harm happens at run time (nothing is executed);
//! anything about shapes not listed here.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000d8";
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
        .prefix(&format!("v8-{name}-"))
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

/// Learn `sibling` allow; `probe` must not become Allow.
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
    // Round 9 update (askrules target narrowing: Worktree class is only the
    // inside of a linked worktree): the sibling is no longer Allow-learnable,
    // so the control is inverted. It stays Ask even with its own allow entry.
    assert_eq!(
        decide_with(e, &t, sibling),
        "ask",
        "control: sibling {:?} is not Allow-eligible even with its own allow entry {k}",
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
fn condukt_cleanup(e: &Env) -> Value {
    let stale = e.home.join(".condukt").join("worktrees").join("stale");
    bash(&e.wt, &format!("rm -rf {}", stale.display()))
}

fn class_of_key(k: &str) -> &str {
    k.split('|').nth(1).unwrap_or("")
}

const ELIGIBLE: &[&str] = &["s4-none", "s4-worktree", "s4-scratchpad"];

// ------------------------------------------------ the payload cwd is a "worktree"

/// `shape::class_of_path_spelled` ends with "the session's own tree (the
/// payload cwd), unless the cwd is `$HOME` or one of its ancestors" ->
/// `Worktree`. So ANY other cwd makes everything below it (that is not on the
/// protected list) `s4-worktree`, git or not. Spec, Components 1: `Worktree`
/// is "inside a git linked worktree or the session worktree /
/// ~/.condukt/worktrees/...", and paths under `$HOME` are `Home` (never
/// Allow-eligible). With the payload cwd `~/.claude` (a session started there),
/// `rm -rf projects` (every transcript and every memory file) Asks with the
/// key of the condukt cleanup and rides its learned Allow. Observed at
/// 7b58a1b5f: probe `ask` without a table, key
/// `scoped-destructive-confined|s4-worktree|rm -f -r`; the same absolute path
/// from a worktree cwd is `s4-home`.
#[test]
fn r8_cwd_under_home_makes_dot_claude_a_worktree_and_rides_rm_allow() {
    let e = env_prod("cwdclaude");
    fixtures(&e);
    let claude = e.home.join(".claude");
    let abs = claude.join("projects");
    assert_eq!(
        class_of_key(
            shape(&e, &bash(&e.wt, &format!("rm -rf {}", abs.display())))["shape"]
                .as_str()
                .unwrap()
        ),
        "s4-home",
        "control: the same path from a worktree cwd is Home"
    );
    for probe in [
        "rm -rf projects".to_string(),
        format!("rm -rf {}", abs.display()),
    ] {
        assert_no_ride(&e, &condukt_cleanup(&e), &bash(&claude, &probe));
    }
}

/// Same root cause with the payload cwd `<home>/src`, the parent of the main
/// tree and of `.harness-worktrees`: `rm -rf .harness-worktrees` removes every
/// linked worktree with its uncommitted work. The detector Denies removing the
/// one worktree that holds untracked files (`rm -rf .harness-worktrees/wt`,
/// precondition below) but only Asks for the container, and that Ask carries
/// the condukt-cleanup key. Observed at 7b58a1b5f.
#[test]
fn r8_cwd_parent_of_trees_rm_worktree_container_rides_rm_allow() {
    let e = env_prod("cwdsrc");
    fixtures(&e);
    let src = e.home.join("src");
    assert_eq!(
        decide_with(
            &e,
            &no_table(&e),
            &bash(&src, "rm -rf .harness-worktrees/wt")
        ),
        "deny",
        "precondition: removing the worktree that holds untracked work is Deny"
    );
    assert_no_ride(
        &e,
        &condukt_cleanup(&e),
        &bash(&src, "rm -rf .harness-worktrees"),
    );
}

/// Direct property (spec Components 1 + "Git writes take the class of the
/// repository": the MAIN working tree is `main-tree`, never Allow-eligible,
/// CLAUDE.md §8). With the payload cwd = the main tree, `git add -A` is
/// `s4-main-tree`, but a PATH target inside that same tree is `s4-worktree`
/// (the cwd fallback above). Observed at 7b58a1b5f:
/// `rm -rf nonexistent` / `rm -rf <main>/nonexistent` ->
/// `scoped-destructive-confined|s4-worktree|rm -f -r` (Ask; rides the condukt
/// cleanup allow), `Write <main>/src/lib.rs` -> `allow|s4-worktree|Write`.
#[test]
fn r8_main_tree_path_targets_are_not_classed_as_a_placed_class() {
    let e = env_prod("mainpath");
    fixtures(&e);
    let m = e.main.display();
    let control = shape(&e, &bash(&e.main, "git add -A"));
    assert_eq!(
        class_of_key(control["shape"].as_str().unwrap()),
        "s4-main-tree",
        "control: the git write in the same cwd is main-tree"
    );
    let mut bad = vec![];
    for p in [
        bash(&e.main, "rm -rf nonexistent"),
        bash(&e.main, &format!("rm -rf {m}/nonexistent")),
        bash(&e.main, "rm -rf src"),
        write_tool(&e.main, &format!("{m}/src/lib.rs")),
    ] {
        let s = shape(&e, &p);
        let k = s["shape"].as_str().unwrap();
        if ELIGIBLE.contains(&class_of_key(k)) {
            bad.push(format!("`{}` => {} {k}", show(&p), s["decision"]));
        }
    }
    assert!(
        bad.is_empty(),
        "a path inside the MAIN working tree is classed as an Allow-eligible place:\n{}",
        bad.join("\n")
    );
    // And the ride: the Ask among them takes the condukt cleanup's allow.
    assert_no_ride(
        &e,
        &condukt_cleanup(&e),
        &bash(&e.main, "rm -rf nonexistent"),
    );
}

// ------------------------------------------------ zsh numeric glob `<->`

/// Condition 2 ("no glob"): zsh (the Bash tool's shell) expands `<n-m>` /
/// `<->` as a numeric-range glob by default (no option needed), but the brush
/// AST reads `v<-> build` as the word `v` plus the redirects `< -` and
/// `> build`, all static. Observed below: zsh expands
/// `<sp>/scratchpad<->` to the SIBLING directory `<root>/scratchpad1`, outside
/// the scratchpad. No ride was observed (the detector Denies the `rm -rf`
/// forms tried: `rm-recursive`), so this is the structural property only.
#[test]
fn r8_zsh_numeric_glob_is_not_a_static_word() {
    let e = env_prod("zshnum");
    fixtures(&e);
    // fixture re-anchored 2026-11-28 (leader ruling): Scratchpad is the session's own scratchpad only
    // (the sibling `scratchpad1` follows the scratchpad into `<root>/<SID>/`)
    std::fs::create_dir_all(e.sp.parent().unwrap().join("scratchpad1")).unwrap();
    let mut c = Command::new("zsh");
    c.args([
        "-f",
        "-c",
        &format!("print -r -- {}<-> build", e.sp.display()),
    ])
    .current_dir(&e.sp)
    .env("HOME", &e.home)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut c, b"", bounded::TOOL_DEADLINE);
    let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(
        got,
        format!("{}1 build", e.sp.display()),
        "precondition: zsh expands `<->` to the sibling directory"
    );
    let mut bad = vec![];
    for cmd in [
        "rm -rf v<-> build".to_string(),
        format!("rm -rf {}<-> build", e.sp.display()),
        "git add -A v<1-9> x".to_string(),
    ] {
        if blastguard::shape::is_single_static_command(&cmd) {
            bad.push(cmd);
        }
    }
    assert!(
        bad.is_empty(),
        "zsh numeric globs pass as static literal words:\n{}",
        bad.join("\n")
    );
}

// ------------------------------------------------ `cp f dir/` through a symlinked child

/// Condition 5 ("every component of a path that decides the class is
/// lstat'ed; a symlink component makes the class undetermined"). `cp f <dir>/`
/// writes `<dir>/<basename f>`; when that entry is a symlink (here into the
/// main tree) cp writes THROUGH it. The class is taken from `<dir>/` only.
/// Observed at 7b58a1b5f: `cp <sp>/payload <sp>/into/` (`into/payload ->
/// <main>/src/lib.rs`) is `allow|s4-scratchpad|cp`. (The rules-file guard
/// already judges `DEST/<basename SRC>`, spec "Rules-file protection", and a
/// protected target is caught: `cp payload into/` in the worktree with
/// `into/payload -> ~/.claude/settings.json` is `s4-protected-config`.) No
/// ride observed: the detector Allows this line without a table.
#[test]
fn r8_cp_into_dir_through_symlinked_child_is_not_a_placed_class() {
    let e = env_prod("cpchild");
    fixtures(&e);
    let sp = e.sp.display();
    let mut bad = vec![];
    for (cwd, probe) in [
        (&e.sp, format!("cp {sp}/payload {sp}/into/")),
        (&e.sp, format!("cp {sp}/payload {sp}/into")),
        (&e.sp, "cp payload into/".to_string()),
    ] {
        let s = shape(&e, &bash(cwd, &probe));
        let k = s["shape"].as_str().unwrap();
        if ELIGIBLE.contains(&class_of_key(k)) {
            bad.push(format!("`{probe}` => {} {k}", s["decision"]));
        }
    }
    assert!(
        bad.is_empty(),
        "a copy written through a symlinked directory entry (into the main tree) is \
         classed as an Allow-eligible place:\n{}",
        bad.join("\n")
    );
}

// ------------------------------------------------ detector (not askrules)

/// DETECTOR, outside askrules (no table): with the payload cwd `~/.claude`,
/// `rm settings.json` / `rm -f ./settings.json` delete `~/.claude/settings.json`
/// (protected config) and are a plain Allow, although `blastguard shape`
/// classes them `s4-protected-config`. Control: the absolute spelling is Deny
/// (`protected-path-disarm`). Observed at 7b58a1b5f.
#[test]
#[ignore = "detector, filed separately"]
fn r8_detector_relative_rm_of_claude_settings_is_allow() {
    let e = env_prod("detrel");
    fixtures(&e);
    let claude = e.home.join(".claude");
    let abs = format!("rm -f {}", claude.join("settings.json").display());
    assert_eq!(
        decide_with(&e, &no_table(&e), &bash(&e.wt, &abs)),
        "deny",
        "control: `{abs}`"
    );
    let mut bad = vec![];
    for c in [
        "rm settings.json",
        "rm -f settings.json",
        "rm -f ./settings.json",
    ] {
        let got = decide_with(&e, &no_table(&e), &bash(&claude, c));
        if got == "allow" {
            bad.push(format!("`{c}` (cwd ~/.claude) => {got}"));
        }
    }
    assert!(
        bad.is_empty(),
        "these delete ~/.claude/settings.json but are Allow:\n{}",
        bad.join("\n")
    );
}

/// Measurement helper (asserts nothing). `<where> <cmd>`: where = W / M / S /
/// P (cwd = worktree / main tree / scratchpad / `<home>/src`, the parent of
/// both trees); prefix F = a Write tool call whose file_path is `<cmd>`.
#[test]
#[ignore]
fn probe_dump() {
    let e = env_prod("probe");
    fixtures(&e);
    for line in PROBES.lines().filter(|l| !l.trim().is_empty()) {
        let (where_, cmd) = line.split_once(' ').unwrap();
        let cmd = cmd
            .replace("$SP", e.sp.to_str().unwrap())
            .replace("$WT", e.wt.to_str().unwrap())
            .replace("$MAIN", e.main.to_str().unwrap())
            .replace("$HOMEDIR", e.home.to_str().unwrap());
        let w = where_.trim_start_matches('F');
        let src = e.home.join("src");
        let dotclaude = e.home.join(".claude");
        let cwd = match w {
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
W git -C $MAIN add -A
W git --git-dir=$MAIN/.git --work-tree=$MAIN add -A
W git --git-dir $MAIN/.git add -A
W git --work-tree=$MAIN add -A
W git -c core.fsmonitor=$SP/bin/git commit -a
W git -c alias.add=!true add -A
W git --exec-path=$SP/bin add -A
W git config --global core.fsmonitor $SP/bin/git
W git config core.fsmonitor $SP/bin/git
W git -C $WT/src/.. add -A
W ./git add -A
S ./bin/rm -rf build
S rm -rf =git
W git add -A
W git commit -a
"#;

#[allow(dead_code)]
const PROBES_SIXTH: &str = r#"
W rm -f $HOMEDIR/.claude/settings.json
W rm $HOMEDIR/.claude/settings.json
C rm settings.json
C rm -f ./settings.json
W rm -f $HOMEDIR/.zshrc
W rm -f $MAIN/.githooks/pre-commit
W unlink $HOMEDIR/.claude/settings.json
W mv $HOMEDIR/.claude/settings.json $SP/x
C rm -rf projects
C rm -rf projects/
C rm -rf $HOMEDIR/.claude/projects
W rm -rf $HOMEDIR/.claude/projects
C rm -rf plugins
C rm -rf plugins/cache
C rm -rf hooks
C rm -f settings.json
C cp $SP/payload settings.json
FC $HOMEDIR/.claude/CLAUDE.md
FC $HOMEDIR/.claude/projects/p/memory/MEMORY.md
FW $HOMEDIR/.claude/CLAUDE.md
C rm -rf nonexistent
C mv projects $SP/x
C rm -f CLAUDE.md
"#;

#[allow(dead_code)]
const PROBES_FIFTH: &str = r#"
S rm -rf v<-> build
S rm -rf $SP<-> build
W rm -rf $SP<-> build
S rm -rf $SP<->
S rm -rf $SP
S rm -rf build<->x
S rm -rf build <- >x
"#;

#[allow(dead_code)]
const PROBES_FOURTH: &str = r#"
M rm -rf .claude/worktrees/w2
M rm -rf .claude/worktrees
M rm -rf $MAIN/.claude/worktrees
W rm -rf $MAIN/.claude/worktrees/w2
W rm -rf $MAIN/.claude/worktrees
M rm -rf $HOMEDIR/src/.harness-worktrees
M rm -rf $HOMEDIR/src/.harness-worktrees/wt
M mv src $SP/x
M mv $WT $SP/x
M rm -rf ign
M rm -rf $MAIN
M rm -rf .
"#;

#[allow(dead_code)]
const PROBES_THIRD: &str = r#"
P rm -rf repo/src
P rm -rf repo/src/
P rm -rf repo/build
P rm -rf repo/nonexistent
P rm -rf repo/notes
P rm -f repo/notes
P mv repo/src repo/src2
P cp payload repo/src/lib.rs
M rm -rf $HOMEDIR/.condukt/worktrees/x
M rm -rf nonexistent
M rm -rf src/
M rm -rf $MAIN/build
M rm -rf $MAIN/nonexistent
M rm -rf $HOMEDIR/src/repo/src
W rm -rf $HOMEDIR/src/repo/src
W rm -rf $HOMEDIR/src/repo/nonexistent
W rm -rf $HOMEDIR/x
W rm -rf $HOMEDIR/.claude/projects
P rm -rf $HOMEDIR/src/repo/src
P rm -rf nonexistent
P rm -rf .harness-worktrees
"#;

#[allow(dead_code)]
const PROBES_SECOND: &str = r#"
P rm -rf .harness-worktrees
P rm -rf .harness-worktrees/wt
P rm -rf .harness-worktrees/wt/build
W rm -rf $HOMEDIR/src/.harness-worktrees
W rm -rf $HOMEDIR/src/.harness-worktrees/
W rm -rf $HOMEDIR/src/.harness-worktrees/wt
W rm -rf $HOMEDIR/src/.harness-worktrees/wt/
W rm -rf $HOMEDIR/src/.harness-worktrees/old
W rm -rf $WT
W rm -rf $WT/
W rm -rf $WT/build
W rm -rf build/o
S rm -rf $WT/build
S rm -rf $HOMEDIR/src/.harness-worktrees/
S rm -rf $HOMEDIR/src/.harness-worktrees/wt
W rm -rf $HOMEDIR/.condukt/worktrees/
W rm -rf $HOMEDIR/.condukt/worktrees/x
W rm -rf $MAIN/.git/worktrees
W rm -rf $HOMEDIR/src/.harness-worktrees/wt/.git
W rm -f .git
M rm -rf .git
P rm -rf repo/.git
W rm -rf $SP
S rm -rf .
S rm -rf $SP/
W mv $WT $SP/moved
W mv $HOMEDIR/src/.harness-worktrees/ $SP/moved
S mv $SP/build $SP/b2
M rm -rf build/
P rm -rf src
"#;

#[allow(dead_code)]
const PROBES_FIRST: &str = r#"
W git add -A
M git add -A
W rm -rf build
M rm -rf build
M rm -rf src
M rm -rf $MAIN/src
M rm -f notes
M touch src/new.rs
M mkdir newdir
M cp payload src/lib.rs
M cp $SP/payload src/lib.rs
M mv notes notes2
W rm -rf $MAIN/src
W cp $SP/payload $MAIN/src/lib.rs
P rm -rf repo
P rm -rf repo/src
P rm -rf $MAIN
P rm -rf .harness-worktrees
FW $WT/src/lib.rs
FM $MAIN/src/lib.rs
FM $MAIN/notes
FW $MAIN/src/lib.rs
FP $MAIN/src/lib.rs
S cp payload into/
S cp $SP/payload $SP/into/
S cp $SP/payload $SP/into
S cp $SP/payload $SP/build/
W cp payload into/
W cp payload build/
W mv payload into/
W git rebase -x true HEAD
W git rebase --exec true HEAD
W git bisect run true
W git grep -Otrue x
W git diff --output=$SP/o
W git worktree add $SP/wt2
W git worktree add $HOMEDIR/.claude/w
W git worktree move $WT $SP/wt3
W git branch -D other
W git stash clear
W git clean -fdx
M git clean -fdx
W git apply --unsafe-paths --directory=$HOMEDIR/.claude $SP/payload
W git am $SP/payload
W git checkout -- .githooks
W git rm -r .githooks
W git rm crates/blastguard/rules/ask-rules.toml
S rm -rf build
S rm -rf $SP/build
W git commit -m x
W git merge other
W git reset --hard
"#;
