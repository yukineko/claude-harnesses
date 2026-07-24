//! End-to-end coverage for the fugu-router learning-signal wiring: `condukt
//! state set --agent-id/--route-basis/...` followed by `condukt state
//! record-run --all` must produce a REAL `fugu-router` episode
//! (`~/.fugu-router/episodes.jsonl`) whose `model`, `route_basis`,
//! `tokens_input`, and `tokens_output` fields are actually populated —
//! not left at their zero-value defaults.
//!
//! Investigation context (see backlog / hypothesis f5f9522a): a survey of the
//! production `episodes.jsonl` (537 episodes) found `tokens_input`/
//! `tokens_output` at 0/537 and `route_basis` populated on only 2/537. Reading
//! `fugu-router/src/store.rs` (the `Episode` struct) and `condukt/src/main.rs`
//! (`record_runs`) / `state.rs` (`resolve_agent_tokens`, `records_for_run`)
//! shows every field and call site already wired correctly as of this
//! worktree's HEAD (commit a4098a0 landed this exact feature). This test is
//! the missing end-to-end proof that the wiring works when a caller actually
//! supplies `--agent-id`/`--route-basis` — no test previously exercised the
//! REAL spawned `condukt` -> REAL spawned `fugu-router` -> episodes.jsonl
//! path (only the pure join helpers `records_for_run`/`parse_agent_tokens`
//! had unit coverage, never the full pipeline). The likely explanation for
//! the 0/537 in production is that those episodes predate this feature
//! (a4098a0, landed after most of the historical data) and/or the live
//! plugin cache was behind the source (see `scripts/check-plugin-rollout.py`
//! drift for `condukt` at the time of this investigation) — not a code bug.
//!
//! `gauge` is faked here (a tiny shell script) rather than driving a real
//! Claude Code transcript, since `gauge subagents --json`'s job is just to be
//! *some* process on PATH emitting the documented JSON shape; `condukt`'s
//! `resolve_agent_tokens`/`resolve_agent_cost` only care about that shape.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "condukt-fugu-record-e2e-{tag}-{}-{}",
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create isolated dir");
    dir
}

/// Locate a `fugu-router` binary usable by the spawned `condukt` process,
/// building it (once, via `cargo build -p fugu-router --bin fugu-router`) if
/// it isn't already sitting next to this condukt test binary in the target
/// profile directory. Returns the directory to prepend to `PATH` (mirrors
/// `fp_oracle_e2e.rs`'s `tdd_bin_dir`).
fn fugu_router_bin_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe of this test binary");
    let deps_dir = exe.parent().expect("deps dir").to_path_buf();
    let profile_dir = deps_dir.parent().expect("profile dir").to_path_buf();
    let bin_name = if cfg!(windows) {
        "fugu-router.exe"
    } else {
        "fugu-router"
    };
    let bin_path = profile_dir.join(bin_name);
    if !bin_path.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let is_release = profile_dir.file_name().and_then(|s| s.to_str()) == Some("release");
        let mut cmd = Command::new(&cargo);
        cmd.args(["build", "-p", "fugu-router", "--bin", "fugu-router"]);
        if is_release {
            cmd.arg("--release");
        }
        let status = cmd.status().expect("spawning `cargo build -p fugu-router`");
        assert!(status.success(), "`cargo build -p fugu-router` failed");
    }
    assert!(
        bin_path.exists(),
        "expected a fugu-router binary at {} after build",
        bin_path.display()
    );
    profile_dir
}

/// Write a fake `gauge` shell script (executable) into `dir` that emits a
/// fixed `subagents --json` response with ONE entry, keyed by `agent_id`,
/// with a real `tokens_input`/`tokens_output` reading — the exact shape
/// `condukt::state::parse_agent_tokens`/`parse_agent_cost` expect. Any other
/// invocation prints `[]` (fail-soft default), matching real `gauge`'s
/// no-transcript behaviour.
fn write_fake_gauge(
    dir: &Path,
    agent_id: &str,
    tokens_input: u64,
    tokens_output: u64,
    cost_usd: f64,
) {
    let path = dir.join("gauge");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"subagents\" ]; then\n\
         echo '[{{\"agent_id\":\"{agent_id}\",\"agent_type\":\"condukt:condukt-worker\",\"description\":\"demo\",\"cost_usd\":{cost_usd},\"turns\":1,\"tokens_input\":{tokens_input},\"tokens_output\":{tokens_output}}}]'\n\
         else\n\
         echo '[]'\n\
         fi\n"
    );
    std::fs::write(&path, script).expect("write fake gauge script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
}

/// Like [`write_fake_gauge`] but emits one entry per `(agent_id,
/// tokens_input, tokens_output, cost_usd)` tuple in `entries`, so a single
/// test can exercise exact-match resolution for MULTIPLE distinct agent ids
/// (e.g. a worker agent and a verifier agent in the same run).
fn write_fake_gauge_multi(dir: &Path, entries: &[(&str, u64, u64, f64)]) {
    let path = dir.join("gauge");
    let entries_json: Vec<String> = entries
        .iter()
        .map(|(agent_id, tokens_input, tokens_output, cost_usd)| {
            format!(
                "{{\"agent_id\":\"{agent_id}\",\"agent_type\":\"condukt:condukt-worker\",\"description\":\"demo\",\"cost_usd\":{cost_usd},\"turns\":1,\"tokens_input\":{tokens_input},\"tokens_output\":{tokens_output}}}"
            )
        })
        .collect();
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"subagents\" ]; then\n\
         echo '[{}]'\n\
         else\n\
         echo '[]'\n\
         fi\n",
        entries_json.join(",")
    );
    std::fs::write(&path, script).expect("write fake gauge script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
}

fn run_id_from(stdout: &str, stderr: &str) -> String {
    stdout
        .lines()
        .chain(stderr.lines())
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("run-"))
        .expect("a run- id in init output")
        .to_string()
}

#[test]
fn record_run_populates_model_route_basis_and_tokens_in_episode() {
    let dir = unique_dir("main");
    let home = unique_dir("home");

    // fugu-router and the fake gauge live in two different directories, so
    // both are prepended to PATH (condukt shells out to each by bare name).
    let fugu_dir = fugu_router_bin_dir();
    let fake_bin_dir = unique_dir("fakebin");
    let agent_id = "aFAKEAGENT123";
    write_fake_gauge(&fake_bin_dir, agent_id, 4242, 1337, 0.42);

    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![fake_bin_dir.clone(), fugu_dir.clone()];
    paths.extend(std::env::split_paths(&existing_path));
    let combined_path = std::env::join_paths(paths).expect("join PATH");
    let condukt_bin = env!("CARGO_BIN_EXE_condukt");
    let run_condukt_multi = |args: &[&str]| -> (i32, String, String) {
        let out = Command::new(condukt_bin)
            .args(args)
            .current_dir(&dir)
            .env("HOME", &home)
            .env("PATH", &combined_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("condukt spawns");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    // 1. Seed a single-task decomposition and init a run.
    let decomp = serde_json::json!({
        "goal": "demo goal",
        "tasks": [{"id": "t1", "title": "demo task", "touched_files": []}]
    })
    .to_string();
    let dec_path = dir.join("decomposition.json");
    std::fs::write(&dec_path, decomp).expect("write decomposition");
    let (code, stdout, stderr) =
        run_condukt_multi(&["state", "init", "--file", dec_path.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "state init failed: stdout={stdout} stderr={stderr}"
    );
    let rid = run_id_from(&stdout, &stderr);

    // 2. Mark the task running, then verified — supplying --agent-id (drives
    // both cost AND token resolution via the fake gauge) and
    // --route-basis/--route-confidence/--route-rationale (routing
    // provenance), exactly as SKILL.md Phase 6 documents.
    let (code, stdout, stderr) = run_condukt_multi(&[
        "state", "set", "--run", &rid, "--task", "t1", "--status", "running",
    ]);
    assert_eq!(
        code, 0,
        "set running failed: stdout={stdout} stderr={stderr}"
    );

    let (code, stdout, stderr) = run_condukt_multi(&[
        "state",
        "set",
        "--run",
        &rid,
        "--task",
        "t1",
        "--status",
        "verified",
        "--model",
        "sonnet",
        "--agent-id",
        agent_id,
        "--cost",
        "0",
        "--route-basis",
        "learned",
        "--route-confidence",
        "high",
        "--route-rationale",
        "neighbours cleared 90%",
    ]);
    assert_eq!(
        code, 0,
        "set verified failed: stdout={stdout} stderr={stderr}"
    );

    // 3. Fire the record sweep.
    let (code, stdout, stderr) = run_condukt_multi(&["state", "record-run", "--all"]);
    assert_eq!(
        code, 0,
        "record-run --all failed: stdout={stdout} stderr={stderr}"
    );

    // 4. Read the REAL episodes.jsonl fugu-router wrote under the isolated
    // HOME and assert the fields this task exists to wire up.
    let episodes_path = home.join(".fugu-router").join("episodes.jsonl");
    let raw = std::fs::read_to_string(&episodes_path).unwrap_or_else(|e| {
        panic!(
            "expected {} to exist after record-run (stdout={stdout} stderr={stderr}): {e}",
            episodes_path.display()
        )
    });
    let line = raw
        .lines()
        .next_back()
        .expect("episodes.jsonl must have at least one line");
    let ep: serde_json::Value = serde_json::from_str(line).expect("episode line is valid JSON");

    assert_eq!(ep["model"], "sonnet", "episode JSON: {ep}");
    assert_eq!(ep["route_basis"], "learned", "episode JSON: {ep}");
    assert_eq!(ep["route_confidence"], "high", "episode JSON: {ep}");
    assert_eq!(
        ep["route_rationale"], "neighbours cleared 90%",
        "episode JSON: {ep}"
    );
    assert_eq!(ep["tokens_input"], 4242, "episode JSON: {ep}");
    assert_eq!(ep["tokens_output"], 1337, "episode JSON: {ep}");
    assert_eq!(ep["pass"], true, "episode JSON: {ep}");
    assert_eq!(ep["role"], "worker", "episode JSON: {ep}");
}

/// hypothesis f5f9522a: when a task carries `--verifier-model` (recorded via
/// `state set`), `record-run` must emit a SECOND fugu-router episode with
/// `role=verifier`, reusing the same title/files/class/done_criteria/status
/// as the worker episode — in addition to the existing role=worker episode.
/// A task with NO verifier model must still emit exactly one (worker) episode.
#[test]
fn record_run_emits_worker_and_verifier_episodes_when_verifier_model_set() {
    let dir = unique_dir("verifier-main");
    let home = unique_dir("verifier-home");

    let fugu_dir = fugu_router_bin_dir();
    let fake_bin_dir = unique_dir("verifier-fakebin");
    let worker_agent_id = "wWORKERAGENT1";
    let verifier_agent_id = "vVERIFIERAGENT1";
    write_fake_gauge_multi(
        &fake_bin_dir,
        &[
            (worker_agent_id, 100, 200, 0.10),
            (verifier_agent_id, 300, 400, 0.05),
        ],
    );

    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![fake_bin_dir.clone(), fugu_dir.clone()];
    paths.extend(std::env::split_paths(&existing_path));
    let combined_path = std::env::join_paths(paths).expect("join PATH");
    let condukt_bin = env!("CARGO_BIN_EXE_condukt");
    let run_condukt = |args: &[&str]| -> (i32, String, String) {
        let out = Command::new(condukt_bin)
            .args(args)
            .current_dir(&dir)
            .env("HOME", &home)
            .env("PATH", &combined_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("condukt spawns");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    // Seed a two-task decomposition: t1 will carry a verifier_model, t2 will not.
    let decomp = serde_json::json!({
        "goal": "demo goal",
        "tasks": [
            {"id": "t1", "title": "task with verifier", "touched_files": []},
            {"id": "t2", "title": "task without verifier", "touched_files": []}
        ]
    })
    .to_string();
    let dec_path = dir.join("decomposition.json");
    std::fs::write(&dec_path, decomp).expect("write decomposition");
    let (code, stdout, stderr) =
        run_condukt(&["state", "init", "--file", dec_path.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "state init failed: stdout={stdout} stderr={stderr}"
    );
    let rid = run_id_from(&stdout, &stderr);

    for task in ["t1", "t2"] {
        let (code, stdout, stderr) = run_condukt(&[
            "state", "set", "--run", &rid, "--task", task, "--status", "running",
        ]);
        assert_eq!(
            code, 0,
            "set running ({task}) failed: stdout={stdout} stderr={stderr}"
        );
    }

    // t1: verified WITH a verifier model + verifier agent id.
    let (code, stdout, stderr) = run_condukt(&[
        "state",
        "set",
        "--run",
        &rid,
        "--task",
        "t1",
        "--status",
        "verified",
        "--model",
        "sonnet",
        "--agent-id",
        worker_agent_id,
        "--cost",
        "0",
        "--verifier-model",
        "opus",
        "--verifier-agent-id",
        verifier_agent_id,
        "--verifier-cost",
        "0",
    ]);
    assert_eq!(
        code, 0,
        "set verified (t1) failed: stdout={stdout} stderr={stderr}"
    );

    // t2: verified WITHOUT any verifier model.
    let (code, stdout, stderr) = run_condukt(&[
        "state", "set", "--run", &rid, "--task", "t2", "--status", "verified", "--model", "sonnet",
        "--cost", "0",
    ]);
    assert_eq!(
        code, 0,
        "set verified (t2) failed: stdout={stdout} stderr={stderr}"
    );

    // Fire the record sweep.
    let (code, stdout, stderr) = run_condukt(&["state", "record-run", "--all"]);
    assert_eq!(
        code, 0,
        "record-run --all failed: stdout={stdout} stderr={stderr}"
    );

    let episodes_path = home.join(".fugu-router").join("episodes.jsonl");
    let raw = std::fs::read_to_string(&episodes_path).unwrap_or_else(|e| {
        panic!(
            "expected {} to exist after record-run (stdout={stdout} stderr={stderr}): {e}",
            episodes_path.display()
        )
    });
    let episodes: Vec<serde_json::Value> = raw
        .lines()
        .map(|l| serde_json::from_str(l).expect("episode line is valid JSON"))
        .collect();

    let t1_episodes: Vec<&serde_json::Value> = episodes
        .iter()
        .filter(|e| e["title"] == "task with verifier")
        .collect();
    assert_eq!(
        t1_episodes.len(),
        2,
        "expected exactly 2 episodes (worker + verifier) for t1, got {}: {episodes:?}",
        t1_episodes.len()
    );
    let worker_ep = t1_episodes
        .iter()
        .find(|e| e["role"] == "worker")
        .unwrap_or_else(|| panic!("no role=worker episode for t1 in {t1_episodes:?}"));
    let verifier_ep = t1_episodes
        .iter()
        .find(|e| e["role"] == "verifier")
        .unwrap_or_else(|| panic!("no role=verifier episode for t1 in {t1_episodes:?}"));
    assert_eq!(worker_ep["model"], "sonnet", "worker episode: {worker_ep}");
    assert_eq!(
        worker_ep["tokens_input"], 100,
        "worker episode: {worker_ep}"
    );
    assert_eq!(
        verifier_ep["model"], "opus",
        "verifier episode: {verifier_ep}"
    );
    assert_eq!(
        verifier_ep["tokens_input"], 300,
        "verifier episode: {verifier_ep}"
    );
    assert_eq!(
        verifier_ep["tokens_output"], 400,
        "verifier episode: {verifier_ep}"
    );
    // Same identity fields reused across both episodes.
    assert_eq!(worker_ep["class"], verifier_ep["class"]);
    assert_eq!(worker_ep["touched_files"], verifier_ep["touched_files"]);
    assert_eq!(worker_ep["pass"], true, "worker episode: {worker_ep}");
    assert_eq!(verifier_ep["pass"], true, "verifier episode: {verifier_ep}");

    // t2 (no verifier_model) must produce exactly ONE (worker) episode.
    let t2_episodes: Vec<&serde_json::Value> = episodes
        .iter()
        .filter(|e| e["title"] == "task without verifier")
        .collect();
    assert_eq!(
        t2_episodes.len(),
        1,
        "expected exactly 1 episode (worker only) for t2, got {}: {episodes:?}",
        t2_episodes.len()
    );
    assert_eq!(
        t2_episodes[0]["role"], "worker",
        "t2 episode: {:?}",
        t2_episodes[0]
    );
}
