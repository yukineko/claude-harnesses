//! Phase-7 "foundation capstone" end-to-end test: proves the autonomy-safety
//! stack COMPOSES in a single unattended run driven entirely through the REAL
//! `condukt` binary (`env!("CARGO_BIN_EXE_condukt")`), in one isolated `$HOME`
//! and one shared run-state — not just individually (as `checkpoint_cli.rs`
//! and `edit_gate_hook.rs` prove separately), but coexisting without
//! interfering with each other:
//!
//! 1. autonomy precondition — `condukt state autonomy-check` collapses human
//!    gates to auto when `CONDUKT_AUTONOMOUS=1` (charter's routine-gate
//!    policy).
//! 2. checkpoint — `state checkpoint` durably snapshots the run (charter #7).
//! 3. editgate — `condukt editgate` blocks a real compile-broken Rust edit
//!    under the run's live worktree, and fails soft (empty stdout, exit 0)
//!    on invalid/empty stdin (never-break-a-turn).
//! 4. auto-rollback — a task that goes `verified -> failed` is automatically
//!    restored to the last checkpoint and the restore is journaled.
//! 5. non-interference — interleaving editgate calls with the
//!    checkpoint/rollback steps never corrupts the shared run-state, its
//!    checkpoint journal, or the task's recorded worktree.
//!
//! Modeled directly on the three sibling harnesses: the `Fixture` +
//! `run_git`/`write_decomp`/`run_id_from`/journal-lookup plumbing from
//! `checkpoint_cli.rs`, the `cargo_project`/`edit_payload`/editgate-spawning
//! plumbing from `edit_gate_hook.rs`, and the isolated-`$HOME` real-binary
//! driving style of `replan_recovery_e2e.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

/// A throwaway git repo + an isolated HOME (so `~/.condukt/state` is never
/// touched) + a standalone compile-broken fixture cargo crate that a task's
/// worktree can be pointed at for the editgate leg.
struct Fixture {
    repo: PathBuf,
    home: PathBuf,
    state_dir: PathBuf,
    broken_wt: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-capstone-e2e-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        let state_dir = home.join(".condukt").join("state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();
        // Minimal git repo so `worktree::toplevel` resolves without error.
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);

        let broken_wt = base.join("broken_wt");
        // Broken crate: `s` is `&str` but the fn returns `i32` -> E0308, so
        // `cargo check` fails and editgate's compile gate blocks edits to it.
        cargo_project(
            &broken_wt,
            "broken_capstone_fixture",
            "pub fn f() -> i32 {\n    let s: &str = \"nope\";\n    s\n}\n",
        );

        Self {
            repo,
            home,
            state_dir,
            broken_wt,
        }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("CONDUKT_AUTONOMOUS")
            .output()
            .expect("spawn condukt")
    }

    /// Same as [`Fixture::condukt`] but with `CONDUKT_AUTONOMOUS=1` set, so
    /// the autonomy precondition can be asserted without depending on the
    /// fixture home's (nonexistent) `config.toml`.
    fn condukt_autonomous(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CONDUKT_AUTONOMOUS", "1")
            .output()
            .expect("spawn condukt (autonomous)")
    }

    /// Spawn `condukt editgate` with `cwd == repo`, `HOME == fixture home`,
    /// and the given PostToolUse stdin payload.
    fn editgate(&self, stdin: &str) -> Output {
        let mut child = Command::new(bin())
            .arg("editgate")
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("CONDUKT_DISABLE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn condukt editgate");
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().expect("wait for condukt editgate")
    }

    /// Mirror the binary's project-key layout by globbing for the journal
    /// (same trick `checkpoint_cli.rs` uses to avoid recomputing the hash).
    fn journal(&self, rid: &str) -> PathBuf {
        find_by_suffix(&self.state_dir, &format!("{rid}.journal.jsonl"))
            .unwrap_or_else(|| self.state_dir.join(format!("{rid}.journal.jsonl")))
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

/// Write a minimal standalone cargo project (its own manifest, no workspace)
/// with the given crate name and `src/lib.rs` body.
fn cargo_project(dir: &Path, name: &str, lib_rs: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src").join("lib.rs"), lib_rs).unwrap();
}

fn write_decomp(fx: &Fixture) -> PathBuf {
    let p = fx.repo.join("decomp.json");
    std::fs::write(
        &p,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"x","touched_files":["a.rs"],"deps":[],"class":"serial","done_criteria":"d"}]}"#,
    )
    .unwrap();
    p
}

/// Extract the run id from `state init`'s output (the last non-empty line).
fn run_id_from(out: &Output) -> String {
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .chain(String::from_utf8_lossy(&out.stderr).lines())
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("run-"))
        .expect("a run- id in init output")
        .to_string()
}

/// Recursively find a file whose path ends with `suffix` (the project-key dir
/// is a hash we don't want to recompute in the test).
fn find_by_suffix(root: &Path, suffix: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_by_suffix(&p, suffix) {
                return Some(found);
            }
        } else if p.to_string_lossy().ends_with(suffix) {
            return Some(p);
        }
    }
    None
}

fn edit_payload(file_path: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": { "file_path": file_path.to_string_lossy() },
    })
    .to_string()
}

#[test]
fn foundation_capstone_composes_autonomy_checkpoint_editgate_and_auto_rollback_in_one_run() {
    let fx = Fixture::new("capstone");
    let decomp = write_decomp(&fx);

    // ── 1. Autonomy precondition: human gates collapse to auto ────────────
    let auto = fx.condukt_autonomous(&["state", "autonomy-check"]);
    assert!(
        auto.status.success(),
        "autonomy-check must exit 0 when CONDUKT_AUTONOMOUS=1: {auto:?}"
    );
    let auto_out = String::from_utf8_lossy(&auto.stdout);
    assert_eq!(
        auto_out.trim(),
        r#"{"autonomous":true}"#,
        "unexpected autonomy-check output: {auto_out}"
    );

    // Init the run that every remaining step shares.
    let init = fx.condukt(&["state", "init", "--file", decomp.to_str().unwrap()]);
    assert!(init.status.success(), "init failed: {init:?}");
    let rid = run_id_from(&init);

    // Point the task's worktree at the compile-broken fixture crate, so
    // editgate has a live, resolvable worktree to gate edits under (status
    // stays `pending`, so no fp-oracle/verified gate is triggered here).
    let assign = fx.condukt(&[
        "state",
        "set",
        "--run",
        &rid,
        "--task",
        "t1",
        "--status",
        "pending",
        "--worktree",
        fx.broken_wt.to_str().unwrap(),
    ]);
    assert!(
        assign.status.success(),
        "worktree assignment failed: {assign:?}"
    );

    // ── 2. Checkpoint the baseline (pending, worktree assigned) → seq 1 ───
    let cp = fx.condukt(&["state", "checkpoint", "--run", &rid, "--label", "baseline"]);
    assert!(cp.status.success(), "checkpoint failed: {cp:?}");
    assert_eq!(String::from_utf8_lossy(&cp.stdout).trim(), "1");

    let journal = fx.journal(&rid);
    assert!(
        journal.exists(),
        "journal not written at {}",
        journal.display()
    );
    let jtext_after_checkpoint = std::fs::read_to_string(&journal).unwrap();
    assert!(
        jtext_after_checkpoint.contains("checkpoint"),
        "first entry not checkpoint: {jtext_after_checkpoint}"
    );

    // ── 3. Editgate coexists with the checkpointed run ─────────────────────
    // A broken edit under the run's live worktree is BLOCKED.
    let broken_file = fx.broken_wt.join("src").join("lib.rs");
    let block_out = fx.editgate(&edit_payload(&broken_file));
    assert!(
        block_out.status.success(),
        "editgate must exit 0 (never break a turn); stderr: {}",
        String::from_utf8_lossy(&block_out.stderr)
    );
    let block_line = String::from_utf8_lossy(&block_out.stdout)
        .trim()
        .to_string();
    assert!(
        !block_line.is_empty(),
        "a broken edit under the checkpointed run must produce a block verdict, got empty stdout"
    );
    let block_v: serde_json::Value =
        serde_json::from_str(&block_line).expect("block verdict must be one JSON line");
    assert_eq!(
        block_v["decision"], "block",
        "broken edit must be blocked; got {block_line}"
    );
    assert!(
        !block_v["reason"].as_str().unwrap_or("").is_empty(),
        "block verdict must carry a non-empty reason; got {block_line}"
    );

    // An empty/invalid stdin payload fails soft: no block, exit 0
    // (never-break-a-turn), proving editgate doesn't misfire against the
    // shared run-state just because a checkpoint now exists.
    let allow_out = fx.editgate("");
    assert!(allow_out.status.success());
    assert!(
        allow_out.stdout.is_empty(),
        "empty stdin must produce no output; got {}",
        String::from_utf8_lossy(&allow_out.stdout)
    );

    // ── 5. Non-interference: editgate calls must not corrupt the shared
    // run-state, its worktree assignment, or its checkpoint journal ────────
    let show_mid = fx.condukt(&["state", "show", "--run", &rid]);
    assert!(show_mid.status.success());
    let mid = String::from_utf8_lossy(&show_mid.stdout);
    assert!(
        mid.contains("\"status\": \"pending\"") || mid.contains("Pending"),
        "run-state corrupted by editgate calls: {mid}"
    );
    assert!(
        mid.contains(fx.broken_wt.to_str().unwrap()),
        "worktree assignment lost after editgate calls: {mid}"
    );
    let jtext_mid = std::fs::read_to_string(&journal).unwrap();
    assert_eq!(
        jtext_mid, jtext_after_checkpoint,
        "journal mutated by editgate calls (non-interference violated)"
    );

    // ── 4. Auto-rollback composition: verified -> failed restores the
    // checkpoint, journaled as `auto_rollback` ─────────────────────────────
    let verify = fx.condukt(&[
        "state", "set", "--run", &rid, "--task", "t1", "--status", "verified",
    ]);
    assert!(verify.status.success(), "verify failed: {verify:?}");
    let fail = fx.condukt(&[
        "state", "set", "--run", &rid, "--task", "t1", "--status", "failed",
    ]);
    assert!(fail.status.success(), "fail-set failed: {fail:?}");

    let show_after = fx.condukt(&["state", "show", "--run", &rid]);
    let after = String::from_utf8_lossy(&show_after.stdout);
    assert!(
        after.contains("\"status\": \"pending\"") || after.contains("Pending"),
        "auto-rollback did not restore snapshot: {after}"
    );
    assert!(
        !after.contains("failed"),
        "task still failed after auto-rollback: {after}"
    );
    assert!(
        after.contains(fx.broken_wt.to_str().unwrap()),
        "worktree assignment lost after auto-rollback: {after}"
    );

    let jtext_final = std::fs::read_to_string(&journal).unwrap();
    assert!(
        jtext_final.contains("auto_rollback"),
        "no auto_rollback journal entry: {jtext_final}"
    );

    // A further editgate call after the composed rollback still resolves the
    // (restored) worktree and blocks correctly: checkpoint/rollback and
    // editgate share the run-state without interfering with each other.
    let block_out_2 = fx.editgate(&edit_payload(&broken_file));
    assert!(block_out_2.status.success());
    let block_line_2 = String::from_utf8_lossy(&block_out_2.stdout)
        .trim()
        .to_string();
    assert!(
        !block_line_2.is_empty(),
        "post-rollback broken edit must still be blocked, got empty stdout"
    );
    let block_v2: serde_json::Value = serde_json::from_str(&block_line_2)
        .expect("post-rollback block verdict must be one JSON line");
    assert_eq!(block_v2["decision"], "block");
}
