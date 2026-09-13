//! End-to-end: a STALLED condukt run must not have its pending tasks flipped
//! back to `running` on every Stop.
//!
//! `condukt::mark_running` exists so an interrupted worker is detectable: a task
//! left `running` with a stale `updated_at` is later reverted by
//! `find_pending`'s 2-hour sweep. That signal only works if `running` means
//! "someone is on it". When the Stop branch re-marks the SAME pending ids on
//! every cycle, two things break at once:
//!
//! * a task that a worker already `failed` is silently laundered back to
//!   `running`, so the failure stops being visible; and
//! * the escalation message says "stalled" while the run-state simultaneously
//!   claims every one of those tasks is actively running — the two readings a
//!   human gets from the same Stop contradict each other, and the second one
//!   invites a SECOND worker onto the same worktree/branch (memory:
//!   condukt-redispatch-stuck-worker-dup).
//!
//! Backlog `1bac3de6`, human ruling 2026-09-13: `state.rs` is canonical, so the
//! ticket's "escalate N times then transition to Done" half is retracted — the
//! loop legitimately keeps going and only an empty set stops it. The real defect
//! is narrower: `main.rs` called `mark_running` BEFORE `match decision`, so it
//! fired unconditionally, `EscalateStuck` included. Only that is fixed here.
//!
//! Both directions are present on purpose. Without the `Continue` control, the
//! escalation test would also pass against an implementation that deleted
//! `mark_running` outright, which is not the fix.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use harness_core::projkey::project_key;

/// `stuck_threshold` default (`crates/autoflow/src/config.rs`). A no-progress
/// observation with the streak already at `STUCK_THRESHOLD - 1` is the one that
/// reaches the threshold and yields `EscalateStuck`.
const STUCK_THRESHOLD: u32 = 3;

/// A temp `HOME` holding a fake repo, its condukt run-state dir, and autoflow's
/// own session-state dir — the same layout `tests/undetermined_stop.rs` uses.
struct Env {
    home: PathBuf,
    repo: PathBuf,
    run_dir: PathBuf,
    session: String,
}

impl Env {
    fn new(tag: &str) -> Self {
        let home = std::env::temp_dir().join(format!(
            "autoflow-markrunning-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = home.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let run_dir = home.join(".condukt").join("state").join(project_key(&repo));
        std::fs::create_dir_all(&run_dir).unwrap();
        Env {
            home,
            repo,
            run_dir,
            session: format!("sess-{tag}"),
        }
    }

    fn state_path(&self) -> PathBuf {
        self.home
            .join(".autoflow")
            .join("state")
            .join(format!("{}.json", self.session))
    }

    /// Seed autoflow's session state directly. `streak` is the persisted
    /// consecutive-no-progress count; `prev` the pending-set size seen last
    /// Stop. Driving these through the file (rather than through N real Stops)
    /// keeps the test about ONE Stop's behaviour.
    fn write_state(&self, prev: Option<u32>, streak: u32) {
        let p = self.state_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let prev = match prev {
            Some(n) => n.to_string(),
            None => "null".to_string(),
        };
        std::fs::write(
            &p,
            format!(
                r#"{{"phase":"continuing","condukt_prev_pending":{prev},"condukt_no_progress_streak":{streak}}}"#
            ),
        )
        .unwrap();
    }

    fn run_path(&self) -> PathBuf {
        self.run_dir.join("run-20260101-000000-1.json")
    }

    fn write_run_state(&self, tasks_json: &str) {
        std::fs::write(
            self.run_path(),
            format!(r#"{{"run_id":"run-20260101-000000-1","goal":"g","tasks":{tasks_json}}}"#),
        )
        .unwrap();
    }

    /// `(id, status)` for every task in the run-state, as it stands on disk.
    fn statuses(&self) -> Vec<(String, String)> {
        let text = std::fs::read_to_string(self.run_path()).expect("run-state readable");
        let v: serde_json::Value = serde_json::from_str(&text).expect("run-state parses");
        v["tasks"]
            .as_array()
            .expect("tasks array")
            .iter()
            .map(|t| {
                (
                    t["id"].as_str().unwrap_or_default().to_string(),
                    t["status"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    /// Run `autoflow stop`. `PATH` is narrowed so `backlog` is genuinely absent
    /// (an observation that there is no queue), keeping the condukt run-state
    /// the only variable under test.
    fn stop(&self) -> (i32, String, String) {
        let payload = format!(
            r#"{{"hook_event_name":"Stop","session_id":"{}","cwd":"{}","transcript_path":""}}"#,
            self.session,
            self.repo.to_string_lossy()
        );
        let mut child = Command::new(env!("CARGO_BIN_EXE_autoflow"))
            .arg("stop")
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("autoflow spawns");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        let out = child.wait_with_output().expect("autoflow runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn block_reason(stdout: &str) -> String {
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stop must emit a JSON decision");
    assert_eq!(v["decision"], "block", "stop must block: {stdout}");
    v["reason"].as_str().unwrap_or_default().to_string()
}

fn tasks_json() -> &'static str {
    r#"[{"id":"t1","status":"pending"},{"id":"t2","status":"failed"}]"#
}

/// THE FIX. Streak already at `STUCK_THRESHOLD - 1` and the pending set has not
/// shrunk (prev == current == 2), so this Stop yields `EscalateStuck`. The
/// escalation must NOT also claim the stalled tasks are running.
#[test]
fn escalate_stuck_does_not_re_mark_tasks_running() {
    let env = Env::new("escalate");
    env.write_run_state(tasks_json());
    env.write_state(Some(2), STUCK_THRESHOLD - 1);

    let (_code, stdout, _stderr) = env.stop();
    let reason = block_reason(&stdout);
    assert!(
        reason.contains("進捗が止まっています") || reason.contains("進捗停滞"),
        "control: this Stop must really be the stuck escalation, not a routine \
         continue -- otherwise the assertion below proves nothing: {reason}"
    );

    let after = env.statuses();
    assert_eq!(
        after,
        vec![
            ("t1".to_string(), "pending".to_string()),
            ("t2".to_string(), "failed".to_string()),
        ],
        "a stalled run must keep its real statuses: re-marking them running hides \
         the failure and invites a second worker onto the same worktree"
    );
}

/// CONTROL. The same run-state on a PROGRESSING Stop (prev 3 > current 2) still
/// gets marked running — the fix narrows `mark_running`, it does not delete it.
#[test]
fn routine_continue_still_marks_tasks_running() {
    let env = Env::new("continue");
    env.write_run_state(tasks_json());
    env.write_state(Some(3), 0);

    let (_code, stdout, _stderr) = env.stop();
    let reason = block_reason(&stdout);
    assert!(
        reason.contains("残課題"),
        "control: this Stop must be the routine continuation: {reason}"
    );

    let after = env.statuses();
    assert_eq!(
        after,
        vec![
            ("t1".to_string(), "running".to_string()),
            ("t2".to_string(), "running".to_string()),
        ],
        "the routine continuation still claims the tasks so an interruption \
         stays detectable"
    );
}

/// CONTROL. The very first observation (`prev == None`) also counts as progress,
/// so it too must mark running — a fix that keyed off "has a previous
/// observation" rather than off the decision would fail here.
#[test]
fn first_observation_still_marks_tasks_running() {
    let env = Env::new("first");
    env.write_run_state(tasks_json());
    env.write_state(None, 0);

    let (_code, stdout, _stderr) = env.stop();
    block_reason(&stdout);

    let after = env.statuses();
    assert!(
        after.iter().all(|(_, s)| s == "running"),
        "first observation is progress and must mark running: {after:?}"
    );
}

/// The escalation resets the streak to 0 (`decide_progress`), so the NEXT Stop
/// is a routine `Continue` again and does mark running. The suppression is
/// scoped to the escalating Stop, not a permanent stand-down.
#[test]
fn the_stop_after_an_escalation_marks_running_again() {
    let env = Env::new("after");
    env.write_run_state(tasks_json());
    env.write_state(Some(2), STUCK_THRESHOLD - 1);

    let _ = env.stop(); // escalates; leaves statuses alone
    assert!(
        env.statuses().iter().all(|(_, s)| s != "running"),
        "precondition: the escalating Stop left the statuses alone"
    );

    let _ = env.stop(); // streak reset to 0 -> Continue
    let after = env.statuses();
    assert!(
        after.iter().all(|(_, s)| s == "running"),
        "the next Stop is a routine continuation and marks running again: {after:?}"
    );
}
