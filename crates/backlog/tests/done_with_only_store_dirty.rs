// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling (2026-10-24): `.backlog` store changes made by `backlog
//! add/done/fail` are left UNCOMMITTED and ride along with the next work or
//! merge commit. For that to be livable, the clean-tree requirement of
//! `done` (`evidence::require_clean_tree`) must ignore `.backlog/`, while an
//! uncommitted file OUTSIDE `.backlog` must still make it refuse.
//!
//! Written by an independent test author (CLAUDE.md 2(a)).
mod common;
use common::*;

fn porcelain(f: &Fixture) -> String {
    f.git(&["status", "--porcelain=v1", "--untracked-files=all"])
}

fn only_store_dirty(st: &str) -> bool {
    !st.trim().is_empty() && st.lines().all(|l| l[3..].starts_with(".backlog/"))
}

#[test]
fn done_succeeds_when_only_the_store_is_dirty() {
    let f = Fixture::new("sd-done");
    let (red, _head) = f.bug_then_fix();
    let id = f.add("t");
    // Precondition (observed, not assumed): the ONLY uncommitted change is
    // under .backlog/.
    let st = porcelain(&f);
    assert!(
        only_store_dirty(&st),
        "precondition: only .backlog is dirty, got {st:?}"
    );
    let o = f.run(&[
        "done",
        &id,
        "--test",
        "bash tests/check.sh",
        "--red-rev",
        &red,
    ]);
    assert_eq!(o.code, 0, "done must ignore the dirty store: {}", o.both());
    assert_eq!(f.status(&id), "done");
}

#[test]
fn fail_succeeds_when_only_the_store_is_dirty() {
    let f = Fixture::new("sd-fail");
    let id = f.add("t");
    let st = porcelain(&f);
    assert!(
        only_store_dirty(&st),
        "precondition: only .backlog is dirty, got {st:?}"
    );
    let o = f.run(&["fail", &id, "--reason", "x"]);
    assert_eq!(o.code, 0, "fail must ignore the dirty store: {}", o.both());
    assert_eq!(f.status(&id), "deferred");
}

#[test]
fn done_refuses_when_a_non_store_file_is_dirty() {
    let f = Fixture::new("sd-ctl");
    let (red, _head) = f.bug_then_fix();
    let id = f.add("t");
    f.write("stray.txt", "uncommitted\n");
    let o = f.run(&[
        "done",
        &id,
        "--test",
        "bash tests/check.sh",
        "--red-rev",
        &red,
    ]);
    assert_ne!(
        o.code,
        0,
        "an uncommitted non-.backlog file must refuse: {}",
        o.both()
    );
    assert!(
        o.both().contains("uncommitted changes outside .backlog"),
        "refusal must name the clean-tree reason: {}",
        o.both()
    );
    assert_eq!(f.status(&id), "pending");
}
