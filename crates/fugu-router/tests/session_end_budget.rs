// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The `SessionEnd` hook must fit Claude Code's SessionEnd budget, and a sync
//! that is cut short must not go dark.
//!
//! Observed 2026-10-25: Claude Code gives SessionEnd hooks a ~1.5 s budget, and
//! a plugin hook's `timeout` does not raise it. The wired command
//! (`hooks/hooks.json`) ran `harvest; sync` in the foreground, and `sync` does
//! a network `git pull` + `git push` (~1.5-2 s). Claude Code aborted it
//! ("Hook cancelled"); 9 local commits in the sync checkout stayed unpushed
//! since 2026-10-22. A killed process writes no `sync-error.json`, so the
//! `UserPromptSubmit` notice path never fired and nothing said so.
//!
//! Decided design (implemented separately): the hook returns within
//! milliseconds and runs harvest+sync in a fully detached child; a "pending"
//! marker is written BEFORE the work starts, cleared on success; failure writes
//! `sync-error.json`; `fugu-router prompt` reports a stale pending marker as a
//! failed/unfinished sync through the existing notice path.
//!
//! These tests are implementation-agnostic on purpose:
//!   * the hook command is read from `hooks/hooks.json`, not hard-coded;
//!   * "slow network" is a `git` shim first on PATH that sleeps on `pull` and
//!     `push` and otherwise execs the real git;
//!   * the kill in (3) targets whichever process is running `git pull` (the
//!     shim records its `$PPID`), so it holds whether the work runs in the
//!     hook process itself or in a detached child of any shape.
//!
//! Timing in (1) is measured until the hook process has exited AND its
//! stdout/stderr pipes reached EOF (`Command::output`). A "detached" child that
//! still holds the hook's stdio keeps the hook alive from the caller's point of
//! view, so that is not a fix.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_fugu-router");

/// The hook must be done well inside the ~1.5 s SessionEnd budget.
const HOOK_BUDGET: Duration = Duration::from_millis(1000);
/// How long the shim stalls `git pull` / `git push`.
const SHIM_SLEEP_SECS: u32 = 3;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} could not run: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed ({:?}):\n{}\n{}",
        dir.display(),
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn append_line(path: &Path, line: &str) {
    let mut body = std::fs::read_to_string(path).unwrap_or_default();
    body.push_str(line);
    body.push('\n');
    std::fs::write(path, body).unwrap();
}

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "fugu-sessionend-budget-{name}-{}",
        std::process::id()
    ));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// Everything one test needs: a scratch HOME whose sync checkout has an
/// uncommitted episode append, a local bare remote, a plugin root whose
/// `bin/fugu-router` is the built test binary, a slow-git shim dir, and a
/// non-git cwd (so `harvest` has nothing to read and stays a no-op).
struct Env {
    home: PathBuf,
    remote: PathBuf,
    plugin_root: PathBuf,
    shim_dir: PathBuf,
    cwd: PathBuf,
}

const MARKER_EPISODE: &str = r#"{"task":"session-end-budget-episode"}"#;

fn setup(name: &str) -> Env {
    let root = scratch(name);
    let home = root.join("home");
    let cfg_dir = home.join(".fugu-router");
    std::fs::create_dir_all(&cfg_dir).unwrap();

    let remote = root.join("remote.git");
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "--bare", "--initial-branch=main", "."]);

    let seed = root.join("seed");
    git(&root, &["clone", remote.to_str().unwrap(), "seed"]);
    append_line(&seed.join("episodes.jsonl"), r#"{"task":"seed"}"#);
    append_line(&seed.join("playbooks.jsonl"), r#"{"task":"seed"}"#);
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-m", "seed"]);
    git(&seed, &["push", "origin", "main"]);

    let record = cfg_dir.join("record-repo");
    git(
        &cfg_dir,
        &["clone", remote.to_str().unwrap(), "record-repo"],
    );
    git(&record, &["config", "user.name", "t"]);
    git(&record, &["config", "user.email", "t@example.com"]);
    // A dirty episodes store: what `record` leaves behind every session.
    append_line(&record.join("episodes.jsonl"), MARKER_EPISODE);

    std::fs::write(
        cfg_dir.join("config.toml"),
        format!("sync_repo = \"{}\"\n", remote.to_str().unwrap()),
    )
    .unwrap();

    let plugin_root = root.join("plugin");
    std::fs::create_dir_all(plugin_root.join("bin")).unwrap();
    std::os::unix::fs::symlink(EXE, plugin_root.join("bin").join("fugu-router")).unwrap();

    let shim_dir = root.join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    write_git_shim(&shim_dir, &root.join("shim-log"));

    let cwd = root.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();

    Env {
        home,
        remote,
        plugin_root,
        shim_dir,
        cwd,
    }
}

fn real_git() -> String {
    let out = harness_core::git_env::scrub(&mut Command::new("sh"))
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        out.status.success() && path.starts_with('/'),
        "could not resolve the real git on PATH: {path:?}"
    );
    path
}

/// `git` shim: on `pull` / `push` it records `<own pid> <parent pid>` to
/// `<log>/<verb>.pids`, sleeps, then execs the real git; every other verb
/// execs the real git immediately.
fn write_git_shim(shim_dir: &Path, log_dir: &Path) {
    std::fs::create_dir_all(log_dir).unwrap();
    let script = format!(
        "#!/bin/sh\n\
         for a in \"$@\"; do\n\
         \x20 case \"$a\" in\n\
         \x20   pull|push)\n\
         \x20     echo \"$$ $PPID\" >> '{log}/'\"$a\".pids\n\
         \x20     sleep {sleep}\n\
         \x20     break ;;\n\
         \x20 esac\n\
         done\n\
         exec '{real}' \"$@\"\n",
        log = log_dir.display(),
        sleep = SHIM_SLEEP_SECS,
        real = real_git(),
    );
    let path = shim_dir.join("git");
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn shim_log(env: &Env) -> PathBuf {
    env.shim_dir.parent().unwrap().join("shim-log")
}

/// The SessionEnd command string(s), exactly as `hooks/hooks.json` wires them.
fn session_end_commands() -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("hooks")
        .join("hooks.json");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let cmds: Vec<String> = doc["hooks"]["SessionEnd"]
        .as_array()
        .expect("hooks.json declares no SessionEnd array")
        .iter()
        .filter_map(|g| g["hooks"].as_array())
        .flatten()
        .filter_map(|h| h["command"].as_str())
        .map(str::to_string)
        .collect();
    assert!(
        !cmds.is_empty(),
        "hooks.json declares no SessionEnd command"
    );
    cmds
}

/// Common environment for every fugu-router / hook invocation in these tests.
fn apply_env(cmd: &mut Command, env: &Env) {
    let path = format!(
        "{}:{}",
        env.shim_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    cmd.env("HOME", &env.home)
        .env("PATH", path)
        .env("CLAUDE_PLUGIN_ROOT", &env.plugin_root)
        .env_remove("FUGU_ROUTER_DISABLED")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CEILING_DIRECTORIES", env.cwd.parent().unwrap())
        .current_dir(&env.cwd);
}

fn session_end_payload(env: &Env) -> String {
    serde_json::json!({
        "session_id": "budget-test",
        "hook_event_name": "SessionEnd",
        "reason": "exit",
        "cwd": env.cwd,
        "transcript_path": env.cwd.join("transcript.jsonl"),
    })
    .to_string()
}

/// Spawn one SessionEnd command via `sh -c`, the way Claude Code runs it.
fn spawn_hook(env: &Env, command: &str) -> std::process::Child {
    let mut cmd = Command::new("sh");
    harness_core::git_env::scrub(&mut cmd);
    cmd.args(["-c", command]);
    apply_env(&mut cmd, env);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(session_end_payload(env).as_bytes())
        .unwrap();
    drop(child.stdin.take());
    child
}

fn run_prompt(env: &Env) -> std::process::Output {
    let mut cmd = Command::new(EXE);
    harness_core::git_env::scrub(&mut cmd);
    cmd.arg("prompt");
    apply_env(&mut cmd, env);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Not "actionable" (see inject::looks_actionable), so the routing summary
    // stays out of stdout and only a sync/harvest notice can appear there.
    let payload = r#"{"session_id":"budget-test","hook_event_name":"UserPromptSubmit","prompt":"hello there"}"#;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

fn remote_has_episode(env: &Env) -> bool {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(&env.remote)
        .args(["show", "main:episodes.jsonl"])
        .output()
        .unwrap();
    out.status.success() && String::from_utf8_lossy(&out.stdout).contains(MARKER_EPISODE)
}

/// (1) The SessionEnd command must return well inside the budget even when the
/// network is slow, because Claude Code cancels it at ~1.5 s regardless of the
/// declared `timeout`.
#[test]
fn session_end_hook_returns_within_budget_on_slow_network() {
    let env = setup("fast");
    for command in session_end_commands() {
        let started = Instant::now();
        let out = spawn_hook(&env, &command).wait_with_output().unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed < HOOK_BUDGET,
            "SessionEnd command {command:?} took {elapsed:?} (budget {HOOK_BUDGET:?}) \
             with `git pull`/`git push` stalled {SHIM_SLEEP_SECS}s each. Claude Code \
             cancels SessionEnd hooks at ~1.5 s, so this sync is killed mid-flight \
             and local records never reach sync_repo.\n\
             exit: {:?}\nstdout: {}\nstderr: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// (2) Returning fast must not mean dropping the work: the dirty episode store
/// is still committed and pushed to the remote.
#[test]
fn session_end_hook_work_still_lands_on_the_remote() {
    let env = setup("lands");
    assert!(
        !remote_has_episode(&env),
        "precondition failed: the remote already has the episode"
    );
    for command in session_end_commands() {
        let _ = spawn_hook(&env, &command).wait_with_output().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if remote_has_episode(&env) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let log = String::from_utf8_lossy(
        &harness_core::git_env::scrub(&mut Command::new("git"))
            .current_dir(&env.remote)
            .args(["log", "--oneline", "main"])
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned();
    panic!(
        "20 s after the SessionEnd hook, the bare remote still does not have the \
         episode appended to the dirty store ({MARKER_EPISODE}). Remote log:\n{log}"
    );
}

/// (3) A sync killed mid-flight (what Claude Code's cancellation does) must be
/// visible on the next prompt. The kill targets whichever process is running
/// `git pull` — the hook itself today, a detached child after the fix — so the
/// test holds for any implementation that writes its pending marker before it
/// starts working.
///
/// Note for the implementer: `prompt` runs right after the kill, so a pending
/// marker whose owner is dead must be reported immediately (e.g. judged by
/// owner liveness), not only after a long age threshold.
#[test]
fn a_killed_session_end_sync_is_reported_by_the_prompt_hook() {
    let env = setup("killed");
    let commands = session_end_commands();
    let started = Instant::now();
    let mut hooks: Vec<std::process::Child> =
        commands.iter().map(|c| spawn_hook(&env, c)).collect();

    // Wait until some process is inside the (stalled) `git pull`.
    let pids_file = shim_log(&env).join("pull.pids");
    let deadline = Instant::now() + Duration::from_secs(20);
    let pids = loop {
        if let Ok(s) = std::fs::read_to_string(&pids_file) {
            if let Some(line) = s.lines().next() {
                break line.to_string();
            }
        }
        assert!(
            Instant::now() < deadline,
            "precondition failed: no process reached `git pull` within 20 s of the \
             SessionEnd hook, so there is no in-flight sync to kill"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    // ~1.5 s after the hook started, as Claude Code's cancellation would.
    let at = Duration::from_millis(1500);
    if started.elapsed() < at {
        std::thread::sleep(at - started.elapsed());
    }
    let mut parts = pids.split_whitespace();
    let shim_pid = parts.next().unwrap().to_string();
    let worker_pid = parts.next().unwrap().to_string();
    let kill = Command::new("kill")
        .args(["-9", &worker_pid, &shim_pid])
        .status()
        .unwrap();
    assert!(
        kill.success(),
        "precondition failed: could not SIGKILL the syncing process {worker_pid}"
    );
    for h in hooks.iter_mut() {
        let _ = h.kill();
        let _ = h.wait();
    }
    // Give a killed process nothing to finish: it is gone. Let the dust settle.
    std::thread::sleep(Duration::from_millis(200));

    assert!(
        !remote_has_episode(&env),
        "precondition failed: the episode reached the remote before the kill, so \
         this run did not observe an interrupted sync"
    );

    let out = run_prompt(&env);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "`fugu-router prompt` exited {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.to_lowercase().contains("sync"),
        "the sync process (pid {worker_pid}) was SIGKILLed inside `git pull`, yet the \
         next `fugu-router prompt` says nothing about a failed or unfinished sync. \
         A killed process writes no sync-error.json, so without a pending marker \
         written BEFORE the work, the cancelled sync is dark.\n\
         prompt stdout: {stdout:?}\nprompt stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// (4) Anti-vacuity control for (3): with no pending marker and no
/// sync-error.json, `prompt` emits no sync notice — so (3) passing means a
/// notice was actually produced by the interrupted sync, not by `prompt`
/// always talking about sync.
#[test]
fn prompt_emits_no_sync_notice_without_a_marker() {
    let env = setup("control");
    let fugu = env.home.join(".fugu-router");
    assert!(
        !fugu.join("sync-error.json").exists(),
        "precondition failed: sync-error.json already exists"
    );
    let out = run_prompt(&env);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "`fugu-router prompt` exited {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !stdout.to_lowercase().contains("sync"),
        "with no sync ever attempted, `prompt` still emitted a sync notice, so \
         a notice in the killed-sync test proves nothing.\nprompt stdout: {stdout:?}"
    );

    // Positive half: under this exact env and prompt payload (non-actionable,
    // so no routing summary), an existing sync-error.json DOES produce a
    // notice — so (3)'s assertion is reachable, not unsatisfiable.
    std::fs::write(fugu.join("sync-error.json"), "not json").unwrap();
    let out = run_prompt(&env);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.to_lowercase().contains("sync"),
        "an existing sync-error.json produced no sync notice under this test's \
         env, so the killed-sync assertion could never pass.\nprompt stdout: {stdout:?}"
    );
}
