// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! End-to-end tests for the propguard binary.
//!
//! `check` is the Stop hook: it must ALWAYS exit 0 toward Claude (the `decision`
//! field, not the exit code, is what blocks a stop). The other subcommands are a
//! plain clap CLI; `status`/`derive` are read-only.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("propguard-it-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Build the Stop-hook stdin JSON payload through a real JSON encoder rather
/// than hand-formatting a string literal. `format!(r#"{{"cwd":"{}"}}"#, ...)`
/// embeds the path's raw bytes unescaped — on Windows a `cwd` containing `\`
/// corrupts the JSON, `HookInput::parse` returns `None`, and the session key
/// then collapses onto the shared `_local` bucket (see
/// `harness_core::hook::HookInput::session_key`), silently merging distinct
/// test sessions' state. `serde_json::json!` guarantees valid, correctly
/// escaped JSON regardless of the path's separator style or byte content.
fn hook_payload(session: &str, cwd: &Path) -> String {
    serde_json::json!({
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
    })
    .to_string()
}

/// Run `propguard <args>` with `payload` on stdin in an isolated HOME/CWD.
/// Returns (exit_code, stdout).
///
/// `PROPGUARD_STATE_DIR` explicitly pins propguard's own session-state
/// directory under `dir` (in addition to the `HOME` override). Belt and
/// suspenders: `harness_core::config::home()` resolves via `dirs::home_dir()`,
/// which on Windows ignores a `HOME` env override and resolves the real
/// profile via `SHGetKnownFolderPath` — an explicit state-dir override keeps
/// these tests from ever writing under the real home directory regardless of
/// that platform quirk.
fn run_in(dir: &Path, args: &[&str], payload: &str, extra_env: &[(&str, &str)]) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_propguard");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("PROPGUARD_STATE_DIR", dir.join(".propguard-state"))
        .env_remove("PROPGUARD_CRITERIA")
        .env_remove("PROPGUARD_DISABLE");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn git_init(dir: &Path) {
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        let _ = Command::new("git").current_dir(dir).args(&args).output();
    }
}

#[test]
fn help_describes_the_gate() {
    let home = temp_home();
    let (code, stdout) = run_in(&home, &["--help"], "", &[]);
    assert_eq!(code, 0);
    assert!(stdout.to_lowercase().contains("property"));
}

#[test]
fn derive_subcommand_lists_properties() {
    let home = temp_home();
    let (code, stdout) = run_in(
        &home,
        &["derive", "must be idempotent and never panic on bad input"],
        "",
        &[],
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("idempotence"), "got: {stdout}");
    assert!(stdout.contains("error-path"), "got: {stdout}");
}

/// No done_criteria source anywhere → the stop is allowed (exit 0, no block JSON).
#[test]
fn no_criteria_allows_the_stop() {
    let home = temp_home();
    git_init(&home);
    // create a changed file so it's not the "no code" path either
    std::fs::write(home.join("a.rs"), "fn main() {}\n").unwrap();
    let payload = hook_payload("s1", &home);
    let (code, stdout) = run_in(&home, &["check"], &payload, &[]);
    assert_eq!(code, 0, "hook must always exit 0 toward Claude");
    assert!(
        !stdout.contains("\"decision\":\"block\""),
        "no criteria must not block: {stdout}"
    );
}

/// With a done_criteria (via env) and a changed source file, inject mode blocks
/// the first stop (properties unverified) — but still exits 0.
#[test]
fn with_criteria_inject_blocks_first_stop_but_exits_zero() {
    let home = temp_home();
    git_init(&home);
    std::fs::write(home.join("a.rs"), "fn f() { panic!() }\n").unwrap();
    let payload = hook_payload("s2", &home);
    let (code, stdout) = run_in(
        &home,
        &["check"],
        &payload,
        &[(
            "PROPGUARD_CRITERIA",
            "idempotent; never panic; stable output schema",
        )],
    );
    assert_eq!(code, 0, "hook exits 0 toward Claude even when blocking");
    assert!(
        stdout.contains("\"decision\": \"block\"") || stdout.contains("\"decision\":\"block\""),
        "inject mode must block the first stop: {stdout}"
    );
    assert!(
        stdout.contains("propguard"),
        "block reason present: {stdout}"
    );
}

/// PROPGUARD_DISABLE=1 short-circuits to allow.
#[test]
fn disable_env_allows() {
    let home = temp_home();
    git_init(&home);
    std::fs::write(home.join("a.rs"), "fn f() {}\n").unwrap();
    let payload = hook_payload("s3", &home);
    let (code, stdout) = run_in(
        &home,
        &["check"],
        &payload,
        &[
            ("PROPGUARD_CRITERIA", "idempotent; never panic"),
            ("PROPGUARD_DISABLE", "1"),
        ],
    );
    assert_eq!(code, 0);
    assert!(!stdout.contains("block"), "disabled must allow: {stdout}");
}

/// Malformed hook JSON on stdin must never trap the turn (exit 0, no block).
#[test]
fn malformed_input_never_breaks_the_turn() {
    let home = temp_home();
    let (code, stdout) = run_in(&home, &["check"], "{ this is not json", &[]);
    assert_eq!(code, 0);
    assert!(!stdout.contains("\"decision\":\"block\""));
}

#[test]
fn status_is_read_only_and_exits_zero() {
    let home = temp_home();
    let (code, stdout) = run_in(&home, &["status"], "", &[]);
    assert_eq!(code, 0);
    assert!(stdout.contains("threshold:"));
}

// ── overwatch fleet-violation emission (fail-soft, additive) ──────────────

/// Read the overwatch violations.jsonl written under an isolated HOME, for a
/// repo rooted at `home` (mirrors `overwatch::store::violations_path`).
fn read_violations_jsonl(home: &Path) -> Vec<serde_json::Value> {
    // project_key is derived from the repo root path; walk .overwatch/*/overwatch/violations.jsonl
    let base = home.join(".overwatch");
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(&base) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path().join("overwatch").join("violations.jsonl");
        if let Ok(txt) = std::fs::read_to_string(&path) {
            for line in txt.lines() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    found.push(v);
                }
            }
        }
    }
    found
}

/// A Block decision must append `propguard:<property-id>` signed violation(s)
/// (one per failing PROP-* property) to the overwatch store (fleet-level
/// correlated-error detection).
#[test]
fn block_appends_propguard_signed_violation() {
    let home = temp_home();
    git_init(&home);
    std::fs::write(home.join("a.rs"), "fn f() { panic!() }\n").unwrap();
    let payload = hook_payload("s-ov1", &home);
    let (code, stdout) = run_in(
        &home,
        &["check"],
        &payload,
        &[(
            "PROPGUARD_CRITERIA",
            "idempotent; never panic; stable output schema",
        )],
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("block"), "expected a block: {stdout}");

    let violations = read_violations_jsonl(&home);
    assert!(
        !violations.is_empty(),
        "expected at least one recorded overwatch violation"
    );
    assert!(
        violations.iter().any(|v| {
            v.get("source").and_then(|s| s.as_str()) == Some("propguard")
                && v.get("signature")
                    .and_then(|s| s.as_str())
                    .map(|s| s.starts_with("propguard:"))
                    .unwrap_or(false)
        }),
        "expected a propguard:<property-id> signature among {violations:?}"
    );
    assert!(
        violations
            .iter()
            .all(|v| v.get("session_id").and_then(|s| s.as_str()) == Some("s-ov1")),
        "violation session_id must come from the hook input: {violations:?}"
    );
}

/// The block decision/output (reason, decision JSON, exit code) must be
/// byte-identical whether or not the overwatch emission path runs — emission
/// is purely additive telemetry, never a mutation of the gate's own decision.
#[test]
fn block_decision_unchanged_with_and_without_overwatch_store() {
    let payload_for = |home: &Path, session: &str| hook_payload(session, home);

    // Run A: normal HOME, overwatch store writable.
    let home_a = temp_home();
    git_init(&home_a);
    std::fs::write(home_a.join("a.rs"), "fn f() { panic!() }\n").unwrap();
    let (code_a, stdout_a) = run_in(
        &home_a,
        &["check"],
        &payload_for(&home_a, "s-cmp"),
        &[(
            "PROPGUARD_CRITERIA",
            "idempotent; never panic; stable output schema",
        )],
    );

    // Run B: HOME's .overwatch path replaced with an unwritable file, so
    // `append_violation`'s create_dir_all/open must fail — the block
    // decision must still come out identically.
    let home_b = temp_home();
    git_init(&home_b);
    std::fs::write(home_b.join("a.rs"), "fn f() { panic!() }\n").unwrap();
    std::fs::write(home_b.join(".overwatch"), "not a directory").unwrap();
    let (code_b, stdout_b) = run_in(
        &home_b,
        &["check"],
        &payload_for(&home_b, "s-cmp"),
        &[(
            "PROPGUARD_CRITERIA",
            "idempotent; never panic; stable output schema",
        )],
    );

    assert_eq!(code_a, code_b);
    assert_eq!(code_a, 0, "hook always exits 0 toward Claude");
    assert_eq!(
        stdout_a, stdout_b,
        "block reason/decision JSON must be unaffected by overwatch emission"
    );
}

/// When the overwatch store path is unwritable (a file sits where the
/// directory should be), emission must be silently skipped: no panic, and
/// the hook still exits 0 with the block decision intact.
#[test]
fn no_panic_when_overwatch_store_unwritable() {
    let home = temp_home();
    git_init(&home);
    std::fs::write(home.join("a.rs"), "fn f() { panic!() }\n").unwrap();
    // Pre-create `.overwatch` as a plain file so store writes underneath it fail.
    std::fs::write(home.join(".overwatch"), "not a directory").unwrap();
    let payload = hook_payload("s-ov2", &home);
    let (code, stdout) = run_in(
        &home,
        &["check"],
        &payload,
        &[(
            "PROPGUARD_CRITERIA",
            "idempotent; never panic; stable output schema",
        )],
    );
    assert_eq!(
        code, 0,
        "must not panic/crash when overwatch store is unwritable"
    );
    assert!(
        stdout.contains("block"),
        "block decision preserved: {stdout}"
    );
}

// ── regression: hook-payload JSON escaping (docs/review-redesign-implementation-items.md #2) ──

/// Regression for this test harness's own payload construction, not the
/// binary: hand-formatting `format!(r#"{{"cwd":"{}"}}"#, path)` embeds the
/// path unescaped. A Windows-style `cwd` (containing `\`) then breaks the
/// JSON, `HookInput::parse` returns `None`, and every such run's session
/// silently collapses onto the shared `_local` state bucket. Building the
/// payload through `serde_json::json!` (as `hook_payload` now does) instead
/// guarantees valid, correctly escaped JSON for any path content, on any
/// platform — this is checked directly here (no process spawn needed) so it
/// can't regress even on a platform whose paths never contain backslashes.
#[test]
fn payload_json_is_valid_even_for_windows_style_paths() {
    let windows_like_cwd = r"C:\Users\dev\project";

    let payload = hook_payload("s-esc", Path::new(windows_like_cwd));
    let parsed: serde_json::Value = serde_json::from_str(&payload)
        .expect("hook_payload must always produce valid JSON, even for backslash-heavy paths");
    assert_eq!(parsed["session_id"], "s-esc");
    assert_eq!(parsed["cwd"], windows_like_cwd);

    // Sanity check that this regression test is not vacuous: the naive
    // hand-formatted string this file used to build really is broken JSON
    // for a backslash path.
    let naive = format!(r#"{{"session_id":"s-esc","cwd":"{}"}}"#, windows_like_cwd);
    assert!(
        serde_json::from_str::<serde_json::Value>(&naive).is_err(),
        "sanity: an unescaped backslash path must indeed break hand-formatted JSON, \
         or this regression test no longer demonstrates anything"
    );
}
