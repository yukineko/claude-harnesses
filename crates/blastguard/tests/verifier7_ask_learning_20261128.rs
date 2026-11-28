#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(verifier round 7, 2026-11-28, against 448ed546d): reach paths of the
//! askrules judge after the structural gate ("Lines an Allow can apply to":
//! one simple command, static literal words, real-path spellings). Written by
//! an independent verifier, not the implementer (CLAUDE.md §2(a)). Black-box:
//! spawns the built binary with a sandbox HOME, a learned table whose ONLY
//! allow entry is the key `blastguard shape` prints for a benign sibling, and
//! (where named) a human rules file via `BLASTGUARD_ASK_RULES`.
//!
//! Fixtures live in a fresh `tempfile` directory under
//! `/private/tmp/claude-askrules`; `<root>/scratchpad` is classed
//! `s4-scratchpad`; a linked git worktree `<root>/wt` of the main tree
//! `<root>/repo` is created per test.
//!
//! What these tests do NOT prove: that the learner would actually promote the
//! sibling; that the probe's harm happens at run time (nothing is executed);
//! anything about shapes not listed here.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000d7";
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
    env_with(name, false)
}

/// HOME inside the temp root: the configuration in which a scratchpad /
/// worktree `rm -rf` is an Ask (verifier5 `probe_home_location`).
fn env_home_in_tmp(name: &str) -> Env {
    env_with(name, true)
}

/// Production layout: HOME is an ANCESTOR of the main tree and the worktree
/// (`<home>/src/repo`, `<home>/src/.harness-worktrees/wt`, like
/// `/Users/<u>/src/harness`), the scratchpad is `/private/tmp/claude-*/..`.
fn env_prod(name: &str) -> Env {
    std::fs::create_dir_all(BASE).unwrap();
    let dir = tempfile::Builder::new()
        .prefix(&format!("v7-{name}-"))
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

fn env_with(name: &str, home_in_tmp: bool) -> Env {
    std::fs::create_dir_all(BASE).unwrap();
    let dir = tempfile::Builder::new()
        .prefix(&format!("v7-{name}-"))
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
            .prefix(&format!("v7-home-{name}-"))
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

/// Learn `sibling` allow; `probe` must not become Allow.
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

fn fixtures(e: &Env) {
    use std::os::unix::fs::PermissionsExt;
    let claude = e.home.join(".claude");
    std::fs::create_dir_all(claude.join("hooks")).unwrap();
    std::fs::write(claude.join("settings.json"), "{}").unwrap();
    std::fs::write(e.main.join("notes"), "untracked").unwrap();
    std::fs::create_dir_all(e.sp.join("r")).unwrap();
    std::fs::write(e.sp.join("r").join("ask-rules.toml"), PROBE_RULES).unwrap();
    std::fs::create_dir_all(e.sp.join("evil")).unwrap();
    std::fs::write(e.sp.join("evil").join("ask-rules.toml"), "").unwrap();
    // An executable named `git` with NO shebang: the shell runs it as a
    // shell script (ENOEXEC fallback), whatever it contains.
    let bin = e.sp.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for f in [bin.join("git"), e.wt.join("git"), bin.join("rm")] {
        std::fs::write(&f, "echo any program\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(e.sp.join("build"), "x").unwrap();
    std::fs::create_dir_all(e.wt.join("build")).unwrap();
    std::fs::create_dir_all(e.main.join("build")).unwrap();
    std::fs::write(e.sp.join("payload"), "payload").unwrap();
    // DANGLING symlinks (their targets do not exist): canonicalize fails on
    // them, but `cp` / `touch` / `>` write THROUGH them and create the target.
    let ln = |t: PathBuf, l: PathBuf| std::os::unix::fs::symlink(t, l).unwrap();
    ln(claude.join("hooks").join("new.sh"), e.sp.join("dangle"));
    ln(e.main.join("newfile"), e.sp.join("dangm"));
    ln(e.home.join(".zshrc"), e.wt.join("dangle"));
    ln(e.main.join("src").join("new.rs"), e.wt.join("dangm"));
}

/// Run `script` with `zsh -f -c` (the Bash tool's shell on this machine:
/// zsh, `equals` on) in `cwd` with HOME = the sandbox home; stdout.
fn zsh(e: &Env, cwd: &Path, script: &str) -> String {
    let mut c = Command::new("zsh");
    c.args(["-f", "-c", script])
        .current_dir(cwd)
        .env("HOME", &e.home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut c, b"", bounded::TOOL_DEADLINE);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ------------------------------------------------ a path-spelled command word

/// Condition 2 holds (every word is a static literal) and the shape keeps
/// only the BASENAME of a path-spelled command word. A directly executed
/// file gets a content hash only when it starts with `#!`
/// (`file_program_token(.., shebang_only = true)`); an executable WITHOUT a
/// shebang adds nothing, yet the shell runs it as a shell script (ENOEXEC
/// fallback, observed below). So `<sp>/bin/git add -A` and `./git add -A`
/// have the key of `git add -A` (observed at 448ed546d:
/// `git-whole-tree-stage|s4-worktree|git add -A`) and ride its learned Allow:
/// an unknown program runs.
#[test]
fn r7_path_spelled_git_without_shebang_rides_git_add_allow() {
    let e = env_prod("pathgit");
    fixtures(&e);
    let fake = e.sp.join("bin").join("git");
    assert_eq!(
        zsh(&e, &e.wt, &format!("{} add -A", fake.display())).trim(),
        "any program",
        "precondition: the shell runs the shebang-less file"
    );
    let sibling = bash(&e.wt, "git add -A");
    for probe in [format!("{} add -A", fake.display()), "./git add -A".into()] {
        assert_no_ride(&e, None, &sibling, &bash(&e.wt, &probe));
    }
}

/// Same hole with the scratchpad `rm -rf` Ask (observed key
/// `scoped-destructive-confined|s4-scratchpad|rm -f -r` for the plain line
/// and for `<sp>/bin/rm -rf build` / `./bin/rm -rf build`).
#[test]
fn r7_path_spelled_rm_without_shebang_rides_scratchpad_rm_allow() {
    let e = env_prod("pathrm");
    fixtures(&e);
    let sibling = bash(&e.sp, "rm -rf build");
    for probe in [
        format!("{}/bin/rm -rf build", e.sp.display()),
        "./bin/rm -rf build".to_string(),
    ] {
        assert_no_ride(&e, None, &sibling, &bash(&e.sp, &probe));
    }
}

// ------------------------------------------------ zsh `=word`

/// zsh (the Bash tool's shell; ruling 2026-11-18: `=word` IS expanded there)
/// replaces a word starting with `=` by the command's path, but
/// `is_static_word` counts `=git` as plain text, and the operand is classed as
/// `<cwd>/=git`. Observed at 448ed546d: `rm -rf =git` in the scratchpad Asks
/// with the plain `rm -rf build` key
/// `scoped-destructive-confined|s4-scratchpad|rm -f -r` and rides its learned
/// Allow, while zsh runs `rm -rf /usr/bin/git` (a system path; any `=tool`
/// names a PATH binary, e.g. one under `~/.cargo/bin`).
#[test]
fn r7_zsh_equals_word_rides_scratchpad_rm_allow() {
    let e = env_prod("zsheq");
    fixtures(&e);
    let expanded = zsh(&e, &e.sp, "print -r -- =git");
    let expanded = expanded.trim();
    assert!(
        expanded.starts_with('/') && !expanded.starts_with(e.sp.to_str().unwrap()),
        "precondition: zsh expands `=git` to a path outside the scratchpad, got {expanded:?}"
    );
    assert_no_ride(
        &e,
        None,
        &bash(&e.sp, "rm -rf build"),
        &bash(&e.sp, "rm -rf =git"),
    );
    assert!(
        !blastguard::shape::is_single_static_command("rm -rf =git"),
        "`=git` is a zsh expansion (to {expanded}), not a static literal word"
    );
}

// ------------------------------------------------ dangling symlinks

/// Condition 3 asks for "the canonical path of the longest existing prefix
/// equals the spelling". A DANGLING symlink does not canonicalise (its target
/// does not exist), so `shape::realpath` treats the link itself as a
/// not-yet-existing remainder and the spelling counts as the real path; but
/// `>` / `cp` / `touch` write THROUGH it and create the target. Observed at
/// 448ed546d: `git status > <sp>/dangm` (`dangm -> <main>/newfile`) Asks with
/// the key of `git status > <sp>/payload` (truncating a scratchpad file),
/// `scoped-destructive-confined|s4-scratchpad|git status`, and rides its
/// learned Allow: the main working tree gets a new file.
#[test]
fn r7_dangling_symlink_redirect_rides_scratchpad_truncate_allow() {
    let e = env_prod("dangride");
    fixtures(&e);
    assert!(
        std::fs::symlink_metadata(e.sp.join("dangm")).is_ok() && !e.main.join("newfile").exists(),
        "precondition: <sp>/dangm is a dangling link into the main tree"
    );
    // Round 9 update (askrules target narrowing, user policy): a line with a
    // redirect is no longer Allow-eligible, so the control is inverted: the
    // sibling stays Ask even with its own allow entry, and so does the probe.
    let sibling = bash(&e.sp, &format!("git status > {}/payload", e.sp.display()));
    let probe = bash(&e.sp, &format!("git status > {}/dangm", e.sp.display()));
    let sib = shape_with(&e, None, &sibling);
    assert_eq!(sib["decision"], "ask", "precondition: sibling Asks: {sib}");
    let k = sib["shape"].as_str().unwrap().to_string();
    let t = table_with(&e, &k);
    assert_eq!(
        decide_with(&e, &t, None, &sibling),
        "ask",
        "a redirecting line is not Allow-eligible even with its own allow entry {k}"
    );
    assert_ne!(
        decide_with(&e, &t, None, &probe),
        "allow",
        "probe must not ride the learned key {k}"
    );
}

/// Direct property of condition 3: a target spelled through a dangling
/// symlink is not "the real path", so it must not be classed as an
/// Allow-eligible place. Observed at 448ed546d: `s4-scratchpad` for
/// `git status > <sp>/dangm` and `s4-worktree` for `cp <sp>/payload
/// <wt>/dangm` (`dangm -> <main>/src/new.rs`).
#[test]
fn r7_dangling_symlink_target_is_not_classed_as_a_placed_class() {
    let e = env_prod("dangclass");
    fixtures(&e);
    let sp = e.sp.display();
    let wt = e.wt.display();
    let mut bad = vec![];
    for (cwd, probe) in [
        (&e.sp, format!("git status > {sp}/dangm")),
        (&e.wt, format!("cp {sp}/payload {wt}/dangm")),
        (&e.wt, format!("git status > {wt}/dangm")),
    ] {
        let s = shape(&e, &bash(cwd, &probe));
        let k = s["shape"].as_str().unwrap();
        let class = k.split('|').nth(1).unwrap_or("");
        if ["s4-scratchpad", "s4-worktree", "s4-none"].contains(&class) {
            bad.push(format!("`{probe}` => {} {k}", s["decision"]));
        }
    }
    assert!(
        bad.is_empty(),
        "a write through a dangling symlink (into the main tree) is classed as an \
         Allow-eligible place:\n{}",
        bad.join("\n")
    );
}

// ------------------------------------------------ guards (observed GREEN)

/// GUARD: the literal-only dangerous git forms of the task list are not
/// Allow under their OWN learned key (observed at 448ed546d: main-tree class,
/// a refused flag, or a detector Deny).
#[test]
fn r7_guard_literal_git_options_never_ride() {
    let e = env_prod("gitopts");
    fixtures(&e);
    let m = e.main.display();
    let sp = e.sp.display();
    let mut bad = vec![];
    for c in [
        format!("git -C {m} add -A"),
        format!("git --git-dir={m}/.git --work-tree={m} add -A"),
        format!("git --git-dir {m}/.git add -A"),
        format!("git --work-tree={m} add -A"),
        format!("git -c core.fsmonitor={sp}/bin/git commit -a"),
        "git -c alias.add=!true add -A".to_string(),
        format!("git --exec-path={sp}/bin add -A"),
        format!("git config --global core.fsmonitor {sp}/bin/git"),
        format!("git config core.fsmonitor {sp}/bin/git"),
        "git rebase -x true HEAD".to_string(),
        "git bisect run true".to_string(),
        format!("git -C {}/src/.. add -A", e.wt.display()),
    ] {
        let p = bash(&e.wt, &c);
        let k = shape(&e, &p)["shape"].as_str().unwrap().to_string();
        let got = decide_with(&e, &table_with(&e, &k), None, &p);
        if got == "allow" {
            bad.push(format!("`{c}` is Allow under its own learned key {k}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// GUARD: writes to the `BLASTGUARD_ASK_RULES` file through other tools and
/// spellings are not Allow (observed at 448ed546d: protected-config Ask, or a
/// detector Deny), including the `/tmp` spelling of `/private/tmp`.
#[test]
fn r7_guard_rules_override_writes_are_not_allow() {
    let e = env_prod("ovr7");
    fixtures(&e);
    let rules = e.sp.join("r").join("ask-rules.toml");
    let sp = e.sp.display().to_string();
    let tsp = sp.replacen("/private/tmp/", "/tmp/", 1);
    let ev = format!("{sp}/evil/ask-rules.toml");
    let mut bad = vec![];
    for c in [
        format!("install {ev} {sp}/r/ask-rules.toml"),
        format!("install {ev} {sp}/r"),
        format!("rsync {ev} {sp}/r"),
        format!("rsync -a {sp}/evil/ {sp}/r/"),
        format!("cp -R {sp}/evil/ {sp}/r"),
        format!("ditto {sp}/evil {sp}/r"),
        format!("tee {sp}/r/ask-rules.toml"),
        format!("dd if={ev} of={sp}/r/ask-rules.toml"),
        format!("ln -sf {ev} {sp}/r/ask-rules.toml"),
        format!("ln -f {ev} {sp}/r"),
        format!("truncate -s 0 {sp}/r/ask-rules.toml"),
        format!("true > {sp}/r/ask-rules.toml"),
        format!("echo >> {sp}/r/ask-rules.toml"),
        format!("sed -i '' s/a/b/ {sp}/r/ask-rules.toml"),
        format!("touch {sp}/r/ask-rules.toml"),
        format!("cp {ev} {sp}/r/./ask-rules.toml"),
        format!("cp {ev} {sp}//r/ask-rules.toml"),
        format!("cp {ev} {sp}/R/ASK-RULES.TOML"),
        format!("mv {ev} {sp}/R"),
        format!("cp {ev} {tsp}/r/ask-rules.toml"),
        format!("cp {ev} {tsp}/r"),
        format!("rm -f {tsp}/r/ask-rules.toml"),
        format!("mv {tsp}/r {sp}/r2"),
    ] {
        let p = bash(&e.wt, &c);
        let got = decide_with(&e, &no_table(&e), Some(&rules), &p);
        if got == "allow" {
            bad.push(c);
        }
    }
    assert!(
        bad.is_empty(),
        "these replace / remove the BLASTGUARD_ASK_RULES file but are Allow:\n{}",
        bad.join("\n")
    );
}

/// GUARD (false positives): eligible lines stay eligible under the gate —
/// `git add -A` / `git commit -a` in a linked worktree under HOME, and
/// `rm -rf build` in a scratchpad at its real `/private/tmp/claude-*` path
/// (relative, absolute, trailing `/`, `.` and `//` components, a
/// not-yet-existing remainder). Each Asks without a table and is Allow under
/// its own learned key.
#[test]
fn r7_guard_eligible_lines_stay_eligible() {
    let e = env_prod("fp7");
    fixtures(&e);
    let sp = e.sp.display();
    let mut bad = vec![];
    for (cwd, c) in [
        (&e.wt, "git add -A".to_string()),
        (&e.wt, "git commit -a".to_string()),
        (&e.sp, "rm -rf build".to_string()),
        (&e.sp, format!("rm -rf {sp}/build")),
        (&e.sp, format!("rm -rf {sp}/build/")),
        (&e.sp, format!("rm -rf {sp}/./build")),
        (&e.sp, format!("rm -rf {sp}//build")),
        (&e.sp, format!("rm -rf {sp}/later/x")),
    ] {
        let p = bash(cwd, &c);
        let before = decide_with(&e, &no_table(&e), None, &p);
        let k = shape(&e, &p)["shape"].as_str().unwrap().to_string();
        let own = decide_with(&e, &table_with(&e, &k), None, &p);
        if before != "ask" || own != "allow" {
            bad.push(format!("`{c}`: {before} without a table, {own} under {k}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ------------------------------------------------ detector (not askrules)

/// DETECTOR, outside askrules (no table): `git diff --output=<file>` /
/// `git log --output=<file>` write that file, but the git row is read-only
/// (`GIT_READONLY`) and the line is a plain Allow (observed at 448ed546d,
/// shape `allow|s4-none|git diff --output`) even for
/// `~/.claude/settings.json`. Control: `cp` onto it is not Allow.
#[test]
#[ignore = "detector, filed separately"]
fn r7_detector_git_output_option_overwrites_protected_config() {
    let e = env_prod("detout");
    fixtures(&e);
    let target = e.home.join(".claude").join("settings.json");
    let control = format!("cp {}/payload {}", e.sp.display(), target.display());
    assert_ne!(
        decide_with(&e, &no_table(&e), None, &bash(&e.wt, &control)),
        "allow",
        "control: `{control}`"
    );
    let mut bad = vec![];
    for c in [
        format!("git diff --output={}", target.display()),
        format!("git log --output={}", target.display()),
    ] {
        if decide_with(&e, &no_table(&e), None, &bash(&e.wt, &c)) == "allow" {
            bad.push(c);
        }
    }
    assert!(
        bad.is_empty(),
        "these overwrite ~/.claude/settings.json but are Allow:\n{}",
        bad.join("\n")
    );
}

const PROBE_RULES: &str = r#"
[[not_learnable]]
reason = "probe: never matches"
[not_learnable.match]
command_regex = 'ZZZNEVERMATCHES'
"#;

/// Measurement helper (asserts nothing): for every probe prints the verdict
/// without a table, the shape, the verdict under a table that allows the
/// probe's OWN key, and the structural gate's answer.
/// `<where> <cmd>`; where = W / M / S (cwd = worktree / main tree /
/// scratchpad); prefix H = HOME inside /private/tmp; prefix R = with the
/// rules override `$SP/r/ask-rules.toml`.
#[test]
#[ignore]
fn probe_dump() {
    for mode in ['-', 'H', 'P'] {
        let e = match mode {
            'H' => env_home_in_tmp("probe"),
            'P' => env_prod("probe"),
            _ => env("probe"),
        };
        fixtures(&e);
        let rules = e.sp.join("r").join("ask-rules.toml");
        for line in PROBES.lines().filter(|l| !l.trim().is_empty()) {
            let (where_, cmd) = line.split_once(' ').unwrap();
            let m = if where_.contains('H') {
                'H'
            } else if where_.contains('P') {
                'P'
            } else {
                '-'
            };
            if m != mode {
                continue;
            }
            let with_rules = where_.contains('R');
            let w = where_.trim_start_matches(['H', 'R', 'P']);
            let tmp_root = e
                .root
                .to_str()
                .unwrap()
                .replacen("/private/tmp/", "/tmp/", 1);
            let cmd = cmd
                .replace("\\n", "\n")
                .replace("\\0", "\0")
                .replace("$TROOT", &tmp_root)
                .replace("$ROOT", e.root.to_str().unwrap())
                .replace("$SP", e.sp.to_str().unwrap())
                .replace("$WT", e.wt.to_str().unwrap())
                .replace("$MAIN", e.main.to_str().unwrap())
                .replace("$HOMEDIR", e.home.to_str().unwrap());
            let cwd = match w {
                "M" => &e.main,
                "S" => &e.sp,
                _ => &e.wt,
            };
            let r = with_rules.then_some(rules.as_path());
            let p = bash(cwd, &cmd);
            let s = shape_with(&e, r, &p);
            let d = decide_with(&e, &no_table(&e), r, &p);
            let k = s["shape"].as_str().unwrap_or("").to_string();
            let own = decide_with(&e, &table_with(&e, &k), r, &p);
            let st = blastguard::shape::is_single_static_command(&cmd);
            let shown = cmd.replace('\n', "\\n").replace('\0', "\\0");
            println!("{where_} {shown:<70} => {d:<5} own={own:<5} static={st} {k}");
        }
    }
}

#[allow(dead_code)]
const PROBES_FIRST_PASS: &str = r#"
W git add -A
M git add -A
W git -C $MAIN add -A
W git --git-dir=$MAIN/.git --work-tree=$MAIN add -A
W git -c core.fsmonitor=$SP/bin/git commit -a
W git --exec-path=$SP/bin add -A
W git rebase -x true HEAD
W git rebase -x 'rm -rf ~' HEAD
W git rebase --exec=true HEAD
W git bisect run true
W git bisect run rm -rf $HOMEDIR
W git worktree add $SP/wt2
W git worktree add $MAIN/newtree
W git worktree add $HOMEDIR/.claude/hooks/w
W git merge -s evil HEAD
W git diff --output=$SP/o
W git diff --output=$HOMEDIR/.claude/settings.json
W git log --output=$MAIN/notes
W git status > $SP/o
W git status > $MAIN/notes
W git status > $HOMEDIR/.claude/settings.json
W git -p add -A
W git --bare add -A
W $SP/bin/git add -A
W ./git add -A
W /usr/bin/git add -A
W exec git add -A
W command git add -A
W coproc git add -A
W time git add -A
W git add -A # c
W git add \\n-A
W "git" add -A
W rm -rf =git
W rm -f =git
HW rm -rf build
HW rm -rf $WT/build
HM rm -rf build
HM rm -rf src
HM rm -rf $MAIN/src
HM touch x
HM mkdir newdir
HM cp notes notes2
HS rm -rf build
HS rm -rf $SP/build
HS rm -rf $SP/build/
HS rm -rf $SP/./build
HS rm -rf $SP//build
HS rm -rf $SP/later/x
HW $SP/bin/rm -rf build
HW rm -rf =rm
RW cp $SP/evil/ask-rules.toml $SP/r/ask-rules.toml
RW install $SP/evil/ask-rules.toml $SP/r/ask-rules.toml
RW install $SP/evil/ask-rules.toml $SP/r
RW rsync $SP/evil/ask-rules.toml $SP/r/ask-rules.toml
RW rsync $SP/evil/ask-rules.toml $SP/r
RW rsync -a $SP/evil/ $SP/r/
RW cp -R $SP/evil/ $SP/r
RW ditto $SP/evil $SP/r
RW tee $SP/r/ask-rules.toml
RW dd if=$SP/evil/ask-rules.toml of=$SP/r/ask-rules.toml
RW ln -sf $SP/evil/ask-rules.toml $SP/r/ask-rules.toml
RW ln -f $SP/evil/ask-rules.toml $SP/r/ask-rules.toml
RW ln -f $SP/evil/ask-rules.toml $SP/r
RW truncate -s 0 $SP/r/ask-rules.toml
RW echo > $SP/r/ask-rules.toml
RW echo >> $SP/r/ask-rules.toml
RW true > $SP/r/ask-rules.toml
RW sed -i '' s/a/b/ $SP/r/ask-rules.toml
RW touch $SP/r/ask-rules.toml
RW cp $SP/evil/ask-rules.toml $SP/r/./ask-rules.toml
RW cp $SP/evil/ask-rules.toml $SP//r/ask-rules.toml
RW cp $SP/evil/ask-rules.toml $SP/r/ask-rules.toml/
RW cp $SP/evil/ask-rules.toml $SP/R/ASK-RULES.TOML
RW mv $SP/evil/ask-rules.toml $SP/R
RW mv $SP/r $SP/r2
RW mv $SP $ROOT/sp2
RW rm -rf $SP
RW mv $SP/evil/ask-rules.toml $SP/r/
"#;

const PROBES: &str = r#"
PS git status > $SP/payload
PS git status > $SP/newout
PS echo x > $SP/payload
PS cat $SP/payload > $SP/dangm
PW git status > notes
PW git status > $WT/build/o
PM git status > notes
PM git status > src/lib.rs
PM echo x > notes
PW echo x > git
PM rm -rf build
PS rm -rf $SP/dangm
PS cp $SP/payload $SP/dangm
PS cp -f $SP/payload $SP/dangm
PS git status > $SP/dangle
PS git status > $SP/build
"#;

#[allow(dead_code)]
const PROBES_THIRD_PASS: &str = r#"
PS $SP/bin/rm -rf build
PS ./bin/rm -rf build
PS rm -rf =rm
PS rm -rf =git
PM rm -rf .git
PW rm -rf .git
PM rm -rf .
PW rm -rf .
PM rm -rf $MAIN
PW rm -rf $WT
PM rm -rf src/lib.rs
PM git clean -fdx
PM git reset --hard
PW git reset --hard
PW git clean -fdx
PM git stash drop
PW git stash drop
PM rm -rf build/
PW git status > $WT/dangm
PS git status > $SP/dangm
PW cp $SP/payload ../repo/x
"#;

#[allow(dead_code)]
const PROBES_SECOND_PASS: &str = r#"
PW git add -A
PW git commit -a
PM git add -A
PW rm -rf build
PW rm -rf $WT/build
PM rm -rf build
PM rm -rf src
PM rm -rf $MAIN/src
PM rm -f notes
PM mv notes notes2
PM cp $SP/payload src/lib.rs
PM touch src/lib.rs
PS rm -rf build
PS rm -rf $SP/build
PS rm -rf $TROOT/scratchpad/build
PW $SP/bin/git add -A
PW ./git add -A
PW $SP/bin/git commit -a
PW rm -rf =git
PW rm -rf =rm
PW rm -rf =cargo
PW rm -f =git
PW cp $SP/payload dangle
PW cp $SP/payload $WT/dangle
PW cp $SP/payload $WT/dangm
PW touch $WT/dangle
PW mkdir -p $WT/dangle
PS cp $SP/payload $SP/dangle
PS cp $SP/payload $SP/dangm
PS touch $SP/dangle
PS mv $SP/payload $SP/dangle
PW git add -A > $WT/dangle
PW git add -A 3> $HOMEDIR/.zshrc
PW git add -A &> $HOMEDIR/.zshrc
PW git add -A >> $HOMEDIR/.zshrc
PW rm -rf build 3> $MAIN/notes
PW git add -A\nrm -rf $HOMEDIR
PW git add -A # x\nrm -rf $HOMEDIR
PW git add -A;
PW git add -A\0 rm -rf $HOMEDIR
PW git add -A ''
PW '' git add -A
PW git add -A '-f'
PW g'i't add -A
PW git -C $WT add -A
PW git -C $TROOT/home add -A
PW git -C . add -A
PW git -C src/.. add -A
RPW cp $SP/evil/ask-rules.toml $TROOT/scratchpad/r/ask-rules.toml
RPW cp $SP/evil/ask-rules.toml $TROOT/scratchpad/r
RPW rm -f $TROOT/scratchpad/r/ask-rules.toml
RPW mv $TROOT/scratchpad/r $SP/r2
RPW cp $SP/evil/ask-rules.toml $SP/r/ask-rules.toml
"#;
