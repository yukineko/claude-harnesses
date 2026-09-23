//! End-to-end proof that `Verdict::Undetermined` (exit 2 from `check_verdict`)
//! is reachable through the **CLI**, not just through library-level unit tests
//! that build a `Field` slice by hand.
//!
//! Backlog d17107ad / user ruling 2026-09-23, option (a): the five schemas
//! registered today (`decomposition`/`episode`/`playbook`/`scout-measure`/
//! `verdict`) cannot reach the `Undetermined` arm end-to-end, because the only
//! field that declares `items` (`decomposition.tasks`) is also typed
//! `Ty::Array`, so a non-array value is rejected by the *type* check before
//! the `items` constraint is ever reached (see `main.rs`'s `check_verdict`
//! doc-comment and `docs/specs/schemaguard.md`).
//!
//! The fix is to register a **new schema** whose items-declaring field is
//! typed `Ty::Any` instead of `Ty::Array` — `Ty::Any` waives the type check
//! (see `schema.rs`'s `Ty::Any` arm), so a non-array value sails past it and
//! reaches the `items` constraint, which then cannot be applied and resolves
//! to `Undetermined` → exit 2.
//!
//! ## Schema contract the implementer must register in `registry.rs`
//!
//! Schema name (as passed to `--schema`): **`undetermined-probe`**
//!
//! Top-level fields:
//!
//! - `name` — `Ty::String`, `required: true`, no `enum_values`, no `items`.
//!   Exists purely so a genuine, ordinary violation (missing required field)
//!   is also reachable on this schema, independent of the items probe below.
//! - `items_any` — `Ty::Any`, `required: true`, no `enum_values`, `items:
//!   NON-EMPTY` (a sub-schema with at least one field, e.g. a single
//!   `id: Ty::String, required: true` field, mirroring `ITEM_FIELDS` already
//!   used elsewhere in this crate's tests). The `Ty::Any` + non-empty `items`
//!   pairing is exactly the shape `schema.rs`'s own unit tests
//!   (`declared_items_that_cannot_be_applied_is_not_a_silent_pass` et al.)
//!   already exercise at the library level; this test requires the same
//!   shape to exist as a *named, registered* schema reachable from the CLI.
//!
//! Add `"undetermined-probe"` to `registry::names()` and its `Schema` to
//! `registry::get()` alongside the other five.
//!
//! ## What this test file asserts
//!
//! 1. `undetermined_items_value_exits_two` — `items_any` holds a non-array
//!    value → `check_verdict` cannot apply the declared `items` sub-schema →
//!    exit 2, and the CLI output names the field as undetermined.
//! 2. `valid_array_items_value_exits_zero` — positive control: `items_any`
//!    holds a conforming array → exit 0.
//! 3. `missing_required_name_exits_one` — a genuine violation (the ordinary
//!    required-field-missing case, unrelated to the items probe) → exit 1.
//!
//! Until `undetermined-probe` is registered, `check --schema undetermined-probe`
//! hits the CLI's fail-fast "unknown schema" branch (`cmd_check` in
//! `main.rs`), which also exits 2 — so today, test 1 passes for the WRONG
//! reason (unknown schema, not undetermined-items) while tests 2 and 3 fail
//! outright (both expect exit 0 / exit 1 but get exit 2 from the same unknown-
//! schema branch, and the stdout shape does not match). Test 1 additionally
//! asserts on the stdout shape (`schema` field name and `undetermined`
//! section) precisely so it cannot be satisfied by the unknown-schema path.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_home() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("schemaguard-up-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Run `schemaguard <args>` with `payload` on stdin in an isolated HOME (so the
/// reject-metrics store at ~/.schemaguard is never the real one).
fn run(args: &[&str], payload: &str) -> (i32, String, String) {
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
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn undetermined_items_value_exits_two() {
    // `items_any` is an object, not an array: the declared `items` sub-schema
    // cannot be applied to it, so `check_verdict` must resolve this to
    // `Undetermined` → exit 2 — reached through the registered
    // `undetermined-probe` schema, not through the unit-tested `Field` slice
    // built by hand in `main.rs`'s own tests.
    let payload = r#"{"name":"x","items_any":{"id":"a"}}"#;
    let (code, stdout, stderr) = run(&["check", "--schema", "undetermined-probe"], payload);
    assert_eq!(
        code, 2,
        "a declared items constraint that cannot be applied must exit 2; \
         stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stdout.contains("\"schema\":\"undetermined-probe\""),
        "must be judged by the registered schema (not the unknown-schema \
         fail-fast branch, which never prints a schema field); got: {stdout}"
    );
    assert!(
        stdout.contains("undetermined") && stdout.contains("items_any"),
        "the verdict must name items_any under `undetermined`, not just \
         report a bare exit 2; got stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        !stdout.contains("unknown schema"),
        "must not fall through the unknown-schema branch; got: {stdout}"
    );
}

#[test]
fn valid_array_items_value_exits_zero() {
    // Positive control: `items_any` holds a conforming array, so the declared
    // items sub-schema is actually applied (and passes) rather than being
    // undetermined.
    let payload = r#"{"name":"x","items_any":[{"id":"a"}]}"#;
    let (code, stdout, stderr) = run(&["check", "--schema", "undetermined-probe"], payload);
    assert_eq!(
        code, 0,
        "a conforming array value must exit 0; stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stdout.contains("\"valid\":true"),
        "expected valid:true, got: {stdout}"
    );
}

#[test]
fn missing_required_name_exits_one() {
    // Genuine violation control: `name` is required and absent. Independent of
    // the items-undetermined probe — proves this schema can also reach the
    // ordinary "checked and failed" exit 1 arm.
    let payload = r#"{"items_any":[{"id":"a"}]}"#;
    let (code, stdout, stderr) = run(&["check", "--schema", "undetermined-probe"], payload);
    assert_eq!(
        code, 1,
        "a missing required field must exit 1; stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stdout.contains("\"valid\":false"),
        "expected valid:false, got: {stdout}"
    );
    assert!(
        stdout.contains("\"path\":\"name\""),
        "the violation must name the missing `name` field, got: {stdout}"
    );
}
