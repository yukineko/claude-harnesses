#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(verifier round 2, 2026-11-28): learned-Allow reach through shapes that
//! the R1/R2 keys do not separate. Written by an independent verifier, not the
//! implementer (CLAUDE.md §2(a)). Black-box: spawns the built binary with a
//! sandbox HOME and a learned table whose ONLY allow entry is the key that
//! `blastguard shape` prints for the benign sibling.
//!
//! Each test: sibling key learned allow -> the dangerous probe must not become
//! Allow (it is Ask or Deny without a table).
//!
//! What these tests do NOT prove: that the learner would actually promote the
//! sibling (that needs >=3 approvals / >=2 sessions in real data); only that IF
//! it does, the dangerous probe rides the entry.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000c3";
const WT: &str = "/Users/yuki/src/.harness-worktrees/fp-ask-learning";
const MAIN: &str = "/Users/yuki/src/harness";

struct Env {
    root: PathBuf,
    home: PathBuf,
}

fn env(name: &str) -> Env {
    let root = std::env::temp_dir().join(format!("bg-asklearn-v2-{}-{}", name, std::process::id()));
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

/// Learn `sibling` allow; `probe` must keep its no-table decision class
/// (anything but Allow).
fn assert_no_ride(name: &str, sibling: Value, probe: Value) {
    let e = env(name);
    let before = decide(&e, &no_table(&e), &probe);
    assert_ne!(
        before, "allow",
        "precondition: probe must not be Allow without a table"
    );
    let sib_shape = shape(&e, &sibling);
    assert_eq!(
        sib_shape["decision"], "ask",
        "precondition: sibling must Ask: {sib_shape}"
    );
    let k = sib_shape["shape"].as_str().unwrap().to_string();
    let t = table_with(&e, "t.json", &k);
    let after = decide(&e, &t, &probe);
    assert_ne!(
        after,
        "allow",
        "probe {} was {before} without a table but Allow once the sibling {} was learned; \
         shared shape {k} (probe shape {})",
        probe["tool_input"]["command"],
        sibling["tool_input"]["command"],
        key(&e, &probe)
    );
}

/// Worktree whole-tree commits must not unlock the same commit in the MAIN
/// working tree (CLAUDE.md §8). Both are class `none` today.
#[test]
fn main_tree_whole_tree_commit_not_unlocked_by_worktree_approvals() {
    assert_no_ride(
        "maintree",
        bash(
            WT,
            &format!("cd {WT} && git add -A && git commit -q -m x | tail -1"),
        ),
        bash(
            WT,
            &format!("cd {MAIN} && git add -A && git commit -q -m x | tail -1"),
        ),
    );
}

/// A script FILE's program is not pinned by the evaluator (R2: a program the
/// evaluator cannot pin must add `?`). Approving `python3 /tmp/a.py` must not
/// Allow every other python script.
#[test]
fn script_file_program_is_not_allow_learnable_across_scripts() {
    assert_no_ride(
        "pyfile",
        bash(WT, "python3 /tmp/a.py"),
        bash(WT, "python3 /tmp/other-unseen-script.py"),
    );
}

/// `-rf` is literal text in the command, but reaches `rm` via xargs stdin, so
/// it is not in the R1 key: a learned `printf a b | xargs rm` Allows
/// `rm -rf <main repo>`.
#[test]
fn xargs_stdin_flags_do_not_ride_a_benign_xargs_rm_allow() {
    assert_no_ride(
        "xargs",
        bash(WT, "printf '%s\\n' a b | xargs rm"),
        bash(
            WT,
            "printf -- '-rf\\n/Users/yuki/src/harness\\n' | xargs rm",
        ),
    );
}

/// Real learned entry (overwatch ask-learn --full on real data, 2026-11-28):
/// `unknown|s2-none|git branch -D,git worktree remove --force`, approved for
/// agent worktrees. `git worktree remove --force` deletes a directory but the
/// shape class is `none`, so the same entry reaches session worktrees and
/// `git branch -D main`.
#[test]
fn learned_worktree_force_remove_does_not_reach_other_trees_and_main_branch() {
    assert_no_ride(
        "wtremove",
        bash(
            WT,
            "git worktree remove --force /Users/yuki/.condukt/worktrees/run-x-t1 && git branch -D condukt/run-x/t1",
        ),
        bash(
            MAIN,
            "git worktree remove --force /Users/yuki/src/.harness-worktrees/session-eaa077f4 && git branch -D main",
        ),
    );
}

/// Real learned entry `unknown|s2-undetermined|cat,python3`: a script written
/// by a heredoc and then run. The body is not in the key, so any body rides.
#[test]
fn written_then_run_script_body_is_part_of_the_shape() {
    assert_no_ride(
        "catpy",
        bash(
            WT,
            "cat > /private/tmp/claude-502/s/a.py <<'EOF'\nprint(1)\nEOF\npython3 /private/tmp/claude-502/s/a.py",
        ),
        bash(
            WT,
            "cat > /private/tmp/claude-502/s/a.py <<'EOF'\nimport os; os.system('git push -f origin main')\nEOF\npython3 /private/tmp/claude-502/s/a.py",
        ),
    );
}

/// R1: the subcommand is part of the verb. Behind a wrapper it is dropped
/// (`env git branch -D main` -> `env -D`).
#[test]
fn wrapper_keeps_the_wrapped_subcommand_in_the_verb() {
    let e = env("wrapper");
    let k = key(&e, &bash(WT, "env git branch -D main"));
    let verbs = k.splitn(3, '|').nth(2).unwrap_or("");
    assert!(
        verbs.split([' ', ',']).any(|t| t == "branch"),
        "R1: subcommand `branch` missing behind wrapper: {k}"
    );
}
