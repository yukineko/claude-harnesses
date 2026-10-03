//! `needs-ruling`: non-terminal status for judgment / untestable closes. Only a
//! human `ruling approve` (TTY stdin + typing the id back) closes it.
//! On-disk shape (fixed by the spec): flat on the task row: `ruling_kind`,
//! `rationale`, `untestable_reason`; on approval `[task.closure.ruling]` with
//! `kind, rationale, approved_by, approved_at, approved_via`.
mod common;
use common::*;

fn request(f: &Fixture, id: &str) -> Out {
    f.run(&[
        "ruling",
        "request",
        id,
        "--kind",
        "judgment",
        "--rationale",
        "value call: not worth doing",
    ])
}

#[test]
fn judgment_request_sets_needs_ruling_and_records_flat_fields() {
    let f = Fixture::new("jr");
    let id = f.add("t");
    let o = request(&f, &id);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&id), "needs-ruling");
    let r = f.row(&id);
    assert_eq!(r["ruling_kind"], "judgment", "{r}");
    assert_eq!(r["rationale"], "value call: not worth doing", "{r}");
}

#[test]
fn untestable_request_requires_its_reason_and_records_it() {
    let f = Fixture::new("ur");
    let id = f.add("t");
    let o = f.run(&["ruling", "request", &id, "--kind", "untestable"]);
    assert_refused_unchanged(
        &f,
        &id,
        "pending",
        &o,
        "untestable without --untestable-reason",
    );
    let o = f.run(&[
        "ruling",
        "request",
        &id,
        "--kind",
        "untestable",
        "--untestable-reason",
        "needs real hardware",
    ]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&id), "needs-ruling");
    let r = f.row(&id);
    assert_eq!(r["ruling_kind"], "untestable", "{r}");
    assert_eq!(r["untestable_reason"], "needs real hardware", "{r}");
}

#[test]
fn judgment_request_requires_rationale_and_known_kind() {
    let f = Fixture::new("jbad");
    let id = f.add("t");
    let o = f.run(&["ruling", "request", &id, "--kind", "judgment"]);
    assert_refused_unchanged(&f, &id, "pending", &o, "judgment without --rationale");
    let o = f.run(&[
        "ruling",
        "request",
        &id,
        "--kind",
        "whim",
        "--rationale",
        "x",
    ]);
    assert_refused_unchanged(&f, &id, "pending", &o, "unknown kind");
    let o = f.run(&[
        "ruling",
        "request",
        "00000000",
        "--kind",
        "judgment",
        "--rationale",
        "x",
    ]);
    assert_ne!(o.code, 0, "unknown task id: {}", o.both());
}

#[test]
fn needs_ruling_is_excluded_from_next_and_claim_but_shown_in_list() {
    let f = Fixture::new("excl");
    let ruled = f.add("awaiting human");
    let other = f.add("workable");
    assert_eq!(request(&f, &ruled).code, 0);
    let p = f.project();
    for args in [
        vec!["next", "--project", &p],
        vec!["next", "--claim", "--project", &p],
    ] {
        let o = f.run(&args);
        assert!(
            !o.stdout.contains(&ruled),
            "{args:?} must not return a needs-ruling row: {}",
            o.stdout
        );
        assert!(
            o.stdout.contains(&other),
            "{args:?} positive control (workable row): {}",
            o.both()
        );
    }
    let l = f.run(&["list", "--all"]);
    assert!(
        l.stdout.contains(&ruled) && l.stdout.contains("needs-ruling"),
        "list shows it: {}",
        l.stdout
    );
}

#[test]
fn ruling_list_shows_pending_rulings() {
    let f = Fixture::new("rlist");
    let id = f.add("t");
    let other = f.add("not ruled");
    assert_eq!(request(&f, &id).code, 0);
    let o = f.run(&["ruling", "list"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(
        o.stdout.contains(&id) && !o.stdout.contains(&other),
        "{}",
        o.stdout
    );
}

#[test]
fn done_on_needs_ruling_is_refused_even_with_valid_evidence() {
    let f = Fixture::new("donenr");
    let (red, _) = f.bug_then_fix();
    let id = f.add("t");
    assert_eq!(request(&f, &id).code, 0);
    let o = f.run(&["done", &id]);
    assert_refused_unchanged(&f, &id, "needs-ruling", &o, "bare done on needs-ruling");
    let o = f.run(&[
        "done",
        &id,
        "--test",
        "bash tests/check.sh",
        "--red-rev",
        &red,
    ]);
    assert_refused_unchanged(
        &f,
        &id,
        "needs-ruling",
        &o,
        "only `ruling approve` closes a needs-ruling row",
    );
    let o = f.run(&["edit", &id, "--status", "done"]);
    assert_refused_unchanged(&f, &id, "needs-ruling", &o, "edit bypass on needs-ruling");
    let o = f.run(&["edit", &id, "--status", "cancelled"]);
    assert_refused_unchanged(
        &f,
        &id,
        "needs-ruling",
        &o,
        "cancelled needs the ruling approval too",
    );
}

#[test]
fn approve_refuses_without_a_tty_even_if_the_id_is_piped() {
    let f = Fixture::new("notty");
    let id = f.add("t");
    assert_eq!(request(&f, &id).code, 0);
    let before = f.store_bytes();
    let o = f.run_stdin(
        &["ruling", "approve", &id],
        &[],
        format!("{id}\n").as_bytes(),
    );
    assert_ne!(o.code, 0, "non-TTY stdin must refuse: {}", o.both());
    assert_eq!(f.status(&id), "needs-ruling");
    assert_eq!(
        f.store_bytes(),
        before,
        "a refused approve must not touch the store"
    );
    let s = f.row(&id).to_string();
    assert!(
        !s.contains("approved_by"),
        "no approval record may exist: {s}"
    );
}

#[test]
fn approve_on_a_non_needs_ruling_row_is_refused() {
    let f = Fixture::new("apppend");
    let id = f.add("t");
    let o = f.run_stdin(
        &["ruling", "approve", &id],
        &[],
        format!("{id}\n").as_bytes(),
    );
    assert_refused_unchanged(&f, &id, "pending", &o, "nothing to approve");
}

#[test]
fn withdraw_returns_to_pending_and_evidence_gated_done_applies() {
    let f = Fixture::new("wd");
    let (red, _) = f.bug_then_fix();
    let id = f.add("t");
    assert_eq!(request(&f, &id).code, 0);
    let o = f.run(&["ruling", "withdraw", &id]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&id), "pending");
    let o = f.run(&["done", &id]);
    assert_refused_unchanged(
        &f,
        &id,
        "pending",
        &o,
        "bare done is still refused after withdraw",
    );
    let o = f.run(&[
        "done",
        &id,
        "--test",
        "bash tests/check.sh",
        "--red-rev",
        &red,
    ]);
    assert_eq!(o.code, 0, "{}", o.both());
}

#[test]
fn withdraw_on_a_non_needs_ruling_row_is_refused() {
    let f = Fixture::new("wdbad");
    let id = f.add("t");
    let o = f.run(&["ruling", "withdraw", &id]);
    assert_refused_unchanged(&f, &id, "pending", &o, "withdraw of nothing");
}

#[test]
fn request_on_a_terminal_row_is_refused() {
    let f = Fixture::new("reqterm");
    f.plant_legacy_done("a1b2c3d4", "legacy", "done");
    let o = f.run(&[
        "ruling",
        "request",
        "a1b2c3d4",
        "--kind",
        "judgment",
        "--rationale",
        "x",
    ]);
    assert_ne!(o.code, 0, "{}", o.both());
    assert_eq!(f.status("a1b2c3d4"), "done");
}

/// Approve WITH a pty. macOS `script -q /dev/null CMD` gives the child a TTY
/// stdin; our stdin is forwarded to it. Returns None only if `script` cannot
/// be spawned (the caller then FAILS, it does not skip).
fn approve_under_pty(f: &Fixture, id: &str, typed: &str) -> Option<Out> {
    let bin = env!("CARGO_BIN_EXE_backlog");
    let mut child = std::process::Command::new("script")
        .args(["-q", "/dev/null", bin, "ruling", "approve", id])
        .env("HOME", &f.home)
        .current_dir(&f.repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    {
        use std::io::Write;
        let mut s = child.stdin.take().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(700));
        let _ = s.write_all(format!("{typed}\n").as_bytes());
        std::thread::sleep(std::time::Duration::from_millis(700));
    }
    let out = child.wait_with_output().ok()?;
    Some(Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

#[test]
fn approve_under_a_tty_with_the_id_typed_back_closes_and_records_provenance() {
    let f = Fixture::new("tty");
    let id = f.add("t");
    assert_eq!(request(&f, &id).code, 0);
    let o = approve_under_pty(&f, &id, &id).expect("`script` (pty) must be available");
    assert_eq!(o.code, 0, "TTY approve must succeed: {}", o.both());
    assert_eq!(f.status(&id), "done");
    let rl = f.row(&id)["closure"]["ruling"].clone();
    assert_eq!(rl["kind"], "judgment", "{rl}");
    assert_eq!(rl["rationale"], "value call: not worth doing", "{rl}");
    assert!(
        rl["approved_by"].as_str().is_some_and(|s| !s.is_empty()),
        "{rl}"
    );
    assert!(
        rl["approved_at"].is_i64() || rl["approved_at"].is_string(),
        "{rl}"
    );
    assert_eq!(rl["approved_via"], "tty", "{rl}");
}

#[test]
fn approve_under_a_tty_with_the_wrong_id_typed_is_refused() {
    let f = Fixture::new("ttywrong");
    let id = f.add("t");
    assert_eq!(request(&f, &id).code, 0);
    let o = approve_under_pty(&f, &id, "not-the-id").expect("pty available");
    assert_ne!(o.code, 0, "{}", o.both());
    assert_eq!(f.status(&id), "needs-ruling");
}
