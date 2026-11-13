#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED for the fix "in a REPO store the store's repo is the identity of every
//! row" (user ruling 2026-11-14, verbatim: 「本質的というのは問題の質は通常パスでゆらぐものではない」).
//!
//! In a repo store (`<repo>/.backlog/tasks.toml`) a row's `project` label is
//! only where it was filed. A same-repo row filed on another machine carries a
//! label such as `/some/old/root` that does not exist here; it must be treated
//! as THIS repo: claimable by `next --claim`, its stored repro re-run in this
//! checkout, and listed by `list --project <this checkout>`. Today
//! `claim_recheck::rerun_dir` derives identity from the label, so such a row is
//! skipped forever.
//!
//! Fixture: isolated HOME + linked-worktree git repo (tests/common); the label
//! is planted by rewriting the stored `project` line. Measurement point: HEAD of
//! branch fix/backlog-repo-store-identity before the fix, 2026-11-14.
//!
//! Does NOT prove (whole file): behaviour for a label that EXISTS here as a
//! different repo or an undeterminable dir (re-anchored in the K1 and 32d9232c
//! tests), nor pinned multi-project stores, nor fix_landed.

mod common;

const OLD_ROOT: &str = "/some/old/root";

fn file(fx: &common::Fixture, title: &str, prio: &str, repro: &str) -> String {
    let (o, id) = fx.add_extra(title, &["--priority", prio, "--repro-test", repro]);
    id.unwrap_or_else(|| panic!("setup: `{title}` not filed: {}", o.both()))
}

fn plant_old_root(fx: &common::Fixture, id: &str) {
    assert!(
        !std::path::Path::new(OLD_ROOT).exists(),
        "premise: {OLD_ROOT} must not exist on this machine"
    );
    fx.plant_project_label(id, OLD_ROOT);
    assert_eq!(fx.row(id)["project"], OLD_ROOT, "premise: label planted");
}

// RED observed at c4702fad (user ruling 2026-11-14
// 「本質的というのは問題の質は通常パスでゆらぐものではない」); un-ignored by the
// fix (backlog d3f7a4af).
#[test]
/// Same-repo row with a nonexistent filer label (p0) ahead of a valid local row
/// (p2): `next --claim` claims the p0 row.
///
/// Does NOT prove: that the re-run happened in this checkout (see the marker
/// test) nor that the other row stays untouched beyond not being claimed.
fn a_claim_takes_same_repo_row_with_nonexistent_label() {
    let fx = common::Fixture::new("id-a");
    let x = file(&fx, "alpha old-root row", "p0", common::REPRO_YES);
    let y = file(&fx, "zulu local row", "p2", common::REPRO_YES);
    plant_old_root(&fx, &x);
    let o = fx.run(&["next", "--claim"]);
    assert_eq!(o.code, 0, "claim must succeed: {}", o.both());
    assert!(o.stdout.contains(&x), "stdout must name {x}: {}", o.both());
    assert_eq!(fx.status(&x), "claimed", "p0 old-root row must be claimed");
    assert_ne!(fx.status(&y), "claimed", "p2 row must not be claimed");
}

// RED observed at c4702fad; un-ignored by the fix (backlog d3f7a4af).
#[test]
/// The stored repro of an old-root row is executed with cwd = this checkout.
/// The committed repro script records `pwd -P` in `rerun_marker.txt`; the
/// marker is deleted after filing (which also runs the repro), so a marker
/// after the claim proves the claim-time re-run happened and where.
///
/// Does NOT prove: that no other process wrote the marker (the fixture runs
/// only this binary), nor that the re-run is in the repo toplevel for a
/// subdirectory cwd (the fixture cwd is the toplevel).
fn b_repro_rerun_happens_in_this_checkout() {
    let fx = common::Fixture::new("id-b");
    fx.write_script(
        "tests/repro_marker.sh",
        "pwd -P > rerun_marker.txt; echo 'bug present'; exit 1",
    );
    fx.commit_paths(&["tests/repro_marker.sh"], "marker repro");
    let x = file(
        &fx,
        "old-root marker row",
        "p0",
        "bash tests/repro_marker.sh",
    );
    plant_old_root(&fx, &x);
    let marker = fx.repo.join("rerun_marker.txt");
    std::fs::remove_file(&marker).expect("premise: filing ran the repro and left a marker");
    let o = fx.run(&["next", "--claim"]);
    assert_eq!(o.code, 0, "claim must succeed: {}", o.both());
    let got = std::fs::read_to_string(&marker)
        .unwrap_or_else(|e| panic!("repro was not re-run in this checkout ({e}): {}", o.both()));
    assert_eq!(
        std::path::PathBuf::from(got.trim()).canonicalize().unwrap(),
        fx.repo.canonicalize().unwrap(),
        "repro must run in this checkout"
    );
}

#[test]
/// `list --project <this checkout> --json` includes the old-root row. This
/// PASSES at HEAD (observed 2026-11-14: list already ignores the label in a
/// repo store), so it is a live guard, not a RED, and is not ignored.
///
/// Does NOT prove: the default (no `--project`) scope, nor `--all`.
fn c_list_project_includes_old_root_row() {
    let fx = common::Fixture::new("id-c");
    let x = file(&fx, "old-root listed row", "p0", common::REPRO_YES);
    plant_old_root(&fx, &x);
    let p = fx.project();
    let o = fx.run(&["list", "--project", &p, "--json"]);
    assert_eq!(o.code, 0, "list failed: {}", o.both());
    let rows: Vec<serde_json::Value> = serde_json::from_str(o.stdout.trim()).unwrap();
    assert!(
        rows.iter().any(|r| r["id"] == x.as_str()),
        "row {x} (label {OLD_ROOT}) missing from `list --project {p}`: {}",
        o.stdout
    );
}

// RED observed at c4702fad; un-ignored by the fix (backlog d3f7a4af).
#[test]
/// The ONLY candidate has the old-root label: `next --claim` claims it (exit 0)
/// instead of refusing.
///
/// Does NOT prove: anything about a sole candidate whose label exists as a
/// different repo (that must still be REFUSED; see the 32d9232c test).
fn d_sole_old_root_candidate_is_claimed_not_refused() {
    let fx = common::Fixture::new("id-d");
    let x = file(&fx, "sole old-root row", "p0", common::REPRO_YES);
    plant_old_root(&fx, &x);
    let o = fx.run(&["next", "--claim"]);
    assert_eq!(o.code, 0, "must claim, not refuse: {}", o.both());
    assert!(o.stdout.contains(&x), "stdout must name {x}: {}", o.both());
    assert_eq!(fx.status(&x), "claimed");
}
