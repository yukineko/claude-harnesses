// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A failing claim-upkeep step inside `condukt state set` must be SURFACED, not
//! discarded.
//!
//! `StateAction::Set` ends with two claim-upkeep calls whose `Result` was
//! thrown away (`let _ = claim::release_files(..)` / `let _ = claim::heartbeat(..)`),
//! justified by a comment reading "Both are fail-soft — never break the update."
//! That is the phrasing CLAUDE.md §1 names as a red flag on a verdict-carrying
//! path, and here it did exactly what the section predicts:
//!
//! - a failed `heartbeat` means this run's claims were NOT refreshed, so a live
//!   session's claims can be reaped as stale while it is still working (the very
//!   failure mode backlog `cd624e4c` exists to close);
//! - a failed `release_files` means a terminal task's files stay claimed and
//!   block other sessions until the TTL expires.
//!
//! Neither was observable anywhere: no stderr, no exit code, no journal.
//!
//! Contract pinned here (see the doc comment on the upkeep block in
//! `main.rs`): the durable state write has ALREADY happened by this point
//! (`rs.save(..)?` runs earlier), so the process still exits 0 — reporting
//! failure for an operation that succeeded would be its own lie — but the
//! upkeep failure is printed to stderr naming the step, the run, the underlying
//! error and the CONSEQUENCE. That is not a §3 exemption: §3 forbids mapping
//! "cannot determine" onto "clean", and a loud stderr warning is precisely the
//! refusal to call it clean.
//!
//! Fault injection: `claim::load_or_refuse` refuses on an unparseable claim
//! registry (commit `6907b0e3`), so garbage in `claims.json` makes both upkeep
//! calls return `Err`.
//!
//! Every warning arm here is paired with an ANTI-VACUITY CONTROL that runs the
//! SAME command against an INTACT registry and asserts (a) the warning is
//! absent and (b) the upkeep step demonstrably ran anyway (the heartbeat
//! advanced / the claim was released). Without (b) the control would also pass
//! against code that never calls the step at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let mut base = std::env::temp_dir();
        base.push(format!("condukt-hb-surfaced-{pid}-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "t@t.t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        Self { repo, home }
    }

    fn condukt(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-test")
            .output()
            .expect("spawn condukt")
    }

    fn write_decomp(&self, name: &str, file: &str) -> PathBuf {
        let p = self.repo.join(name);
        let json = format!(
            r#"{{"goal":"touch {file}","tasks":[{{"id":"t1","title":"edit {file}","touched_files":["{file}"],"deps":[],"class":"parallel","done_criteria":"d"}}]}}"#
        );
        std::fs::write(&p, json).unwrap();
        p
    }

    fn init(&self, run: &str, decomp: &Path) {
        let out = self.condukt(&[
            "state",
            "init",
            "--run",
            run,
            "--file",
            decomp.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "state init {run} failed: {out:?}");
    }

    fn set(&self, run: &str, status: &str) -> Output {
        self.condukt(&[
            "state", "set", "--run", run, "--task", "t1", "--status", status,
        ])
    }

    /// The durable task status as `condukt state show` reports it — the
    /// observable that tells us whether the state write survived.
    fn shown_status(&self, run: &str) -> String {
        let out = self.condukt(&["state", "show", "--run", run]);
        assert!(out.status.success(), "state show failed: {out:?}");
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("state show must emit JSON");
        v["tasks"][0]["status"]
            .as_str()
            .expect("task status must be a string")
            .to_string()
    }

    /// Locate the on-disk `claims.json` the binary writes under the sandboxed
    /// HOME, without duplicating the private (project-key partitioned) path
    /// derivation.
    fn claims_json(&self) -> Option<PathBuf> {
        find_named(&self.home, "claims.json")
    }

    fn registry(&self) -> serde_json::Value {
        let p = self.claims_json().expect("claims.json must exist");
        serde_json::from_slice(&std::fs::read(&p).unwrap()).expect("claims.json must be JSON")
    }

    fn heartbeat_at(&self, file: &str) -> i64 {
        let v = self.registry();
        file_claim(&v, file)
            .and_then(|c| c["heartbeat_at"].as_i64())
            .unwrap_or_else(|| panic!("no heartbeat_at for {file} in {v}"))
    }
}

/// The file table is `#[serde(flatten)]`ed to the registry's top level for
/// backward compatibility, so accept either shape rather than pinning one.
fn file_claim<'a>(reg: &'a serde_json::Value, file: &str) -> Option<&'a serde_json::Value> {
    reg.get("files")
        .and_then(|f| f.get(file))
        .or_else(|| reg.get(file))
}

fn find_named(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_named(&p, name) {
                return Some(found);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
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

const CORRUPT: &[u8] = b"{ this is not valid json ]]";

/// Which required ingredients of the warning are MISSING from `stderr`.
///
/// Asserted on substance, not on punctuation/wording: the step that failed, the
/// run it belongs to, the underlying error, and the consequence in the caller's
/// terms.
fn missing_heartbeat_parts(stderr: &str, run: &str) -> Vec<&'static str> {
    let low = stderr.to_ascii_lowercase();
    let mut missing = Vec::new();
    if !low.contains("heartbeat") {
        missing.push("names the failing step (heartbeat)");
    }
    if !stderr.contains(run) {
        missing.push("names the run id");
    }
    if !low.contains("could not be read/parsed") {
        missing.push("quotes the underlying error");
    }
    if !(low.contains("reap") && low.contains("stale")) {
        missing.push("states the consequence (claims can be reaped as stale)");
    }
    missing
}

fn missing_release_parts(stderr: &str, run: &str) -> Vec<&'static str> {
    let low = stderr.to_ascii_lowercase();
    let mut missing = Vec::new();
    if !low.contains("release") {
        missing.push("names the failing step (release)");
    }
    if !stderr.contains(run) {
        missing.push("names the run id");
    }
    if !low.contains("could not be read/parsed") {
        missing.push("quotes the underlying error");
    }
    if !(low.contains("claimed") && low.contains("block")) {
        missing.push("states the consequence (files stay claimed and block others)");
    }
    missing
}

/// Did stderr mention a heartbeat-upkeep failure at all? Used by the controls,
/// which must see NOTHING of the sort.
fn mentions_heartbeat_failure(stderr: &str) -> bool {
    stderr.to_ascii_lowercase().contains("heartbeat")
}

fn mentions_release_failure(stderr: &str) -> bool {
    let low = stderr.to_ascii_lowercase();
    low.contains("claim release") || (low.contains("release") && low.contains("upkeep"))
}

/// Bring a run to `running` with an INTACT registry, so `claims.json` exists and
/// holds this run's file claim.
fn claiming_run(fx: &Fixture, run: &str, file: &str) {
    let dec = fx.write_decomp(&format!("dec-{run}.json"), file);
    fx.init(run, &dec);
    let out = fx.set(run, "running");
    assert!(
        out.status.success(),
        "precondition: {run} must claim {file}: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// (a) A corrupt registry makes `claim::heartbeat` fail — and that failure must
/// be NAMED on stderr instead of vanishing into `let _ =`.
///
/// `--status done` is used deliberately: it is not terminal, so `release_files`
/// does not run and this test observes the heartbeat arm ALONE.
#[test]
fn heartbeat_failure_is_named_on_stderr() {
    let fx = Fixture::new("hb-err");
    claiming_run(&fx, "runA", "src/shared.rs");
    let path = fx.claims_json().expect("precondition: registry must exist");
    std::fs::write(&path, CORRUPT).unwrap();

    let out = fx.set("runA", "done");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let missing = missing_heartbeat_parts(&stderr, "runA");
    assert!(
        missing.is_empty(),
        "the heartbeat failure was swallowed — stderr does not: {missing:?}\n\
         --- stderr ---\n{stderr}\n--- stdout ---\n{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // It must also not have clobbered the bytes it could not parse.
    assert_eq!(
        std::fs::read(&path).unwrap(),
        CORRUPT,
        "the upkeep path overwrote a registry it could not parse"
    );
}

/// (b) ANTI-VACUITY CONTROL for (a): the SAME command against an INTACT
/// registry must print no such warning — and the heartbeat must demonstrably
/// have run anyway (its timestamp advanced), so the silence means "it
/// succeeded", not "it was never called".
#[test]
fn intact_registry_prints_no_heartbeat_warning_yet_still_beats() {
    let fx = Fixture::new("hb-ok");
    claiming_run(&fx, "runA", "src/shared.rs");
    let before = fx.heartbeat_at("src/shared.rs");

    // now_secs() has 1s granularity, so wait past a tick or the advance is
    // unobservable and the control would prove nothing.
    std::thread::sleep(std::time::Duration::from_millis(1_200));

    let out = fx.set("runA", "done");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !mentions_heartbeat_failure(&stderr),
        "a healthy registry must not produce a heartbeat warning (otherwise the \
         warning is unconditional and proves nothing):\n{stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "healthy upkeep must exit 0: stderr={stderr}"
    );
    let after = fx.heartbeat_at("src/shared.rs");
    assert!(
        after > before,
        "the control is vacuous unless the heartbeat actually ran on this path: \
         heartbeat_at {before} -> {after}"
    );
}

/// (c) The chosen contract, pinned: the durable state write ALREADY happened
/// before the upkeep runs, so a failing upkeep step does NOT roll it back and
/// does NOT change the exit code. The failure is surfaced instead of being
/// resolved to silence.
#[test]
fn state_write_stands_and_exit_code_is_zero_when_upkeep_fails() {
    let fx = Fixture::new("hb-exit");
    claiming_run(&fx, "runA", "src/shared.rs");
    let path = fx.claims_json().expect("precondition: registry must exist");
    std::fs::write(&path, CORRUPT).unwrap();

    let out = fx.set("runA", "done");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(
        out.status.code(),
        Some(0),
        "the state write succeeded, so `state set` must still report success — \
         propagating the upkeep error here would report failure for an operation \
         that did happen:\n{stderr}"
    );
    assert_eq!(
        fx.shown_status("runA"),
        "done",
        "the durable state write must stand even though claim upkeep failed"
    );
    // …and the failure is still not silent.
    assert!(
        missing_heartbeat_parts(&stderr, "runA").is_empty(),
        "exit 0 is only honest if the failure is loud; stderr was:\n{stderr}"
    );
}

/// (d) The `release_files` half, same treatment: a terminal transition whose
/// release fails must say so, naming the consequence (the files stay claimed
/// and block other sessions).
#[test]
fn release_files_failure_is_named_on_stderr() {
    let fx = Fixture::new("rel-err");
    claiming_run(&fx, "runA", "src/shared.rs");
    let path = fx.claims_json().expect("precondition: registry must exist");
    std::fs::write(&path, CORRUPT).unwrap();

    let out = fx.set("runA", "failed");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let missing = missing_release_parts(&stderr, "runA");
    assert!(
        missing.is_empty(),
        "the release_files failure was swallowed — stderr does not: {missing:?}\n\
         --- stderr ---\n{stderr}"
    );
    // Both arms fail here, and both must be individually visible: a single
    // catch-all message would hide which half broke.
    assert!(
        missing_heartbeat_parts(&stderr, "runA").is_empty(),
        "the heartbeat half of the same upkeep block must be reported too:\n{stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "same contract as the heartbeat half: the state write stands\n{stderr}"
    );
    assert_eq!(fx.shown_status("runA"), "failed");
}

/// (b') ANTI-VACUITY CONTROL for (d): with an intact registry the same terminal
/// transition prints no release warning, and the claim is demonstrably gone —
/// so the silence means the release succeeded, not that it never ran.
#[test]
fn intact_registry_prints_no_release_warning_yet_still_releases() {
    let fx = Fixture::new("rel-ok");
    claiming_run(&fx, "runA", "src/shared.rs");
    assert!(
        fx.heartbeat_at("src/shared.rs") > 0,
        "precondition: the file must be claimed before we can watch it released"
    );

    let out = fx.set("runA", "failed");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !mentions_release_failure(&stderr),
        "a healthy registry must not produce a release warning:\n{stderr}"
    );
    assert!(
        !mentions_heartbeat_failure(&stderr),
        "a healthy registry must not produce a heartbeat warning:\n{stderr}"
    );
    assert_eq!(out.status.code(), Some(0), "stderr={stderr}");

    let v = fx.registry();
    assert!(
        file_claim(&v, "src/shared.rs").is_none(),
        "the control is vacuous unless release_files actually ran on this path; \
         registry still holds the file: {v}"
    );
}
