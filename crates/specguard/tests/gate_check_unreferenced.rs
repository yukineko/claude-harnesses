// IMPLEMENTER-WRITTEN (backlog 0c277117, ruling 5): written by the implementer of
// `map gate-check`, not by an independent agent. Treat it as weaker evidence than
// tests/gate_check.rs. Its RED was observed by deleting the unreferenced-path
// handling in src/gatecheck.rs (see the commit report).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A changed gate-crate file that no spec-map entry references must not pass
//! silently: the map has not observed it, so its spec-doc status is unknown
//! (exit 2). Controls: a file excluded by `[map].exclude` is not spec-bearing and
//! passes; the same change passes once an entry covering it carries a spec_doc.

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

const TRACKED: &str = "crates/blastguard/src/a.rs";
const UNMAPPED: &str = "crates/blastguard/src/new_module.rs";

/// Base commit + a map whose only gate entry covers TRACKED (with a spec doc).
fn fixture(repo: &Path, extra_config: &str) -> String {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    write(
        repo,
        "specguard.toml",
        &format!(
            "[project]\nname = \"Demo\"\nroot = \".\"\n[output]\nreport_dir = \"reports\"\nsentinel = \".pending\"\n{extra_config}"
        ),
    );
    write(repo, TRACKED, "fn a() {}\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    let base = git(repo, &["rev-parse", "HEAD"]);
    write_map(repo, &[TRACKED]);
    base
}

fn write_map(repo: &Path, impls: &[&str]) {
    let list = impls
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    write(
        repo,
        ".specguard/spec-map.toml",
        &format!(
            "last_synced = \"x\"\n\n[entries.\"blast\"]\nkey = \"blast\"\nspec_doc = \"docs/specs/blastguard.md\"\nstatus = \"changed\"\nimpl_files = [{list}]\n"
        ),
    );
}

fn commit(repo: &Path, files: &[&str]) {
    for f in files {
        write(repo, f, "fn changed() {}\n");
    }
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "change"]);
}

fn gate_check(repo: &Path, base: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_specguard"))
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
        .unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

#[test]
fn changed_gate_file_referenced_by_no_entry_exits_2_and_names_it() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path(), "");
    commit(d.path(), &[UNMAPPED]);
    let o = gate_check(d.path(), &base);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(
        code(&o),
        2,
        "stdout={} stderr={err}",
        String::from_utf8_lossy(&o.stdout)
    );
    assert!(
        err.contains(UNMAPPED),
        "the unreferenced path must be named: {err}"
    );
}

#[test]
fn control_same_change_passes_once_an_entry_with_spec_doc_covers_it() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path(), "");
    write_map(d.path(), &[TRACKED, UNMAPPED]);
    commit(d.path(), &[UNMAPPED]);
    assert_eq!(code(&gate_check(d.path(), &base)), 0);
}

#[test]
fn control_excluded_gate_file_is_not_spec_bearing() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path(), "[map]\nexclude = [\"**/*.toml\"]\n");
    commit(d.path(), &["crates/blastguard/Cargo.toml"]);
    assert_eq!(code(&gate_check(d.path(), &base)), 0);
}
