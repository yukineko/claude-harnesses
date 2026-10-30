//! backlog 1eae7cb9: condukt keys its per-project run state by the MAIN
//! worktree root (`harness_core::projkey::main_worktree_root`, backlog
//! 43393ce2), but autoflow's `condukt::load_latest` still keys by
//! `repo_root(cwd)`, which for a LINKED worktree is the worktree itself. From
//! a linked worktree (the normal case under CLAUDE.md §8) autoflow therefore
//! looks in a different directory than condukt wrote to, and does not see the
//! run at all.
//!
//! Observable at the binary boundary: a routine progressing `autoflow stop`
//! marks the run's pending tasks `running`. The control does it from the main
//! tree; the repro does it from a linked worktree of the same repo.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use harness_core::projkey::project_key;

struct Env {
    home: PathBuf,
    main: PathBuf,
    linked: PathBuf,
    run_path: PathBuf,
    session: String,
}

fn git(cwd: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

impl Env {
    fn new(tag: &str) -> Self {
        let home = std::env::temp_dir().join(format!(
            "autoflow-1eae7cb9-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let main = home.join("repo");
        std::fs::create_dir_all(&main).unwrap();
        let main = main.canonicalize().unwrap();
        let home = home.canonicalize().unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let linked = home.join("wt");
        git(
            &main,
            &["worktree", "add", "-q", "-b", "w", linked.to_str().unwrap()],
        );
        // Where condukt writes: keyed by the MAIN worktree root.
        let run_dir = home.join(".condukt").join("state").join(project_key(&main));
        std::fs::create_dir_all(&run_dir).unwrap();
        let run_path = run_dir.join("run-20260101-000000-1.json");
        std::fs::write(
            &run_path,
            r#"{"run_id":"run-20260101-000000-1","goal":"g","tasks":[{"id":"t1","status":"pending"}]}"#,
        )
        .unwrap();
        let session = format!("sess-{tag}");
        // autoflow only drives an ACTIVE session: seed its state as
        // `continuing` with no previous observation (first Stop = progress),
        // the same seeding tests/mark_running_on_escalation.rs uses.
        let state = home
            .join(".autoflow")
            .join("state")
            .join(format!("{session}.json"));
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        std::fs::write(
            &state,
            r#"{"phase":"continuing","condukt_prev_pending":null,"condukt_no_progress_streak":0}"#,
        )
        .unwrap();
        Env {
            home,
            main,
            linked,
            run_path,
            session,
        }
    }

    fn status(&self) -> String {
        let text = std::fs::read_to_string(&self.run_path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v["tasks"][0]["status"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn stop(&self, cwd: &Path) -> String {
        let payload = format!(
            r#"{{"hook_event_name":"Stop","session_id":"{}","cwd":"{}","transcript_path":""}}"#,
            self.session,
            cwd.to_string_lossy()
        );
        let mut child =
            harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_autoflow")))
                .arg("stop")
                .current_dir(cwd)
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
        format!(
            "exit={:?} stdout={} stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// CONTROL: from the main tree autoflow finds condukt's run and marks it.
#[test]
fn control_from_main_tree_autoflow_sees_condukt_run() {
    let env = Env::new("main");
    let out = env.stop(&env.main);
    assert_eq!(env.status(), "running", "control failed: {out}");
}

#[test]
fn from_linked_worktree_autoflow_sees_the_same_condukt_run() {
    let env = Env::new("linked");
    let out = env.stop(&env.linked);
    assert_eq!(
        env.status(),
        "running",
        "autoflow stop from a linked worktree did not see the condukt run that condukt \
         keys by the MAIN worktree root ({}); autoflow keyed by repo_root(cwd) = the \
         linked worktree: {out}",
        env.main.display()
    );
}
