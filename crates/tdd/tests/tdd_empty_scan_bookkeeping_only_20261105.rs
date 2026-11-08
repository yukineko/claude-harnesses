#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! NOTE 2026-11-08 (user ruling 2026-11-08: gates judge own worktree only): the
//! rule this file was written against ("the session wrote files but no
//! implementation change was evaluated in any root", the `sr.roots.len() > 1
//! && sr.wrote_any && !evaluated_something` arm and `session_roots`) has been
//! REMOVED from the tdd Stop hook, which now judges only the hook cwd checkout.
//! Part (a) below therefore passes trivially and is kept as a regression guard
//! that bookkeeping sessions are never blocked; part (b) is inverted/re-scoped
//! (see its header). The text below is the 2026-11-05 history.
//!
//! Spec (user ruling 2026-11-05, session 2d8f4149, main 09508f0b): a session
//! whose writes are only integration / bookkeeping must NOT be blocked by the
//! rule "the session wrote files but no implementation change was evaluated in
//! any root (an empty scan is not a pass)" (crates/tdd/src/main.rs, the
//! `sr.roots.len() > 1 && sr.wrote_any && !evaluated_something` arm).
//! Bookkeeping here means: `git merge` (ff-only / no-ff), `backlog done` plus a
//! commit of `.backlog/*.toml`, running a script that writes outside every
//! repository (`scripts/rollout-plugins.sh`), and tool writes to files outside
//! every git repository.
//!
//! Observed mechanism (pinned by experiment in this file, not assumed):
//! `harness_core::session_roots::resolve` sets `wrote_any` when
//! `SessionWrites::bash` is non-empty, and `session_written_paths`
//! (crates/harness-core/src/attribution.rs) puts into `bash` every existing
//! path word of every Bash statement that `read_only_stage` does not list as
//! read-only. `git merge` / `git add` / `git commit` / `backlog` / a script are
//! not on that list, and the `cd <worktree>` before a kept statement is kept
//! too, so `cd <wt> && git merge --ff-only main` alone yields both a second
//! root (the worktree) and `wrote_any = true`. No implementation line is in any
//! scan, so the rule blocks.
//!
//! The fail-closed intent must survive (CLAUDE.md section 3): when the session
//! DID write an implementation file and the scan is empty or undetermined, the
//! gate still blocks. The `*_still_blocks` tests below guard against an
//! over-broad fix ("allow whenever only git/backlog ran", "allow every empty
//! scan").
//!
//! Layout mirrors the observed session: hook cwd = the primary checkout, the
//! session worked in a linked worktree. Dates are pinned as in
//! tests/committed_session_work_is_evaluated.rs. `$HOME` is a per-test tempdir.
//! Written by a disinterested test author (CLAUDE.md 2(a)), not the implementer.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

const IMPL: &str =
    "pub fn f() -> i32 {\n    let x = 1;\n    let y = x + 1;\n    println!(\"{}\", y);\n    y\n}\n";
const TASKS: &str = "[[task]]\nid = \"3b3c22a5\"\ntitle = \"x\"\nstatus = \"open\"\n";
const TASKS_AFTER: &str = "";
const DONE_AFTER: &str = "[[task]]\nid = \"3b3c22a5\"\ntitle = \"x\"\nstatus = \"done\"\n";

const SEED_DATE: &str = "2026-01-01T00:00:00Z";
const PRE_SESSION: &str = "2026-05-01T00:00:00Z";
const SESSION_START: &str = "2026-06-01T05:00:00.000Z";
const IN_SESSION_TOOL: &str = "2026-06-01T05:10:00.000Z";
const IN_SESSION_COMMIT: &str = "2026-06-01T05:20:00Z";
const SESSION_LAST: &str = "2026-06-01T05:30:00.000Z";

const EMPTY_SCAN_MSG: &str = "no implementation change was evaluated";

struct Fixture {
    home: PathBuf,
    primary: PathBuf,
    wt: PathBuf,
    /// A directory outside every git repository (stands in for
    /// ~/.claude/plugins/cache, ~/.condukt, ...).
    outside: PathBuf,
    recs: Vec<serde_json::Value>,
    k: u32,
    /// false: strip every `timestamp` from the transcript (session start
    /// undetermined).
    timestamps: bool,
}

fn git(dir: &Path, args: &[&str]) {
    git_env(dir, args, &[]);
}

fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) {
    let mut c = Command::new("git");
    harness_core::git_env::scrub(&mut c);
    c.current_dir(dir).args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let st = c
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?} failed in {}", dir.display());
}

fn dated(date: &str) -> [(&'static str, String); 2] {
    [
        ("GIT_AUTHOR_DATE", date.to_string()),
        ("GIT_COMMITTER_DATE", date.to_string()),
    ]
}

fn git_at(dir: &Path, args: &[&str], date: &str) {
    let d = dated(date);
    let env: Vec<(&str, &str)> = d.iter().map(|(k, v)| (*k, v.as_str())).collect();
    git_env(dir, args, &env);
}

fn commit_all_at(dir: &Path, msg: &str, date: &str) {
    git(dir, &["add", "-A"]);
    git_at(dir, &["commit", "-qm", msg], date);
}

impl Fixture {
    fn new() -> Fixture {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("tdd-bookkeeping-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let primary = home.join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "-q", "-b", "main"]);
        git(&primary, &["config", "user.email", "t@t.com"]);
        git(&primary, &["config", "user.name", "t"]);
        git(&primary, &["config", "commit.gpgsign", "false"]);
        std::fs::write(primary.join("seed.txt"), "seed\n").unwrap();
        std::fs::create_dir_all(primary.join("src")).unwrap();
        std::fs::write(primary.join("src/lib.rs"), IMPL).unwrap();
        std::fs::create_dir_all(primary.join("scripts")).unwrap();
        std::fs::write(
            primary.join("scripts/rollout-plugins.sh"),
            "#!/bin/sh\necho rollout\n",
        )
        .unwrap();
        std::fs::create_dir_all(primary.join(".backlog")).unwrap();
        std::fs::write(primary.join(".backlog/tasks.toml"), TASKS).unwrap();
        std::fs::write(primary.join(".backlog/tasks.done.toml"), "").unwrap();
        commit_all_at(&primary, "init", SEED_DATE);
        let wt = home.join("session-wt");
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "session-branch",
                wt.to_str().unwrap(),
            ],
        );
        // Work landed on main before the session (another session's merge):
        // the session's `git merge --ff-only main` brings it into the worktree.
        std::fs::write(primary.join("seed.txt"), "seed\nmore\n").unwrap();
        commit_all_at(&primary, "earlier work on main", PRE_SESSION);
        let wt = wt.canonicalize().unwrap();
        let primary = primary.canonicalize().unwrap();
        let outside = home.join("outside-any-repo/plugins/cache");
        std::fs::create_dir_all(&outside).unwrap();
        let outside = outside.canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".harness")).unwrap();
        std::fs::write(
            home.join(".harness/trust.toml"),
            format!(
                "trusted = [\"{}\", \"{}\"]\n",
                primary.display(),
                wt.display()
            ),
        )
        .unwrap();
        let recs = vec![
            serde_json::json!({"type": "summary", "summary": "s", "leafUuid": "x"}),
            serde_json::json!({
                "type": "user", "uuid": "u0", "sessionId": "s-bk",
                "timestamp": SESSION_START,
                "message": {"role": "user", "content": "integrate"}
            }),
        ];
        Fixture {
            home,
            primary,
            wt,
            outside,
            recs,
            k: 0,
            timestamps: true,
        }
    }

    fn tool(&mut self, name: &str, input: serde_json::Value, cwd: &Path) {
        self.k += 1;
        let id = format!("m{}", self.k);
        self.recs.push(serde_json::json!({
            "type": "assistant", "parentUuid": "u0", "isSidechain": false,
            "sessionId": "s-bk", "cwd": cwd.to_string_lossy(),
            "timestamp": IN_SESSION_TOOL,
            "message": {"model": "claude-opus-5", "id": id, "type": "message",
                "role": "assistant",
                "content": [{"type": "tool_use", "id": format!("t{}", self.k),
                    "name": name, "input": input}]}
        }));
    }

    /// Record a Bash call (the command is NOT executed by this helper; the
    /// caller performs its effect on the fixture to match).
    fn bash(&mut self, command: &str, cwd: &Path) {
        self.tool("Bash", serde_json::json!({"command": command}), cwd);
    }

    fn write_tool(&mut self, path: &Path, cwd: &Path) {
        self.tool(
            "Write",
            serde_json::json!({"file_path": path.to_string_lossy(), "content": "x"}),
            cwd,
        );
    }

    // ---- bookkeeping actions: record the call AND perform its effect ----

    /// `cd <wt> && git merge --ff-only main` (hook cwd = primary).
    fn merge_ff_in_worktree(&mut self) {
        let cmd = format!("cd {} && git merge --ff-only main", self.wt.display());
        self.bash(&cmd, &self.primary.clone());
        git(&self.wt.clone(), &["merge", "--ff-only", "-q", "main"]);
    }

    /// `git merge --no-ff -m ... session-branch` in the primary tree, after a
    /// `git -C <wt>` look (read-only) — a merge commit in main.
    fn merge_no_ff_in_primary(&mut self) {
        let wt = self.wt.clone();
        std::fs::write(wt.join("NOTES.txt"), "integration note\n").unwrap();
        commit_all_at(&wt, "notes", PRE_SESSION);
        let cmd = format!(
            "cd {} && git merge --no-ff -m 'Merge session' session-branch",
            self.primary.display()
        );
        self.bash(&cmd, &self.primary.clone());
        git_at(
            &self.primary.clone(),
            &[
                "merge",
                "--no-ff",
                "-q",
                "-m",
                "Merge session",
                "session-branch",
            ],
            IN_SESSION_COMMIT,
        );
    }

    /// `backlog done 3b3c22a5` then `git add .backlog/... && git commit`,
    /// both run in the worktree (hook cwd = primary).
    fn backlog_done_and_commit(&mut self) {
        let wt = self.wt.clone();
        self.bash(
            &format!(
                "cd {} && backlog done 3b3c22a5 --note 'F->P at 07e9814c'",
                wt.display()
            ),
            &self.primary.clone(),
        );
        std::fs::write(wt.join(".backlog/tasks.toml"), TASKS_AFTER).unwrap();
        std::fs::write(wt.join(".backlog/tasks.done.toml"), DONE_AFTER).unwrap();
        self.bash(
            "git add .backlog/tasks.toml .backlog/tasks.done.toml && git commit -m 'backlog: done 3b3c22a5'",
            &wt,
        );
        git(
            &wt,
            &["add", ".backlog/tasks.toml", ".backlog/tasks.done.toml"],
        );
        git_at(
            &wt,
            &["commit", "-qm", "backlog: done 3b3c22a5"],
            IN_SESSION_COMMIT,
        );
    }

    /// `scripts/rollout-plugins.sh` run in the worktree; it writes only
    /// outside every repository (the plugin cache stand-in).
    fn rollout_writes_outside(&mut self) {
        let wt = self.wt.clone();
        self.bash("scripts/rollout-plugins.sh", &wt);
        std::fs::write(self.outside.join("deployed.json"), "{}\n").unwrap();
    }

    /// condukt/overwatch/fugu-router state writes + a Write tool call, all to
    /// a file outside every git repository; plus `tdd skip`.
    fn state_writes_outside(&mut self) {
        let wt = self.wt.clone();
        let out = self.outside.join("state.json");
        std::fs::write(&out, "{}\n").unwrap();
        self.write_tool(&out, &wt);
        self.bash(
            &format!("condukt state set-status t2 done --state {}", out.display()),
            &wt,
        );
        self.bash("tdd skip --reason 'integration only'", &wt);
    }

    fn finish(&mut self) -> PathBuf {
        self.recs.push(serde_json::json!({
            "type": "assistant", "parentUuid": "u0", "isSidechain": false,
            "sessionId": "s-bk", "timestamp": SESSION_LAST,
            "message": {"model": "claude-opus-5", "id": "mz", "type": "message",
                "role": "assistant", "content": [{"type": "text", "text": "done"}]}
        }));
        let d = self.home.join(".claude/projects/-slug");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("sess.jsonl");
        let mut body = String::new();
        for r in &self.recs {
            let mut r = r.clone();
            if !self.timestamps {
                if let Some(o) = r.as_object_mut() {
                    o.remove("timestamp");
                }
            }
            body.push_str(&r.to_string());
            body.push('\n');
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Run the real `tdd gate` with hook cwd = primary; (stdout, stderr).
    fn stop(&mut self) -> (String, String) {
        let cwd = self.primary.clone();
        self.stop_at(&cwd)
    }

    /// Run the real `tdd gate` with hook cwd = `cwd`; (stdout, stderr).
    fn stop_at(&mut self, cwd: &Path) -> (String, String) {
        let transcript = self.finish();
        let payload = serde_json::json!({
            "session_id": "s-bk",
            "transcript_path": transcript.to_string_lossy(),
            "cwd": cwd.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string();
        let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
            .arg("gate")
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "s-bk")
            .env_remove("TDD_DISABLE")
            .env_remove("HARNESS_TRUST_ALL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("tdd spawns");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "Stop hook must exit 0");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn blocks(stdout: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .and_then(|v| {
            v.get("decision")
                .and_then(|d| d.as_str().map(str::to_string))
        })
        .as_deref()
        == Some("block")
}

fn assert_allowed(what: &str, stdout: &str, stderr: &str) {
    assert!(
        !stdout.contains(EMPTY_SCAN_MSG),
        "{what}: a bookkeeping-only session must not get the empty-scan block; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !blocks(stdout),
        "{what}: a bookkeeping-only session must not be blocked at all; \
         stdout={stdout:?} stderr={stderr:?}"
    );
}

// ---------------------------------------------------------------------------
// (a) bookkeeping-only sessions must be allowed. Observed at 09508f0b: the
// ff-merge, backlog, rollout and full-integration tests FAIL (RED); the no-ff
// merge in the primary tree and the outside-repo-only writes PASS already
// (they add no second root, so the rule's `roots.len() > 1` arm is not
// reached) and stay as controls so a fix cannot regress them.
// ---------------------------------------------------------------------------

/// RED at 09508f0b: the `cd <wt>` word is kept as a write candidate.
#[test]
fn ff_merge_in_worktree_only_is_not_blocked() {
    let mut f = Fixture::new();
    f.merge_ff_in_worktree();
    let (o, e) = f.stop();
    assert_allowed("git merge --ff-only in the session worktree", &o, &e);
}

/// GREEN control at 09508f0b (the merge names only the hook cwd's checkout).
#[test]
fn no_ff_merge_in_primary_only_is_not_blocked() {
    let mut f = Fixture::new();
    f.merge_no_ff_in_primary();
    let (o, e) = f.stop();
    assert_allowed("git merge --no-ff in the primary tree", &o, &e);
}

/// RED at 09508f0b: `cd <wt>` and `.backlog/*.toml` are write candidates.
#[test]
fn backlog_done_and_toml_commit_only_is_not_blocked() {
    let mut f = Fixture::new();
    f.backlog_done_and_commit();
    let (o, e) = f.stop();
    assert_allowed("backlog done + commit of .backlog/*.toml", &o, &e);
}

/// RED at 09508f0b: the script path itself (`scripts/...`, relative, with
/// `/`) resolves to an existing file in the worktree and becomes a candidate.
#[test]
fn rollout_script_writing_outside_repo_only_is_not_blocked() {
    let mut f = Fixture::new();
    f.rollout_writes_outside();
    let (o, e) = f.stop();
    assert_allowed(
        "scripts/rollout-plugins.sh writing outside every repo",
        &o,
        &e,
    );
}

/// GREEN control at 09508f0b (outside-repo writes add no root).
#[test]
fn outside_repo_state_writes_only_are_not_blocked() {
    let mut f = Fixture::new();
    f.state_writes_outside();
    let (o, e) = f.stop();
    assert_allowed("state writes outside every repo + tdd skip", &o, &e);
}

/// The observed session: all of the above in one transcript.
#[test]
fn full_integration_session_is_not_blocked() {
    let mut f = Fixture::new();
    f.merge_ff_in_worktree();
    f.backlog_done_and_commit();
    f.merge_no_ff_in_primary();
    f.rollout_writes_outside();
    f.state_writes_outside();
    let (o, e) = f.stop();
    assert_allowed("merge + backlog + rollout + outside-repo state", &o, &e);
}

// ---------------------------------------------------------------------------
// (b) RE-SCOPED 2026-11-08 (user ruling 2026-11-08: gates judge own worktree
// only). The "session wrote files but no implementation change was evaluated in
// any root" block existed only for >1 roots (hook cwd + the worktrees the
// session wrote into); that rule and the multi-root resolution are removed
// from the Stop hook, so "an implementation write in ANOTHER worktree with an
// empty scan blocks" no longer exists as a property and its tests are
// inverted below (the other worktree is not judged from a primary cwd). The
// fail-closed intent that survives is about the checkout the hook runs in:
// an undetermined committed-work scan of the cwd checkout still blocks.
// ---------------------------------------------------------------------------

/// INVERTED (user ruling 2026-11-08: gates judge own worktree only). Was
/// `impl_write_with_empty_scan_still_blocks`: the session Wrote src/lib.rs in
/// the worktree (content identical to the pre-session commit; empty scan) and
/// the stop, with cwd = primary, blocked with the >1-roots empty-scan message.
/// That rule is gone; the other worktree is not judged, so no block and no
/// empty-scan message.
#[test]
fn impl_write_in_other_worktree_with_empty_scan_is_not_blocked() {
    let mut f = Fixture::new();
    let p = f.wt.join("src/lib.rs");
    let primary = f.primary.clone();
    f.write_tool(&p, &primary);
    let (o, e) = f.stop();
    assert_allowed("impl write (empty scan) in another worktree", &o, &e);
}

/// INVERTED (user ruling 2026-11-08). Was
/// `impl_write_with_empty_scan_amid_bookkeeping_still_blocks`. Bookkeeping plus
/// an implementation write in ANOTHER worktree is not judged from cwd = primary.
#[test]
fn impl_write_in_other_worktree_amid_bookkeeping_is_not_blocked() {
    let mut f = Fixture::new();
    f.merge_ff_in_worktree();
    f.backlog_done_and_commit();
    let p = f.wt.join("src/lib.rs");
    let primary = f.primary.clone();
    f.write_tool(&p, &primary);
    f.rollout_writes_outside();
    f.state_writes_outside();
    let (o, e) = f.stop();
    assert_allowed("bookkeeping + impl write in another worktree", &o, &e);
}

/// RE-SCOPED and WEAKENED (user ruling 2026-11-08). Was
/// `impl_write_with_undetermined_scan_still_blocks`: the session wrote and
/// committed new untested impl in the worktree, but no transcript record
/// carries a timestamp, so the session start (and with it the committed-work
/// fold) is undetermined; with cwd = primary the stop blocked, via the removed
/// more-than-one-root rule. With cwd = the worktree itself (the only checkout judged now)
/// the remaining behaviour is the single-root one: the committed work is not
/// scanned and `gate::committed_note` says so on stderr. This test pins that
/// disclosure and does NOT pin allow/block: whether an undetermined session
/// start should block (CLAUDE.md section 3) is an open question for the user
/// (see the report of the 2026-11-08 test-author pass), not a fact this test
/// establishes.
#[test]
fn impl_write_with_undetermined_scan_in_cwd_reports_not_scanned() {
    let mut f = Fixture::new();
    let p = f.wt.join("src/new.rs");
    std::fs::write(&p, IMPL).unwrap();
    let wt = f.wt.clone();
    commit_all_at(&wt, "impl", IN_SESSION_COMMIT);
    f.write_tool(&p, &wt);
    f.rollout_writes_outside();
    f.timestamps = false;
    let (o, e) = f.stop_at(&wt);
    assert!(
        e.contains("NOT scanned"),
        "an undetermined committed-work scan must be disclosed; stdout={o:?} stderr={e:?}"
    );
}

/// Same fixture with timestamps present and cwd = the worktree: the committed
/// untested impl in the cwd checkout blocks, proving the allow-side tests above
/// are not an inert gate.
#[test]
fn committed_untested_impl_in_cwd_worktree_blocks() {
    let mut f = Fixture::new();
    let p = f.wt.join("src/new.rs");
    std::fs::write(&p, IMPL).unwrap();
    let wt = f.wt.clone();
    commit_all_at(&wt, "impl", IN_SESSION_COMMIT);
    f.write_tool(&p, &wt);
    let (o, e) = f.stop_at(&wt);
    assert!(
        blocks(&o),
        "committed untested impl in the cwd checkout must block; stdout={o:?} stderr={e:?}"
    );
}
