#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Residual sites of the 2026-11-14 ruling 「本質的というのは問題の質は通常パスでゆらぐものではない」
//! (fix d3f7a4af made the store's repo the identity of every row in a REPO
//! store). A row filed on another machine carries a foreign absolute `project`
//! label; it must behave exactly like the identical local row.
//!
//! Site 1 (`hashkey(title, project)` via `store::check_duplicate`): re-filing
//! the same title locally while the first copy carries a foreign label must be
//! rejected as a duplicate (exit != 0, still one pending row).
//! Site 2 (`dedup::near_duplicates`, `t.project == anchor.project`): a
//! near-duplicate peer carrying a foreign label must still be reported in the
//! `near_duplicates` field of `next --claim`.
//!
//! Does NOT prove: pinned multi-project stores (label identity is correct
//! there), nor the legacy-store scan in divergence.rs.

mod common;

const OLD_ROOT: &str = "/some/old/root";

fn file(fx: &common::Fixture, title: &str, prio: &str) -> String {
    let (o, id) = fx.add_extra(
        title,
        &["--priority", prio, "--repro-test", common::REPRO_YES],
    );
    id.unwrap_or_else(|| panic!("setup: `{title}` not filed: {}", o.both()))
}

#[test]
fn duplicate_title_filed_locally_is_rejected_when_first_copy_has_foreign_label() {
    let fx = common::Fixture::new("lid-hk");
    let x = file(&fx, "same task refiled from other machine", "p1");
    assert!(!std::path::Path::new(OLD_ROOT).exists());
    fx.plant_project_label(&x, OLD_ROOT);
    let (o, id2) = fx.add_extra(
        "same task refiled from other machine",
        &["--priority", "p1", "--repro-test", common::REPRO_YES],
    );
    let pending = fx
        .list_all()
        .iter()
        .filter(|r| r["status"] == "pending")
        .count();
    assert!(
        o.code != 0 && id2.is_none() && pending == 1,
        "duplicate must be rejected (code {}, pending rows {pending}): {}",
        o.code,
        o.both()
    );
}

#[test]
fn near_duplicate_peer_with_foreign_label_is_reported_on_claim() {
    let fx = common::Fixture::new("lid-nd");
    let x = file(&fx, "alpha bravo charlie delta echo foxtrot golf", "p2");
    let y = file(&fx, "alpha bravo charlie delta echo foxtrot hotel", "p0");
    assert!(!std::path::Path::new(OLD_ROOT).exists());
    fx.plant_project_label(&x, OLD_ROOT);
    let o = fx.run(&["next", "--claim"]);
    assert_eq!(o.code, 0, "claim must succeed: {}", o.both());
    let v: serde_json::Value = serde_json::from_str(o.stdout.trim())
        .unwrap_or_else(|e| panic!("claim stdout not JSON ({e}): {}", o.both()));
    assert_eq!(v["id"], y.as_str(), "premise: {y} claimed: {}", o.both());
    let peers = v["near_duplicates"]
        .as_array()
        .expect("near_duplicates array");
    assert!(
        peers.iter().any(|p| p["id"] == x.as_str()),
        "peer {x} (label {OLD_ROOT}) missing from near_duplicates: {}",
        o.stdout
    );
}
