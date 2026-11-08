// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro of a misleading blastguard reason observed 2026-11-07: a
//! `cd <worktree> && python3 - <<'EOF' … EOF` program whose text contains a
//! Python string literal holding
//! `shutil.copy2(f.warm / "Cargo.lock", new / "Cargo.lock")` was refused with
//!
//! ```text
//! `python3 - <<'EOF'` takes its program from stdin, and it writes `/`
//! (`shutil.copy2(`) — it exists, and it is not inside a git work tree —
//! nothing holds a second copy of these bytes
//! ```
//!
//! Measured at 5d2a61e3 the main-session verdict was Deny, from the
//! unrecoverable-write arm, not an R2 Ask — and that Deny came only from the
//! misread `/` (see the re-scoped control below, user ruling 2026-11-08). The
//! defect is the REASON: `/` there is Python's path-join operator
//! (`new / "Cargo.lock"`), not the filesystem root, and the reason then states
//! facts about `/` ("not inside a git work tree") that are not facts about
//! anything this program writes. The human deciding the Ask is sent to the
//! wrong conclusion.
//!
//! Produced at crates/blastguard/src/detect/interpreter.rs
//! (`"{shape}, and it writes `{candidate}` (`{call}`) — {why}"`, the
//! recoverability probe over the program's string literals), with `{why}`
//! from crates/blastguard/src/reversible.rs `decide_recovery` (`NotRepo` row).
//!
//! Target: the reason does not claim the write target is `/`; with that
//! fixed the program is Allow (control, user ruling 2026-11-08). Control: a genuine literal write onto an
//! existing system file through `shutil.copy2` still names that path.
//!
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The misresolved target as it appears in the observed reason.
const ROOT_TARGET: &str = "it writes `/` ";

fn fresh_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-pyjoin-reason-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("scripts")).expect("mkdir");
    let mut git = Command::new("git");
    git.arg("init").arg("-q").arg(&dir);
    let out =
        bounded::run_bounded_stdin(&mut git, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(out.status.success(), "git init failed");
    std::fs::write(dir.join("scripts/x.py"), "print(1)\n").expect("write fixture");
    dir
}

/// (decision, reason) for `command` judged as a main-session Bash call.
/// Empty stdout is Allow.
fn verdict(cwd: &Path, command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": cwd.to_string_lossy(),
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.current_dir(cwd)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 (cmd: {command:?}); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let decision = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}, got: {stdout}"))
        .to_string();
    let reason = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (decision, reason)
}

/// The observed shape, minimized from the logged command (subagent
/// a89c1f0770f4f4a54, tool_use toolu_017HytjXX8aCZ5hbfgre11jV, 6692 bytes; a
/// line-level delta-debug kept the apostrophe-bearing docstring lines, the
/// `shutil.copy2(...)` line and the `new / "target" / "debug" / "app"` line).
/// The heredoc program appends a test snippet held in a `'''` string.
///
/// Measured 2026-11-07 at 5d2a61e3 (main session, cwd = a git work tree):
/// this exact shape is DENIED with the observed reason. Mechanism (read, not
/// proven by this file): `quoted_string_literals` pairs quotes sequentially,
/// so the apostrophe in `cargo's` and the odd `"` count of the `"""` docstring
/// shift the pairing until `" / "` (between `"target"` and `"debug"`) is read
/// as a literal; trimmed, it is `/`, which the recoverability probe resolves
/// to the filesystem root.
///
/// Without the docstring line the same program is Allow (measured), so the
/// docstring is load-bearing for the repro.
fn observed_command(repo: &Path) -> String {
    let r = repo.to_string_lossy();
    format!(
        "cd {r} && python3 - <<'EOF'\n\
p=\"{r}/scripts/x.py\"\n\
s=open(p).read()\n\
tests = '''\n\
        \"\"\"Fails while cargo's lock is held.\"\"\"\n\
        shutil.copy2(f.warm / \"Cargo.lock\", new / \"Cargo.lock\")\n\
        self.assertTrue((new / \"target\" / \"debug\" / \"app\").exists())\n\
'''\n\
open(p,\"w\").write(s + tests)\n\
EOF"
    )
}

/// Re-scoped by user ruling 2026-11-08 (backlog 8bb1cec8). This control used
/// to require a refusal, but the only refusal this program ever earned was the
/// misread `" / "` literal (the defect above). With that fixed, the program's
/// one real write is `open(p,"w")` onto `scripts/x.py`, an untracked file
/// inside a git work tree, which the 2026-09-18 recoverability ruling counts as
/// recoverable (`reversible::decide_recovery`). The ruling: Allow is the
/// correct verdict here.
#[test]
fn path_join_copy2_program_writing_only_a_worktree_file_is_allowed() {
    let repo = fresh_repo("ctl-decision");
    let (d, r) = verdict(&repo, &observed_command(&repo));
    assert_eq!(
        d, "allow",
        "the program writes only an untracked file inside a work tree (recoverable per the \
2026-09-18 ruling), so it is Allow (user ruling 2026-11-08); got {d:?}: {r}"
    );
}

/// Target (RED at e346bb04): the reason must not name `/` — the path-join
/// operator — as the write target and assert recoverability facts about it.
#[test]
fn path_join_slash_is_not_reported_as_write_target() {
    let repo = fresh_repo("target");
    let (d, r) = verdict(&repo, &observed_command(&repo));
    assert!(
        !r.contains(ROOT_TARGET),
        "the reason names `/` (Python's path-join operator, e.g. `new / \"target\" / \"debug\"`) as the \
write target and states facts about the filesystem root; decision {d:?}, reason: {r}"
    );
}

/// Control (must pass at HEAD): a genuine literal `shutil.copy2` onto an
/// existing system file is still refused and its reason names that path.
/// `/etc/hosts` is used rather than a non-existent `/etc/x`: measured
/// 2026-11-07 at 5d2a61e3, `shutil.copy2("<repo>/scripts/x.py", "/etc/x")` in
/// the same heredoc shape is ALLOW (`reversible::decide_recovery` answers
/// `NothingToDestroy` for a path that does not exist), so it names nothing.
#[test]
fn literal_system_path_copy2_target_is_named() {
    let repo = fresh_repo("ctl-literal");
    let r_ = repo.to_string_lossy();
    let cmd = format!(
        "cd {r_} && python3 - <<'EOF'\n\
import shutil\n\
shutil.copy2(\"{r_}/scripts/x.py\", \"/etc/hosts\")\n\
EOF"
    );
    let (d, r) = verdict(&repo, &cmd);
    assert!(
        (d == "ask" || d == "deny") && r.contains("`/etc/hosts`"),
        "a literal copy2 onto /etc/hosts must be refused naming it; got {d:?}: {r}"
    );
}
