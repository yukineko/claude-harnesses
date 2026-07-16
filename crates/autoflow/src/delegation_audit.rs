//! Tier 2 delegation-record advisory: an independent, non-LLM-controlled check
//! that inspects the session's own transcript (Stop hook's `transcript_path`)
//! to catch a `/flow`-driven condukt run completing without the LLM ever
//! calling `fugu-router record` for it. Tier 1 (`fugu-router audit-recent`)
//! relies on the LLM remembering to self-verify; this tier catches the case
//! where it forgets to even try.

use std::path::Path;

/// Fail-soft check: does this session's transcript show `/flow` drove a
/// condukt run to completion without ever calling `fugu-router record` (with
/// `--class flow-delegation` or `--delegation`)? Returns `false` on any
/// read/parse failure or when any of the three firing conditions isn't met —
/// never panics, never blocks the Stop turn.
///
/// Firing requires ALL of:
/// 1. The transcript contains `"backlog lock acquire"` — proof this session
///    was `/flow`-driven (flow's SKILL.md Step 2 always calls this).
/// 2. This session's condukt run reached a terminal state on at least one
///    task (`condukt::has_completed_tasks`) — a completion happened.
/// 3. The transcript has no line mentioning both `"fugu-router record"` and
///    (`"flow-delegation"` or `"--delegation"`) — no evidence a delegation
///    record call was ever made.
pub fn missing_delegation_record(transcript_path: &str, cwd: &Path) -> bool {
    if transcript_path.is_empty() {
        return false;
    }
    let text = match std::fs::read_to_string(transcript_path) {
        Ok(t) => t,
        Err(_) => return false,
    };

    let flow_driven = text.contains("backlog lock acquire");
    if !flow_driven {
        return false;
    }

    if !crate::condukt::has_completed_tasks(cwd) {
        return false;
    }

    let has_record = text.lines().any(|l| {
        l.contains("fugu-router record")
            && (l.contains("flow-delegation") || l.contains("--delegation"))
    });

    !has_record
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests exercise `missing_delegation_record`'s transcript-reading half
    // directly; they don't touch `$HOME`, so no `test_home_guard` is needed —
    // but they DO call `condukt::has_completed_tasks`, which reads
    // `$HOME/.condukt/state/...`. To keep condition 2 controllable without a
    // real condukt run on disk, we point `cwd` at a repo with no `.condukt`
    // state at all (so `has_completed_tasks` is deterministically `false`)
    // except where a test explicitly needs it `true`, in which case it reuses
    // condukt.rs's own HOME-mutating test harness pattern inline.

    fn write_transcript(dir: &std::path::Path, lines: &[&str]) -> String {
        let path = dir.join("transcript.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn not_flow_driven_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(dir.path(), &["some ordinary session line"]);
        // Whatever cwd/condukt state, condition 1 fails first.
        assert!(!missing_delegation_record(&path, dir.path()));
    }

    #[test]
    fn unreadable_transcript_path_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.jsonl");
        assert!(!missing_delegation_record(
            missing.to_str().unwrap(),
            dir.path()
        ));
    }

    #[test]
    fn empty_transcript_path_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!missing_delegation_record("", dir.path()));
    }

    #[test]
    fn flow_driven_but_no_completed_tasks_returns_false() {
        let _guard = crate::test_home_guard();
        let home_dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home_dir.path());
        let repo = home_dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        // No .condukt run state at all → has_completed_tasks is false.

        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(dir.path(), &["backlog lock acquire"]);
        assert!(!missing_delegation_record(&path, &repo));
    }

    #[test]
    fn flow_driven_and_completed_but_record_present_returns_false() {
        let _guard = crate::test_home_guard();
        let home_dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home_dir.path());
        let repo = home_dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let key = harness_core::projkey::project_key(&harness_core::projkey::repo_root(&repo));
        let run_dir = home_dir.path().join(".condukt").join("state").join(&key);
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("run-0001.json"),
            r#"{"run_id":"r1","goal":"g","tasks":[{"id":"t1","status":"verified"}]}"#,
        )
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &[
                "backlog lock acquire",
                r#"tool_call: fugu-router record --class flow-delegation --delegation fork"#,
            ],
        );
        assert!(!missing_delegation_record(&path, &repo));
    }

    #[test]
    fn flow_driven_and_completed_and_no_record_returns_true() {
        let _guard = crate::test_home_guard();
        let home_dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home_dir.path());
        let repo = home_dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let key = harness_core::projkey::project_key(&harness_core::projkey::repo_root(&repo));
        let run_dir = home_dir.path().join(".condukt").join("state").join(&key);
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("run-0001.json"),
            r#"{"run_id":"r1","goal":"g","tasks":[{"id":"t1","status":"verified"}]}"#,
        )
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(
            dir.path(),
            &["backlog lock acquire", "some other tool call"],
        );
        assert!(missing_delegation_record(&path, &repo));
    }

    /// False-positive avoidance: an ordinary condukt run driven WITHOUT `/flow`
    /// (no "backlog lock acquire" anywhere in the transcript) must never trigger
    /// the advisory, even when it completed tasks and never called
    /// `fugu-router record` — because it was never expected to.
    #[test]
    fn ordinary_non_flow_condukt_run_never_fires() {
        let _guard = crate::test_home_guard();
        let home_dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home_dir.path());
        let repo = home_dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let key = harness_core::projkey::project_key(&harness_core::projkey::repo_root(&repo));
        let run_dir = home_dir.path().join(".condukt").join("state").join(&key);
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("run-0001.json"),
            r#"{"run_id":"r1","goal":"g","tasks":[{"id":"t1","status":"verified"}]}"#,
        )
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        // A plain `/condukt` invocation transcript: tasks completed, no
        // delegation record call, but crucially no "backlog lock acquire" —
        // this session was never /flow-driven.
        let path = write_transcript(
            dir.path(),
            &[
                "user ran /condukt directly",
                "condukt state set --status verified",
            ],
        );
        assert!(
            !missing_delegation_record(&path, &repo),
            "non-/flow condukt runs must never trigger the delegation advisory"
        );
    }
}
