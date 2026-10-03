#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog 230c34ec: `specguard brief` cannot return `covered`
//! for an area whose spec binding is recorded in the TRACKED
//! `.specguard/spec-docs.toml` — brief reads only the gitignored map's
//! `spec_doc`, so every real topic resolves to `not-covered` and flow 3-1.5's
//! spec-gap trigger 3 fires vacuously.
//!
//! The control (`map_spec_doc_control`) shows `covered` IS reachable when the
//! binding sits in the map itself, so the RED below is the wiring gap, not an
//! unreachable verdict.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "specguard-backlog-230c34ec-{tag}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A repo with one map entry for `src/gate.rs`. `spec_doc_in_map` puts the
/// binding into the map entry; otherwise the binding lives ONLY in the tracked
/// `.specguard/spec-docs.toml` (the repo's durable record).
fn repo(tag: &str, spec_doc_in_map: bool) -> PathBuf {
    let dir = scratch(tag);
    fs::create_dir_all(dir.join(".specguard")).unwrap();
    fs::create_dir_all(dir.join("docs/specs")).unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("specguard.toml"),
        "[project]\nname = \"Demo\"\nroot = \".\"\n\n[output]\nreport_dir = \"reports\"\nsentinel = \".pending\"\n\n[[area]]\nname = \"src\"\nglobs = [\"src/**\"]\ncanon = [\"docs/specs/gate.md\"]\n",
    )
    .unwrap();
    fs::write(
        dir.join("docs/specs/gate.md"),
        "# Gate\n\nThe gate blocks.\n",
    )
    .unwrap();
    fs::write(dir.join("src/gate.rs"), "fn main() {}\n").unwrap();
    let spec_line = if spec_doc_in_map {
        "spec_doc = \"docs/specs/gate.md\"\n"
    } else {
        ""
    };
    fs::write(
        dir.join(".specguard/spec-map.toml"),
        format!(
            "last_synced = \"deadbeef\"\n[entries.\"src/gate.rs\"]\nkey = \"src/gate.rs\"\nkind = \"feature\"\nstatus = \"changed\"\n{spec_line}impl_files = [\"src/gate.rs\"]\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join(".specguard/spec-docs.toml"),
        "[[spec]]\npath = \"src/gate.rs\"\ndoc = \"docs/specs/gate.md\"\nreason = \"docs/specs/gate.md describes src/gate.rs\"\n",
    )
    .unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.email", "t@t.t"]);
    git(&dir, &["config", "user.name", "t"]);
    git(&dir, &["config", "commit.gpgsign", "false"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "init"]);
    dir
}

fn brief_verdict(dir: &Path) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_specguard"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .current_dir(dir)
        .args([
            "--config",
            "specguard.toml",
            "brief",
            "--json",
            "src/gate.rs",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("brief --json not JSON ({e}): {out:?}"));
    (
        v["verdict"].as_str().unwrap_or("<none>").to_string(),
        stdout,
    )
}

#[test]
fn backlog_230c34ec_map_spec_doc_control() {
    let dir = repo("control", true);
    let (verdict, out) = brief_verdict(&dir);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        verdict, "covered",
        "control: map-level spec_doc must give covered: {out}"
    );
}

#[test]
fn backlog_230c34ec_tracked_spec_docs_binding_makes_brief_covered() {
    let dir = repo("tracked", false);
    let (verdict, out) = brief_verdict(&dir);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        verdict, "covered",
        "the tracked .specguard/spec-docs.toml binds src/gate.rs to an existing spec, \
         yet brief does not report covered: {out}"
    );
}
