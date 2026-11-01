// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `condukt circuit check --run flow-S --session S` gains a compaction axis:
//! once session S's transcript holds `--max-compactions N` (default 2, N=0
//! clamped to 1) top-level `"subtype":"compact_boundary"` system lines, the
//! circuit trips with reason `session_compacted` so /flow hands off to a fresh
//! session instead of replaying an ever-compacted context.
//!
//! Driven through the binary so a missing implementation is a RUNTIME failure.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

const SESSION: &str = "sess-circuit-compact-e2e";
const COMPACTED: &str = "session_compacted";

/// Shape copied from a real Claude Code transcript (`type`/`subtype`/
/// `content`/`compactMetadata` at top level).
const BOUNDARY: &str = r#"{"parentUuid":null,"isSidechain":false,"type":"system","subtype":"compact_boundary","content":"Conversation compacted","level":"info","compactMetadata":{"trigger":"auto","preTokens":272284,"postTokens":12126},"sessionId":"sess-circuit-compact-e2e"}"#;
const USER: &str = r#"{"type":"user","message":{"role":"user","content":"hello"},"sessionId":"sess-circuit-compact-e2e"}"#;

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-circuit-compact-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        Self { base, repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        harness_core::git_env::scrub(&mut Command::new(bin()))
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .output()
            .expect("spawn condukt")
    }

    /// `<home>/.claude/projects/-fixture/<SESSION>.jsonl`, written fresh
    /// (mtime = now, so the idle axis never trips).
    fn write_transcript(&self, lines: &[&str]) -> PathBuf {
        let dir = self.home.join(".claude").join("projects").join("-fixture");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{SESSION}.jsonl"));
        let mut body = String::new();
        for l in lines {
            body.push_str(l);
            body.push('\n');
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Run `circuit check --run flow-S --session S` plus `extra` flags and
    /// return the exit code and parsed stdout JSON.
    fn circuit(&self, extra: &[&str]) -> (i32, serde_json::Value) {
        self.circuit_with_idle_ttl("1800", extra)
    }

    /// Run `circuit check` with a custom idle-ttl-secs (overrides default 1800).
    fn circuit_with_idle_ttl(&self, idle_ttl: &str, extra: &[&str]) -> (i32, serde_json::Value) {
        let run = format!("flow-{SESSION}");
        let mut args = vec![
            "circuit",
            "check",
            "--run",
            run.as_str(),
            "--idle-ttl-secs",
            idle_ttl,
            "--session",
            SESSION,
        ];
        args.extend_from_slice(extra);
        let out = self.condukt(&args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!(
                "circuit check stdout is not JSON ({e}); exit={:?} stdout={stdout} stderr={}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code().expect("exit code"), v)
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let ok = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

fn assert_trips_compacted(code: i32, v: &serde_json::Value) {
    assert_ne!(code, 0, "a compaction trip must exit nonzero: {v}");
    assert_eq!(v["verdict"], "trip", "{v}");
    assert_eq!(v["reason"], COMPACTED, "{v}");
}

fn assert_continues(code: i32, v: &serde_json::Value) {
    assert_eq!(code, 0, "must continue: {v}");
    assert_eq!(v["verdict"], "continue", "{v}");
}

#[test]
fn two_compactions_trip_session_compacted_by_default() {
    let fx = Fixture::new("two-default");
    fx.write_transcript(&[USER, BOUNDARY, USER, BOUNDARY, USER]);
    let (code, v) = fx.circuit(&[]);
    assert_trips_compacted(code, &v);
}

#[test]
fn one_compaction_continues_by_default() {
    let fx = Fixture::new("one-default");
    fx.write_transcript(&[USER, BOUNDARY, USER]);
    let (code, v) = fx.circuit(&[]);
    assert_continues(code, &v);
}

#[test]
fn zero_compactions_continue_control() {
    // Control: the fixture itself (fresh transcript, vouched session) is a
    // continue, so a trip in the other tests comes from the boundary lines.
    let fx = Fixture::new("zero-default");
    fx.write_transcript(&[USER, USER]);
    let (code, v) = fx.circuit(&[]);
    assert_continues(code, &v);
}

#[test]
fn max_compactions_five_lets_two_continue() {
    let fx = Fixture::new("two-max5");
    fx.write_transcript(&[USER, BOUNDARY, USER, BOUNDARY, USER]);
    let (code, v) = fx.circuit(&["--max-compactions", "5"]);
    assert_continues(code, &v);
}

#[test]
fn max_compactions_five_trips_at_five() {
    let fx = Fixture::new("five-max5");
    fx.write_transcript(&[BOUNDARY, BOUNDARY, BOUNDARY, BOUNDARY, BOUNDARY]);
    let (code, v) = fx.circuit(&["--max-compactions", "5"]);
    assert_trips_compacted(code, &v);
}

#[test]
fn max_compactions_zero_is_clamped_to_one_not_disabled() {
    let fx = Fixture::new("one-max0");
    fx.write_transcript(&[USER, BOUNDARY, USER]);
    let (code, v) = fx.circuit(&["--max-compactions", "0"]);
    assert_trips_compacted(code, &v);
}

#[test]
fn max_compactions_zero_with_no_compaction_still_continues() {
    // The clamp floor is 1, not "always trip": zero boundaries stay continue.
    let fx = Fixture::new("zero-max0");
    fx.write_transcript(&[USER, USER]);
    let (code, v) = fx.circuit(&["--max-compactions", "0"]);
    assert_continues(code, &v);
}

#[test]
fn compact_boundary_text_inside_a_user_message_does_not_count() {
    let fx = Fixture::new("text-only");
    // Three lines that mention compact_boundary only inside string content,
    // including the escaped `"subtype":"compact_boundary"` form; none is a
    // top-level `subtype` field. Three >= default 2, so counting any of them
    // as a boundary would trip.
    let plain =
        r#"{"type":"user","message":{"role":"user","content":"why did compact_boundary appear?"}}"#;
    let escaped = r#"{"type":"user","message":{"role":"user","content":"grep '{\"type\":\"system\",\"subtype\":\"compact_boundary\"}'"}}"#;
    let assistant = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"\"subtype\":\"compact_boundary\" marks a compaction"}]}}"#;
    fx.write_transcript(&[plain, escaped, assistant]);
    let (code, v) = fx.circuit(&[]);
    assert_continues(code, &v);
}

#[test]
fn compacted_reason_slug_is_stable_and_distinct_from_idle() {
    // Slug stability through the binary: the stdout reason is exactly
    // `session_compacted`, not `stall` / `idle_unmeasured`.
    let fx = Fixture::new("slug");
    fx.write_transcript(&[BOUNDARY, BOUNDARY, BOUNDARY]);
    let (code, v) = fx.circuit(&[]);
    assert_trips_compacted(code, &v);
    assert_ne!(v["reason"], "stall", "{v}");
    assert_ne!(v["reason"], "idle_unmeasured", "{v}");
}

#[test]
fn unreadable_transcript_trips_even_with_idle_axis_off() {
    let fx = Fixture::new("unreadable-idle-off");
    let transcript_path = fx.write_transcript(&[USER]);

    // Make the transcript unreadable (chmod 000).
    #[cfg(unix)]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&transcript_path, Permissions::from_mode(0o000)).unwrap();
    }

    // Run with idle-ttl-secs 0 (idle axis off).
    // This should still trip because compactions_unknown_reason is present.
    let (code, v) = fx.circuit_with_idle_ttl("0", &[]);

    // Expected: trips because compactions is unmeasurable, regardless of idle setting
    assert_ne!(code, 0, "should trip even with idle axis off: {v}");
    assert_eq!(v["verdict"], "trip", "{v}");
    assert!(
        v["compactions_unknown_reason"].is_string(),
        "compactions_unknown_reason should be non-null: {v}"
    );
}
