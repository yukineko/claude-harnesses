#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(verifier round 4, 2026-11-28, against fc872a50d): learned-Allow reach
//! that the round-4 `s3-` keys still do not separate. Written by an independent
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

const SID: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000d4";
const WT: &str = "/Users/yuki/src/.harness-worktrees/fp-ask-learning";
const MAIN: &str = "/Users/yuki/src/harness";

struct Env {
    root: PathBuf,
    home: PathBuf,
}

fn env(name: &str) -> Env {
    let root = std::env::temp_dir().join(format!("bg-asklearn-v4-{}-{}", name, std::process::id()));
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

/// Learn `sibling` allow; `probe` must not become Allow.
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

/// A script directory with `run.sh` (`#!/bin/sh`) and `sub/run.sh`.
fn scripts(e: &Env) -> PathBuf {
    let d = e.root.join("s");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("run.sh"), "#!/bin/sh\necho one\n").unwrap();
    std::fs::write(d.join("sub/run.sh"), "#!/bin/sh\necho two\n").unwrap();
    d
}

/// Spec r4 rule 1/2: `env -C <dir>` (macOS env: `-C altwd`, observed
/// `env -C / pwd` -> `/`) changes the directory git runs in. The key takes the
/// hook cwd's class (`s3-worktree`), so an approved `env -C <wt>/crates git
/// add -A` rides to `env -C <main> git add -A`.
#[test]
fn env_chdir_git_write_does_not_unlock_main_tree() {
    assert_no_ride(
        "envchdir",
        bash(WT, &format!("env -C {WT}/crates git add -A")),
        bash(WT, &format!("env -C {MAIN} git add -A")),
    );
}

/// `env -S "<cmd>"` (macOS env splits the string and runs it; observed
/// `env -S "echo hi"` -> `hi`): key `env -S`, class `s3-none`, no body hash
/// and no `?`. A worktree approval rides into the main tree.
#[test]
fn env_split_string_git_write_does_not_unlock_main_tree() {
    assert_no_ride(
        "envsplit",
        bash(WT, "env -S \"git add -A\""),
        bash(MAIN, "env -S \"git add -A\""),
    );
}

/// `bash -c "<git write>"`: class `s3-undetermined` (Allow-learnable) and the
/// body hash is the same text in every cwd; spec r4 rule 2 says a git write
/// whose repository is undetermined is never Allow-learnable.
#[test]
fn shell_c_git_write_does_not_unlock_main_tree() {
    assert_no_ride(
        "bashc",
        bash(WT, "bash -c \"git add -A\""),
        bash(MAIN, "bash -c \"git add -A\""),
    );
}

/// `find -exec git add -A \;`: key `find -A -a -c ...` (no git verb at all),
/// class `s3-none`; a worktree approval rides into the main tree.
#[test]
fn find_exec_git_write_does_not_unlock_main_tree() {
    assert_no_ride(
        "findexec",
        bash(WT, "find . -maxdepth 0 -exec git add -A \\;"),
        bash(MAIN, "find . -maxdepth 0 -exec git add -A \\;"),
    );
}

/// Spec r4 rule 6 (directly executed scripts carry the content hash) behind a
/// wrapper: `timeout 5 ./run.sh` is keyed `timeout` alone, so it neither pins
/// the content nor tells `./run.sh` from `./sub/run.sh`.
#[test]
fn wrapped_direct_script_is_pinned_or_unresolved() {
    let e = env("wrapscript");
    let d = scripts(&e);
    let ds = d.to_str().unwrap();
    for w in ["timeout 5", "nohup", "nice", "command", "env", "exec"] {
        let k1 = key(&e, &bash(ds, &format!("{w} ./run.sh")));
        let other = key(&e, &bash(ds, &format!("{w} ./sub/run.sh")));
        std::fs::write(d.join("run.sh"), "#!/bin/sh\ngit push -f origin main\n").unwrap();
        let k2 = key(&e, &bash(ds, &format!("{w} ./run.sh")));
        std::fs::write(d.join("run.sh"), "#!/bin/sh\necho one\n").unwrap();
        assert!(
            k1.contains('?') || (k1 != k2 && k1 != other),
            "`{w} ./run.sh` key does not pin the script: before={k1} after-edit={k2} \
             other-script={other}"
        );
    }
}

/// `sh -c ./run.sh`: the body hash is of the TEXT `./run.sh`, not of the
/// script it runs; editing run.sh keeps the key.
#[test]
fn shell_c_script_path_is_pinned_or_unresolved() {
    let e = env("shcpath");
    let d = scripts(&e);
    let ds = d.to_str().unwrap();
    let k1 = key(&e, &bash(ds, "sh -c ./run.sh"));
    std::fs::write(d.join("run.sh"), "#!/bin/sh\ngit push -f origin main\n").unwrap();
    let k2 = key(&e, &bash(ds, "sh -c ./run.sh"));
    assert!(
        k1 != k2 || k1.contains('?'),
        "`sh -c ./run.sh` key ignores the script content: before={k1} after={k2}"
    );
}

/// Interpreters not in the r4 list: `fish <file>` / `pwsh [-File] <file>` key
/// is the bare interpreter (`fish`, `pwsh`, `pwsh -F -e -i -l`) with no hash
/// and no `?`, even for a file that does not exist.
#[test]
fn other_shell_interpreter_files_are_pinned_or_unresolved() {
    let e = env("otherinterp");
    let d = e.root.join("s");
    std::fs::create_dir_all(&d).unwrap();
    let ds = d.to_str().unwrap();
    for (file, cmd) in [
        ("a.fish", "fish a.fish"),
        ("a.ps1", "pwsh a.ps1"),
        ("a.ps1", "pwsh -File a.ps1"),
    ] {
        std::fs::write(d.join(file), "echo 1\n").unwrap();
        let k1 = key(&e, &bash(ds, cmd));
        std::fs::write(d.join(file), "rm -rf ~\n").unwrap();
        let k2 = key(&e, &bash(ds, cmd));
        assert!(
            k1 != k2 || k1.contains('?'),
            "`{cmd}` key ignores the program file: before={k1} after={k2}"
        );
    }
}
