// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED tests written by an agent that did not write the fixes (CLAUDE.md §2(a)).
//!
//! Three backlog items, one file:
//!
//!   * d5613105 — `cp`/`mv`/`install` whose DESTINATION is a system directory
//!     must answer exactly as a redirect into that path does. 0.2.60 wired
//!     `scope::is_inside_system_dir` into the redirect loops only; the copy/move
//!     rule (`analyze_copy_move`) consults `protected_path_block` and
//!     `protected_glob_deny` and nothing else. A mirror gap.
//!   * 9271d739 — `rm -rf <home>/.claude/plugins` and `<home>/.claude/projects`
//!     are Allow (ALLOW_GLOBS `**/.claude/**` swallows them) while
//!     `rm -rf <home>/.claude` is denied. Deleting the deployed gate binaries
//!     darkens every gate; deleting projects destroys transcripts and memory.
//!   * 70883137 — a panic on the verdict path outside `analyse` reaches
//!     `hook::run_hook`, which exits 0 with nothing on stdout: an allow.
//!
//! Every RED test is paired with a control that already passes, so a RED result
//! cannot be "the harness is broken".

use blastguard::detect;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;
use std::io::Write;
use std::process::{Command, Stdio};

const PROJECT: &str = "/home/yuki/proj";
const HOME: &str = "/home/yuki";

fn identity(p: &str) -> Option<String> {
    Some(p.to_string())
}

fn roots() -> SafeRoots {
    SafeRoots::new(
        Some(PROJECT),
        Some(PROJECT),
        Some(HOME),
        None,
        Some(identity),
    )
}

fn scoped(cmd: &str) -> Decision {
    detect::detect_scoped("Bash", Some(&json!({ "command": cmd })), &roots())
}

fn unscoped(cmd: &str) -> Decision {
    detect::detect("Bash", Some(&json!({ "command": cmd })))
}

fn restrictiveness(d: &Decision) -> u8 {
    match d {
        Decision::Allow => 0,
        Decision::Ask(_) => 1,
        Decision::Deny(_) => 2,
    }
}

// ------------------------------------------------------------ d5613105 ----

/// CONTROL (already correct): the redirect spelling of a system-directory write
/// is stopped. If this fails, the RED test below proves nothing.
#[test]
fn control_redirect_into_system_dirs_is_stopped() {
    for cmd in [
        "echo x > /etc/paths.d/zz",
        "echo x > /usr/local/bin/x",
        "echo x > /Library/LaunchDaemons/e.plist",
    ] {
        let d = scoped(cmd);
        assert!(
            d.is_blocking(),
            "control: `{cmd}` must be stopped; got {d:?}"
        );
    }
}

/// CONTROL: a copy into an ordinary in-project destination stays Allow, so the
/// fix cannot be "block every cp".
#[test]
fn control_copy_into_the_project_is_still_allowed() {
    for cmd in [
        "cp a.txt /home/yuki/proj/ok.txt",
        "mv a.txt /home/yuki/proj/ok.txt",
        "install -m644 a.txt /home/yuki/proj/ok.txt",
    ] {
        let d = scoped(cmd);
        assert!(
            matches!(d, Decision::Allow),
            "control: `{cmd}` must stay Allow; got {d:?}"
        );
    }
}

/// RED: cp/mv/install into a system directory is stopped, in the scoped hook
/// entry AND the location-blind library entry.
#[test]
fn copy_move_install_into_a_system_dir_is_stopped() {
    for cmd in [
        "cp evil /etc/paths.d/zz",
        "install -m755 evil /usr/local/bin/x",
        "mv evil /Library/LaunchDaemons/e.plist",
        "cp -t /etc/paths.d evil",
        "cp evil /usr/local/bin/",
    ] {
        for (entry, d) in [("scoped", scoped(cmd)), ("unscoped", unscoped(cmd))] {
            assert!(
                d.is_blocking(),
                "`{cmd}` ({entry}) writes into a system directory and must be \
                 Ask/Deny like the redirect; got {d:?}"
            );
        }
    }
}

/// RED, stated as the backlog item states it: same verdict as the redirect into
/// the same path (never weaker).
#[test]
fn copy_verdict_is_not_weaker_than_the_redirect_into_the_same_path() {
    for path in [
        "/etc/paths.d/zz",
        "/usr/local/bin/x",
        "/Library/LaunchDaemons/e.plist",
    ] {
        let redirect = scoped(&format!("echo x > {path}"));
        for verb in ["cp evil", "mv evil", "install -m755 evil"] {
            let d = scoped(&format!("{verb} {path}"));
            assert!(
                restrictiveness(&d) >= restrictiveness(&redirect),
                "`{verb} {path}` = {d:?} is weaker than the redirect = {redirect:?}"
            );
        }
    }
}

// ------------------------------------------------------------ 9271d739 ----

/// CONTROL (already correct): deleting the Claude home directory itself is
/// denied, and so is the project's own `.githooks`.
#[test]
fn control_rm_rf_claude_home_and_githooks_are_denied() {
    for cmd in [
        "rm -rf /home/yuki/.claude",
        "rm -rf /home/yuki/proj/.githooks",
    ] {
        let d = scoped(cmd);
        assert!(d.is_deny(), "control: `{cmd}` must be Deny; got {d:?}");
    }
}

/// RED: the deployed-plugin and transcript subtrees of the Claude home are not
/// covered by the config exemption.
#[test]
fn rm_rf_claude_home_plugins_and_projects_is_stopped() {
    for cmd in [
        "rm -rf /home/yuki/.claude/plugins",
        "rm -rf /home/yuki/.claude/projects",
        "rm -rf /home/yuki/.claude/plugins/cache",
        "rm -rf /home/yuki/.claude/projects/-home-yuki-proj",
    ] {
        for (entry, d) in [("scoped", scoped(cmd)), ("unscoped", unscoped(cmd))] {
            assert!(
                d.is_blocking(),
                "`{cmd}` ({entry}) deletes deployed gate binaries / transcripts \
                 and must be Ask/Deny; got {d:?}"
            );
        }
    }
}

// ------------------------------------------------------------ 70883137 ----

/// Spawn the real hook binary with a verdict-worthy payload whose STDOUT reader
/// is already gone. `println!` on a closed pipe panics (SIGPIPE is ignored by
/// the Rust runtime), and that print sits on the verdict path OUTSIDE
/// `analyse`'s `catch_unwind` — the only externally-triggerable panic there is
/// without a production seam. Returns (exit code, stderr).
fn run_with_closed_stdout(command: &str) -> (Option<i32>, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = Command::new(bin)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    // Close the read end BEFORE the child can possibly print: it is blocked
    // reading stdin until we write below.
    drop(child.stdout.take());
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// CONTROL: with stdout open, a deny-worthy command prints a deny (so the
/// payload really is one the gate refuses).
#[test]
fn control_verdict_is_printed_when_stdout_is_open() {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": "rm -rf /usr/lib" },
    })
    .to_string();
    let mut child = Command::new(bin)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"permissionDecision\":\"deny\""),
        "control: expected a deny on stdout, got {stdout:?}"
    );
}

/// RED: a panic on the verdict path (here: the decision print) must not resolve
/// to exit 0 with no decision. Exit 0 + empty stdout is what Claude Code reads
/// as allow; the only remaining channel for a block is a non-zero exit (2 for
/// PreToolUse). Caveat recorded in the report: the panic is induced through a
/// closed stdout, the sole seam reachable without a production change.
#[test]
fn a_panic_on_the_verdict_path_is_not_an_exit_zero_allow() {
    let (code, stderr) = run_with_closed_stdout("rm -rf /usr/lib");
    assert!(
        stderr.contains("panic"),
        "precondition: the run must actually have panicked on the verdict \
         path (else this test observes nothing); stderr = {stderr:?}"
    );
    assert_ne!(
        code,
        Some(0),
        "a panic on the verdict path exited 0 with no decision printed, which \
         the hook protocol reads as ALLOW; stderr = {stderr:?}"
    );
}
