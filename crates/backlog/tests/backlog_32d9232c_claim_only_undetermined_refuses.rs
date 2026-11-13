#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression guard (backlog 1657, audit 2026-11-13), re-anchored 2026-11-14:
//! `backlog next --claim` whose ONLY candidate is foreign (label is an existing
//! DIFFERENT git repo) or undeterminable (label is an existing dir whose `.git`
//! file dangles) must REFUSE (non-zero exit, "REFUSED", stdout not "no pending
//! tasks", row still pending, not claimed, not cancelled). An unverifiable
//! queue must not render as empty.
//!
//! // test-weakening-justified: user ruling 2026-11-14 「本質的というのは問題の質は通常パスでゆらぐものではない」.
//! The original fixture used the label `/some/old/root` (nonexistent). In a
//! repo store that is a SAME-repo row filed elsewhere, which must now be
//! claimable (moved to the RED in
//! backlog_20261114_repo_store_identity_not_path.rs). The protected property
//! (sole foreign/undeterminable candidate -> REFUSED, pending) is re-anchored
//! on fixtures that are REALLY a different repo / REALLY undeterminable; the
//! premises are asserted in tests/common helpers.
//!
//! Fixture: isolated HOME + linked-worktree git repo (tests/common). Single
//! row X (p0) carrying the foreign label.
//!
//! Does NOT prove: what happens with mixed foreign and valid candidates (see
//! K1 test), nor undeterminable identity for causes other than a dangling
//! gitfile, nor the same-repo-other-machine label case.

mod common;

#[derive(Clone, Copy)]
enum Kind {
    OtherRepo,
    Undeterminable,
}

fn setup(tag: &str, kind: Kind) -> (common::Fixture, String) {
    let fx = common::Fixture::new(tag);
    let (o, id) = fx.add_extra(
        "sole ticket with foreign project",
        &["--priority", "p0", "--repro-test", common::REPRO_YES],
    );
    let id = id.unwrap_or_else(|| panic!("setup: ticket not filed: {}", o.both()));
    let dir = match kind {
        Kind::OtherRepo => common::make_other_repo(tag),
        Kind::Undeterminable => common::make_dangling_gitfile_dir(tag),
    };
    assert_ne!(dir, fx.repo, "premise: label dir is not this checkout");
    fx.plant_project_label(&id, &dir.to_string_lossy());
    (fx, id)
}

fn sole_candidate_refuses(kind: Kind, tag: &str) {
    let (fx, id) = setup(tag, kind);
    let o = fx.run(&["next", "--claim"]);
    assert_ne!(
        o.code,
        0,
        "`next --claim` with only a foreign candidate must refuse (non-zero): {}",
        o.both()
    );
    assert!(
        !o.stdout.contains(&id),
        "refused but stdout contains the id {id}: {}",
        o.both()
    );
    assert!(
        !o.stdout.contains("no pending tasks"),
        "stdout must not say 'no pending tasks' (the row IS pending): {}",
        o.both()
    );
    assert!(
        o.both().to_lowercase().contains("refused"),
        "combined output must contain 'REFUSED': {}",
        o.both()
    );
    assert_eq!(
        fx.status(&id),
        "pending",
        "a refused claim must leave the row pending (not claimed, not cancelled)"
    );
}

// RED observed at c4702fad (user ruling 2026-11-14
// 「本質的というのは問題の質は通常パスでゆらぐものではない」): a sole
// different-repo row was re-run in that repo and AUTO-CANCELLED, so
// `next --claim` printed "no pending tasks" with exit 0 instead of REFUSED.
// Un-ignored by the fix (backlog d3f7a4af).
#[test]
fn next_claim_with_only_other_repo_candidate_refuses() {
    sole_candidate_refuses(Kind::OtherRepo, "32d9-repo");
}

#[test]
fn next_claim_with_only_undeterminable_candidate_refuses() {
    sole_candidate_refuses(Kind::Undeterminable, "32d9-undet");
}
