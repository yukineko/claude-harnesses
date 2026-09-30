#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! IMPLEMENTER-WRITTEN (backlog 6becbf8d slice 1). These tests were written by
//! the agent that implemented the fix, NOT by an independent verifier — weigh
//! them accordingly. The independent RED tests live in `map_resolve_reason.rs`.
//!
//! Pins two things that file does not:
//!   * `reviewed_at` (HEAD commit + run date) and `reviewed_reason` are recorded
//!     on every entry `map resolve` / `map set-spec` marks tracked, and only on
//!     those entries;
//!   * a store written before those fields existed still loads (serde default)
//!     and its entries read as "no recorded review", not as reviewed.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A temp repo with one commit and a PRE-6becbf8d store: one `changed` entry
/// and one `tracked` entry, neither carrying any review field.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "seed\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "seed"]);
    fs::write(
        repo.join("specguard.toml"),
        "[project]\nname = \"Demo\"\nroot = \".\"\n\n[output]\nreport_dir = \"reports\"\nsentinel = \".pending\"\n\n[[area]]\nname = \"src\"\nglobs = [\"src/**\"]\ncanon = [\"docs/spec.md\"]\n",
    )
    .unwrap();
    let store = repo.join(".specguard/spec-map.toml");
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    fs::write(
        &store,
        "last_synced = \"deadbeef\"\n\
         [entries.\"src/a.rs\"]\nkey = \"src/a.rs\"\nkind = \"feature\"\nstatus = \"changed\"\nimpl_files = [\"src/a.rs\"]\n\
         [entries.\"src/old.rs\"]\nkey = \"src/old.rs\"\nkind = \"feature\"\nstatus = \"tracked\"\nspec_doc = \"docs/spec.md\"\nimpl_files = [\"src/old.rs\"]\n",
    )
    .unwrap();
    dir
}

fn sg(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(repo)
        .args(["--config", "specguard.toml", "--date", "2026-01-01", "map"])
        .args(args)
        .output()
        .unwrap()
}

fn stored(repo: &Path) -> toml::Value {
    toml::from_str(&fs::read_to_string(repo.join(".specguard/spec-map.toml")).unwrap()).unwrap()
}

fn assert_ok(out: &Output) {
    assert!(
        out.status.success(),
        "exit {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn assert_review(v: &toml::Value, key: &str, reason: &str, head: &str) {
    let e = &v["entries"][key];
    assert_eq!(e["status"].as_str(), Some("tracked"), "{key}: {e:?}");
    assert_eq!(e["reviewed_reason"].as_str(), Some(reason), "{key}: {e:?}");
    assert_eq!(
        e["reviewed_at"]["commit"].as_str(),
        Some(head),
        "{key}: reviewed_at.commit must be the repo HEAD: {e:?}"
    );
    assert_eq!(
        e["reviewed_at"]["date"].as_str(),
        Some("2026-01-01"),
        "{key}: reviewed_at.date must be the run date: {e:?}"
    );
}

#[test]
fn resolve_records_reviewed_at_commit_and_date() {
    let d = fixture();
    let head = git(d.path(), &["rev-parse", "HEAD"]);
    assert_ok(&sg(d.path(), &["resolve", "src/a.rs", "--reason", "why a"]));
    let v = stored(d.path());
    assert_review(&v, "src/a.rs", "why a", &head);
    // The unselected (old) entry gains no review it never had.
    assert!(v["entries"]["src/old.rs"].get("reviewed_at").is_none());
    assert!(v["entries"]["src/old.rs"].get("reviewed_reason").is_none());
}

#[test]
fn set_spec_records_reviewed_at_commit_and_date() {
    let d = fixture();
    let head = git(d.path(), &["rev-parse", "HEAD"]);
    assert_ok(&sg(
        d.path(),
        &[
            "set-spec",
            "src/a.rs",
            "docs/spec.md",
            "-m",
            "spec covers a",
        ],
    ));
    let v = stored(d.path());
    assert_review(&v, "src/a.rs", "spec covers a", &head);
    assert_eq!(
        v["entries"]["src/a.rs"]["spec_doc"].as_str(),
        Some("docs/spec.md")
    );
    assert!(v["entries"]["src/old.rs"].get("reviewed_at").is_none());
}

#[test]
fn old_store_without_review_fields_still_loads_as_unreviewed() {
    let d = fixture();
    let out = sg(d.path(), &["list", "--json"]);
    assert_ok(&out);
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let old = &j["entries"]["src/old.rs"];
    assert_eq!(old["status"].as_str(), Some("tracked"), "{j}");
    assert!(
        old["reviewed_reason"].is_null() && old["reviewed_at"].is_null(),
        "a pre-existing entry must read as having NO recorded review: {old}"
    );
    // And a load->save cycle through a write command keeps it loadable.
    assert_ok(&sg(d.path(), &["resolve", "src/a.rs", "-m", "r"]));
    assert_ok(&sg(d.path(), &["list"]));
}
