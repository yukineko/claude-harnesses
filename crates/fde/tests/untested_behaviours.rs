// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Contract C4: documented behaviours of `fde gate` / `fde evidence` that no
//! earlier test pinned (items 1-8 of the C4 contract; the module docs of
//! `src/main.rs`, `src/gate.rs` and `src/evidence.rs` are the source).
//!
//! Written by an independent test author (CLAUDE.md §2(a)); the implementer
//! must match the contract, not edit these tests to pass.
//!
//! Every deny/refusal case has an allow/accept partner in the same test (or
//! names the existing test that is its partner), so a constant answer fails.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{json, Value};

use common::{
    changed_files, code, parse_resolved, pretooluse_payload, run_gate, show, snapshot_files,
    stderr, stdout, Engagement, TempPlace,
};

const REQS: &[(&str, &str)] = &[("R1", "the fixture does one thing")];

// ------------------------------------------------------------------ helpers

fn gate(e: &Engagement, payload: &str) -> Output {
    let out = run_gate(&e.spec_dir.path, &e.state.path, payload.as_bytes());
    if code(&out) == Some(0) {
        assert_eq!(
            decision(&out).as_deref(),
            Some("allow"),
            "exit 0 must carry permissionDecision \"allow\": {}",
            show(&out)
        );
    }
    out
}

fn decision(out: &Output) -> Option<String> {
    let v: Value = serde_json::from_str(stdout(out).trim()).ok()?;
    let h = v.get("hookSpecificOutput")?;
    if h.get("hookEventName").and_then(Value::as_str) != Some("PreToolUse") {
        return None;
    }
    h.get("permissionDecision")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn reason(out: &Output) -> String {
    let v: Value = serde_json::from_str(stdout(out).trim()).unwrap_or(Value::Null);
    v.get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn assert_allowed(out: &Output, what: &str) {
    assert_eq!(
        decision(out).as_deref(),
        Some("allow"),
        "{what}: must be allowed: {}",
        show(out)
    );
    assert_eq!(code(out), Some(0), "{what}: allow exits 0: {}", show(out));
}

fn assert_denied(out: &Output, what: &str) {
    assert_eq!(
        decision(out).as_deref(),
        Some("deny"),
        "{what}: must be denied: {}",
        show(out)
    );
    assert_eq!(code(out), Some(2), "{what}: deny exits 2: {}", show(out));
}

fn assert_code(out: &Output, want: i32, what: &str) {
    assert_eq!(code(out), Some(want), "{what}: {}", show(out));
}

fn s(p: &Path) -> String {
    p.to_str().expect("utf-8 path").to_string()
}

fn edit(cwd: &Path, file: &str) -> String {
    pretooluse_payload(
        cwd,
        "Edit",
        json!({"file_path": file, "old_string": "fixture", "new_string": "changed"}),
    )
}

fn bash(cwd: &Path, command: &str) -> String {
    pretooluse_payload(cwd, "Bash", json!({"command": command}))
}

fn unratified() -> Engagement {
    let e = Engagement::new(REQS);
    e.begin();
    assert_code(
        &e.fde(&["status"]),
        1,
        "fixture: begun target is unratified",
    );
    e
}

fn ev(e: &Engagement, kind: &str, req: &str, cmd: &str) -> Output {
    e.fde(&["evidence", kind, "--req", req, "--cmd", cmd])
}

fn verify(e: &Engagement) -> Output {
    e.fde(&["evidence", "verify"])
}

/// Run `f` and assert it changed no file under the state dir.
fn assert_records_nothing(e: &Engagement, what: &str, f: impl FnOnce() -> Output) -> Output {
    let before = snapshot_files(&e.state.path);
    let out = f();
    let after = snapshot_files(&e.state.path);
    let changed = changed_files(&e.state.path, &before, &after);
    assert!(
        changed.is_empty(),
        "{what}: nothing may be recorded, but {changed:?} changed: {}",
        show(&out)
    );
    out
}

// ------------------------------------------------- (1) unknown tool_name

/// C4-1: a `tool_name` outside `Bash|Edit|Write|MultiEdit|NotebookEdit` is
/// Undetermined (deny) while any target is registered — unratified or
/// ratified — and allowed when nothing is registered.
#[test]
fn unknown_tool_is_denied_while_a_target_is_registered() {
    let elsewhere = TempPlace::new("fde-c4-elsewhere-");
    let tools: Vec<(&str, Value)> = vec![
        ("Read", json!({"file_path": s(&elsewhere.path.join("x"))})),
        (
            "WebFetch",
            json!({"url": "https://example.invalid/", "prompt": "p"}),
        ),
        ("SomethingNew", json!({})),
    ];
    let un = unratified();
    let ra = Engagement::ratified(REQS);
    let none = Engagement::new(REQS);
    for (tool, input) in &tools {
        let p = pretooluse_payload(&elsewhere.path, tool, input.clone());
        assert_denied(&gate(&un, &p), &format!("{tool} with an unratified target"));
        let out = gate(&ra, &p);
        assert_denied(&out, &format!("{tool} with a ratified target"));
        assert!(
            reason(&out).contains(tool),
            "the deny reason names the tool {tool}: {}",
            show(&out)
        );
        assert_allowed(&gate(&none, &p), &format!("{tool} with nothing registered"));
    }
}

// ------------------------------------------------- (2) stdin size cap

/// `harness_core::hook::MAX_STDIN_BYTES` (crates/harness-core/src/hook.rs).
const MAX_STDIN_BYTES: usize = 10 * 1024 * 1024;

/// A syntactically valid Bash payload of exactly `len` bytes (padding in an
/// extra string field), cwd an unrelated dir.
fn padded_payload(cwd: &Path, len: usize) -> String {
    let base = pretooluse_payload(cwd, "Bash", json!({"command": "true", "pad": ""}));
    assert!(len > base.len());
    let pad = "x".repeat(len - base.len());
    let p = pretooluse_payload(cwd, "Bash", json!({"command": "true", "pad": pad}));
    assert_eq!(p.len(), len, "fixture: padded payload length");
    serde_json::from_str::<Value>(&p).expect("fixture: padded payload is valid JSON");
    p
}

/// C4-2: a payload larger than MAX_STDIN_BYTES is denied with a size reason
/// on stderr, even with nothing registered. Partner: the same payload shape
/// at exactly the cap is allowed.
#[test]
fn stdin_over_the_size_cap_is_denied_even_with_nothing_registered() {
    let none = Engagement::new(REQS);
    let elsewhere = TempPlace::new("fde-c4-elsewhere-");

    let at_cap = padded_payload(&elsewhere.path, MAX_STDIN_BYTES);
    assert_allowed(&gate(&none, &at_cap), "a payload of exactly the cap");

    let over = padded_payload(&elsewhere.path, MAX_STDIN_BYTES + 4096);
    let out = gate(&none, &over);
    assert_denied(&out, "a payload over the cap with nothing registered");
    let err = stderr(&out).to_lowercase();
    assert!(
        err.contains("exceeds") && err.contains(&MAX_STDIN_BYTES.to_string()),
        "stderr must say the payload exceeds the {MAX_STDIN_BYTES}-byte cap: {}",
        show(&out)
    );
}

// --------------------------------------- (3) `git worktree list` failure

/// C4-3: when `git worktree list` fails for a guarded target, a call outside
/// its root cannot be placed: Undetermined (deny), reason about the work
/// trees. The probe is made to fail by corrupting `.git/HEAD` (the root still
/// exists; deletion is item C3). Partner: the same Edit before corruption is
/// allowed.
#[test]
fn worktree_probe_failure_denies_calls_outside_the_root() {
    let e = unratified();
    let elsewhere = TempPlace::new("fde-c4-elsewhere-");
    let file = s(&elsewhere.path.join("unrelated.txt"));
    let p = edit(&elsewhere.path, &file);
    assert_allowed(&gate(&e, &p), "Edit outside the target, probe intact");

    fs::write(e.root().join(".git/HEAD"), "garbage\n").expect("corrupt .git/HEAD");
    let probe = std::process::Command::new("git")
        .current_dir(e.root())
        .args(["worktree", "list", "--porcelain", "-z"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("spawn git");
    assert!(
        !probe.status.success(),
        "fixture: `git worktree list` must fail on the corrupted repo"
    );

    let out = gate(&e, &p);
    assert_denied(
        &out,
        "Edit outside the target while its work trees cannot be listed",
    );
    let r = reason(&out);
    assert!(
        r.contains("work trees") && r.contains("undetermined"),
        "reason must say the work trees cannot be listed (undetermined): {}",
        show(&out)
    );
}

// --------------------------------------------- (4) CMD exit 126 / 127

/// C4-4: RED / GREEN / PASS whose CMD exits 126 or 127 (the shell could not
/// execute / find it) are Undetermined (exit 2) and record nothing.
#[test]
fn evidence_cmd_exit_126_or_127_is_undetermined_and_records_nothing() {
    let e = Engagement::ratified(REQS);
    for cmd in ["exit 127", "exit 126", "fde-c4-no-such-command-xyz"] {
        let out = assert_records_nothing(&e, cmd, || ev(&e, "red", "R1", cmd));
        assert_code(&out, 2, &format!("RED with {cmd:?}"));
        let out = assert_records_nothing(&e, cmd, || ev(&e, "pass", "R1", cmd));
        assert_code(&out, 2, &format!("PASS with {cmd:?}"));
    }
    assert_code(&verify(&e), 1, "nothing recorded: R1 unmeasured");

    // GREEN: a RED of the same CMD (exit 1), then the CMD exits 127.
    let cmd = "test -f flag && exit 127; exit 1";
    assert_code(&ev(&e, "red", "R1", cmd), 0, "RED (exit 1) is recorded");
    fs::write(e.root().join("flag"), "").unwrap();
    let out = assert_records_nothing(&e, "GREEN exit 127", || ev(&e, "green", "R1", cmd));
    assert_code(&out, 2, "GREEN whose CMD exits 127");
    let v = verify(&e);
    assert_code(&v, 1, "R1 is still unresolved after the undetermined GREEN");
    assert!(stderr(&v).contains("R1"), "verify names R1: {}", show(&v));

    // Partner: an ordinary exit 1 is a determined RED (exit 0 recorded) and
    // an ordinary exit 0 a determined PASS for another spec.
    let p = Engagement::ratified(REQS);
    assert_code(&ev(&p, "red", "R1", "exit 1"), 0, "RED with exit 1");
    let q = Engagement::ratified(REQS);
    assert_code(&ev(&q, "pass", "R1", "exit 0"), 0, "PASS with exit 0");
}

// --------------------------------------------- (5) evidence sequencing

/// C4-5: PASS after a RED of the same requirement is refused (exit 1,
/// nothing recorded); RED after a PASS is allowed and the requirement then
/// needs a GREEN; PASS -> RED -> GREEN is complete for verify and report.
#[test]
fn evidence_sequencing_pass_after_red_and_red_after_pass() {
    // PASS after RED: refused.
    let e = Engagement::ratified(REQS);
    assert_code(&ev(&e, "red", "R1", "false"), 0, "RED R1");
    let out = assert_records_nothing(&e, "PASS after RED", || ev(&e, "pass", "R1", "true"));
    assert_code(&out, 1, "PASS after a RED of the same requirement");
    assert!(!stderr(&out).trim().is_empty(), "refusal states a reason");
    assert_code(&verify(&e), 1, "R1 stays unresolved");

    // RED after PASS: allowed, then needs a GREEN.
    let e = Engagement::ratified(REQS);
    assert_code(&ev(&e, "pass", "R1", "true"), 0, "PASS R1");
    // user ruling 2026-11-04: PASS-only is not complete
    assert_code(&verify(&e), 1, "PASS-only is not complete for verify");
    assert_code(&ev(&e, "red", "R1", "test -f fixed"), 0, "RED after PASS");
    let v = verify(&e);
    assert_code(&v, 1, "after RED the requirement needs a GREEN");
    assert!(stderr(&v).contains("R1"), "verify names R1: {}", show(&v));
    assert_code(&e.fde(&["report"]), 1, "report: not done before GREEN");
    fs::write(e.root().join("fixed"), "").unwrap();
    assert_code(&ev(&e, "green", "R1", "test -f fixed"), 0, "GREEN R1");
    assert_code(&verify(&e), 0, "PASS -> RED -> GREEN is complete");
    assert_code(
        &e.fde(&["report"]),
        0,
        "PASS -> RED -> GREEN is done for report",
    );
}

// ------------------------------------ (6) allowance whitespace variants

/// C4-6: leading/trailing/repeated spaces and tabs around an exact
/// `fde status|report` keep the allowance; any other whitespace (newline,
/// CR, NBSP, ideographic space) does not.
#[test]
fn fde_allowance_whitespace_variants() {
    let e = unratified();
    let cwds = [e.root().to_path_buf(), e.repo.subdir("src")];
    for cwd in &cwds {
        for cmd in [
            "fde\tstatus",
            "  fde status  ",
            "fde  status",
            "\tfde report\t",
        ] {
            assert_allowed(
                &gate(&e, &bash(cwd, cmd)),
                &format!("{cmd:?} (cwd {})", cwd.display()),
            );
        }
        for cmd in [
            "fde status\n",
            "fde\nstatus",
            "fde\u{00a0}status",
            "fde\u{3000}status",
            "fde\rstatus",
        ] {
            assert_denied(
                &gate(&e, &bash(cwd, cmd)),
                &format!("{cmd:?} (cwd {})", cwd.display()),
            );
        }
    }
}

// ------------------------------- (7) allowance vs. Undetermined status

fn ratification_path(e: &Engagement) -> PathBuf {
    let (_root, key) = parse_resolved(&e.fde(&["resolve"]));
    e.state
        .path
        .join("targets")
        .join(key)
        .join("ratification.json")
}

/// C4-7: a target whose status is Undetermined (ratification.json corrupt)
/// gets no `fde status|report|begin` allowance, and an Edit inside is denied.
/// Partner: `hook_gate.rs::fde_status_in_unratified_target_is_allowed` (the
/// same commands allowed for an Unratified status).
#[test]
fn undetermined_status_target_gets_no_fde_allowance() {
    let e = Engagement::ratified(REQS);
    let rat = ratification_path(&e);
    assert!(rat.is_file(), "fixture: {} exists", rat.display());
    fs::write(&rat, b"\x00 garbage {{{").unwrap();
    assert_code(
        &e.fde(&["status"]),
        2,
        "corrupt ratification: status undetermined",
    );

    let spec2 = common::write_spec(&e.spec_dir.path, "spec-v2.toml", "v2", REQS);
    let begin = format!("fde begin --spec {}", s(&spec2));
    for cwd in [e.root().to_path_buf(), e.repo.subdir("src")] {
        for cmd in ["fde status", "fde report", begin.as_str()] {
            assert_denied(
                &gate(&e, &bash(&cwd, cmd)),
                &format!("{cmd:?} (cwd {}) on an Undetermined target", cwd.display()),
            );
        }
    }
    let out = gate(&e, &edit(e.root(), &s(&e.root().join("README.md"))));
    assert_denied(&out, "Edit inside an Undetermined target");

    // Partner on the same gate: an unrelated Bash elsewhere is allowed only
    // if the gate can still place it; the Undetermined status does not deny
    // calls outside the area.
    let elsewhere = TempPlace::new("fde-c4-elsewhere-");
    assert_allowed(
        &gate(&e, &bash(&elsewhere.path, "echo hi > notes.txt")),
        "unrelated Bash outside an Undetermined target",
    );
}

// --------------------------------------- (8) deny reason names the target

/// C4-8: the deny reason for an Edit and for a Bash inside an unratified
/// target names the canonical target root, on stdout and on stderr.
#[test]
fn deny_reason_names_the_canonical_target_root() {
    let e = unratified();
    let root = s(e.root());
    let sub = e.repo.subdir("src");
    let cases = [
        ("Edit", edit(&sub, &s(&sub.join("new.rs")))),
        ("Bash", bash(&sub, "touch x")),
    ];
    for (what, p) in &cases {
        let out = gate(&e, p);
        assert_denied(&out, what);
        assert!(
            reason(&out).contains(&root),
            "{what}: permissionDecisionReason must name the target root {root}: {}",
            show(&out)
        );
        assert!(
            stderr(&out).contains(&root),
            "{what}: stderr must name the target root {root}: {}",
            show(&out)
        );
    }
}
