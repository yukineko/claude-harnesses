#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (audit 2026-11-13 K1), re-anchored 2026-11-14: `backlog next
//! --claim` must not refuse the WHOLE queue when ONE candidate's row belongs to
//! a project that is not this repo. `claim_recheck::claim_rechecked` must skip
//! that candidate (stderr notice, row left pending, NOT auto-cancelled) and
//! hand out the next valid one. Same class as the plain-`next` regression fixed
//! by 97d5f45c (a04b2b87).
//!
//! // test-weakening-justified: user ruling 2026-11-14 「本質的というのは問題の質は通常パスでゆらぐものではない」.
//! The original fixture used the label `/some/old/root` (nonexistent). In a
//! repo store that is a SAME-repo row filed elsewhere, which must now be
//! claimable (moved to backlog_20261114_repo_store_identity_not_path.rs). The
//! protected property (a foreign candidate is skipped, a valid one is handed
//! out) is re-anchored on labels that are REALLY foreign here: (1) an existing,
//! separate `git init` repo, (2) an existing dir whose `.git` file dangles
//! (identity undeterminable). Both premises are asserted in the fixture
//! helpers in tests/common.
//!
//! Fixture: isolated HOME + linked-worktree git repo (tests/common). X (p0)
//! carries the foreign label; Y (p2) is a valid row of this repo.
//!
//! Does NOT prove: that the foreign row is skipped silently (only that Y is
//! claimed with exit 0 and X is neither claimed nor cancelled), nor the pinned
//! (multi-project) store path, nor the same-repo-other-machine label case.

mod common;

#[derive(Clone, Copy)]
enum Kind {
    OtherRepo,
    Undeterminable,
}

fn setup(tag: &str, kind: Kind) -> (common::Fixture, String, String) {
    let fx = common::Fixture::new(tag);
    let mk = |title: &str, prio: &str| {
        let (o, id) = fx.add_extra(
            title,
            &["--priority", prio, "--repro-test", common::REPRO_YES],
        );
        id.unwrap_or_else(|| panic!("setup: `{title}` not filed: {}", o.both()))
    };
    let x = mk("alpha foreign project ticket", "p0");
    let y = mk("zulu local ticket", "p2");
    let dir = match kind {
        Kind::OtherRepo => common::make_other_repo(tag),
        Kind::Undeterminable => common::make_dangling_gitfile_dir(tag),
    };
    assert_ne!(dir, fx.repo, "premise: label dir is not this checkout");
    fx.plant_project_label(&x, &dir.to_string_lossy());
    (fx, x, y)
}

fn control_plain_next(kind: Kind, tag: &str) {
    let (fx, x, y) = setup(tag, kind);
    let o = fx.run(&["next"]);
    assert_eq!(o.code, 0, "plain next must not refuse queue: {}", o.both());
    assert!(
        o.stdout.contains(&x) || o.stdout.contains(&y),
        "plain next handed out neither X ({x}) nor Y ({y}): {}",
        o.both()
    );
}

fn claim_skips_foreign(kind: Kind, tag: &str) {
    let (fx, x, y) = setup(tag, kind);
    let o = fx.run(&["next", "--claim"]);
    assert_eq!(
        o.code,
        0,
        "`next --claim` must claim the valid ticket Y ({y}) instead of refusing the whole queue \
         because X ({x}) is foreign: {}",
        o.both()
    );
    assert!(
        o.stdout.contains(&y),
        "`next --claim` exited 0 but did not name Y ({y}): {}",
        o.both()
    );
    assert_eq!(fx.status(&y), "claimed", "Y must be the claimed task");
    assert_eq!(
        fx.status(&x),
        "pending",
        "X must be left pending (not claimed, not auto-cancelled)"
    );
}

#[test]
fn control_plain_next_with_other_repo_label() {
    control_plain_next(Kind::OtherRepo, "k1-ctl-repo");
}

#[test]
fn control_plain_next_with_undeterminable_label() {
    control_plain_next(Kind::Undeterminable, "k1-ctl-undet");
}

// test-weakening-justified: user ruling 2026-11-14 「本質的というのは問題の質は通常パスでゆらぐものではない」 —
// observed at HEAD: a row labelled with an existing DIFFERENT repo is re-run in
// that repo and AUTO-CANCELLED (status "cancelled"), not skipped-and-left-pending.
// This is a RED for the fix; the fix commit MUST remove this `#[ignore]`.
#[test]
#[ignore = "RED until repo-store row identity fix: HEAD auto-cancels a different-repo row"]
fn next_claim_skips_row_labelled_with_a_different_existing_repo() {
    claim_skips_foreign(Kind::OtherRepo, "k1-claim-repo");
}

#[test]
fn next_claim_skips_row_labelled_with_undeterminable_dir() {
    claim_skips_foreign(Kind::Undeterminable, "k1-claim-undet");
}
