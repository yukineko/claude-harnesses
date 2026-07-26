// テスト内の unwrap/expect/panic は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! taintguard — a Claude Code hook trio implementing a provenance-scoped
//! least-privilege gate.
//!
//! Contract (shared by every plugin in this repo): a hook must NEVER break the
//! user's turn. All three subcommands read a hook payload from stdin and
//! always exit 0 (`harness_core::hook::run_hook`).
//!
//! * `mark`  (PostToolUse, matcher `WebFetch|WebSearch|Read`) — after a tool
//!   call that may have introduced untrusted-provenance content into context,
//!   record this session as tainted.
//! * `gate`  (PreToolUse, matcher `Bash|Write|Edit|MultiEdit|NotebookEdit`) —
//!   before a write-class tool runs, if this session is tainted, emit the
//!   blastguard-style `ask` (interactive) / `deny` (headless) decision instead
//!   of staying silent (an allow).
//! * `clear` (Stop) — a clean turn ends: reset the taint marker so the next
//!   turn starts trusted again.
//!
//! Both `mark` and `gate` run their real logic behind a `catch_unwind` panic
//! barrier that resolves a panic to the FAIL-CLOSED outcome (a forced taint
//! mark; a forced ask/deny) rather than letting it unwind into `run_hook`'s
//! outer catch, which would silently exit 0 with no mark/no decision — i.e. an
//! allow. This mirrors `blastguard::main::analyse` / `ctxrot`'s
//! `preguard`/`toolguard` `analyse` barriers.

use clap::{Parser, Subcommand};

use harness_core::hook::{read_stdin, run_hook, HookInput};
use taintguard::{classify, hookio, interactive, state};

#[derive(Parser)]
#[command(
    name = "taintguard",
    version,
    about = "Provenance-scoped least-privilege gate for Claude Code."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// PostToolUse hook (matcher `WebFetch|WebSearch|Read`): record taint.
    Mark,
    /// PreToolUse hook (matcher `Bash|Write|Edit|MultiEdit|NotebookEdit`):
    /// ask/deny when this session is tainted.
    Gate,
    /// Stop hook: clear the taint marker after a clean turn.
    Clear,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Mark => run_hook(|| {
            let raw = read_stdin();
            if let Some(input) = HookInput::parse(&raw) {
                analyse_mark(&input);
            }
        }),
        Command::Gate => run_hook(|| {
            let raw = read_stdin();
            if let Some(input) = HookInput::parse(&raw) {
                if let Some(line) = analyse_gate(&input) {
                    println!("{line}");
                }
            }
        }),
        Command::Clear => run_hook(|| {
            let raw = read_stdin();
            if let Some(input) = HookInput::parse(&raw) {
                let cwd = input.cwd_or_current();
                if let Err(reason) = state::clear(&cwd, &input.session_id) {
                    eprintln!(
                        "[taintguard] clear failed (staying tainted, the safe side): {reason}"
                    );
                }
            }
        }),
    }
}

// ---------------------------------------------------------------------------
// mark
// ---------------------------------------------------------------------------

/// Core `mark` decision, pure given `input` (all I/O is inside `state::mark`).
fn decide_mark(input: &HookInput) -> Result<(), String> {
    let cwd = input.cwd_or_current();
    let session = input.session_id.as_str();
    match input.tool_name.as_str() {
        "WebFetch" | "WebSearch" => state::mark(&cwd, session, "web"),
        "Read" => match input.target() {
            Some(target) => match classify::classify(&cwd, &target) {
                classify::Trust::Trusted => Ok(()),
                classify::Trust::Untrusted | classify::Trust::Indeterminate => {
                    state::mark(&cwd, session, "external-read")
                }
            },
            // A Read with no extractable file_path is indeterminate — fail
            // closed the same as an indeterminate path, not a silent no-op.
            None => state::mark(&cwd, session, "external-read"),
        },
        _ => Ok(()),
    }
}

/// Run [`decide_mark`] behind a panic barrier: a panic anywhere in the
/// analysis (classification, serialization) must not fall through to `main`'s
/// outer `run_hook` backstop, which would silently exit 0 with NO mark
/// written — the exact fail-open this barrier exists to prevent (mirrors
/// `blastguard::main::analyse` / `ctxrot::hooks::toolguard::analyse`). On a
/// caught panic we force a mark with source `"internal-error"` so the `gate`
/// subcommand treats the rest of this turn as tainted rather than clean.
fn analyse_mark(input: &HookInput) {
    let cwd = input.cwd_or_current();
    let session = input.session_id.clone();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decide_mark(input))) {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => {
            eprintln!("[taintguard] mark failed: {reason}");
        }
        Err(_) => {
            eprintln!(
                "[taintguard] internal error while analysing a tool call; failing closed (marking tainted)"
            );
            if let Err(reason) = state::mark(&cwd, &session, "internal-error") {
                eprintln!("[taintguard] fail-closed mark also failed: {reason}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// gate
// ---------------------------------------------------------------------------

/// Build the `ask`/`deny` decision line for `reason`, hardening to `deny`
/// exactly like `blastguard::model::Decision::hardened` when no human is
/// affirmatively detected (`interactive::ask_available`).
fn build_decision(reason: &str) -> String {
    if interactive::ask_available() {
        hookio::ask_json(reason)
    } else {
        hookio::deny_json(reason)
    }
}

fn format_sources(sources: &[String]) -> String {
    if sources.is_empty() {
        "an unspecified source".to_string()
    } else {
        sources.join(", ")
    }
}

/// Core `gate` decision, pure given `input` + the on-disk taint marker.
/// `None` means allow (stay silent); `Some(json)` is the line to print.
fn decide_gate(input: &HookInput) -> Option<String> {
    let cwd = input.cwd_or_current();
    match state::check(&cwd, &input.session_id) {
        state::Check::Clean => None,
        state::Check::Tainted(sources) => Some(build_decision(&format!(
            "[taintguard] this turn consumed untrusted-provenance content ({}); \
             write-class tools are downgraded until this turn ends cleanly \
             (a clean Stop restores normal access).",
            format_sources(&sources)
        ))),
        state::Check::Undetermined(why) => Some(build_decision(&format!(
            "[taintguard] could not verify this session's taint state ({why}); \
             failing closed (treating this turn as tainted)."
        ))),
    }
}

/// Run [`decide_gate`] behind a panic barrier: a panic in the taint check must
/// not fall through to a silent allow. A caught panic resolves to the same
/// fail-closed ask/deny as `Check::Undetermined`.
fn analyse_gate(input: &HookInput) -> Option<String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decide_gate(input))) {
        Ok(out) => out,
        Err(_) => Some(build_decision(
            "[taintguard] internal error while checking taint state; failing closed (ask/deny).",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hook_input(
        tool: &str,
        tool_input: serde_json::Value,
        cwd: &std::path::Path,
        session: &str,
    ) -> HookInput {
        HookInput {
            tool_name: tool.to_string(),
            tool_input: Some(tool_input),
            cwd: cwd.to_string_lossy().into_owned(),
            session_id: session.to_string(),
            ..Default::default()
        }
    }

    /// `TAINTGUARD_STATE_DIR` is a process-global env var; tests that set it
    /// must not run concurrently with each other (`cargo test` parallelizes
    /// by default within one binary). Every test below holds this for its
    /// whole body via the returned guard.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn temp_env(
        name: &str,
    ) -> (
        std::sync::MutexGuard<'static, ()>,
        tempfile::TempDir,
        std::path::PathBuf,
    ) {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::Builder::new()
            .prefix(&format!("taintguard-main-{name}-"))
            .tempdir()
            .expect("tempdir");
        std::env::set_var("TAINTGUARD_STATE_DIR", dir.path());
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        (guard, dir, cwd)
    }

    #[test]
    fn webfetch_marks_and_gate_then_asks_or_denies() {
        let (_guard, _dir, cwd) = temp_env("webfetch");
        let session = "s-webfetch";
        let mark_input = hook_input(
            "WebFetch",
            json!({"url": "https://example.com"}),
            &cwd,
            session,
        );
        decide_mark(&mark_input).unwrap();

        let gate_input = hook_input("Bash", json!({"command": "echo hi"}), &cwd, session);
        let line = decide_gate(&gate_input).expect("tainted session must not allow silently");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(
            v["hookSpecificOutput"]["permissionDecision"] == "ask"
                || v["hookSpecificOutput"]["permissionDecision"] == "deny"
        );
    }

    #[test]
    fn in_repo_read_does_not_mark() {
        let (_guard, _dir, cwd) = temp_env("inrepo-read");
        let session = "s-inrepo";
        let f = cwd.join("src.rs");
        std::fs::write(&f, "fn main() {}").unwrap();
        let mark_input = hook_input(
            "Read",
            json!({"file_path": f.to_string_lossy()}),
            &cwd,
            session,
        );
        decide_mark(&mark_input).unwrap();

        let gate_input = hook_input("Write", json!({"file_path": "out.rs"}), &cwd, session);
        assert!(
            decide_gate(&gate_input).is_none(),
            "an in-repo Read must not taint the session"
        );
    }

    #[test]
    fn external_read_marks_and_gate_blocks() {
        let (_guard, _dir, cwd) = temp_env("external-read");
        let session = "s-external";
        let outside = tempfile::Builder::new()
            .prefix("taintguard-external-")
            .tempdir()
            .unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "s").unwrap();
        let mark_input = hook_input(
            "Read",
            json!({"file_path": secret.to_string_lossy()}),
            &cwd,
            session,
        );
        decide_mark(&mark_input).unwrap();

        let gate_input = hook_input("Edit", json!({"file_path": "src.rs"}), &cwd, session);
        assert!(decide_gate(&gate_input).is_some());
    }

    #[test]
    fn clean_session_gate_allows_silently() {
        let (_guard, _dir, cwd) = temp_env("clean-gate");
        let gate_input = hook_input("Bash", json!({"command": "cargo test"}), &cwd, "s-clean");
        assert!(decide_gate(&gate_input).is_none());
    }

    #[test]
    fn clear_restores_allow_after_mark() {
        let (_guard, _dir, cwd) = temp_env("clear-restores");
        let session = "s-clear";
        let mark_input = hook_input("WebSearch", json!({"query": "x"}), &cwd, session);
        decide_mark(&mark_input).unwrap();
        assert!(decide_gate(&hook_input("Bash", json!({"command": "x"}), &cwd, session)).is_some());

        state::clear(&cwd, session).unwrap();
        assert!(decide_gate(&hook_input("Bash", json!({"command": "x"}), &cwd, session)).is_none());
    }

    #[test]
    fn analyse_gate_panic_barrier_fails_closed() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            analyse_gate_with(|_: &HookInput| -> Option<String> { panic!("boom") })
        }));
        std::panic::set_hook(prev);
        let line = out.unwrap().expect("a panic must fail closed, not allow");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(
            v["hookSpecificOutput"]["permissionDecision"] == "ask"
                || v["hookSpecificOutput"]["permissionDecision"] == "deny"
        );
    }

    /// Test-only seam so the panic-barrier test above can inject a panicking
    /// closure without needing a real panic-triggering input through
    /// `decide_gate`'s taint-check logic (mirrors `blastguard`'s
    /// `analyse_barrier` helper in `ctxrot::hooks::toolguard`).
    fn analyse_gate_with<F>(f: F) -> Option<String>
    where
        F: FnOnce(&HookInput) -> Option<String> + std::panic::UnwindSafe,
    {
        let dummy = HookInput::default();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&dummy))) {
            Ok(out) => out,
            Err(_) => Some(build_decision(
                "[taintguard] internal error while checking taint state; failing closed (ask/deny).",
            )),
        }
    }
}
