//! End-to-end: an observation autoflow **could not make** must never be read as
//! "there is no work left".
//!
//! `Phase::Done` is a latch — once a session reaches it, `Phase::Done => {}` in
//! the Stop state machine means autoflow stays silent for the rest of that
//! session. Reaching it therefore requires an *observation* that the pending set
//! is empty, not merely a failure to observe it. The audit
//! (`docs/autoflow-verdict-audit.md` §4.4, FAULT B) recorded the opposite:
//! a condukt run-state file that is valid JSON but whose first task omits
//! `status` fails to deserialize as a whole (`TaskState.status` has no
//! `#[serde(default)]`), so a run containing perfectly healthy tasks became
//! invisible and the session latched `done` with no diagnostic at all.
//!
//! These tests drive the real binary (the only place the whole chain
//! run-state → `find_pending` → Stop branch → persisted phase is observable) and
//! deliberately include both directions:
//!
//! * `undetermined_*` — could not observe ⇒ must NOT latch `done`, and must say
//!   so visibly (silence would be the same fail-open wearing a different mask).
//! * `genuinely_empty_*` / `pending_*` — real observations still decide. Without
//!   these two controls the undetermined test would also pass against a trivial
//!   "always block, never finish" implementation, which is not the fix.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use harness_core::projkey::project_key;

/// A temp `HOME` holding a fake repo, its condukt run-state dir, and autoflow's
/// own session-state dir — the exact layout `docs/autoflow-verdict-audit.md`
/// §4.9 prescribes for these fault injections.
struct Env {
    home: PathBuf,
    repo: PathBuf,
    run_dir: PathBuf,
    session: String,
}

impl Env {
    fn new(tag: &str) -> Self {
        let home = std::env::temp_dir().join(format!(
            "autoflow-undetermined-{}-{}-{}",
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

        let session = format!("sess-{tag}");
        let env = Env {
            home,
            repo,
            run_dir,
            session,
        };
        // Enter the continuation branch of the state machine (Idle would just
        // check session-insights metrics and return).
        env.write_phase("record_requested");
        env
    }

    fn state_path(&self) -> PathBuf {
        self.home
            .join(".autoflow")
            .join("state")
            .join(format!("{}.json", self.session))
    }

    fn write_phase(&self, phase: &str) {
        let p = self.state_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, format!(r#"{{"phase":"{phase}"}}"#)).unwrap();
    }

    fn write_run_state(&self, tasks_json: &str) {
        std::fs::write(
            self.run_dir.join("run-20260101-000000-1.json"),
            format!(r#"{{"run_id":"run-20260101-000000-1","goal":"g","tasks":{tasks_json}}}"#),
        )
        .unwrap();
    }

    /// Write a raw (possibly non-JSON) run-state body.
    fn write_run_state_raw(&self, body: &str) {
        std::fs::write(self.run_dir.join("run-20260101-000000-1.json"), body).unwrap();
    }

    /// The phase autoflow persisted, as a bare string (`"done"`, `"continuing"`,
    /// …). Absent state file ⇒ `None`.
    fn phase(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.state_path()).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        Some(v.get("phase")?.as_str()?.to_string())
    }

    /// Run `autoflow stop` for this session. `PATH` is narrowed so the
    /// `backlog` binary is genuinely absent (an *observation* that there is no
    /// queue — backlog's documented carve-out), keeping the condukt run-state
    /// the only variable under test.
    fn stop(&self) -> (i32, String, String) {
        run_stop(&self.session, &self.repo, &self.home, "/usr/bin:/bin")
    }

    /// As [`Env::stop`], but with `dir` prepended to the child's `PATH` (used to
    /// plant a stub `backlog`). Only the child's environment is touched, so
    /// tests stay independent of the host's PATH and of each other.
    fn stop_with_path(&self, dir: &Path) -> (i32, String, String) {
        let path = format!("{}:/usr/bin:/bin", dir.to_string_lossy());
        run_stop(&self.session, &self.repo, &self.home, &path)
    }

    /// The plugin-cache directory `find_backlog_binary` scans when `backlog` is
    /// not on PATH.
    fn plugin_cache(&self) -> PathBuf {
        self.home
            .join(".claude")
            .join("plugins")
            .join("cache")
            .join("yukineko")
            .join("backlog")
    }
}

fn run_stop(session: &str, cwd: &Path, home: &Path, path: &str) -> (i32, String, String) {
    let payload = format!(
        r#"{{"hook_event_name":"Stop","session_id":"{}","cwd":"{}","transcript_path":""}}"#,
        session,
        cwd.to_string_lossy()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_autoflow"))
        .arg("stop")
        .env("HOME", home)
        .env("PATH", path)
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

fn blocks(stdout: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .and_then(|v| v.get("decision").and_then(|d| d.as_str()).map(String::from))
        .as_deref()
        == Some("block")
}

/// FAULT B (audit §4.4): valid JSON, but one task omits `status`. `TaskState`
/// has no `#[serde(default)]` for it, so the WHOLE run fails to deserialize —
/// including `t2`, which is a healthy pending task. autoflow cannot see the
/// pending set at all here; it must not conclude the set is empty.
#[test]
fn undetermined_run_state_missing_a_task_status_does_not_latch_done() {
    let env = Env::new("missing-status");
    env.write_run_state(r#"[{"id":"t1"},{"id":"t2","status":"pending"}]"#);

    let (code, stdout, stderr) = env.stop();
    assert_eq!(code, 0, "a Stop hook always exits 0; stderr: {stderr}");
    assert_ne!(
        env.phase().as_deref(),
        Some("done"),
        "an unreadable run-state is not an observation of an empty queue, \
         but the session latched Phase::Done (stdout: {stdout:?})"
    );
    assert!(
        blocks(&stdout),
        "an undetermined observation must be surfaced, not silently swallowed \
         (silence reads as 'nothing to do'); got stdout: {stdout:?}"
    );
}

/// The same sink reached through a different fault: the file is not JSON at all
/// (audit §4.4 FAULT A). Pinning both inputs keeps a fix that special-cases one
/// serde error shape from passing.
#[test]
fn undetermined_run_state_unparseable_does_not_latch_done() {
    let env = Env::new("unparseable");
    env.write_run_state_raw("{ this is not json ");

    let (code, stdout, stderr) = env.stop();
    assert_eq!(code, 0, "a Stop hook always exits 0; stderr: {stderr}");
    assert_ne!(
        env.phase().as_deref(),
        Some("done"),
        "unparseable run-state latched Phase::Done (stdout: {stdout:?})"
    );
    assert!(
        blocks(&stdout),
        "unparseable run-state must be surfaced; got stdout: {stdout:?}"
    );
}

/// ANTI-VACUITY CONTROL 1. A genuinely observed empty world — no run files
/// (`read_dir` succeeds and finds none) and no `backlog` binary (backlog's
/// documented "no queue exists" carve-out) — is a real observation, and must
/// still reach `Phase::Done` silently. If the fix simply blocked whenever it saw
/// no work, this test fails.
#[test]
fn genuinely_empty_world_still_latches_done_silently() {
    let env = Env::new("empty");
    // run_dir exists and is readable; it just contains no run-*.json.

    let (code, stdout, stderr) = env.stop();
    assert_eq!(code, 0, "a Stop hook always exits 0; stderr: {stderr}");
    assert!(
        stdout.trim().is_empty(),
        "an observed-empty queue is the one legitimate stop — no block expected, got: {stdout:?}"
    );
    assert_eq!(
        env.phase().as_deref(),
        Some("done"),
        "an observed-empty queue must still conclude Done"
    );
}

/// Audit §4.2 (P-2): the queue was asked for `--status pending` and answered
/// with items, but none carry that status — the two sides disagree about the
/// vocabulary, so the reply could not be interpreted. Filtering it down to an
/// empty vec and latching `done` reports "no work" on the strength of an answer
/// we failed to read.
#[test]
fn backlog_answering_in_an_unknown_status_vocabulary_does_not_latch_done() {
    let env = Env::new("vocabulary");
    let bin_dir = env.home.join("stub-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    // A stub `backlog`: nothing is driving the queue (`none`), and `list`
    // answers with an item whose status is a word this autoflow does not know.
    write_stub_backlog(
        &bin_dir,
        r#"[{"id":"a","title":"T","project":"/p","status":"open"}]"#,
    );

    let (code, stdout, stderr) = env.stop_with_path(&bin_dir);
    assert_eq!(code, 0, "a Stop hook always exits 0; stderr: {stderr}");
    assert_ne!(
        env.phase().as_deref(),
        Some("done"),
        "an uninterpretable queue answer latched Phase::Done (stdout: {stdout:?})"
    );
    assert!(
        blocks(&stdout),
        "an uninterpretable queue answer must be surfaced; got stdout: {stdout:?}"
    );
}

/// Audit §4.5 (P-5), the one permissive-A path: a plugin-cache directory that
/// cannot be listed is a failure to observe whether `backlog` is installed —
/// not an observation that it isn't. Reading it as "not installed" skips the
/// driver-liveness check entirely (`backlog_driver_active` returns false with no
/// binary), so autoflow would start an unattended auto-loop that may be racing a
/// live `/flow` driver. Cannot-determine must stand down instead.
#[cfg(unix)]
#[test]
fn unreadable_plugin_cache_stands_down_instead_of_auto_driving() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new("cache-unreadable");
    // Work is waiting, so a stand-down is observably different from "nothing
    // to do": if autoflow proceeds, it blocks with the condukt continuation.
    env.write_run_state(r#"[{"id":"t1","status":"pending"}]"#);

    let cache = env.plugin_cache();
    std::fs::create_dir_all(&cache).unwrap();
    // Executable but NOT readable: a cached binary could still be launched,
    // only the listing is denied.
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o111)).unwrap();

    let (code, stdout, stderr) = env.stop();

    // Restore before asserting so a failure can't leave an unreadable temp dir.
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(code, 0, "a Stop hook always exits 0; stderr: {stderr}");
    assert!(
        stdout.trim().is_empty(),
        "could not determine whether a driver is active ⇒ stand down for this \
         tick, but autoflow drove the queue anyway: {stdout:?}"
    );
    assert_eq!(
        env.phase().as_deref(),
        Some("record_requested"),
        "standing down must leave the phase untouched for the next Stop"
    );
}

/// Write a stub `backlog` that reports an idle queue for `lock status` and the
/// given JSON array for `list`. `find_backlog_binary` only checks that
/// `backlog --version` *spawns*, so any executable named `backlog` is found.
fn write_stub_backlog(dir: &Path, list_json: &str) {
    let script = format!(
        "#!/bin/sh\n\
         case \"$1\" in\n\
         lock) echo none ;;\n\
         list) echo '{list_json}' ;;\n\
         esac\n\
         exit 0\n"
    );
    let path = dir.join("backlog");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// ANTI-VACUITY CONTROL 3. The same stub `backlog`, answering in the vocabulary
/// autoflow does expect, must drive the queue normally — proving the test above
/// fails on the *vocabulary*, not merely on the presence of a stub binary.
#[test]
fn backlog_answering_with_pending_items_still_drives_the_queue() {
    let env = Env::new("vocabulary-control");
    let bin_dir = env.home.join("stub-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    write_stub_backlog(
        &bin_dir,
        r#"[{"id":"a","title":"Real task","project":"/p","status":"pending"}]"#,
    );

    let (code, stdout, stderr) = env.stop_with_path(&bin_dir);
    assert_eq!(code, 0, "a Stop hook always exits 0; stderr: {stderr}");
    assert!(
        blocks(&stdout) && stdout.contains("Real task"),
        "a readable, pending queue must drive /backlog, got: {stdout:?}"
    );
    assert_eq!(env.phase().as_deref(), Some("continuing"));
}

/// ANTI-VACUITY CONTROL 2. A well-formed run-state with a pending task must
/// still drive the condukt continuation (and not be swept into the undetermined
/// path). Proves the harness itself reaches the branch under test.
#[test]
fn pending_run_state_still_drives_condukt() {
    let env = Env::new("pending");
    env.write_run_state(r#"[{"id":"t1","status":"pending"}]"#);

    let (code, stdout, stderr) = env.stop();
    assert_eq!(code, 0, "a Stop hook always exits 0; stderr: {stderr}");
    assert!(
        blocks(&stdout) && stdout.contains("condukt"),
        "a pending task must block with the condukt continuation, got: {stdout:?}"
    );
    assert_eq!(
        env.phase().as_deref(),
        Some("continuing"),
        "driving condukt keeps the session continuing"
    );
}
