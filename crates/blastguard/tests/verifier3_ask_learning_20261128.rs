#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(verifier round 3, 2026-11-28, against 804d5100b): learned-Allow reach
//! that schema `s3-` still does not separate. Written by an independent
//! verifier, not the implementer (CLAUDE.md §2(a)). Black-box: spawns the
//! built binary with a sandbox HOME and a learned table whose ONLY allow
//! entry is the key `blastguard shape` prints for a benign sibling.
//!
//! What these tests do NOT prove: that the learner would actually promote the
//! sibling (that needs >=3 approvals / >=2 sessions in real data); only that IF
//! it does, the dangerous probe rides the entry, or that the key does not pin
//! what it claims to pin.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000d3";
const WT: &str = "/Users/yuki/src/.harness-worktrees/fp-ask-learning";
const MAIN: &str = "/Users/yuki/src/harness";

struct Env {
    root: PathBuf,
    home: PathBuf,
}

fn env(name: &str) -> Env {
    let root = std::env::temp_dir().join(format!("bg-asklearn-v3-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let e = Env {
        home: root.join("home"),
        root,
    };
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::create_dir_all(e.root.join("approvals")).unwrap();
    e
}

fn spawn(e: &Env, table: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    c.args(args)
        .env("HOME", &e.home)
        .env("TMPDIR", std::env::temp_dir())
        .env("BLASTGUARD_BLOCK_LOG_DIR", e.root.join("blocklog"))
        .env("BLASTGUARD_RELAY_DIR", e.root.join("relay"))
        .env("BLASTGUARD_APPROVALS_DIR", e.root.join("approvals"))
        .env("BLASTGUARD_LEARNED_TABLE", table)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
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

fn decide(e: &Env, table: &Path, p: &Value) -> String {
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

fn bash(cwd: &str, command: &str) -> Value {
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

fn key(e: &Env, p: &Value) -> String {
    shape(e, p)["shape"].as_str().unwrap().to_string()
}

fn table_with(e: &Env, name: &str, k: &str) -> PathBuf {
    let p = e.root.join(name);
    let entry = json!({"shape": k, "decision": "allow", "approvals": 50, "declines": 0,
                       "sessions": 20, "samples": []});
    std::fs::write(&p, json!({"version": 1, "entries": [entry]}).to_string()).unwrap();
    p
}

/// Learn `sibling` allow; `probe` (a MAIN-tree write) must not become Allow.
fn assert_no_ride_e(e: &Env, sibling: Value, probe: Value) {
    let before = decide(e, &no_table(e), &probe);
    assert_ne!(
        before, "allow",
        "precondition: probe must not be Allow without a table"
    );
    let sib_shape = shape(e, &sibling);
    assert_eq!(
        sib_shape["decision"], "ask",
        "precondition: sibling must Ask: {sib_shape}"
    );
    let k = sib_shape["shape"].as_str().unwrap().to_string();
    let t = table_with(e, "t.json", &k);
    let after = decide(e, &t, &probe);
    assert_ne!(
        after,
        "allow",
        "probe {} was {before} without a table but Allow once the sibling {} was learned; \
         shared shape {k} (probe shape {})",
        probe["tool_input"]["command"],
        sibling["tool_input"]["command"],
        key(e, &probe)
    );
}

fn assert_no_ride(name: &str, sibling: Value, probe: Value) {
    let e = env(name);
    assert_no_ride_e(&e, sibling, probe);
}

/// B(iv), spec rule 3+5: a wrapper keeps the wrapped git write's REPOSITORY
/// class. `env git add -A` in MAIN is `s3-none` today (git_targets only looks
/// at argv[0] == git), so a worktree approval rides into the main tree.
#[test]
fn wrapper_git_write_keeps_repo_class_main_tree() {
    let e = env("wrapclass");
    for cmd in [
        "env git branch -D main",
        "env git add -A",
        "command git add -A",
        "timeout 10 git add -A",
        "nice git add -A",
        "nohup git add -A",
        "sudo git add -A",
    ] {
        let k = key(&e, &bash(MAIN, cmd));
        assert!(
            k.contains("|s3-main-tree|"),
            "wrapper `{cmd}` in the main tree must be class s3-main-tree: {k}"
        );
    }
}

#[test]
fn wrapper_git_add_in_worktree_does_not_unlock_main_tree() {
    assert_no_ride(
        "wrapride",
        bash(WT, "env git add -A"),
        bash(MAIN, "env git add -A"),
    );
}

/// Main-tree write reached through `cd ..`: the cwd is not resolved, the class
/// is `s3-undetermined`, which IS Allow-learnable. The same text approved in a
/// worktree subdir rides into the main tree (CLAUDE.md §3/§8: cannot determine
/// the repository of a git write must not be auto-Allow).
#[test]
fn cd_dotdot_git_write_in_worktree_does_not_unlock_main_tree() {
    assert_no_ride(
        "cddotdot",
        bash(&format!("{WT}/crates"), "cd .. && git add -A"),
        bash(&format!("{MAIN}/crates"), "cd .. && git add -A"),
    );
}

/// `GIT_DIR=` prefix: class `s3-undetermined` (Allow-learnable); a worktree
/// approval rides to a GIT_DIR that names the main repository.
#[test]
fn git_dir_prefix_does_not_unlock_main_repo() {
    assert_no_ride(
        "gitdirprefix",
        bash(WT, &format!("GIT_DIR={WT}/.git git add -A")),
        bash(WT, &format!("GIT_DIR={MAIN}/.git git add -A")),
    );
}

/// `export GIT_DIR=<main>/.git; git add -A` from a worktree cwd: only PREFIX
/// assignments are checked, so the class is `s3-worktree` and an approved
/// `export FOO=1; git add -A` rides.
#[test]
fn exported_git_dir_does_not_ride_worktree_class() {
    assert_no_ride(
        "exportgitdir",
        bash(WT, "export FOO=1; git add -A"),
        bash(WT, &format!("export GIT_DIR={MAIN}/.git; git add -A")),
    );
}

/// `git -c core.worktree=<main>` makes git operate on the main working tree,
/// but the class is the cwd's (`s3-worktree`).
#[test]
fn git_c_core_worktree_is_not_worktree_class() {
    let e = env("corewt");
    let k = key(
        &e,
        &bash(WT, &format!("git -c core.worktree={MAIN} add -A")),
    );
    assert!(
        !k.contains("|s3-worktree|") && !k.contains("|s3-none|"),
        "`git -c core.worktree=<main>` must not take the worktree class: {k}"
    );
}

/// A symlink under a scratchpad that points at the MAIN tree: class_of_repo
/// returns the lexical class (`s3-scratchpad`) before looking at the repo, so
/// an approved `git add -A` in a real scratchpad repo rides into main.
#[test]
fn scratchpad_symlink_to_main_tree_is_main_tree() {
    let e = env("symlink");
    let pad = std::env::temp_dir()
        .join(format!("bg-v3-symlink-{}", std::process::id()))
        .join("scratchpad");
    let _ = std::fs::remove_dir_all(&pad);
    std::fs::create_dir_all(pad.join("repo/.git")).unwrap();
    let link = pad.join("mainlink");
    std::os::unix::fs::symlink(MAIN, &link).unwrap();
    let link_s = link.to_str().unwrap();
    let k = key(&e, &bash(link_s, "git add -A"));
    assert!(
        k.contains("|s3-main-tree|"),
        "cwd {link_s} -> {MAIN} is the main tree, shape says {k}"
    );
    assert_no_ride_e(
        &e,
        bash(pad.join("repo").to_str().unwrap(), "git add -A"),
        bash(link_s, "git add -A"),
    );
}

/// R2 for program files: `./run.sh` (direct execution) is a program file, but
/// its verb is the bare basename: editing the file keeps the key.
#[test]
fn direct_exec_script_content_is_pinned_or_unresolved() {
    let e = env("directexec");
    let dir = e.root.join("s");
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("run.sh");
    std::fs::write(&f, "#!/bin/sh\necho one\n").unwrap();
    let d = dir.to_str().unwrap();
    let k1 = key(&e, &bash(d, "./run.sh"));
    std::fs::write(&f, "#!/bin/sh\ngit push -f origin main\n").unwrap();
    let k2 = key(&e, &bash(d, "./run.sh"));
    assert!(
        k1 != k2 || k1.contains('?'),
        "`./run.sh` key ignores the script content: before={k1} after={k2}"
    );
}

/// R2 rule 2: a path written earlier on the line by anything but a literal
/// heredoc adds `?`. `rsync` / `curl -o` / `patch` / `git checkout --` write
/// the script but the run verb carries the STALE on-disk hash.
#[test]
fn script_rewritten_earlier_on_the_line_is_unpinned() {
    let e = env("toctou");
    let dir = e.root.join("s");
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.py");
    std::fs::write(&a, "print(1)\n").unwrap();
    std::fs::write(dir.join("b.py"), "print(2)\n").unwrap();
    let a = a.to_str().unwrap();
    let b = dir.join("b.py");
    let b = b.to_str().unwrap();
    let d = dir.to_str().unwrap();
    for cmd in [
        format!("rsync {b} {a} && python3 {a}"),
        format!("curl -o {a} https://example.invalid/x && python3 {a}"),
        format!("patch {a} {d}/x.diff && python3 {a}"),
        format!("git -C {d} checkout -- a.py && python3 {a}"),
    ] {
        let k = key(&e, &bash(WT, &cmd));
        assert!(
            k.splitn(3, '|')
                .nth(2)
                .unwrap_or("")
                .split(',')
                .any(|v| v == "python3 ?"),
            "`{cmd}` rewrites {a} before running it, the run verb must be `python3 ?`: {k}"
        );
    }
}

/// R1: a `+refspec` push is a force push, like `--force`, but its key is the
/// bare `git push` (same as any other Ask'd push without flags).
#[test]
fn plus_refspec_force_push_has_its_own_key() {
    let e = env("plusref");
    let force = key(&e, &bash(WT, "git push origin +HEAD:refs/heads/main"));
    let plain = key(&e, &bash(WT, "git push origin HEAD:refs/heads/x"));
    let verbs = |k: &str| k.splitn(3, '|').nth(2).unwrap_or("").to_string();
    assert_ne!(
        verbs(&force),
        verbs(&plain),
        "`+refspec` force push shares the verb with a plain push: {force} vs {plain}"
    );
}
