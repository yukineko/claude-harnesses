//! `bin/ctxrot` (the POSIX launcher) — what happens when NO per-platform binary
//! is bundled for the host.
//!
//! The launcher used to answer that question with a single `exit 0` for every
//! subcommand: a hook that "must never break the user's turn". But exit 0 with
//! no output is exactly what a *clean* run looks like, so a missing build made
//! every verdict-bearing subcommand silently answer "nothing to flag":
//!
//! * `preguard` — PreToolUse: no deny JSON → the unbounded `Read` of a 1GB log
//!   is ALLOWED, indistinguishable from "the gate examined this call and it was
//!   fine".
//! * `toolguard` — PostToolUse: no `updatedToolOutput`/`additionalContext` →
//!   the huge payload passes through as if it had been measured.
//! * `stop` — Stop: no `{"decision":"block"}` → the turn ends as if the budget
//!   check had run and passed.
//! * `statusline` — statusLine: a blank bar, which reads as "plenty of
//!   headroom" (the `3b1eb24` fail-open CLAUDE.md §1 names).
//!
//! These tests pin the corrected failure mode: a missing binary is a
//! CANNOT-DETERMINE and resolves to the restrictive side of each hook's own
//! protocol, never to a silent allow. The non-verdict observability hooks
//! (`guard`/`rescue`/`restore`) keep exit 0 but must still say on stderr that
//! they did not run, and every other (CLI / machine-consumed) subcommand exits
//! non-zero rather than printing an empty result that reads as "no data".

use std::io::Write;
use std::process::{Command, Stdio};

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run the real launcher from a directory that provably contains no
/// `ctxrot-<os>-<arch>` sibling, so the missing-binary path is taken on every
/// platform (the repo's own `bin/` may hold a built binary).
fn run(args: &[&str], stdin: &str) -> Run {
    let home = tempfile::tempdir().expect("home tempdir");
    run_in(home.path(), args, stdin)
}

/// Same, but with an explicit `HOME` so several launcher runs can share one
/// failure-streak marker store. `HOME` is always a tempdir and
/// `CLAUDE_CODE_SESSION_ID` is always removed, so a test never writes a marker
/// into the real home nor keys on the live session.
fn run_in(home: &std::path::Path, args: &[&str], stdin: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("ctxrot");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("bin")
            .join("ctxrot"),
        &launcher,
    )
    .expect("copy launcher");

    let mut child = Command::new("sh")
        .arg(&launcher)
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sh");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// ---------------------------------------------------------------- verdict side

/// PreToolUse: silence = allow. A launcher that cannot run the gate must not
/// grant it. `ask` (blastguard's third answer: a refusal to guess, not a verdict
/// about the call) is the restrictive-but-escapable resolution.
#[test]
fn preguard_missing_binary_asks_instead_of_silently_allowing() {
    let r = run(&["preguard"], r#"{"tool_name":"Read"}"#);
    assert_eq!(r.code, 0, "PreToolUse decisions ride stdout, not exit code");
    assert!(
        !r.stdout.trim().is_empty(),
        "silence IS the allow — the launcher must emit a decision: {r:?}",
    );
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be hook JSON ({e}): {}", r.stdout));
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "PreToolUse",
        "{v}"
    );
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "ask",
        "cannot-determine must resolve to ask (not allow): {v}",
    );
    let reason = v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default();
    assert!(
        reason.contains("ctxrot"),
        "the reason is the only steering channel; name the cause: {reason}",
    );
}

/// PostToolUse: the payload already landed, so there is nothing to block — but
/// the launcher must still mark the output as UN-measured rather than let it
/// pass as if toolguard had sized it.
#[test]
fn toolguard_missing_binary_marks_output_unmeasured() {
    let r = run(&["toolguard"], r#"{"tool_name":"Read"}"#);
    assert_eq!(r.code, 0);
    assert!(
        !r.stdout.trim().is_empty(),
        "an un-analysed tool output must not pass silently: {r:?}",
    );
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be hook JSON ({e}): {}", r.stdout));
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "PostToolUse",
        "{v}"
    );
    assert!(
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_default()
            .contains("ctxrot"),
        "{v}",
    );
}

/// Stop: exit 0 with no decision is *indistinguishable from a clean stop*
/// (`harness_core::gate::run`'s own words). A missing binary blocks the first
/// stop instead, exactly like a panicking gate body does.
#[test]
fn stop_missing_binary_blocks_the_first_stop() {
    let r = run(
        &["stop"],
        r#"{"session_id":"s1","stop_hook_active":false,"transcript_path":"/nope"}"#,
    );
    assert_eq!(r.code, 0, "the Stop protocol rides stdout, not exit code");
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be a stop decision ({e}): {}", r.stdout));
    assert_eq!(
        v["decision"], "block",
        "a gate that could not run cannot certify the stop: {v}",
    );
    assert!(
        v["reason"].as_str().unwrap_or_default().contains("ctxrot"),
        "{v}",
    );
}

// ------------------------------------------------- failure-streak (9b3f98ec)

/// A re-entry (`stop_hook_active:true`) is set by ANY gate's block, so on its
/// own it says nothing about THIS gate. Only this gate's second consecutive
/// failure in the session may be allowed.
fn ev(active: bool, sid: Option<&str>) -> String {
    match sid {
        Some(s) => format!(r#"{{"session_id":"{s}","stop_hook_active":{active}}}"#),
        None => format!(r#"{{"stop_hook_active":{active}}}"#),
    }
}

fn assert_blocks(r: &Run, what: &str) {
    assert_eq!(r.code, 0, "Stop decisions ride stdout JSON: {r:?}");
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be a block decision ({e}): {r:?}"));
    assert_eq!(v["decision"], "block", "{what}: {r:?}");
}

fn assert_plain_allow(r: &Run, what: &str) {
    assert_eq!(r.code, 0, "{what}: {r:?}");
    assert!(
        r.stdout.trim().is_empty(),
        "{what}: allow carries no stdout: {r:?}"
    );
    assert!(
        !r.stderr.trim().is_empty(),
        "{what}: allowing silently would hide that the gate has NOT run: {r:?}",
    );
}

fn assert_loud_allow(r: &Run, what: &str) {
    assert_eq!(r.code, 0, "{what}: {r:?}");
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("{what}: stdout must be one JSON object ({e}): {r:?}"));
    assert!(v.get("decision").is_none(), "{what}: no decision key: {v}");
    let msg = v["systemMessage"].as_str().unwrap_or_default();
    assert!(
        msg.contains("ctxrot") && msg.contains("NOT"),
        "{what}: systemMessage must name the gate and say it did NOT check: {v}",
    );
}

/// Fresh session (another gate's block caused the re-entry; this gate has not
/// failed before) -> block, not allow.
#[test]
fn stop_reentry_in_a_fresh_session_blocks() {
    let r = run(&["stop"], &ev(true, Some("fresh-s")));
    assert_blocks(
        &r,
        "first failure of this gate must block even on a re-entry",
    );
}

/// The "never trapped" bound: first stop blocks, the re-entry after it (same
/// session, same HOME) is allowed with empty stdout, and the streak continues.
#[test]
fn stop_missing_binary_allows_the_post_block_reentry() {
    let home = tempfile::tempdir().expect("home");
    let r1 = run_in(home.path(), &["stop"], &ev(false, Some("s1")));
    assert_blocks(&r1, "first stop");
    let r2 = run_in(home.path(), &["stop"], &ev(true, Some("s1")));
    assert_plain_allow(&r2, "second consecutive failure");
    let r3 = run_in(home.path(), &["stop"], &ev(true, Some("s1")));
    assert_plain_allow(&r3, "streak continues");
    // a different session in the same HOME has its own (empty) streak
    let r4 = run_in(home.path(), &["stop"], &ev(true, Some("other")));
    assert_blocks(&r4, "markers are per session");
}

/// No session id at all (payload without `session_id`, env removed): a
/// re-entry's streak is undetermined -> loud allow; the same payload as a first
/// stop still blocks.
#[test]
fn stop_reentry_without_session_id_is_a_loud_allow_but_first_stop_still_blocks() {
    let home = tempfile::tempdir().expect("home");
    let r = run_in(home.path(), &["stop"], &ev(true, None));
    assert_loud_allow(&r, "undetermined streak");
    let r = run_in(home.path(), &["stop"], &ev(false, None));
    assert_blocks(&r, "first stop without a session id");
}

/// A manual `ctxrot stop` (empty stdin = no live turn) has no stop to block, so
/// it reports an error instead of emitting a fake decision — the launcher's
/// mirror of `PanicAction::InteractiveError`.
#[test]
fn stop_missing_binary_interactive_errors_without_a_decision() {
    let r = run(&["stop"], "");
    assert_ne!(r.code, 0, "a manual run must report the failure: {r:?}");
    assert!(
        !r.stdout.contains("decision"),
        "no live turn → no decision may be fabricated: {r:?}",
    );
}

/// statusLine: a blank bar reads as a healthy low band (CLAUDE.md §1, `3b1eb24`).
/// Render the same explicit `unknown` state `usage::unknown_line` does.
#[test]
fn statusline_missing_binary_renders_unknown_not_blank() {
    let r = run(&["statusline"], r#"{"session_id":"s1"}"#);
    assert_eq!(r.code, 0, "the status bar must never crash: {r:?}");
    assert!(
        r.stdout.contains("unknown"),
        "blank/absent reads as headroom; say unknown: {r:?}",
    );
    assert!(
        !r.stdout.contains("band0"),
        "must not fabricate a healthy band: {r:?}",
    );
}

// ------------------------------------------------------------ non-verdict side

/// `guard`/`rescue`/`restore` are the pure-observability carve-out (CLAUDE.md §1
/// names them): their output is injected prose with no machine consumer, so a
/// missing binary stays exit 0 — but it must still SAY it did not run rather
/// than pretend it decided.
#[test]
fn observability_hooks_stay_exit_zero_but_are_not_silent() {
    for sub in ["guard", "rescue", "restore", "handoff", "handoff-record"] {
        let r = run(&[sub], r#"{"session_id":"s1"}"#);
        assert_eq!(r.code, 0, "{sub} must not break the turn: {r:?}");
        assert!(
            r.stdout.trim().is_empty(),
            "{sub} must not inject a fabricated block into context: {r:?}",
        );
        assert!(
            r.stderr.contains("ctxrot"),
            "{sub} must report that it did not run: {r:?}",
        );
    }
}

/// Everything else is a CLI/skill-consumed command whose EMPTY output would be
/// read as real data ("no notes", "nothing pinned", "nothing dropped",
/// "usage unknown"). CLAUDE.md §3: an error must not return an empty set.
#[test]
fn machine_consumed_commands_fail_loudly_instead_of_printing_nothing() {
    for args in [
        vec!["note", "list"],
        vec!["ctx", "pinned"],
        vec!["ctx", "dropped"],
        vec!["usage"],
        vec!["metrics"],
        vec![],
    ] {
        let r = run(&args, "");
        assert_ne!(
            r.code,
            0,
            "`ctxrot {}` must fail, not print an empty result: {r:?}",
            args.join(" "),
        );
        assert!(
            r.stdout.trim().is_empty(),
            "no fabricated data on the failure path: {r:?}",
        );
    }
}

// ------------------------------------------------------------- the happy path

/// The whole point of the dispatcher: when the platform build IS there it must
/// still be exec'd, with the subcommand and flags passed through untouched. The
/// fail-closed arms above must never shadow a working install.
#[test]
fn present_binary_is_execed_with_args_passed_through() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("bin")
            .join("ctxrot"),
        dir.path().join("ctxrot"),
    )
    .expect("copy launcher");

    let os = match uname("-s").as_str() {
        "Linux" => "linux".to_string(),
        "Darwin" => "darwin".to_string(),
        other => other.to_string(),
    };
    let arch = match uname("-m").as_str() {
        "x86_64" | "amd64" => "x86_64".to_string(),
        "arm64" | "aarch64" => "arm64".to_string(),
        other => other.to_string(),
    };
    let stub = dir.path().join(format!("ctxrot-{os}-{arch}"));
    std::fs::write(&stub, "#!/bin/sh\necho \"stub:$*\"\n").expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let out = Command::new("sh")
        .arg(dir.path().join("ctxrot"))
        .args(["note", "list", "--cwd", "/tmp"])
        .output()
        .expect("run launcher");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "stub:note list --cwd /tmp",
    );
}

fn uname(flag: &str) -> String {
    let out = Command::new("uname").arg(flag).output().expect("uname");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exit={} stdout={:?} stderr={:?}",
            self.code, self.stdout, self.stderr
        )
    }
}
