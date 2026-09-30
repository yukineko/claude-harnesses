// Integration tests: unwrap/expect are fine here (workspace lints are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED tests for backlog 0c277117: `specguard map gate-check --base <rev>`.
//!
//! Contract (coordinator ruling): exit 0 = every map entry of a GATE crate whose
//! impl files changed in `<rev>..HEAD` has a non-null spec_doc or a reasoned ack
//! (`.specguard/spec-doc-acks.toml`, `[[ack]] path/reason`); exit 1 = at least one
//! such entry has neither (each printed); exit 2 = undetermined (map/ack
//! unparseable, rev unresolvable, git failure, map absent) and never 0.
//! Also pins that the real repo config puts docs/specs in the gate areas' canon.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn git(repo: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn write(repo: &Path, rel: &str, body: &str) {
    let p = repo.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// Repo with a base commit holding gate-crate files and a non-gate file;
/// returns the base rev. Nothing is changed yet.
fn fixture(repo: &Path) -> String {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    write(
        repo,
        "specguard.toml",
        "[project]\nname = \"Demo\"\nroot = \".\"\n[output]\nreport_dir = \"reports\"\nsentinel = \".pending\"\n",
    );
    write(repo, "crates/blastguard/src/a.rs", "fn a() {}\n");
    write(repo, "crates/stuckguard/src/s.rs", "fn s() {}\n");
    write(repo, "crates/notagate/src/b.rs", "fn b() {}\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    git(repo, &["rev-parse", "HEAD"])
}

fn commit_change(repo: &Path, files: &[&str]) {
    for f in files {
        write(repo, f, "fn changed() {}\n");
    }
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "change"]);
}

fn entry(key: &str, spec_doc: Option<&str>, impl_file: &str) -> String {
    let sd = spec_doc
        .map(|d| format!("spec_doc = \"{d}\"\n"))
        .unwrap_or_default();
    format!(
        "[entries.\"{key}\"]\nkey = \"{key}\"\n{sd}status = \"changed\"\nimpl_files = [\"{impl_file}\"]\n\n"
    )
}

fn write_map(repo: &Path, body: &str) {
    write(
        repo,
        ".specguard/spec-map.toml",
        &format!("last_synced = \"x\"\n\n{body}"),
    );
}

fn gate_check(repo: &Path, base: &str) -> Output {
    let o = Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(repo)
        .args([
            "--config",
            "specguard.toml",
            "map",
            "gate-check",
            "--base",
            base,
        ])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    // A missing subcommand is a clap usage error (exit 2): it must not be able to
    // pass the "undetermined" tests by accident.
    assert!(
        !err.contains("unrecognized subcommand") && !err.contains("unexpected argument"),
        "`map gate-check` is not implemented: {err}"
    );
    o
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

const GATE_PATH: &str = "crates/blastguard/src/a.rs";

#[test]
fn changed_gate_entry_without_spec_doc_exits_1_and_names_it() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    write_map(d.path(), &entry("blast-a", None, GATE_PATH));
    commit_change(d.path(), &[GATE_PATH]);
    let o = gate_check(d.path(), &base);
    assert_eq!(
        code(&o),
        1,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        all.contains(GATE_PATH),
        "offending entry must be printed: {all}"
    );
}

#[test]
fn control_changed_gate_entry_with_spec_doc_exits_0() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    write_map(
        d.path(),
        &entry("blast-a", Some("docs/specs/blastguard.md"), GATE_PATH),
    );
    commit_change(d.path(), &[GATE_PATH]);
    assert_eq!(code(&gate_check(d.path(), &base)), 0);
}

#[test]
fn control_reasoned_ack_exits_0() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    write_map(d.path(), &entry("blast-a", None, GATE_PATH));
    write(
        d.path(),
        ".specguard/spec-doc-acks.toml",
        &format!(
            "[[ack]]\npath = \"{GATE_PATH}\"\nreason = \"spec pending, tracked in backlog\"\n"
        ),
    );
    commit_change(d.path(), &[GATE_PATH]);
    assert_eq!(code(&gate_check(d.path(), &base)), 0);
}

#[test]
fn ack_with_empty_or_missing_reason_is_not_an_ack() {
    for ack in [
        format!("[[ack]]\npath = \"{GATE_PATH}\"\nreason = \"\"\n"),
        format!("[[ack]]\npath = \"{GATE_PATH}\"\nreason = \"   \"\n"),
        format!("[[ack]]\npath = \"{GATE_PATH}\"\n"),
    ] {
        let d = tempfile::tempdir().unwrap();
        let base = fixture(d.path());
        write_map(d.path(), &entry("blast-a", None, GATE_PATH));
        write(d.path(), ".specguard/spec-doc-acks.toml", &ack);
        commit_change(d.path(), &[GATE_PATH]);
        assert_eq!(
            code(&gate_check(d.path(), &base)),
            1,
            "ack {ack:?} must not count"
        );
    }
}

#[test]
fn control_non_gate_change_with_null_spec_doc_exits_0() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    write_map(d.path(), &entry("b", None, "crates/notagate/src/b.rs"));
    commit_change(d.path(), &["crates/notagate/src/b.rs"]);
    assert_eq!(code(&gate_check(d.path(), &base)), 0);
}

#[test]
fn control_unchanged_gate_entry_with_null_spec_doc_exits_0() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    // Gate entry exists with null spec_doc, but only the non-gate file changes.
    write_map(
        d.path(),
        &format!(
            "{}{}",
            entry("blast-a", None, GATE_PATH),
            entry("b", None, "crates/notagate/src/b.rs")
        ),
    );
    commit_change(d.path(), &["crates/notagate/src/b.rs"]);
    assert_eq!(code(&gate_check(d.path(), &base)), 0);
}

#[test]
fn one_acked_one_bare_gate_entry_still_exits_1() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    write_map(
        d.path(),
        &format!(
            "{}{}",
            entry("blast-a", None, GATE_PATH),
            entry("stuck-s", None, "crates/stuckguard/src/s.rs")
        ),
    );
    write(
        d.path(),
        ".specguard/spec-doc-acks.toml",
        &format!("[[ack]]\npath = \"{GATE_PATH}\"\nreason = \"r\"\n"),
    );
    commit_change(d.path(), &[GATE_PATH, "crates/stuckguard/src/s.rs"]);
    let o = gate_check(d.path(), &base);
    assert_eq!(code(&o), 1);
    assert!(String::from_utf8_lossy(&o.stdout).contains("crates/stuckguard/src/s.rs"));
}

#[test]
fn unparseable_map_exits_2() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    write(d.path(), ".specguard/spec-map.toml", "this is [not toml\n");
    commit_change(d.path(), &[GATE_PATH]);
    assert_eq!(code(&gate_check(d.path(), &base)), 2);
}

#[test]
fn absent_map_with_changed_gate_file_exits_2() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    commit_change(d.path(), &[GATE_PATH]);
    assert_eq!(code(&gate_check(d.path(), &base)), 2);
}

#[test]
fn unparseable_ack_file_exits_2() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    write_map(
        d.path(),
        &entry("blast-a", Some("docs/specs/blastguard.md"), GATE_PATH),
    );
    write(d.path(), ".specguard/spec-doc-acks.toml", "[[ack\nnope\n");
    commit_change(d.path(), &[GATE_PATH]);
    assert_eq!(code(&gate_check(d.path(), &base)), 2);
}

#[test]
fn unresolvable_base_rev_exits_2() {
    let d = tempfile::tempdir().unwrap();
    fixture(d.path());
    write_map(
        d.path(),
        &entry("blast-a", Some("docs/specs/blastguard.md"), GATE_PATH),
    );
    commit_change(d.path(), &[GATE_PATH]);
    assert_eq!(code(&gate_check(d.path(), "no-such-rev-xyz")), 2);
}

fn real_repo_prompt() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let first = git(&root, &["rev-list", "--max-parents=0", "HEAD"]);
    let first = first.lines().last().unwrap().to_string();
    let o = Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(&root)
        .args(["--config", "specguard.toml", "--baseline", &first, "prompt"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// Option A ruling: the real repo config lists docs/specs files in the gate
/// areas' canon. Observed through `specguard prompt` (which prints each in-scope
/// area's canon pointers), with the root commit as baseline so every area is in scope.
#[test]
fn real_repo_config_puts_docs_specs_in_gate_area_canon() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = real_repo_prompt();
    for gate in ["blastguard", "specguard", "overwatch"] {
        let spec = format!("docs/specs/{gate}.md");
        assert!(root.join(&spec).exists(), "premise: {spec} exists");
        assert!(
            out.contains(&spec),
            "{spec} must be in the {gate} area's canon"
        );
    }
}

/// Control: adding docs/specs must not displace existing README canon.
#[test]
fn control_real_repo_config_keeps_readme_canon() {
    assert!(real_repo_prompt().contains("crates/blastguard/README.md"));
}
