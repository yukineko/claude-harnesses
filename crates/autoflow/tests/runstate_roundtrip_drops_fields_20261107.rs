//! RED: autoflow's Stop hook rewrites condukt's run-state through a LOSSY
//! private mirror struct, destroying every field it does not model.
//!
//! `crates/autoflow/src/condukt.rs` defines its own `TaskState` (id / status /
//! worktree / branch / updated_at) and `RunState` (run_id / goal / tasks).
//! `mark_running` (and `find_pending`'s stale-running sweep) load the run file
//! into that mirror, flip a status, and `save_run` the mirror back over
//! condukt's file. serde drops unknown fields on deserialize, so the write-back
//! erases everything else condukt recorded: `agent_id`, `model`, `branch_sha`,
//! `verifier_*`, `started_at`, `merge_completed_at`, run-level `paused` /
//! `terminal_label`, ... It does so without taking condukt's per-run state lock.
//!
//! Observed in run `run-20261107-114313-25739` (autoflow `condukt-pending`
//! review-queue entry at ts 1794021266): t2/t4/t5 flipped pending -> running
//! with no worktree, and the already-`verified` t3 lost `agent_id`, `model`,
//! `verifier_*`; only the checkpoint copy (seq 2) still held them.
//!
//! The pending -> running flip itself is documented autoflow behaviour; the
//! defect pinned here is that the write-back is not a faithful round trip.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use harness_core::projkey::project_key;

struct Env {
    home: PathBuf,
    repo: PathBuf,
    run_dir: PathBuf,
}

impl Env {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!(
            "autoflow-rtrip-{}-{}",
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
        }
    }

    fn run_path(&self) -> PathBuf {
        self.run_dir.join("run-20261107-000000-1.json")
    }

    fn stop(&self) -> String {
        let payload = format!(
            r#"{{"hook_event_name":"Stop","session_id":"sess-rtrip","cwd":"{}","transcript_path":""}}"#,
            self.repo.to_string_lossy()
        );
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_autoflow")))
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
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[test]
#[ignore = "RED: autoflow mark_running write-back drops condukt per-task/run fields 2026-11-07"]
fn autoflow_stop_preserves_fields_it_does_not_own() {
    let env = Env::new();
    let seeded = serde_json::json!({
        "run_id": "run-20261107-000000-1",
        "goal": "g",
        "tasks": [
            {
                "id": "t2", "status": "pending", "worktree": null, "branch": null,
                "branch_sha": null, "updated_at": null, "model": null,
                "cost_usd": null, "fp_oracle_valid": null
            },
            {
                "id": "t3", "status": "verified",
                "worktree": "/wt/t3", "branch": "condukt/r/t3",
                "branch_sha": "6ac4ba8d441fd8b45ff0d8ed6666e14e35b45c79",
                "updated_at": 1794021143, "model": "sonnet", "cost_usd": 1.5,
                "fp_oracle_valid": true,
                "agent_id": "af4bf84534761cb46",
                "verifier_model": "haiku",
                "verifier_agent_id": "a24228da0000",
                "verifier_ended_at": 1794021143,
                "started_at": 1794020000
            }
        ],
        "paused": true,
        "terminal_label": null,
        "recorded_at": null
    });
    std::fs::write(
        env.run_path(),
        serde_json::to_string_pretty(&seeded).unwrap(),
    )
    .unwrap();

    // Same seed the sibling `mark_running_on_escalation` tests use: a session
    // already past the /record prompt, so this Stop reaches the condukt branch.
    let sp = env
        .home
        .join(".autoflow")
        .join("state")
        .join("sess-rtrip.json");
    std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
    std::fs::write(
        &sp,
        r#"{"phase":"continuing","condukt_prev_pending":null,"condukt_no_progress_streak":0}"#,
    )
    .unwrap();

    let stdout = env.stop();
    assert!(
        stdout.contains("\"block\""),
        "control: the Stop must be the pending-continuation that calls mark_running: {stdout}"
    );

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(env.run_path()).unwrap()).unwrap();
    assert_eq!(
        after["tasks"][0]["status"], "running",
        "control: mark_running did run (pending t2 -> running); without this the \
         field assertions below prove nothing"
    );

    let mut lost = Vec::new();
    for key in [
        "agent_id",
        "model",
        "branch_sha",
        "verifier_model",
        "verifier_agent_id",
        "verifier_ended_at",
        "started_at",
        "cost_usd",
        "fp_oracle_valid",
    ] {
        if after["tasks"][1][key] != seeded["tasks"][1][key] {
            lost.push(format!(
                "t3.{key}: {} -> {}",
                seeded["tasks"][1][key], after["tasks"][1][key]
            ));
        }
    }
    if after["paused"] != seeded["paused"] {
        lost.push(format!(
            "run.paused: {} -> {}",
            seeded["paused"], after["paused"]
        ));
    }
    assert!(
        lost.is_empty(),
        "autoflow's write-back of condukt's run-state erased fields it does not \
         model (an untouched, already-verified task was rewritten): {lost:#?}"
    );
}
