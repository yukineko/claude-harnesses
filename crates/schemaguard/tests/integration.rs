//! End-to-end tests for the schemaguard binary.
//!
//! schemaguard is a plain 0/1/2 gate CLI (NOT a lifecycle hook):
//!   0 — JSON parsed and schema valid (or `metrics`/`list` succeeded)
//!   1 — JSON parsed but schema violations found
//!   2 — could not determine: JSON failed to parse, an unknown schema was
//!       requested, or (`metrics`) the reject store exists but is unreadable
//! `list` is the safe read-only subcommand.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("schemaguard-it-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Run `schemaguard <args>` with `payload` on stdin in an isolated HOME (so the
/// reject-metrics store at ~/.schemaguard is never the real one).
fn run(args: &[&str], payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_schemaguard");
    let home = temp_home();
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(&home)
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    // Best-effort stdin write: a subcommand that exits early (an unknown schema
    // returns exit 2 before reading stdin) closes the read end of the pipe, so
    // `write_all` can return BrokenPipe. That is not a failure — the assertions
    // are on the child's exit code and stdout — so ignore a write error. Dropping
    // the handle at the end of the block closes stdin, giving the child EOF.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn help_describes_the_gate() {
    let (code, stdout) = run(&["--help"], "");
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Schema-validation gate"),
        "expected about string, got: {stdout}"
    );
}

#[test]
fn list_prints_known_schema_names() {
    let (code, stdout) = run(&["list"], "");
    assert_eq!(code, 0);
    assert!(
        stdout.contains("decomposition") && stdout.contains("episode"),
        "expected known schema names, got: {stdout}"
    );
}

#[test]
fn check_valid_decomposition_exits_zero() {
    let payload = r#"{"goal":"ship it","tasks":[{"id":"t1","title":"do","class":"serial","done_criteria":"done"}]}"#;
    let (code, stdout) = run(&["check", "--schema", "decomposition"], payload);
    assert_eq!(code, 0, "valid JSON + valid schema must exit 0");
    assert!(
        stdout.contains("\"valid\":true"),
        "expected valid:true, got: {stdout}"
    );
}

#[test]
fn check_schema_violation_exits_one() {
    // Missing the required `tasks` field → schema violation, exit 1.
    let payload = r#"{"goal":"ship it"}"#;
    let (code, stdout) = run(&["check", "--schema", "decomposition"], payload);
    assert_eq!(code, 1, "schema violations must exit 1");
    assert!(
        stdout.contains("\"valid\":false"),
        "expected valid:false, got: {stdout}"
    );
}

// ── "checked and clean" must be distinguishable from "never checked" ─────────
//
// docs/audit-schemaguard-verdict-paths.md §5-6/§5-8: a decomposition task that
// carries only `id` never evaluates the `class` / `suggested_model` /
// `confidence` enum constraints (the fields are absent and `required: false`,
// so schema.rs:114 `continue`s before the enum check). Its CLI verdict is
// nevertheless byte-identical to a task whose enums WERE evaluated and passed:
// `{"valid":true,"schema":"decomposition","errors":[]}` + exit 0. Silence reads
// as "every declared check ran and passed", which is not what happened.
//
// The optional-and-absent path is a *declared* permissive (registry.rs keeps
// `required` in lockstep with condukt's `model::Task`), so the fix is NOT to
// reject it — it is to stop the verdict from claiming a completeness it does
// not have: the output must name the declared checks that were not performed.

#[test]
fn valid_verdict_names_the_declared_checks_it_did_not_perform() {
    // Only `id` present → the `class`/`suggested_model`/`confidence` enum
    // constraints declared for a task are never evaluated.
    let payload = r#"{"goal":"x","tasks":[{"id":"t1"}]}"#;
    let (code, stdout) = run(&["check", "--schema", "decomposition"], payload);
    assert_eq!(
        code, 0,
        "an absent optional field is a declared permissive, not a violation; got: {stdout}"
    );
    assert!(
        stdout.contains("\"valid\":true"),
        "expected valid:true, got: {stdout}"
    );
    assert!(
        stdout.contains("not_checked"),
        "verdict must carry a not_checked section so 'checked clean' and \
         'never checked' are distinguishable; got: {stdout}"
    );
    assert!(
        stdout.contains("tasks[0].class"),
        "the un-evaluated `class` enum constraint must be named in the verdict; got: {stdout}"
    );
}

#[test]
fn fully_populated_payload_reports_nothing_as_not_checked() {
    // Anti-vacuity control for the test above: an implementation that simply
    // always lists every field would pass that assertion while proving nothing.
    // Here every declared field is present and every declared constraint is
    // actually evaluated, so nothing may be reported as not-checked.
    let payload = r#"{"goal":"x","tasks":[{"id":"t1","title":"do","class":"serial","done_criteria":"d","suggested_model":"sonnet","confidence":"high"}]}"#;
    let (code, stdout) = run(&["check", "--schema", "decomposition"], payload);
    assert_eq!(code, 0, "fully-populated valid payload must exit 0");
    assert!(
        stdout.contains("\"not_checked\":[]"),
        "every declared check ran here, so not_checked must be empty; got: {stdout}"
    );
}

#[test]
fn check_unknown_schema_exits_two() {
    let (code, _stdout) = run(&["check", "--schema", "no-such-schema"], "{}");
    assert_eq!(code, 2, "unknown schema must exit 2");
}

#[test]
fn check_unparseable_json_exits_two() {
    let (code, stdout) = run(&["check", "--schema", "decomposition"], "not json");
    assert_eq!(code, 2, "unparseable JSON must exit 2");
    assert!(
        stdout.contains("invalid JSON"),
        "expected parse-error message, got: {stdout}"
    );
}

// ── metrics: "could not read" must not read as "nothing to report" ───────────
//
// The reject counter exists so that silent drops at a source→executor boundary
// become observable. If an unreadable store folds into an empty map, the CLI
// reports zero rejects — i.e. "no silent drops" — which is the exact inversion
// of the crate's purpose. Absent (nothing recorded yet) and unreadable (cannot
// determine) must therefore be distinguishable downstream.

/// Outcome of a metrics run against a pre-seeded store.
struct StoreRun {
    code: i32,
    stdout: String,
    stderr: String,
    /// Whether the *test process itself* could still read the seeded store after
    /// `seed` ran. When a permission-denial fault is injected this is `false`;
    /// if it is `true` the denial did not take (e.g. running as root, or a
    /// filesystem that ignores mode bits), so the fault was never actually
    /// applied and asserting on it would prove nothing.
    fault_applied: bool,
}

/// Run `schemaguard <args>` in an isolated HOME whose `.schemaguard/rejects.jsonl`
/// has already been seeded by `seed`.
fn run_with_store(args: &[&str], seed: impl FnOnce(&std::path::Path)) -> StoreRun {
    let home = temp_home();
    let store_dir = home.join(".schemaguard");
    std::fs::create_dir_all(&store_dir).expect("create store dir");
    let store = store_dir.join("rejects.jsonl");
    seed(&store);
    let fault_applied = store.exists() && std::fs::read_to_string(&store).is_err();

    let bin = env!("CARGO_BIN_EXE_schemaguard");
    let out = Command::new(bin)
        .args(args)
        .current_dir(&home)
        .env("HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("binary runs");
    StoreRun {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        fault_applied,
    }
}

#[test]
fn metrics_absent_store_reports_empty_and_exits_zero() {
    // Control arm (anti-vacuity): a store that was never written is legitimately
    // empty and must STAY permissive. Without this, the restrictive assertions
    // below would also pass under a trivially broken implementation that simply
    // errors on every path.
    let r = run_with_store(&["metrics", "--json"], |_path| {});
    assert_eq!(
        r.code, 0,
        "absent store is not an error; stdout: {} stderr: {}",
        r.stdout, r.stderr
    );
    assert!(
        r.stdout.contains("{}"),
        "absent store should report an empty map, got: {}",
        r.stdout
    );
}

#[cfg(unix)]
#[test]
fn metrics_unreadable_store_is_not_reported_as_zero_rejects() {
    use std::os::unix::fs::PermissionsExt;

    let r = run_with_store(&["metrics", "--json"], |path| {
        std::fs::write(path, "{\"schema\":\"decomposition\",\"violations\":7}\n")
            .expect("seed store");
        // Fault injection: the store exists and holds 7 rejects, but cannot be read.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
    });

    // If the denial did not take (root, or a mode-ignoring filesystem), the fault
    // was never applied — report that instead of asserting a false pass.
    if !r.fault_applied {
        eprintln!("skipping: permission denial did not take (root or mode-ignoring fs)");
        return;
    }

    assert_ne!(
        r.code, 0,
        "an unreadable store must not exit 0 (that reads as 'no rejects'); \
         stdout: {} stderr: {}",
        r.stdout, r.stderr
    );
}

#[cfg(unix)]
#[test]
fn metrics_human_output_unreadable_store_does_not_say_no_rejects() {
    use std::os::unix::fs::PermissionsExt;

    let r = run_with_store(&["metrics"], |path| {
        std::fs::write(path, "{\"schema\":\"episode\",\"violations\":3}\n").expect("seed store");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
    });

    if !r.fault_applied {
        eprintln!("skipping: permission denial did not take (root or mode-ignoring fs)");
        return;
    }

    assert!(
        !r.stdout.contains("No rejects recorded yet"),
        "an unreadable store must not print the same line as a genuinely empty one; \
         stdout: {} stderr: {}",
        r.stdout,
        r.stderr
    );
}
