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

/// Decide whether a GREEN run is acceptable: a RED proof must exist and the
/// tests MUST now pass.
fn judge_green(passed: bool, has_red: bool) -> Result<()> {
    if !has_red {
        bail!("no RED proof found — run `tdd red --task <id>` before implementing.");
    }
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
///   under strict mode, since separation can't be verified without them.
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
/// `author` is an optional identity for the implementation step. Under
/// `cfg.strict_separation` it is compared against the RED proof's recorded
/// author and rejected (fail-closed) if they are the same identity — see
/// [`judge_separation`]. When `strict_separation` is off (the default) this
/// check is skipped entirely, so behaviour is unchanged.
pub fn green(
    root: &Path,
    cfg: &Config,
    task: &str,
    cmd: &Option<String>,
    author: &Option<String>,
) -> Result<()> {
    let red_path = artifact_path(root, cfg, task, "red");
    let has_red = red_path.exists();
    if cfg.strict_separation {
        let test_author = read_author(root, cfg, task, "red");
        judge_separation(true, test_author.as_deref(), author.as_deref())?;
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
