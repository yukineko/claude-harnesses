//! User ruling 2026-11-14: every new blastguard backlog item must carry a
//! classification tag (`fail-open` or `false-positive`) so the weekly metric
//! `scripts/blastguard-discovery-rate.py` stops showing ~40% unclassified.
//!
//! A task is "blastguard" if its tags contain `blastguard` OR its title starts
//! with `blastguard` (case-insensitive) — the metric script's selection.
//!
//! Proves (driving the real binary in a hermetic fixture):
//!   * `backlog add` of a blastguard task with neither tag is refused:
//!     non-zero exit, stderr names both accepted tags, store bytes unchanged,
//!     and the repro command is NOT executed (marker file never created).
//!   * `backlog edit --tag ...` / `--title ...` that would leave a blastguard
//!     task without either tag is refused with the store unchanged.
//!   * Controls (pass today, must keep passing): tagged add succeeds (either
//!     tag or both), non-blastguard add needs no tag, notes-only edit of a
//!     legacy untagged blastguard row succeeds, edit that supplies a tag works.
//!
//! Does NOT prove: that no GitHub issue is attempted (the fixture has no gh;
//! only the store-unchanged + no-repro-run observations are asserted), nor the
//! exact stderr wording beyond naming both tags, nor behaviour of `sync`/other
//! entry points that might create rows.
mod common;
use common::*;

const TAGS: [&str; 2] = ["fail-open", "false-positive"];

/// Repro script that leaves a marker when executed; exit 1 = reproduced.
fn fixture(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    f.write_script("tests/repro_marker.sh", "touch repro_ran.marker; exit 1");
    f.commit_paths(&["tests/repro_marker.sh"], "marker repro");
    f
}

fn add(f: &Fixture, title: &str, tags: &[&str]) -> (Out, Option<String>) {
    let mut extra: Vec<&str> = vec!["--repro-test", "bash tests/repro_marker.sh"];
    for t in tags {
        extra.push("--tag");
        extra.push(t);
    }
    f.add_extra(title, &extra)
}

fn assert_refused_naming_tags(f: &Fixture, before: &[u8], o: &Out, why: &str) {
    assert_ne!(o.code, 0, "{why}: must be refused: {}", o.both());
    for t in TAGS {
        assert!(o.stderr.contains(t), "{why}: stderr must name `{t}`: {}", o.both());
    }
    assert_eq!(f.store_bytes(), before, "{why}: store must be unchanged");
}

fn tags_of(f: &Fixture, id: &str) -> Vec<String> {
    f.row(id)["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

// ---------- add: refusals (RED today) ----------

#[test]
fn add_blastguard_tag_without_classification_is_refused() {
    let f = fixture("bgc-add-tag");
    let before = f.store_bytes();
    let (o, _) = add(&f, "some gate problem", &["blastguard"]);
    assert_refused_naming_tags(&f, &before, &o, "tag blastguard, no classification");
    assert!(
        !f.repo.join("repro_ran.marker").exists(),
        "repro must not run for a refused add"
    );
}

#[test]
fn add_blastguard_title_without_classification_is_refused() {
    let f = fixture("bgc-add-title");
    let before = f.store_bytes();
    let (o, _) = add(&f, "blastguard: rm -rf wrapper slips through", &[]);
    assert_refused_naming_tags(&f, &before, &o, "blastguard title, no tags");
    assert!(
        !f.repo.join("repro_ran.marker").exists(),
        "repro must not run for a refused add"
    );
}

#[test]
fn add_blastguard_title_is_case_insensitive() {
    let f = fixture("bgc-add-case");
    let before = f.store_bytes();
    let (o, _) = add(&f, "BlastGuard allows X", &["p1"]);
    assert_refused_naming_tags(&f, &before, &o, "mixed-case blastguard title, unrelated tag");
}

#[test]
fn add_blastguard_with_unrelated_tags_only_is_refused() {
    let f = fixture("bgc-add-other");
    let before = f.store_bytes();
    let (o, _) = add(&f, "gate gap", &["blastguard", "p0", "security"]);
    assert_refused_naming_tags(&f, &before, &o, "blastguard + unrelated tags");
}

// ---------- add: controls (pass today) ----------

#[test]
fn control_add_blastguard_with_fail_open_succeeds() {
    let f = fixture("bgc-ok-fo");
    let (o, id) = add(&f, "blastguard: x", &["fail-open"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(tags_of(&f, &id.unwrap()).contains(&"fail-open".to_string()));
}

#[test]
fn control_add_blastguard_with_false_positive_succeeds() {
    let f = fixture("bgc-ok-fp");
    let (o, id) = add(&f, "x", &["blastguard", "false-positive"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(id.is_some());
}

#[test]
fn control_add_blastguard_with_both_tags_succeeds() {
    let f = fixture("bgc-ok-both");
    let (o, id) = add(&f, "blastguard: y", &["fail-open", "false-positive"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(id.is_some());
}

#[test]
fn control_add_non_blastguard_needs_no_tag() {
    let f = fixture("bgc-ok-nonbg");
    let (o, id) = add(&f, "unrelated task about statusline", &[]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(id.is_some());
}

// ---------- edit ----------

/// A legacy open blastguard row with no tags, planted straight into the store.
fn legacy_untagged(f: &Fixture, id: &str, title: &str) {
    let (_other, label) = f.add_with_label("seed row for project label");
    f.plant_open(id, title, "pending", &label);
}

#[test]
fn edit_replacing_tags_so_blastguard_row_loses_classification_is_refused() {
    let f = fixture("bgc-edit-drop");
    let (_, id) = add(&f, "blastguard: z", &["fail-open"]);
    let id = id.unwrap();
    let before = f.store_bytes();
    let o = f.run(&["edit", &id, "--tag", "p1"]);
    assert_refused_naming_tags(&f, &before, &o, "edit drops classification");
}

#[test]
fn edit_adding_blastguard_tag_without_classification_is_refused() {
    let f = fixture("bgc-edit-addbg");
    let (_, id) = add(&f, "plain task", &[]);
    let id = id.unwrap();
    let before = f.store_bytes();
    let o = f.run(&["edit", &id, "--tag", "blastguard"]);
    assert_refused_naming_tags(&f, &before, &o, "edit makes row blastguard by tag");
}

#[test]
fn edit_title_to_blastguard_prefix_on_untagged_row_is_refused() {
    let f = fixture("bgc-edit-title");
    let (_, id) = add(&f, "plain task", &[]);
    let id = id.unwrap();
    let before = f.store_bytes();
    let o = f.run(&["edit", &id, "--title", "blastguard: now it is one"]);
    assert_refused_naming_tags(&f, &before, &o, "edit makes row blastguard by title");
}

#[test]
fn edit_legacy_untagged_row_with_unrelated_tag_is_refused() {
    let f = fixture("bgc-edit-legacy-tag");
    legacy_untagged(&f, "legacy01", "blastguard: old row");
    let before = f.store_bytes();
    let o = f.run(&["edit", "legacy01", "--tag", "p2"]);
    assert_refused_naming_tags(&f, &before, &o, "resulting tags lack classification");
}

// ---------- edit: controls (pass today) ----------

#[test]
fn control_notes_only_edit_of_legacy_untagged_blastguard_row_succeeds() {
    let f = fixture("bgc-edit-legacy-notes");
    legacy_untagged(&f, "legacy02", "blastguard: old row");
    let o = f.run(&["edit", "legacy02", "--notes", "triaged"]);
    assert_eq!(o.code, 0, "legacy rows stay editable: {}", o.both());
    assert_eq!(f.row("legacy02")["notes"], "triaged");
}

#[test]
fn control_edit_supplying_classification_on_legacy_row_succeeds() {
    let f = fixture("bgc-edit-legacy-fix");
    legacy_untagged(&f, "legacy03", "blastguard: old row");
    let o = f.run(&["edit", "legacy03", "--tag", "false-positive"]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert!(tags_of(&f, "legacy03").contains(&"false-positive".to_string()));
}

#[test]
fn control_edit_non_blastguard_row_tags_unaffected() {
    let f = fixture("bgc-edit-nonbg");
    let (_, id) = add(&f, "plain task", &[]);
    let o = f.run(&["edit", &id.unwrap(), "--tag", "p2"]);
    assert_eq!(o.code, 0, "{}", o.both());
}
