// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-21, measured at rev 644ff8dbc): donegate's Stop gate,
//! with hook cwd = the PRIMARY tree, runs and blocks on another session's
//! unfinished merge there, although this session only READ the primary tree
//! and wrote only in a linked worktree.
//!
//! Observed (session ba440926, 2026-11-21, donegate Stop hook, verbatim
//! excerpt): "🚦 donegate: 2 required check(s) failed ... ✗ lint-changed
//! (exit 1) $ scripts/lint-changed-crates.sh error: key with no value,
//! expected `=` --> crates/blastguard/Cargo.toml:6:9 ... 6 | <<<<<<< HEAD ...
//! lint-changed-crates: scope was the changed crates only (blastguard) ...
//! ✗ test-changed ... test-changed-crates: FAILED for blastguard". The
//! blastguard conflict was another session's in-flight merge in the primary
//! tree. CLAUDE.md §8: a gate demanding another session's change is "帰属のバグ".
//!
//! Path (read from the code at 644ff8dbc):
//!   1. `main.rs::gate_run`: `root = input.cwd_or_current()` — the hook cwd
//!      (the primary tree, where the session was started) is ALWAYS the judged
//!      root. `rootcheck::wrote_in_cwd` only matters when no check is
//!      configured (`if cfg.checks.is_empty() && !wrote_here`); with checks
//!      configured, evaluation proceeds whether or not the session wrote here.
//!   2. `gate::attribute_scope` narrows the tree's change set to this
//!      session's named writes (backlog 02ec6595, pinned GREEN by
//!      tests/foreign_main_dirt_attribution_20261117.rs) — UNLESS
//!      `harness_core::attribution::has_unattributable_write(transcript)` is
//!      true, in which case the FULL change set of the primary tree is kept.
//!   3. `has_unattributable_write` is transcript-global and cwd-blind:
//!      `call_is_unattributable` asks only whether some stage is a writer whose
//!      targets are not exactly named. Neither the `cd <dir>` that precedes the
//!      writer nor the line's recorded `cwd` is compared with the judged root.
//!      So ONE path-less writer run in ANOTHER worktree (observed shapes in
//!      ba440926: `cd /Users/yuki/src/.harness-worktrees/session-ba440926 &&
//!      backlog driver register ...`, `cd ~/.claude/.../memory/ && python3 -
//!      <<'EOF' ...`) widens the primary tree's scope to every foreign change
//!      in it, and the `when_changed` checks run on the foreign merge.
//!
//! Two readings, and how this file tells them apart:
//!  (a) Real attribution defect: a writer whose cwd (after its `cd`) is a
//!      different worktree than the judged root cannot have produced the
//!      root's dirt, so it must not widen the root's scope.
//!  (b) Intended: a path-less writer may write ANY path (a `python3 -`
//!      heredoc can open an absolute path), so any such call anywhere in the
//!      session — including subagents whose cwd is the primary tree — keeps
//!      the full scope fail-closed (CLAUDE.md §3).
//!  Discriminator: the REDs use `cargo fmt`, whose writes are confined to the
//!  cargo workspace of its cwd; under (b) they would still have to block,
//!  under (a) they must not. The controls pin what must stay blocked under
//!  both readings: this session's own Edit in the primary tree, and a
//!  path-less writer whose cwd IS the primary tree.
//!  The read-only control shows a read-only cwd / subagent cwd in the primary
//!  tree is by itself NOT what makes the foreign merge judged (already GREEN).
//!
//! DOES NOT PROVE:
//!  - That a cwd-aware rule alone would have cleared the observed block: the
//!    ba440926 transcript also holds path-less writers with no `cd` and line
//!    cwd = the primary tree (`python3 scripts/check-prose-tests.py ...`,
//!    `askrelay dismiss ...`), which keep the full scope under both readings.
//!  - Anything about `python3 -` heredocs (not exercised here).
//!  - What the fix should be (cwd-aware unattributable rule, or judging only
//!    roots the session wrote — rootcheck::wrote_in_cwd as a gate).
//!
//! Written by an agent that did not implement donegate (CLAUDE.md 2(a)).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);
const MARKER: &str = "check-ran.marker";
const FOREIGN: &str = "crates/blastguard/src/lib.rs";

fn git_st(dir: &Path, args: &[&str]) -> bool {
    harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs")
        .success()
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        git_st(dir, args),
        "git {args:?} failed in {}",
        dir.display()
    );
}

struct Fixture {
    home: PathBuf,
    main: PathBuf,
    wt: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("dg-fmerge-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let main = home.join("main");
        std::fs::create_dir_all(main.join("crates/blastguard/src")).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["config", "user.email", "t@t.com"]);
        git(&main, &["config", "user.name", "t"]);
        std::fs::write(main.join(FOREIGN), "pub fn f() {}\n").unwrap();
        let marker = home.join(MARKER);
        std::fs::write(
            main.join("donegate.toml"),
            format!(
                "max_attempts = 50\n\n[[check]]\nname = \"test-changed\"\n\
                 cmd = \"echo ran > {}; exit 1\"\nwhen_changed = [\"**/*.rs\"]\n",
                marker.display()
            ),
        )
        .unwrap();
        git(&main, &["add", "-A"]);
        git(&main, &["commit", "-qm", "init"]);
        // This session's own linked worktree.
        let wt = home.join("wt");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "session",
                &wt.to_string_lossy(),
            ],
        );
        let main = main.canonicalize().unwrap();
        let wt = wt.canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!("trusted = [\"{}\", \"{}\"]\n", main.display(), wt.display()),
        )
        .unwrap();
        Fixture { home, main, wt }
    }

    fn marker(&self) -> PathBuf {
        self.home.join(MARKER)
    }

    /// ANOTHER session's unfinished merge in the primary tree: a conflicted
    /// (UU) crate source file, MERGE_HEAD present.
    fn foreign_unfinished_merge(&self) {
        let m = &self.main;
        git(m, &["checkout", "-q", "-b", "other"]);
        std::fs::write(m.join(FOREIGN), "pub fn f() { other() }\n").unwrap();
        git(m, &["commit", "-qam", "other side"]);
        git(m, &["checkout", "-q", "main"]);
        std::fs::write(m.join(FOREIGN), "pub fn f() { mine() }\n").unwrap();
        git(m, &["commit", "-qam", "main side"]);
        assert!(
            !git_st(m, &["merge", "-q", "other"]),
            "apparatus: the foreign merge must conflict"
        );
        assert!(
            m.join(".git/MERGE_HEAD").exists(),
            "apparatus: unfinished merge"
        );
        let body = std::fs::read_to_string(m.join(FOREIGN)).unwrap();
        assert!(body.contains("<<<<<<<"), "apparatus: conflict markers");
    }

    fn line(i: usize, sidechain: bool, cwd: &Path, tool: &str, input: serde_json::Value) -> String {
        serde_json::json!({
            "parentUuid": "p", "isSidechain": sidechain, "cwd": cwd.to_string_lossy(),
            "message": {"model": "claude-opus-5", "id": format!("m{i}"), "type": "message",
                "role": "assistant",
                "content": [{"type": "tool_use", "id": format!("t{i}"), "name": tool, "input": input}]}
        })
        .to_string()
    }

    fn write_jsonl(p: &Path, lines: &[String]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
        body.push('\n');
        for l in lines {
            body.push_str(l);
            body.push('\n');
        }
        std::fs::write(p, body).unwrap();
    }

    /// Parent transcript lines `(line cwd, tool, input)` plus, optionally, a
    /// subagent sidechain (`<stem>/subagents/agent-a1.jsonl`).
    fn transcript(&self, parent: &[Call], side: &[Call]) -> PathBuf {
        let d = self.home.join(".claude/projects/-slug");
        let p = d.join("sess.jsonl");
        let lines: Vec<String> = parent
            .iter()
            .enumerate()
            .map(|(i, c)| Self::line(i, false, &c.0, c.1, c.2.clone()))
            .collect();
        Self::write_jsonl(&p, &lines);
        if !side.is_empty() {
            let lines: Vec<String> = side
                .iter()
                .enumerate()
                .map(|(i, c)| Self::line(100 + i, true, &c.0, c.1, c.2.clone()))
                .collect();
            Self::write_jsonl(&d.join("sess/subagents/agent-a1.jsonl"), &lines);
        }
        p
    }

    /// `donegate gate` as the Stop hook with cwd = the primary tree.
    fn stop(&self, transcript: &Path) -> (String, String, String) {
        let payload = serde_json::json!({
            "session_id": "s-fmerge",
            "transcript_path": transcript.to_string_lossy(),
            "cwd": self.main.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_donegate")))
                .arg("gate")
                .current_dir(&self.main)
                .env("HOME", &self.home)
                .env("CLAUDE_CODE_SESSION_ID", "s-fmerge")
                .env_remove("DONEGATE_DISABLE")
                .env_remove("HARNESS_TRUST_ALL")
                .env_remove("HARNESS_WORKTREE_ORIGIN_DIR")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("donegate spawns");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "Stop hook must exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let v = serde_json::from_str::<serde_json::Value>(stdout.trim()).ok();
        let g = |k: &str| {
            v.as_ref()
                .and_then(|v| v.get(k))
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string()
        };
        (
            g("decision"),
            g("reason"),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

type Call = (PathBuf, &'static str, serde_json::Value);

fn bash(cwd: &Path, cmd: &str) -> Call {
    (
        cwd.to_path_buf(),
        "Bash",
        serde_json::json!({ "command": cmd }),
    )
}

fn edit(cwd: &Path, path: &Path) -> Call {
    (
        cwd.to_path_buf(),
        "Edit",
        serde_json::json!({"file_path": path.to_string_lossy(), "old_string": "a", "new_string": "b"}),
    )
}

fn assert_not_judged(f: &Fixture, tp: &Path) {
    let (decision, reason, err) = f.stop(tp);
    assert!(
        decision != "block" && !f.marker().exists(),
        "donegate ran the primary tree's check on ANOTHER session's unfinished merge although \
         this session only read the primary tree and wrote only in its linked worktree: \
         decision={decision:?} marker_exists={} reason={reason:?} err={err:?}",
        f.marker().exists()
    );
}

fn assert_judged(f: &Fixture, tp: &Path) {
    let (decision, reason, err) = f.stop(tp);
    assert!(
        f.marker().exists(),
        "apparatus: the check must run here; err={err:?}"
    );
    assert_eq!(decision, "block", "reason={reason:?} err={err:?}");
}

/// RED (observed shape): line cwd = primary tree (Claude Code's Bash cwd is
/// reset to the session cwd), the path-less writer runs after `cd <linked wt>`;
/// a subagent only reads with cwd = primary tree.
#[test]
fn red_cd_linked_worktree_writer_does_not_widen_primary_scope() {
    let f = Fixture::new();
    f.foreign_unfinished_merge();
    let cmd = format!("cd {} && cargo fmt", f.wt.display());
    let tp = f.transcript(
        &[bash(&f.main, "git status --short"), bash(&f.main, &cmd)],
        &[bash(&f.main, "git log --oneline -3")],
    );
    assert_not_judged(&f, &tp);
}

/// RED variant: the path-less writer's recorded line cwd is the linked
/// worktree itself (no `cd`).
#[test]
fn red_writer_with_cwd_in_linked_worktree_does_not_widen_primary_scope() {
    let f = Fixture::new();
    f.foreign_unfinished_merge();
    let tp = f.transcript(
        &[
            bash(&f.main, "git status --short"),
            bash(&f.wt, "cargo fmt"),
        ],
        &[],
    );
    assert_not_judged(&f, &tp);
}

/// CONTROL (GREEN today, fixed by 02ec6595): a session that only READ the
/// primary tree (parent and subagent cwd there) is not judged on the foreign
/// merge — a read-only cwd alone is not the path.
#[test]
fn control_readonly_cwd_and_subagent_cwd_in_primary_is_not_judged() {
    let f = Fixture::new();
    f.foreign_unfinished_merge();
    let tp = f.transcript(
        &[bash(&f.main, "git status --short")],
        &[bash(&f.main, "git diff --stat")],
    );
    assert_not_judged(&f, &tp);
}

/// CONTROL: this session's own Edit of the file in the primary tree IS judged.
#[test]
fn control_own_edit_in_primary_is_judged() {
    let f = Fixture::new();
    f.foreign_unfinished_merge();
    let tp = f.transcript(&[edit(&f.main, &f.main.join(FOREIGN))], &[]);
    assert_judged(&f, &tp);
}

/// CONTROL: a path-less writer whose cwd IS the primary tree keeps the full
/// scope (fail-closed under both readings).
#[test]
fn control_pathless_writer_in_primary_keeps_full_scope() {
    let f = Fixture::new();
    f.foreign_unfinished_merge();
    let cmd = format!("cd {} && cargo fmt", f.main.display());
    let tp = f.transcript(&[bash(&f.main, &cmd)], &[]);
    assert_judged(&f, &tp);
}
