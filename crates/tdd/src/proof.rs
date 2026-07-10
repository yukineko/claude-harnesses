//! Test-first proof: make "the test was written before the implementation"
//! a *verifiable* artifact instead of a claim.
//!
//!   `tdd red`   runs the tests and REQUIRES them to fail (≥1 red). It records
//!               `<proof_dir>/<task>.red.json`. If they already pass, that's not
//!               test-first — it errors.
//!   `tdd green` REQUIRES a prior RED proof, runs the tests, and REQUIRES them to
//!               pass. It records `<proof_dir>/<task>.green.json`.
//!   `tdd verify` succeeds iff both proofs exist (RED then GREEN happened).

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde_json::json;

use crate::config::Config;
use crate::runner;

fn safe(task: &str) -> String {
    task.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

pub fn proof_dir(root: &Path, cfg: &Config) -> PathBuf {
    root.join(&cfg.proof_dir)
}

pub fn artifact_path(root: &Path, cfg: &Config, task: &str, kind: &str) -> PathBuf {
    proof_dir(root, cfg).join(format!("{}.{kind}.json", safe(task)))
}

/// Decide whether a RED run is acceptable: the tests MUST have failed.
fn judge_red(passed: bool) -> Result<()> {
    if passed {
        bail!(
            "tests passed on `tdd red` — that is not test-first. Write a test that FAILS \
             against the not-yet-written behaviour first, then run `tdd red` again."
        );
    }
    Ok(())
}

/// Decide whether a RED proof exists — the single canonical definition of the
/// "no RED proof found" judgment. Shared by [`judge_green`] (the exhaustive
/// GREEN-run judgment) and by `green()`'s own early check, which MUST run
/// this same judgment *before* the `strict_separation` identity check (see
/// the comment at that call site): calling through this one function keeps
/// the error text and behaviour identical at both call sites instead of two
/// independently maintained copies of the same bail.
fn judge_has_red(has_red: bool) -> Result<()> {
    if !has_red {
        bail!("no RED proof found — run `tdd red --task <id>` before implementing.");
    }
    Ok(())
}

/// Decide whether a GREEN run is acceptable: a RED proof must exist and the
/// tests MUST now pass.
fn judge_green(passed: bool, has_red: bool) -> Result<()> {
    judge_has_red(has_red)?;
    if !passed {
        bail!(
            "tests still failing — keep implementing until they pass, then run `tdd green` again."
        );
    }
    Ok(())
}

/// Canonicalize an author identity for comparison: trimmed + lowercased, so
/// `"Agent-A"` and `" agent-a "` are recognised as the same identity. Mirrors
/// `condukt::verify::canonical` used for the worker/verifier-model invariant.
fn canonical_author(id: &str) -> String {
    id.trim().to_lowercase()
}

/// Bind the recorded identity to something real instead of an honor-system
/// bare string: when `--author` is not explicitly given, default to the
/// current Claude Code session id (`CLAUDE_CODE_SESSION_ID`), falling back to
/// a stable, shared `"_local"` bucket when unset (matches
/// `harness_core::hook::HookInput::session_key`'s convention for manual/CLI
/// runs outside a hook). This makes the *common* case — the same agent/session
/// runs both `tdd red` and `tdd green` — actually caught under
/// `strict_separation`, rather than depending entirely on a caller-supplied
/// `--author` string.
///
/// Residual limitation (document plainly, this is HOTL not a hard security
/// boundary): a determined single agent can still defeat this by passing two
/// *different* `--author` overrides explicitly (or by forging
/// `CLAUDE_CODE_SESSION_ID`) — nothing here cryptographically authenticates
/// the identity. `strict_separation` raises the bar for the common/accidental
/// case; it is not a hard security boundary against a deliberately adversarial
/// single agent.
fn default_author() -> String {
    default_author_from(std::env::var("CLAUDE_CODE_SESSION_ID").ok())
}

/// Testable core of [`default_author`]: given what
/// `env::var("CLAUDE_CODE_SESSION_ID")` would have returned, resolve the
/// default identity — trimmed session id, or the shared `"_local"` bucket
/// when unset/blank. Split out so tests can exercise the fallback logic
/// deterministically without mutating real process-global env state.
fn default_author_from(session_id: Option<String>) -> String {
    session_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "_local".to_string())
}

/// Resolve the identity to record for a RED/GREEN step: an explicit
/// `--author` always wins; otherwise fall back to [`default_author`] (the
/// session id, or `"_local"`). Never `None` — there is always *some* recorded
/// identity now, even without `--author`.
fn resolve_author(author: &Option<String>) -> String {
    resolve_author_from(author, default_author)
}

/// Testable core of [`resolve_author`]: an explicit `--author` always wins;
/// otherwise calls `default` (normally [`default_author`], overridable in
/// tests) to resolve the fallback identity.
fn resolve_author_from(author: &Option<String>, default: impl FnOnce() -> String) -> String {
    author
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string)
        .unwrap_or_else(default)
}

/// Pure gate: strict test/impl author separation (opt-in, fail-closed under
/// strict mode). Mirrors condukt's `resolve_verifier_model`/`same_model`
/// invariant that the verifier model must never equal the worker model — here
/// the RED (test-authoring) identity must never equal the GREEN
/// (implementation) identity, so one agent can't write a wrong implementation
/// and a matching wrong test (reward hacking).
///
/// - `strict == false` (default): always allowed, regardless of identities —
///   fully backward compatible.
/// - `strict == true`: allowed iff both identities are present and differ
///   (case-insensitive, trimmed). Missing/empty identities are also rejected
///   under strict mode, since separation can't be verified without them. In
///   practice `green()` always resolves a non-empty identity via
///   [`resolve_author`] (session id default), so this branch now only fires
///   for a RED proof written *before* this identity-default existed (no
///   `author` field at all) or when called directly with `None` (e.g. tests).
pub fn judge_separation(
    strict: bool,
    test_author: Option<&str>,
    impl_author: Option<&str>,
) -> Result<()> {
    if !strict {
        return Ok(());
    }
    let test_author = test_author.map(str::trim).filter(|s| !s.is_empty());
    let impl_author = impl_author.map(str::trim).filter(|s| !s.is_empty());
    let (Some(test_author), Some(impl_author)) = (test_author, impl_author) else {
        bail!(
            "strict_separation is enabled but the test-author and/or impl-author identity is \
             missing — pass `--author <id>` to both `tdd red` and `tdd green` so separation can \
             be verified."
        );
    };
    if canonical_author(test_author) == canonical_author(impl_author) {
        bail!(
            "strict_separation: the RED (test-author=\"{test_author}\") and GREEN \
             (impl-author=\"{impl_author}\") identities are the same — the same agent must not \
             both write the failing test and its implementation (this defeats test-first and \
             enables reward hacking). Have a different agent/session run `tdd green`, or disable \
             strict_separation if this is intentional."
        );
    }
    Ok(())
}

fn write_artifact(path: &Path, value: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn resolve_cmd<'a>(cmd: &'a Option<String>, cfg: &'a Config) -> &'a str {
    match cmd {
        Some(c) if !c.trim().is_empty() => c,
        _ => &cfg.test_cmd,
    }
}

/// `tdd red`: run the tests, require failure, record the RED proof.
///
/// `author` is an optional identity (agent/session id) for the test-authoring
/// step, recorded into the proof so a later `tdd green --author <id>` can be
/// checked against it under `strict_separation` (see [`judge_separation`]).
/// When omitted, the recorded identity defaults to the current session id
/// (see [`default_author`]) rather than being left empty — see
/// [`resolve_author`].
pub fn red(
    root: &Path,
    cfg: &Config,
    task: &str,
    cmd: &Option<String>,
    author: &Option<String>,
) -> Result<()> {
    let cmdline = resolve_cmd(cmd, cfg);
    let tmp = cfg.state_dir.join("tmp");
    let out = runner::run_cmd(
        cmdline,
        root,
        cfg.default_timeout_secs,
        cfg.output_tail_lines,
        &tmp,
    );
    judge_red(out.passed)?;
    let path = artifact_path(root, cfg, task, "red");
    let author = resolve_author(author);
    write_artifact(
        &path,
        &json!({
            "task": task,
            "phase": "red",
            "cmd": cmdline,
            "passed": out.passed,
            "exit_code": out.exit_code,
            "ts": chrono::Local::now().to_rfc3339(),
            "output_tail": out.output_tail,
            "author": author,
        }),
    )?;
    println!(
        "🔴 RED recorded for `{task}` — tests fail as expected ({}).\n   {}",
        out.status_str(),
        path.display()
    );
    Ok(())
}

/// Fail-soft read of a proof artifact's `author` field (`None` if missing,
/// unreadable, corrupt, or the field is absent/not a string).
fn read_author(root: &Path, cfg: &Config, task: &str, kind: &str) -> Option<String> {
    let path = artifact_path(root, cfg, task, kind);
    let text = std::fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("author")?
        .as_str()
        .map(std::string::ToString::to_string)
}

/// `tdd green`: require a RED proof, run the tests, require success, record GREEN.
///
/// `author` is an optional identity for the implementation step. When
/// omitted, it defaults to the current session id (see [`default_author`] /
/// [`resolve_author`]) rather than being left empty — so the *common* case
/// (the same agent/session runs both `red` and `green` without ever passing
/// `--author`) is actually observable under `strict_separation`, instead of
/// silently passing because no identity was recorded. Under
/// `cfg.strict_separation` the resolved identity is compared against the RED
/// proof's recorded author and rejected (fail-closed) if they are the same
/// identity — see [`judge_separation`]. When `strict_separation` is off (the
/// default) this check is skipped entirely, so behaviour is unchanged.
pub fn green(
    root: &Path,
    cfg: &Config,
    task: &str,
    cmd: &Option<String>,
    author: &Option<String>,
) -> Result<()> {
    let red_path = artifact_path(root, cfg, task, "red");
    let has_red = red_path.exists();
    // Check RED-proof existence FIRST, before the strict_separation identity
    // check: otherwise a `tdd green --author X` run *without* a prior `tdd
    // red` gets misdiagnosed as "identity is missing" (there's no RED
    // artifact to read an author from) instead of the correct "no RED proof
    // found" — even though `--author` was passed correctly. This mirrors the
    // same early bail `judge_green` performs, just moved ahead of the
    // identity check so the more fundamental precondition is reported first.
    // `strict_separation`'s own job — rejecting a RED proof that *does*
    // exist but has a missing/matching author identity — is unchanged below.
    //
    // Delegates to `judge_has_red` (the same judgment `judge_green` applies
    // later, once `passed` is known) rather than a second hand-rolled bail,
    // so there is exactly one definition of "no RED proof found".
    judge_has_red(has_red)?;
    let author = resolve_author(author);
    if cfg.strict_separation {
        let test_author = read_author(root, cfg, task, "red");
        judge_separation(true, test_author.as_deref(), Some(&author))?;
    }
    let cmdline = resolve_cmd(cmd, cfg);
    let tmp = cfg.state_dir.join("tmp");
    let out = runner::run_cmd(
        cmdline,
        root,
        cfg.default_timeout_secs,
        cfg.output_tail_lines,
        &tmp,
    );
    judge_green(out.passed, has_red)?;
    let path = artifact_path(root, cfg, task, "green");
    write_artifact(
        &path,
        &json!({
            "task": task,
            "phase": "green",
            "cmd": cmdline,
            "passed": out.passed,
            "exit_code": out.exit_code,
            "ts": chrono::Local::now().to_rfc3339(),
            "red_proof": red_path.display().to_string(),
            "author": author,
        }),
    )?;
    println!(
        "🟢 GREEN recorded for `{task}` — tests pass after RED.\n   {}",
        path.display()
    );
    Ok(())
}

/// `tdd verify`: true iff both RED and GREEN proofs exist for the task.
pub fn verify(root: &Path, cfg: &Config, task: &str) -> bool {
    artifact_path(root, cfg, task, "red").exists()
        && artifact_path(root, cfg, task, "green").exists()
}

/// Fail-soft read of a proof artifact's `passed` field. Returns `None` if the
/// artifact is missing, unreadable, not valid JSON, or lacks a boolean
/// `passed` field — never panics, so an oracle can report `has_red/has_green`
/// honestly instead of crashing a turn.
pub fn read_passed(root: &Path, cfg: &Config, task: &str, kind: &str) -> Option<bool> {
    let path = artifact_path(root, cfg, task, kind);
    let text = std::fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("passed")?.as_bool()
}

impl runner::Outcome {
    fn status_str(&self) -> String {
        if self.timed_out {
            "timed out".to_string()
        } else if let Some(e) = &self.spawn_error {
            e.clone()
        } else {
            match self.exit_code {
                Some(c) => format!("exit {c}"),
                None => "killed".to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn red_requires_failure() {
        assert!(judge_red(false).is_ok()); // failed → good
        assert!(judge_red(true).is_err()); // passed → not test-first
    }

    #[test]
    fn green_requires_red_then_pass() {
        assert!(judge_green(true, true).is_ok());
        assert!(judge_green(true, false).is_err()); // no RED proof
        assert!(judge_green(false, true).is_err()); // still failing
    }

    // ── strict test/impl author separation (Item C) ─────────────────────────
    //
    // Pure gate over (test_author, impl_author, strict_flag) — no LLM in the
    // decision path, mirroring condukt's verifier-model≠worker-model
    // invariant (`resolve_verifier_model`/`same_model`).

    #[test]
    fn separation_allows_when_not_strict_regardless_of_identity() {
        // Backward compatible: strict mode is opt-in. Even identical (or
        // missing) identities are allowed when `strict` is false.
        assert!(judge_separation(false, Some("agent-a"), Some("agent-a")).is_ok());
        assert!(judge_separation(false, None, None).is_ok());
    }

    #[test]
    fn separation_rejects_same_identity_under_strict_mode() {
        assert!(judge_separation(true, Some("agent-a"), Some("agent-a")).is_err());
        // Case-insensitive / whitespace-insensitive identity comparison.
        assert!(judge_separation(true, Some("Agent-A"), Some(" agent-a ")).is_err());
    }

    #[test]
    fn separation_allows_different_identity_under_strict_mode() {
        assert!(judge_separation(true, Some("agent-a"), Some("agent-b")).is_ok());
    }

    #[test]
    fn separation_rejects_missing_identity_under_strict_mode() {
        // Can't verify separation without both identities — fail-closed.
        assert!(judge_separation(true, None, Some("agent-b")).is_err());
        assert!(judge_separation(true, Some("agent-a"), None).is_err());
        assert!(judge_separation(true, None, None).is_err());
        assert!(judge_separation(true, Some(""), Some("agent-b")).is_err());
    }

    // ── identity default (bind to something real; Item 1) ───────────────────
    //
    // Pure, deterministic — no real env mutation, so these are safe under
    // test parallelism (see `default_author_from`/`resolve_author_from`).

    #[test]
    fn default_author_falls_back_to_local_bucket_when_session_id_unset() {
        assert_eq!(default_author_from(None), "_local");
        assert_eq!(default_author_from(Some("".to_string())), "_local");
        assert_eq!(default_author_from(Some("   ".to_string())), "_local");
    }

    #[test]
    fn default_author_uses_trimmed_session_id_when_set() {
        assert_eq!(
            default_author_from(Some("sess-123".to_string())),
            "sess-123"
        );
        assert_eq!(
            default_author_from(Some("  sess-123  ".to_string())),
            "sess-123"
        );
    }

    #[test]
    fn resolve_author_prefers_explicit_author_over_default() {
        // Explicit `--author` always wins, even if it differs from the
        // session-id default — the override still works.
        assert_eq!(
            resolve_author_from(&Some("agent-a".to_string()), || "sess-xyz".to_string()),
            "agent-a"
        );
    }

    #[test]
    fn resolve_author_falls_back_to_default_when_author_absent_or_blank() {
        assert_eq!(
            resolve_author_from(&None, || "sess-xyz".to_string()),
            "sess-xyz"
        );
        assert_eq!(
            resolve_author_from(&Some("   ".to_string()), || "sess-xyz".to_string()),
            "sess-xyz"
        );
    }

    #[test]
    fn same_session_auto_id_rejected_under_strict_separation() {
        // The identity-default gate's actual purpose: two `--author`-less
        // calls from the *same* session (the common case — one agent runs
        // both `tdd red` and `tdd green` without ever passing `--author`)
        // resolve to the SAME identity, and must be rejected under strict
        // separation — this is the "bind to something real" fix, not just an
        // honor-system string.
        let same_session = || "sess-same".to_string();
        let red_author = resolve_author_from(&None, same_session);
        let green_author = resolve_author_from(&None, same_session);
        assert_eq!(red_author, green_author);
        assert!(judge_separation(true, Some(&red_author), Some(&green_author)).is_err());
    }

    #[test]
    fn distinct_session_auto_ids_allowed_under_strict_separation() {
        let red_author = resolve_author_from(&None, || "sess-a".to_string());
        let green_author = resolve_author_from(&None, || "sess-b".to_string());
        assert_ne!(red_author, green_author);
        assert!(judge_separation(true, Some(&red_author), Some(&green_author)).is_ok());
    }

    #[test]
    fn default_off_unchanged_even_with_same_auto_session_id() {
        // strict_separation is opt-in: with it off, same-session auto ids are
        // still allowed (fully backward compatible with pre-existing
        // behaviour/tests).
        let same_session = || "sess-same".to_string();
        let red_author = resolve_author_from(&None, same_session);
        let green_author = resolve_author_from(&None, same_session);
        assert!(judge_separation(false, Some(&red_author), Some(&green_author)).is_ok());
    }

    #[test]
    fn green_rejects_same_author_end_to_end_under_strict_mode() {
        // Exercises the real `green()` wiring: a RED proof recorded with
        // author "agent-a", then `green()` called with the SAME author under
        // strict_separation must be rejected — even before the test command
        // itself would run (fail-closed, checked first).
        let base = std::env::temp_dir().join(format!("tdd-sep-same-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            strict_separation: true,
            ..Config::default()
        };
        write_artifact(
            &artifact_path(&base, &cfg, "t1", "red"),
            &json!({"passed": false, "author": "agent-a"}),
        )
        .unwrap();

        let err = green(
            &base,
            &cfg,
            "t1",
            &Some("true".to_string()),
            &Some("agent-a".to_string()),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("strict_separation"),
            "expected a strict_separation rejection, got: {err}"
        );
        // No GREEN proof should have been written since the gate rejected
        // before the test command ran.
        assert!(!artifact_path(&base, &cfg, "t1", "green").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn green_allows_different_author_end_to_end_under_strict_mode() {
        let base = std::env::temp_dir().join(format!("tdd-sep-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            strict_separation: true,
            ..Config::default()
        };
        write_artifact(
            &artifact_path(&base, &cfg, "t1", "red"),
            &json!({"passed": false, "author": "agent-a"}),
        )
        .unwrap();

        // "true" always exits 0, satisfying judge_green's pass requirement.
        green(
            &base,
            &cfg,
            "t1",
            &Some("true".to_string()),
            &Some("agent-b".to_string()),
        )
        .unwrap();
        assert!(artifact_path(&base, &cfg, "t1", "green").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn green_honors_explicit_author_override_end_to_end_default_off() {
        // Explicit `--author` override still works end-to-end even with
        // strict_separation OFF (default-off unchanged): both red() and
        // green() honor an explicit author and never touch CLAUDE_CODE_SESSION_ID.
        let base = std::env::temp_dir().join(format!("tdd-sep-override-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            strict_separation: false,
            ..Config::default()
        };

        // "false" always exits 1 → satisfies judge_red's must-fail requirement.
        red(
            &base,
            &cfg,
            "t1",
            &Some("false".to_string()),
            &Some("agent-a".to_string()),
        )
        .unwrap();
        assert_eq!(
            read_author(&base, &cfg, "t1", "red").as_deref(),
            Some("agent-a")
        );

        green(
            &base,
            &cfg,
            "t1",
            &Some("true".to_string()),
            &Some("agent-a".to_string()),
        )
        .unwrap();
        assert_eq!(
            read_author(&base, &cfg, "t1", "green").as_deref(),
            Some("agent-a")
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn env_session_id_default_end_to_end_via_real_red_green() {
        // Serialized in a SINGLE #[test] (mirrors config.rs's HOME-mutation
        // test) because CLAUDE_CODE_SESSION_ID is process-global env state and
        // `cargo test` runs tests in parallel by default.
        //
        // Exercises the real `red()`/`green()` wiring end-to-end with NO
        // `--author` passed at all: the recorded identity must default to
        // CLAUDE_CODE_SESSION_ID. Same session id for both RED and GREEN
        // (the common "one agent, no --author" case) is rejected under
        // strict_separation; a different session id for GREEN is allowed.
        let base = std::env::temp_dir().join(format!("tdd-sep-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            strict_separation: true,
            ..Config::default()
        };

        let saved = std::env::var("CLAUDE_CODE_SESSION_ID").ok();

        // Same session for RED and GREEN → rejected (no --author given at all).
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "sess-same-e2e");
        red(&base, &cfg, "t1", &Some("false".to_string()), &None).unwrap();
        assert_eq!(
            read_author(&base, &cfg, "t1", "red").as_deref(),
            Some("sess-same-e2e")
        );
        let err = green(&base, &cfg, "t1", &Some("true".to_string()), &None).unwrap_err();
        assert!(
            err.to_string().contains("strict_separation"),
            "expected same-session auto id to be rejected under strict_separation, got: {err}"
        );
        assert!(!artifact_path(&base, &cfg, "t1", "green").exists());

        // Different session for GREEN → allowed.
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "sess-other-e2e");
        green(&base, &cfg, "t1", &Some("true".to_string()), &None).unwrap();
        assert_eq!(
            read_author(&base, &cfg, "t1", "green").as_deref(),
            Some("sess-other-e2e")
        );

        // Explicit --author override still works, taking priority over env.
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "sess-ignored");
        red(
            &base,
            &cfg,
            "t2",
            &Some("false".to_string()),
            &Some("explicit-author".to_string()),
        )
        .unwrap();
        assert_eq!(
            read_author(&base, &cfg, "t2", "red").as_deref(),
            Some("explicit-author"),
            "explicit --author must override the CLAUDE_CODE_SESSION_ID default"
        );

        match saved {
            Some(v) => std::env::set_var("CLAUDE_CODE_SESSION_ID", v),
            None => std::env::remove_var("CLAUDE_CODE_SESSION_ID"),
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── finding 9: RED-proof existence must be checked before the
    // strict_separation identity check ──────────────────────────────────────
    //
    // Regression coverage for the ordering bug: `tdd green --author X` run
    // without a prior `tdd red` must report "no RED proof found", not the
    // misleading "identity is missing" (even though `--author` was passed).

    #[test]
    fn green_reports_missing_red_proof_not_identity_error_when_red_absent() {
        // (a) No RED proof at all + `--author` given + strict_separation on:
        // must fail with the RED-proof-not-found message, never the identity
        // error (the author WAS supplied correctly).
        let base = std::env::temp_dir().join(format!("tdd-finding9-nored-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            strict_separation: true,
            ..Config::default()
        };

        let err = green(
            &base,
            &cfg,
            "t1",
            &Some("true".to_string()),
            &Some("agent-a".to_string()),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no RED proof found"),
            "expected 'no RED proof found', got: {msg}"
        );
        assert!(
            !msg.contains("identity"),
            "must not misreport as an identity error when the real cause is a missing RED proof, got: {msg}"
        );
        assert!(!artifact_path(&base, &cfg, "t1", "green").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn green_still_reports_identity_error_when_red_exists_but_author_mismatched() {
        // (b) RED proof DOES exist but the author identity is missing/mismatched:
        // strict_separation's own job is unchanged — it must still fail with
        // the identity error (not the RED-proof-not-found message).
        let base =
            std::env::temp_dir().join(format!("tdd-finding9-mismatch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            strict_separation: true,
            ..Config::default()
        };
        write_artifact(
            &artifact_path(&base, &cfg, "t1", "red"),
            &json!({"passed": false, "author": "agent-a"}),
        )
        .unwrap();

        let err = green(
            &base,
            &cfg,
            "t1",
            &Some("true".to_string()),
            &Some("agent-a".to_string()),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("strict_separation"),
            "expected a strict_separation identity rejection, got: {msg}"
        );
        assert!(!artifact_path(&base, &cfg, "t1", "green").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn read_passed_is_fail_soft() {
        let base = std::env::temp_dir().join(format!("tdd-passed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            ..Config::default()
        };
        std::fs::create_dir_all(&base).unwrap();

        // missing artifact → None
        assert_eq!(read_passed(&base, &cfg, "t1", "red"), None);

        // well-formed proofs → the recorded `passed` value
        write_artifact(
            &artifact_path(&base, &cfg, "t1", "red"),
            &json!({"passed": false}),
        )
        .unwrap();
        write_artifact(
            &artifact_path(&base, &cfg, "t1", "green"),
            &json!({"passed": true}),
        )
        .unwrap();
        assert_eq!(read_passed(&base, &cfg, "t1", "red"), Some(false));
        assert_eq!(read_passed(&base, &cfg, "t1", "green"), Some(true));

        // corrupt JSON → None (no panic)
        std::fs::write(artifact_path(&base, &cfg, "t2", "red"), "{not json").unwrap();
        assert_eq!(read_passed(&base, &cfg, "t2", "red"), None);

        // valid JSON but no boolean `passed` → None
        write_artifact(&artifact_path(&base, &cfg, "t3", "red"), &json!({"x": 1})).unwrap();
        assert_eq!(read_passed(&base, &cfg, "t3", "red"), None);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn verify_needs_both_artifacts() {
        let base = std::env::temp_dir().join(format!("tdd-proof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            ..Config::default()
        };
        std::fs::create_dir_all(&base).unwrap();
        assert!(!verify(&base, &cfg, "t1"));
        write_artifact(&artifact_path(&base, &cfg, "t1", "red"), &json!({"x":1})).unwrap();
        assert!(!verify(&base, &cfg, "t1"));
        write_artifact(&artifact_path(&base, &cfg, "t1", "green"), &json!({"x":1})).unwrap();
        assert!(verify(&base, &cfg, "t1"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
