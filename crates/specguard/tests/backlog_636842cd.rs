// Integration tests: unwrap/expect are fine here (workspace lints are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog 636842cd: `gatecheck::doc_problem` stats the bound doc
//! with `std::fs::metadata`, which FOLLOWS symlinks. The documented contract
//! (`src/gatecheck.rs` module doc) is that a binding's `doc` must "exist under
//! the repo root as a regular file with non-blank content". A
//! `docs/specs/x.md` that is a symlink to a file OUTSIDE the repo is neither a
//! regular file nor content under the repo root, yet it is counted as a valid
//! binding, so `specguard map gate-check` passes the changed gate entry.
//!
//! Fixture style mirrors `gate_check_spec_docs.rs`.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const GATE_PATH: &str = "crates/blastguard/src/a.rs";
const ENTRY_KEY: &str = "blast-a";
const SPEC_DOCS: &str = ".specguard/spec-docs.toml";
const DOC: &str = "docs/specs/blastguard.md";
const DOC_BODY: &str = "# blastguard spec\n\nThe gate decides Deny > Ask > Allow.\n";

fn git(repo: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?} failed");
}

fn write(repo: &Path, rel: &str, body: &str) {
    let p = repo.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn base_rev(repo: &Path) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// Build a repo whose changed gate entry is satisfied only by a binding to
/// `DOC`; `place_doc` puts the doc on disk. Returns the gate-check output.
fn run(place_doc: impl FnOnce(&Path)) -> Output {
    let d = tempfile::tempdir().unwrap();
    let repo = d.path();
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    write(
        repo,
        "specguard.toml",
        "[project]\nname = \"Demo\"\nroot = \".\"\n[output]\nreport_dir = \"reports\"\nsentinel = \".pending\"\n",
    );
    write(repo, GATE_PATH, "fn a() {}\n");
    write(repo, "README.md", "# Demo\n\nA real, non-blank readme.\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    let base = base_rev(repo);

    write(
        repo,
        ".specguard/spec-map.toml",
        &format!(
            "last_synced = \"x\"\n\n[entries.\"{ENTRY_KEY}\"]\nkey = \"{ENTRY_KEY}\"\n\
             status = \"changed\"\nimpl_files = [\"{GATE_PATH}\"]\n\n"
        ),
    );
    write(
        repo,
        SPEC_DOCS,
        &format!("[[spec]]\npath = \"{GATE_PATH}\"\ndoc = \"{DOC}\"\nreason = \"describes it\"\n"),
    );
    place_doc(repo);
    write(repo, GATE_PATH, "fn changed() {}\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "change"]);

    let o = Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(repo)
        .args([
            "--config",
            "specguard.toml",
            "map",
            "gate-check",
            "--base",
            &base,
        ])
        .output()
        .unwrap();
    drop(d);
    o
}

fn all(o: &Output) -> String {
    format!(
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Anti-vacuity: the same binding to a real regular file passes, so the RED
/// below is about the symlink and nothing else.
#[test]
fn control_binding_to_regular_file_passes() {
    let o = run(|repo| write(repo, DOC, DOC_BODY));
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
}

#[test]
fn binding_to_symlink_pointing_outside_repo_does_not_count() {
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("elsewhere.md");
    fs::write(&target, DOC_BODY).unwrap();
    let o = run(|repo| {
        let link = repo.join(DOC);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
    });
    assert_eq!(
        o.status.code(),
        Some(1),
        "a docs/specs doc that is a symlink to a file outside the repo is not \
         'a regular file under the repo root' (gatecheck.rs contract), so the \
         binding must not count and the changed gate entry must be a violation\n{}",
        all(&o)
    );
}
