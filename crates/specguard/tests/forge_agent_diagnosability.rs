// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Pins backlog 704dde46: before the fix, THREE different normalize-agent
//! outcomes (silent agent / output without marker / marker+pass but bad TOML
//! body) all collapsed onto `EXIT_NO_MARKER = 3`, two of them with
//! byte-identical stderr, and nothing persisted the agent's raw output for
//! post-hoc diagnosis.
//!
//! These tests drive the real `specforge` binary against a throwaway git repo
//! with a stub agent (a `bash -c` script), exactly like
//! `forge_integration.rs`, but target the draft-failure diagnosability
//! contract specifically: distinct exit codes, distinct messages, and a
//! transcript file that actually contains the agent's stdout.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(repo: &Path) {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "seed\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "seed"]);
}

/// Write a specforge.toml whose `[agent]` is an arbitrary bash -c script
/// (caller controls stdout/exit precisely, unlike `write_config`'s
/// heredoc-wrapper in forge_integration.rs which always emits at least the
/// agent's own trailing newline).
fn write_config_with_script(repo: &Path, script: &str) {
    let cfg = format!(
        r#"
[project]
name = "Demo"
root = "."

[agent]
command = "bash"
args = ["-c", {script:?}]

[output]
spec_dir = "specs"
sentinel = ".forge-pending"
"#,
    );
    fs::write(repo.join("specforge.toml"), cfg).unwrap();
}

fn forge(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_specforge"))
        .current_dir(repo)
        .args(["--config", "specforge.toml", "--date", "2026-01-01"])
        .args(args)
        .output()
        .expect("specforge runs")
}

fn scratch_dir(repo: &Path) -> PathBuf {
    repo.join(".scratch").join("specforge")
}

/// A valid requirement body + clean trailer, for the happy-path guard.
const GOOD_DRAFT: &str = r#"[[requirement]]
id = "R1"
statement = "同一IPから60s内に5回失敗で429"
acceptance = ["5回目まで通る", "6回目は429", "Retry-Afterヘッダ"]
canon = ["docs/auth.md#rate-limit"]
falsifiable = true

<<<SPEC_DRAFT>>>
rigor: pass
needs_user: no
summary: rate-limit を1要求に正規化"#;

// ── case 1: agent exits 0, writes NOTHING to stdout ─────────────────────────

#[test]
fn silent_agent_exits_10_not_3() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    // Drains stdin, writes zero bytes to stdout, exits 0.
    write_config_with_script(repo, "cat >/dev/null");
    fs::write(repo.join("req.md"), "x\n").unwrap();

    let out = forge(repo, &["draft", "--id", "x", "--req", "req.md"]);
    assert_eq!(
        out.status.code(),
        Some(10),
        "silent agent (exit 0, empty stdout) must map to EXIT_AGENT_SILENT=10, not EXIT_NO_MARKER=3; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!repo.join("specs/x.toml").exists(), "no draft fabricated");
    assert!(
        !repo.join(".forge-pending").exists(),
        "no sentinel for a cannot-determine"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    // The pre-fix message asserted output EXISTED and lacked the marker. That
    // sentence must not appear when stdout was in fact empty.
    assert!(
        !stderr.contains("missing in agent output"),
        "silent-agent stderr must not claim the marker was missing FROM OUTPUT \
         (that phrasing asserts output existed) — got: {stderr}"
    );
}

// ── case 2: agent exits 0, writes text, but no marker ───────────────────────

#[test]
fn output_without_marker_exits_3() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config_with_script(
        repo,
        "cat >/dev/null; printf '%s' 'I forgot the trailer entirely'",
    );
    fs::write(repo.join("req.md"), "x\n").unwrap();

    let out = forge(repo, &["draft", "--id", "x", "--req", "req.md"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "non-empty stdout without the marker must stay EXIT_NO_MARKER=3"
    );
    assert!(!repo.join("specs/x.toml").exists());
    assert!(!repo.join(".forge-pending").exists());
}

// ── case 3: marker + rigor:pass but body is not valid requirement TOML ─────

#[test]
fn marker_pass_but_bad_toml_body_exits_11() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    // No fence, no `[[requirement]]` line, and not valid TOML syntax either
    // (bare prose with no `key = value` shape) -> extract_requirement_toml
    // falls through to "whole body unchanged" and toml::from_str must fail.
    let bad_body = "ここはTOMLではなくただの散文です。コロンも等号も使っていません";
    let script = format!(
        "cat >/dev/null; cat <<'FORGE_EOF'\n{bad_body}\n\n<<<SPEC_DRAFT>>>\nrigor: pass\nneeds_user: no\nsummary: bad body\nFORGE_EOF"
    );
    write_config_with_script(repo, &script);
    fs::write(repo.join("req.md"), "x\n").unwrap();

    let out = forge(repo, &["draft", "--id", "x", "--req", "req.md"]);
    assert_eq!(
        out.status.code(),
        Some(11),
        "marker+rigor:pass with a non-TOML body must map to EXIT_BAD_DRAFT_BODY=11; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !repo.join("specs/x.toml").exists(),
        "no draft written for an invalid body"
    );
    assert!(
        !repo.join(".forge-pending").exists(),
        "a parse failure is an observed contract violation, not an escalation sentinel"
    );
}

// ── case 4: agent exits non-zero (regression guard — must stay 4) ──────────

#[test]
fn agent_nonzero_exit_still_maps_to_4() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config_with_script(repo, "cat >/dev/null; exit 7");
    fs::write(repo.join("req.md"), "x\n").unwrap();

    let out = forge(repo, &["draft", "--id", "x", "--req", "req.md"]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "agent nonzero exit must still map to EXIT_AGENT_FAILED=4 (unchanged by the diagnosability fix)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("code 7"),
        "true agent code on stderr: {stderr}"
    );
}

// ── case 5: the three draft-failure outcomes are mutually distinguishable ──

#[test]
fn silent_no_marker_and_bad_body_are_pairwise_distinct() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    fs::write(repo.join("req.md"), "x\n").unwrap();

    write_config_with_script(repo, "cat >/dev/null");
    let silent = forge(repo, &["draft", "--id", "a", "--req", "req.md"]);

    write_config_with_script(repo, "cat >/dev/null; printf '%s' 'no trailer here'");
    let no_marker = forge(repo, &["draft", "--id", "b", "--req", "req.md"]);

    let bad_body = "散文のみ、キーも等号も無い";
    let script = format!(
        "cat >/dev/null; cat <<'FORGE_EOF'\n{bad_body}\n\n<<<SPEC_DRAFT>>>\nrigor: pass\nneeds_user: no\nsummary: bad\nFORGE_EOF"
    );
    write_config_with_script(repo, &script);
    let bad = forge(repo, &["draft", "--id", "c", "--req", "req.md"]);

    let codes = [
        silent.status.code(),
        no_marker.status.code(),
        bad.status.code(),
    ];
    assert_eq!(
        codes,
        [Some(10), Some(3), Some(11)],
        "three distinct exit codes expected"
    );
    assert_ne!(codes[0], codes[1]);
    assert_ne!(codes[0], codes[2]);
    assert_ne!(codes[1], codes[2]);

    // The specific regression: silent-agent and no-marker stderr used to be
    // byte-identical because they shared one code path. They must differ now.
    let silent_stderr = String::from_utf8_lossy(&silent.stderr).into_owned();
    let no_marker_stderr = String::from_utf8_lossy(&no_marker.stderr).into_owned();
    assert_ne!(
        silent_stderr, no_marker_stderr,
        "silent-agent and no-marker stderr must be distinguishable (pre-fix: byte-identical)"
    );
}

// ── case 6: a failing draft persists a transcript the message can point to ─

#[test]
fn failing_draft_persists_transcript_containing_agent_stdout() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    let distinctive_marker = "UNIQUE_STDOUT_PAYLOAD_7f3a9c";
    let script = format!("cat >/dev/null; printf '%s' '{distinctive_marker}'");
    write_config_with_script(repo, &script);
    fs::write(repo.join("req.md"), "x\n").unwrap();

    let out = forge(
        repo,
        &["draft", "--id", "transcript-case", "--req", "req.md"],
    );
    // Output-without-marker (exit 3) — a real failure, so a transcript must exist.
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    let dir = scratch_dir(repo);
    assert!(dir.is_dir(), "transcript dir must exist: {}", dir.display());
    let entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("transcript-case-") && n.ends_with("-agent.txt"))
        })
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "exactly one transcript file for this draft, found: {entries:?}"
    );
    let transcript_path = &entries[0];
    let transcript = fs::read_to_string(transcript_path).unwrap();
    assert!(
        transcript.contains(distinctive_marker),
        "transcript must contain the agent's actual stdout bytes:\n{transcript}"
    );

    // The path named in stderr must be the file that actually exists on disk —
    // not a path that looks plausible but was never written.
    let printed_path = transcript_path.display().to_string();
    assert!(
        stderr.contains(&printed_path),
        "stderr must name the real transcript path ({printed_path}); stderr:\n{stderr}"
    );
    assert!(
        transcript_path.exists(),
        "the path printed in stderr must correspond to a file that exists"
    );
}

#[test]
fn transcript_written_even_for_silent_agent() {
    // The silent-agent (exit 10) case has empty stdout, so the transcript is
    // mostly headers — but it must still be written and named, since "we saw
    // nothing" is itself worth recording for post-hoc diagnosis.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config_with_script(repo, "cat >/dev/null");
    fs::write(repo.join("req.md"), "x\n").unwrap();

    let out = forge(repo, &["draft", "--id", "silent-case", "--req", "req.md"]);
    assert_eq!(out.status.code(), Some(10));
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    let dir = scratch_dir(repo);
    let entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("silent-case-") && n.ends_with("-agent.txt"))
        })
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "transcript written even when stdout was empty"
    );
    let printed_path = entries[0].display().to_string();
    assert!(
        stderr.contains(&printed_path),
        "stderr must name the transcript path for the silent case too; stderr:\n{stderr}"
    );
}

// ── case 7: happy path unaffected (regression guard) ────────────────────────

#[test]
fn happy_path_still_writes_draft_and_exits_0() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    let script = format!("cat >/dev/null; cat <<'FORGE_EOF'\n{GOOD_DRAFT}\nFORGE_EOF");
    write_config_with_script(repo, &script);
    fs::write(repo.join("req.md"), "ログインを制限したい\n").unwrap();

    let out = forge(
        repo,
        &[
            "draft",
            "--id",
            "login",
            "--req",
            "req.md",
            "--canon",
            "docs/auth.md#rate-limit",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let spec = fs::read_to_string(repo.join("specs/login.toml")).unwrap();
    assert!(
        spec.contains("status = \"draft\""),
        "draft written:\n{spec}"
    );
    assert!(spec.contains("Retry-After"));
    assert!(
        !repo.join(".forge-pending").exists(),
        "clean rigor -> no sentinel"
    );
}
