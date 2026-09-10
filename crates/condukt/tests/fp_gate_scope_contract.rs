//! Black-box contract tests for the SCOPE decision of the Fail→Pass
//! reproduction-oracle completion gate that guards
//! `condukt state set --status verified`.
//!
//! These tests are written from the stated contract only, against the REAL
//! built `condukt` binary and a real spawned `tdd` binary. Nothing here reads
//! the implementation.
//!
//! The contract under test:
//!
//! * The gate decides whether the oracle APPLIES by reading the run's
//!   persisted decomposition JSON and locating the entry whose `id` equals the
//!   task id. `kind` of `fix`/`feature` (case-insensitive) => oracle required;
//!   anything else or absent => not required.
//! * Required + genuine Fail→Pass (red `passed:false`, green `passed:true`)
//!   => ACCEPT (exit 0, `1/1 verified`).
//! * Required + anything else (including no proof artifacts at all)
//!   => REFUSE (nonzero, `refusing to verify`).
//! * Not required => ACCEPT regardless of proofs.
//! * The scope question itself can fail. When the decomposition cannot be READ
//!   or UNDERSTOOD, the gate cannot determine whether it applies, and that is
//!   NOT the same as "it does not apply": the promotion must be REFUSED with
//!   both `refusing to verify` and
//!   `cannot determine whether the fail-to-pass oracle applies`.
//!   Exactly three such cases: unreadable bytes, unparseable text, and no
//!   matching entry.
//! * ABSENCE of the decomposition file (ENOENT) is a real observation, not a
//!   failure to observe: the gate does not apply and the promotion succeeds.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

// ---------------------------------------------------------------------------
// Harness (adapted from tests/fp_oracle_e2e.rs)
// ---------------------------------------------------------------------------

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "condukt-fp-scope-{tag}-{}-{}",
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create isolated dir");
    dir
}

/// Locate a `tdd` binary usable by the spawned `condukt` process, building it
/// if it isn't already sitting next to this test binary in the target profile
/// directory. Returns the directory to prepend to `PATH`.
fn tdd_bin_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe of this test binary");
    let deps_dir = exe.parent().expect("deps dir").to_path_buf();
    let profile_dir = deps_dir.parent().expect("profile dir").to_path_buf();
    let bin_name = if cfg!(windows) { "tdd.exe" } else { "tdd" };
    let bin_path = profile_dir.join(bin_name);
    if !bin_path.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let is_release = profile_dir.file_name().and_then(|s| s.to_str()) == Some("release");
        let mut cmd = Command::new(&cargo);
        cmd.args(["build", "-p", "tdd", "--bin", "tdd"]);
        if is_release {
            cmd.arg("--release");
        }
        let status = cmd.status().expect("spawning `cargo build -p tdd`");
        assert!(status.success(), "`cargo build -p tdd` failed");
    }
    assert!(
        bin_path.exists(),
        "expected a tdd binary at {} after build",
        bin_path.display()
    );
    profile_dir
}

/// Run the real `condukt` binary with `args`, in `dir`, with `home` as `$HOME`
/// and `extra_path` prepended to `$PATH`. Returns `(exit_code, stdout, stderr)`.
fn run_condukt(dir: &Path, home: &Path, extra_path: &Path, args: &[&str]) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_condukt");
    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![extra_path.to_path_buf()];
    paths.extend(std::env::split_paths(&existing_path));
    let new_path = std::env::join_paths(paths).expect("join PATH");

    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("PATH", new_path)
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
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A single-task decomposition. `kind: None` omits the field entirely.
/// `fix`/`feature` tasks also declare `reproduction_tests`, so the only thing
/// under test is the SCOPE decision, never a missing declaration.
fn decomposition_json(task_id: &str, kind: Option<&str>) -> String {
    let mut task = serde_json::json!({ "id": task_id, "title": "demo task" });
    if let Some(k) = kind {
        task["kind"] = serde_json::Value::String(k.to_string());
        let lower = k.to_ascii_lowercase();
        if lower == "fix" || lower == "feature" {
            task["reproduction_tests"] =
                serde_json::Value::String("tests/repro.rs::reproduces_the_bug".to_string());
        }
    }
    serde_json::json!({ "goal": "demo goal", "tasks": [task] }).to_string()
}

/// Write a `<task>.<phase>.json` RED/GREEN proof artifact under `<dir>/.tdd/`.
fn write_proof(dir: &Path, task: &str, phase: &str, passed: bool) {
    let proof_dir = dir.join(".tdd");
    std::fs::create_dir_all(&proof_dir).expect("create proof dir");
    let path = proof_dir.join(format!("{task}.{phase}.json"));
    std::fs::write(&path, serde_json::json!({ "passed": passed }).to_string())
        .expect("write proof artifact");
}

/// Seed a fresh isolated run.
fn init_run(dir: &Path, home: &Path, tdd_path: &Path, run_id: &str, decomposition: &str) {
    let dec_path = dir.join("decomposition.json");
    std::fs::write(&dec_path, decomposition).expect("write decomposition");
    let (code, stdout, stderr) = run_condukt(
        dir,
        home,
        tdd_path,
        &[
            "state",
            "init",
            "--run",
            run_id,
            "--file",
            dec_path.to_str().unwrap(),
        ],
    );
    assert_eq!(
        code, 0,
        "state init must succeed\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// Recursively collect every regular file under `root`.
fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Locate the decomposition JSON `condukt state init` persisted for `run_id`
/// under the isolated `$HOME`. The intermediate directory layout is an
/// implementation detail, so it is discovered rather than hard-coded.
fn persisted_decomposition(home: &Path, run_id: &str) -> PathBuf {
    let state_root = home.join(".condukt").join("state");
    let mut files = Vec::new();
    walk(&state_root, &mut files);
    let wanted = format!("{run_id}.decomposition.json");
    let hits: Vec<&PathBuf> = files
        .iter()
        .filter(|p| p.file_name().and_then(|s| s.to_str()) == Some(wanted.as_str()))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one persisted `{wanted}` under {}; saw: {:#?}",
        state_root.display(),
        files
    );
    hits[0].clone()
}

/// Attempt the promotion under test.
fn verify(
    dir: &Path,
    home: &Path,
    tdd_path: &Path,
    run_id: &str,
    task_id: &str,
) -> (i32, String, String) {
    run_condukt(
        dir,
        home,
        tdd_path,
        &[
            "state", "set", "--run", run_id, "--task", task_id, "--status", "verified",
        ],
    )
}

fn assert_accepted(what: &str, code: i32, stdout: &str, stderr: &str) {
    assert_eq!(
        code, 0,
        "{what}: expected exit 0 (accepted)\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("1/1 verified"),
        "{what}: expected stderr to contain `1/1 verified`\nstdout: {stdout}\nstderr: {stderr}"
    );
}

fn assert_refused(what: &str, code: i32, stdout: &str, stderr: &str) {
    assert_ne!(
        code, 0,
        "{what}: expected a nonzero exit (refusal)\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("refusing to verify"),
        "{what}: expected stderr to contain `refusing to verify`\nstdout: {stdout}\nstderr: {stderr}"
    );
}

fn assert_cannot_determine(what: &str, code: i32, stdout: &str, stderr: &str) {
    assert_refused(what, code, stdout, stderr);
    assert!(
        stderr.contains("cannot determine whether the fail-to-pass oracle applies"),
        "{what}: expected stderr to contain `cannot determine whether the fail-to-pass oracle \
         applies`\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// 1. ACCEPT — oracle required and satisfied
// ---------------------------------------------------------------------------

#[test]
fn fix_with_genuine_fail_to_pass_is_accepted() {
    let dir = unique_dir("accept-fix");
    let home = unique_dir("accept-fix-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-accept-fix", "t-fix");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("fix")),
    );
    write_proof(&dir, task, "red", false);
    write_proof(&dir, task, "green", true);

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_accepted("kind:fix with red=false/green=true", code, &out, &err);
}

// ---------------------------------------------------------------------------
// 2. REFUSE — oracle required and not satisfied
// ---------------------------------------------------------------------------

#[test]
fn fix_with_non_fail_to_pass_proofs_is_refused() {
    let dir = unique_dir("refuse-badproof");
    let home = unique_dir("refuse-badproof-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-refuse-badproof", "t-fix");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("fix")),
    );
    // A test that passed in the RED phase never reproduced anything.
    write_proof(&dir, task, "red", true);
    write_proof(&dir, task, "green", true);

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_refused("kind:fix with red=true/green=true", code, &out, &err);
}

#[test]
fn fix_with_no_proof_artifacts_at_all_is_refused() {
    let dir = unique_dir("refuse-noproof");
    let home = unique_dir("refuse-noproof-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-refuse-noproof", "t-fix");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("fix")),
    );
    // No `.tdd/` directory at all.

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_refused("kind:fix with no proof artifacts", code, &out, &err);
}

// ---------------------------------------------------------------------------
// 3. ACCEPT — oracle not required (kind absent / outside fix|feature)
// ---------------------------------------------------------------------------

#[test]
fn task_without_kind_is_accepted_without_any_proofs() {
    let dir = unique_dir("accept-nokind");
    let home = unique_dir("accept-nokind-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-accept-nokind", "t-legacy");

    init_run(&dir, &home, &tdd, run, &decomposition_json(task, None));

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_accepted("task with no `kind`, no proofs", code, &out, &err);
}

#[test]
fn chore_task_is_accepted_even_with_a_failed_oracle_shaped_proof_pair() {
    let dir = unique_dir("accept-chore");
    let home = unique_dir("accept-chore-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-accept-chore", "t-chore");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("chore")),
    );
    // Deliberately NOT a Fail→Pass pair: it must not matter, the gate is
    // out of scope for `kind:"chore"`.
    write_proof(&dir, task, "red", true);
    write_proof(&dir, task, "green", false);

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_accepted("kind:chore with a bogus proof pair", code, &out, &err);
}

// ---------------------------------------------------------------------------
// 4. REFUSE with "cannot determine" — the scope question itself failed
//
// Each case starts from a `kind:"fix"` task with NO proof artifacts, which
// case 2 establishes must be refused while the decomposition is readable.
// Damaging the decomposition must NOT flip that refusal into an acceptance.
// ---------------------------------------------------------------------------

#[test]
fn unreadable_decomposition_bytes_refuse_and_surface_the_read_failure() {
    let dir = unique_dir("undet-nonutf8");
    let home = unique_dir("undet-nonutf8-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-undet-nonutf8", "t-fix");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("fix")),
    );
    let persisted = persisted_decomposition(&home, run);
    // Lone continuation bytes: a file that EXISTS and is readable at the
    // syscall level, but is not decodable as text.
    std::fs::write(&persisted, [0x80u8, 0x81, 0xfe, 0xff, 0x80])
        .expect("overwrite persisted decomposition with non-UTF-8 bytes");

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_cannot_determine("non-UTF-8 decomposition", code, &out, &err);
    let lower = err.to_ascii_lowercase();
    assert!(
        lower.contains("utf"),
        "non-UTF-8 decomposition: expected stderr to surface the underlying read failure reason \
         (a UTF-8 decoding error)\nstdout: {out}\nstderr: {err}"
    );
}

#[test]
fn syntactically_invalid_decomposition_json_refuses_with_cannot_determine() {
    let dir = unique_dir("undet-badjson");
    let home = unique_dir("undet-badjson-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-undet-badjson", "t-fix");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("fix")),
    );
    let persisted = persisted_decomposition(&home, run);
    std::fs::write(&persisted, "{ this is not json").expect("corrupt persisted decomposition");

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_cannot_determine("unparseable decomposition JSON", code, &out, &err);
}

#[test]
fn well_formed_json_of_the_wrong_shape_refuses_with_cannot_determine() {
    let dir = unique_dir("undet-wrongshape");
    let home = unique_dir("undet-wrongshape-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-undet-wrongshape", "t-fix");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("fix")),
    );
    let persisted = persisted_decomposition(&home, run);
    // Valid JSON, but not a decomposition: there is no `tasks` array to
    // search, so the scope question is unanswerable rather than answered "no".
    std::fs::write(&persisted, r#"["not","a","decomposition"]"#)
        .expect("overwrite persisted decomposition with the wrong JSON shape");

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_cannot_determine("wrong-shape decomposition JSON", code, &out, &err);
}

#[test]
fn decomposition_without_a_matching_entry_refuses_and_names_the_missing_entry() {
    let dir = unique_dir("undet-noentry");
    let home = unique_dir("undet-noentry-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-undet-noentry", "t-fix");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("fix")),
    );
    let persisted = persisted_decomposition(&home, run);
    // Parses fine; simply describes a different task. "This file has no
    // opinion about t-fix" is not "t-fix needs no oracle".
    std::fs::write(&persisted, decomposition_json("somebody-else", Some("fix")))
        .expect("overwrite persisted decomposition with a non-matching entry");

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_cannot_determine("decomposition with no matching entry", code, &out, &err);
    assert!(
        err.contains("no decomposition entry"),
        "decomposition with no matching entry: expected stderr to name the missing entry \
         (`no decomposition entry`)\nstdout: {out}\nstderr: {err}"
    );
}

#[test]
fn empty_tasks_list_is_treated_as_a_missing_entry_not_as_out_of_scope() {
    let dir = unique_dir("undet-empty");
    let home = unique_dir("undet-empty-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-undet-empty", "t-fix");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("fix")),
    );
    let persisted = persisted_decomposition(&home, run);
    // The empty set is the classic fail-open shape: "no entries matched" must
    // not read as "nothing to check, therefore clean".
    std::fs::write(&persisted, r#"{"goal":"g","tasks":[]}"#)
        .expect("overwrite persisted decomposition with an empty task list");

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_cannot_determine("empty decomposition task list", code, &out, &err);
}

// ---------------------------------------------------------------------------
// 5. ACCEPT — genuine ABSENCE (ENOENT) is a real observation, not a failure
// ---------------------------------------------------------------------------

#[test]
fn absent_decomposition_file_still_accepts_a_fix_task_with_no_proofs() {
    let dir = unique_dir("absent");
    let home = unique_dir("absent-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-absent", "t-fix");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("fix")),
    );
    let persisted = persisted_decomposition(&home, run);
    std::fs::remove_file(&persisted).expect("delete the persisted decomposition");
    assert!(!persisted.exists(), "the decomposition must really be gone");

    // Same task, same (absent) proofs as `fix_with_no_proof_artifacts_at_all_is_refused`.
    // The ONLY difference is that the decomposition is absent rather than
    // present-but-broken — and that difference is the deliberate carve-out.
    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_accepted(
        "kind:fix, no proofs, decomposition file deleted (ENOENT)",
        code,
        &out,
        &err,
    );
}

// ---------------------------------------------------------------------------
// 6. `feature` behaves like `fix`; the kind match is case-insensitive
// ---------------------------------------------------------------------------

#[test]
fn feature_with_no_proof_artifacts_is_refused_like_fix() {
    let dir = unique_dir("feature-noproof");
    let home = unique_dir("feature-noproof-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-feature-noproof", "t-feature");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("feature")),
    );

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_refused("kind:feature with no proof artifacts", code, &out, &err);
}

#[test]
fn feature_with_genuine_fail_to_pass_is_accepted() {
    let dir = unique_dir("feature-accept");
    let home = unique_dir("feature-accept-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-feature-accept", "t-feature");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("feature")),
    );
    write_proof(&dir, task, "red", false);
    write_proof(&dir, task, "green", true);

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_accepted("kind:feature with red=false/green=true", code, &out, &err);
}

#[test]
fn uppercase_fix_kind_still_requires_the_oracle() {
    let dir = unique_dir("upper-fix");
    let home = unique_dir("upper-fix-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-upper-fix", "t-fix");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("FIX")),
    );

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_refused("kind:FIX with no proof artifacts", code, &out, &err);
}

#[test]
fn mixed_case_feature_kind_still_requires_the_oracle() {
    let dir = unique_dir("mixed-feature");
    let home = unique_dir("mixed-feature-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-mixed-feature", "t-feature");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("Feature")),
    );

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_refused("kind:Feature with no proof artifacts", code, &out, &err);
}

#[test]
fn mixed_case_feature_kind_is_accepted_with_a_genuine_oracle() {
    let dir = unique_dir("mixed-feature-ok");
    let home = unique_dir("mixed-feature-ok-home");
    let tdd = tdd_bin_dir();
    let (run, task) = ("scope-mixed-feature-ok", "t-feature");

    init_run(
        &dir,
        &home,
        &tdd,
        run,
        &decomposition_json(task, Some("Feature")),
    );
    write_proof(&dir, task, "red", false);
    write_proof(&dir, task, "green", true);

    let (code, out, err) = verify(&dir, &home, &tdd, run, task);
    assert_accepted("kind:Feature with red=false/green=true", code, &out, &err);
}
