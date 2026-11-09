//! autoflow's Stop hook and condukt contend the SAME per-run state lock.
//!
//! autoflow's run-state write-back (`mark_running`) must take the lock file
//! `<run_dir>/<run>.lock` that condukt's `RunLock` takes (condukt delegates to
//! `harness_core::run_lock::acquire`). While another party holds it, autoflow
//! must fail closed: leave the run file byte-identical and say so on stderr.
//!
//! Cases:
//! 1. lock held through `harness_core::run_lock::acquire` (the function condukt uses);
//! 2. lock held by a hand-written `LockInfo` file with a live pid (the on-disk
//!    protocol itself, independent of the shared function);
//! 3. control: no lock held -> the pending task becomes `running`.
//!
//! What this does NOT prove: it never runs the real `condukt` binary, so it
//! does not prove condukt's `RunLock` still delegates to `run_lock::acquire`
//! with this same path derivation (that is only asserted by the implementer's
//! reading of condukt's source), nor that condukt itself honours autoflow's
//! lock. It proves autoflow honours the shared lock protocol and path
//! (`<run>.json` -> `<run>.lock`). Each contended case waits the full 10s
//! deadline (no override exists).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use harness_core::projkey::project_key;
use harness_core::run_lock;

const RUN_ID: &str = "run-20261108-000000-1";

struct Env {
    home: PathBuf,
    repo: PathBuf,
    run_dir: PathBuf,
}

struct Outcome {
    stdout: String,
    stderr: String,
}

impl Env {
    fn new() -> Self {
        // A per-process counter keeps parallel tests apart: pid + wall-clock
        // nanos alone collided (two tests shared one home, one contended the
        // other's lock and one's Drop removed the other's run file).
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!(
            "autoflow-lockc-{}-{}-{}",
            std::process::id(),
            seq,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = home.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let run_dir = home.join(".condukt").join("state").join(project_key(&repo));
        std::fs::create_dir_all(&run_dir).unwrap();
        let env = Env {
            home,
            repo,
            run_dir,
        };
        let seeded = serde_json::json!({
            "run_id": RUN_ID,
            "goal": "g",
            "tasks": [{
                "id": "t2", "status": "pending", "worktree": null, "branch": null,
                "updated_at": null
            }]
        });
        std::fs::write(
            env.run_path(),
            serde_json::to_string_pretty(&seeded).unwrap(),
        )
        .unwrap();
        // Session already past the /record prompt so Stop reaches mark_running.
        let sp = env
            .home
            .join(".autoflow")
            .join("state")
            .join("sess-lockc.json");
        std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
        std::fs::write(
            &sp,
            r#"{"phase":"continuing","condukt_prev_pending":null,"condukt_no_progress_streak":0}"#,
        )
        .unwrap();
        env
    }

    fn run_path(&self) -> PathBuf {
        self.run_dir.join(format!("{RUN_ID}.json"))
    }

    fn lock_path(&self) -> PathBuf {
        run_lock::lock_path(&self.run_dir, RUN_ID)
    }

    fn stop(&self) -> Outcome {
        let payload = format!(
            r#"{{"hook_event_name":"Stop","session_id":"sess-lockc","cwd":"{}","transcript_path":""}}"#,
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
        Outcome {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn assert_refused(env: &Env, before: &[u8], out: &Outcome) {
    let after = std::fs::read(env.run_path()).unwrap();
    assert_eq!(
        after, before,
        "run file must be byte-identical while the lock is held; stdout={} stderr={}",
        out.stdout, out.stderr
    );
    assert!(
        out.stderr.contains("NOT written (fail closed)"),
        "stderr must report the refused write: {}",
        out.stderr
    );
    let lock = env.lock_path();
    assert!(
        out.stderr.contains(&lock.display().to_string()),
        "stderr must name the contended lock {}: {}",
        lock.display(),
        out.stderr
    );
    assert!(
        lock.exists(),
        "the holder's lock file must still exist (autoflow must not steal/remove it)"
    );
}

#[test]
fn autoflow_refuses_write_while_lock_held_via_shared_acquire() {
    let env = Env::new();
    let before = std::fs::read(env.run_path()).unwrap();
    let guard = run_lock::acquire(env.lock_path(), run_lock::DEADLINE)
        .expect("test acquires the lock first");
    let out = env.stop();
    assert_refused(&env, &before, &out);
    drop(guard);
}

#[test]
fn autoflow_refuses_write_while_lock_held_by_handwritten_lockinfo() {
    let env = Env::new();
    let before = std::fs::read(env.run_path()).unwrap();
    let info = run_lock::LockInfo {
        pid: std::process::id(),
        run_id: RUN_ID.to_string(),
        acquired_at: 0,
    };
    std::fs::write(env.lock_path(), serde_json::to_string(&info).unwrap()).unwrap();
    let out = env.stop();
    assert_refused(&env, &before, &out);
}

#[test]
fn autoflow_writes_when_no_lock_held_control() {
    let env = Env::new();
    let out = env.stop();
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(env.run_path()).unwrap()).unwrap();
    assert_eq!(
        after["tasks"][0]["status"], "running",
        "control: without a held lock the pending task is marked running; stdout={} stderr={}",
        out.stdout, out.stderr
    );
    assert!(!out.stderr.contains("NOT written"), "{}", out.stderr);
}
