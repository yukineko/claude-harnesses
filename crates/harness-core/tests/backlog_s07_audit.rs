#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit repro tests for backlog items (shard s07-core-spec). Each test asserts
//! the property the ticket says is violated; they are `#[ignore]`d while the
//! defect is open (remove the ignore when fixed).

use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

// --- 873a2621: a peer transcript 2h old still excludes files (no liveness) ---
#[test]
fn backlog_873a2621_two_hour_old_transcript_is_not_a_live_peer() {
    let root = tempfile::tempdir().unwrap();
    let slug = root.path().join("slug");
    std::fs::create_dir_all(&slug).unwrap();
    let p = slug.join("peer-session.jsonl");
    let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/work/a.rs","old_string":"a","new_string":"b"}}]}}"#;
    std::fs::write(&p, format!("{line}\n")).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
    let t = SystemTime::now() - Duration::from_secs(2 * 3600);
    f.set_times(std::fs::FileTimes::new().set_accessed(t).set_modified(t))
        .unwrap();
    let fp = harness_core::transcript::peer_edit_footprint_in(root.path(), "me");
    assert!(
        fp.is_empty(),
        "a session whose transcript last changed 2h ago is not observed-live, yet it still \
         claims files: {fp:?}"
    );
}

// --- ff98fed0: unknown model id priced at $0 ---
#[test]
#[ignore = "backlog ff98fed0: open defect, remove ignore when fixed"]
fn backlog_ff98fed0_unknown_model_is_not_priced_free() {
    let u = harness_core::usage::ModelUsage {
        input: 1_000_000,
        ..Default::default()
    };
    let c = harness_core::pricing::cost("claude-newfamily-1", &u, &[]);
    assert!(
        c > 0.0,
        "unknown model priced at {c} USD: budget gate silently disabled"
    );
}

// --- 00a8ba29: unreadable existing ledger read as empty ledger ---
#[test]
#[ignore = "backlog 00a8ba29: open defect, remove ignore when fixed"]
fn backlog_00a8ba29_unreadable_existing_ledger_is_not_empty_ledger() {
    let dir = tempfile::tempdir().unwrap();
    // ledger.json exists but cannot be read as a file (it is a directory -> EISDIR).
    std::fs::create_dir(dir.path().join("ledger.json")).unwrap();
    let r = harness_core::ledger::Ledger::load_checked(dir.path());
    assert!(
        r.is_err(),
        "present-but-unreadable ledger returned Ok(default) — caller will write it back"
    );
}

// --- f58a303f: probe_repo has no timeout on its git subprocess ---
#[test]
#[ignore = "backlog f58a303f: open defect, remove ignore when fixed"]
#[cfg(unix)]
fn backlog_f58a303f_probe_repo_times_out_on_hung_git() {
    use std::os::unix::fs::PermissionsExt;
    let bin = tempfile::tempdir().unwrap();
    let git = bin.path().join("git");
    std::fs::write(&git, "#!/bin/sh\nsleep 6\n").unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    let work = tempfile::tempdir().unwrap();
    std::env::set_var("PATH", format!("{}:/bin:/usr/bin", bin.path().display()));
    let start = Instant::now();
    let _ = harness_core::git_probe::probe_repo(Path::new(work.path()));
    let el = start.elapsed();
    assert!(
        el < Duration::from_secs(3),
        "probe_repo blocked {el:?} on a hung git"
    );
}

// --- 814cfbbb: undetermined.jsonl default sink is per-checkout, not per-project ---
#[test]
#[ignore = "backlog 814cfbbb: open defect, remove ignore when fixed"]
fn backlog_814cfbbb_default_sink_is_shared_across_worktrees() {
    use std::process::Command;
    let td = tempfile::tempdir().unwrap();
    let main = td.path().join("main");
    let wt = td.path().join("wt");
    std::fs::create_dir_all(&main).unwrap();
    let run = |dir: &Path, args: &[&str]| {
        let o = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    run(&main, &["init", "-q"]);
    run(
        &main,
        &[
            "-c",
            "user.email=a@b",
            "-c",
            "user.name=n",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "i",
        ],
    );
    run(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    let a = harness_core::undetermined::default_sink_path(&main);
    let b = harness_core::undetermined::default_sink_path(&wt);
    assert_eq!(
        a, b,
        "the same project writes undetermined.jsonl to two sinks"
    );
}
