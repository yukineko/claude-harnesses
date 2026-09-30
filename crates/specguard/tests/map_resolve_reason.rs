#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED tests for backlog 6becbf8d: `map resolve` / `map set-spec` flip an entry
//! to `tracked` with no verification and no recorded reason. The pinned
//! behaviour (from the item's done-criteria): both commands REQUIRE a reason
//! (same discipline as `accept-prompt -m/--reason`), a missing/blank reason is
//! rejected with a non-zero exit and leaves the store untouched, and a supplied
//! reason is recorded in the store. Everything runs on a throwaway temp repo.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let git = |args: &[&str]| {
        let s = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(s.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t.t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "seed\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "seed"]);
    fs::write(
        repo.join("specguard.toml"),
        "[project]\nname = \"Demo\"\nroot = \".\"\n\n[output]\nreport_dir = \"reports\"\nsentinel = \".pending\"\n\n[[area]]\nname = \"src\"\nglobs = [\"src/**\"]\ncanon = [\"docs/spec.md\"]\n",
    )
    .unwrap();
    let store = repo.join(".specguard/spec-map.toml");
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    let mut body = String::from("last_synced = \"deadbeef\"\n");
    for k in ["src/a.rs", "src/b.rs"] {
        body.push_str(&format!(
            "[entries.\"{k}\"]\nkey = \"{k}\"\nkind = \"feature\"\nstatus = \"changed\"\nimpl_files = [\"{k}\"]\n"
        ));
    }
    fs::write(&store, body).unwrap();
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

fn store(repo: &Path) -> String {
    fs::read_to_string(repo.join(".specguard/spec-map.toml")).unwrap()
}

fn assert_rejected_and_untouched(repo: &Path, before: &str, out: &Output) {
    assert!(
        !out.status.success(),
        "a resolve without a (non-blank) reason must be rejected; got exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        store(repo),
        before,
        "a rejected command must not write the store"
    );
}

/// The rejection must be about the reason, not a parse error for a flag that
/// does not exist (which would make the blank-reason cases vacuously green).
fn assert_reason_flag_is_known(out: &Output) {
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("unexpected argument"),
        "--reason must be a recognised flag; rejecting it as unknown proves nothing:\n{err}"
    );
}

#[test]
fn resolve_without_reason_is_rejected_and_store_untouched() {
    let d = fixture();
    let before = store(d.path());
    let out = sg(d.path(), &["resolve", "src/a.rs"]);
    assert_rejected_and_untouched(d.path(), &before, &out);
}

#[test]
fn resolve_with_blank_reason_is_rejected_and_store_untouched() {
    let d = fixture();
    let before = store(d.path());
    let out = sg(d.path(), &["resolve", "src/a.rs", "--reason", "   "]);
    assert_reason_flag_is_known(&out);
    assert_rejected_and_untouched(d.path(), &before, &out);
}

#[test]
fn set_spec_without_reason_is_rejected_and_store_untouched() {
    let d = fixture();
    let before = store(d.path());
    let out = sg(d.path(), &["set-spec", "src/a.rs", "docs/spec.md"]);
    assert_rejected_and_untouched(d.path(), &before, &out);
}

#[test]
fn set_spec_with_blank_reason_is_rejected_and_store_untouched() {
    let d = fixture();
    let before = store(d.path());
    let out = sg(
        d.path(),
        &["set-spec", "src/a.rs", "docs/spec.md", "--reason", ""],
    );
    assert_reason_flag_is_known(&out);
    assert_rejected_and_untouched(d.path(), &before, &out);
}

/// CONTROL: a resolve that supplies a reason still succeeds, tracks only the
/// selected entry, and the reason is recorded in the store. (At HEAD this
/// fails only because `--reason` does not exist yet; it guards against a fix
/// that rejects every resolve.)
#[test]
fn control_resolve_with_reason_succeeds_and_records_reason() {
    let d = fixture();
    let reason = "reviewed 2026-10-01: src/a.rs unchanged semantically";
    let out = sg(d.path(), &["resolve", "src/a.rs", "--reason", reason]);
    assert!(
        out.status.success(),
        "resolve with a reason must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = store(d.path());
    let parsed: toml::Value = toml::from_str(&s).unwrap();
    assert_eq!(
        parsed["entries"]["src/a.rs"]["status"].as_str(),
        Some("tracked")
    );
    assert_eq!(
        parsed["entries"]["src/b.rs"]["status"].as_str(),
        Some("changed"),
        "unselected entry must stay changed"
    );
    assert!(
        s.contains(reason),
        "the reason must be recorded in the store:\n{s}"
    );
}

/// CONTROL (green at HEAD and after): the fixture itself is a readable map.
#[test]
fn control_fixture_map_lists() {
    let d = fixture();
    let out = sg(d.path(), &["list"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("src/a.rs"));
}
