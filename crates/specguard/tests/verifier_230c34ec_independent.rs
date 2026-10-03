#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verifier probes for backlog 230c34ec (not written by the worker).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "specguard-vrf-230c34ec-{tag}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Entry key `gate-feature` (NOT a path) with impl file `src/gate.rs`; a second
/// unrelated entry `other-feature` with impl `src/other.rs`.
fn repo(tag: &str, spec_docs: &str) -> PathBuf {
    let dir = scratch(tag);
    for d in [".specguard", "docs/specs/adir.md", "src"] {
        fs::create_dir_all(dir.join(d)).unwrap();
    }
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
    fs::write(dir.join("docs/specs/locked.md"), "# Locked\n\nbody\n").unwrap();
    fs::write(dir.join("src/gate.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.join("src/other.rs"), "fn main() {}\n").unwrap();
    fs::write(
        dir.join(".specguard/spec-map.toml"),
        "last_synced = \"deadbeef\"\n\
         [entries.\"gate-feature\"]\nkey = \"gate-feature\"\nkind = \"feature\"\nstatus = \"changed\"\nimpl_files = [\"src/gate.rs\"]\n\
         [entries.\"other-feature\"]\nkey = \"other-feature\"\nkind = \"feature\"\nstatus = \"changed\"\nimpl_files = [\"src/other.rs\"]\n",
    )
    .unwrap();
    fs::write(dir.join(".specguard/spec-docs.toml"), spec_docs).unwrap();
    dir
}

fn brief(dir: &Path, task: &str) -> (String, i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_specguard"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .current_dir(dir)
        .args(["--config", "specguard.toml", "brief", "--json", task])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("brief --json not JSON ({e}): {out:?}"));
    (
        v["verdict"].as_str().unwrap_or("<none>").to_string(),
        out.status.code().unwrap_or(-1),
        format!("{stdout} / stderr={}", String::from_utf8_lossy(&out.stderr)),
    )
}

fn b(path: &str, doc: &str) -> String {
    format!("[[spec]]\npath = \"{path}\"\ndoc = \"{doc}\"\nreason = \"r\"\n")
}

#[test]
fn binding_on_impl_file_covers_entry_with_non_path_key() {
    let dir = repo("implkey", &b("src/gate.rs", "docs/specs/gate.md"));
    let (v, c, o) = brief(&dir, "src/gate.rs");
    assert_eq!((v.as_str(), c), ("covered", 0), "{o}");
    assert!(o.contains("gate-feature"), "{o}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn binding_to_a_directory_named_md_is_not_covered() {
    let dir = repo("dir", &b("src/gate.rs", "docs/specs/adir.md"));
    let (v, c, o) = brief(&dir, "src/gate.rs");
    assert_eq!((v.as_str(), c), ("not-covered", 0), "{o}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn binding_for_another_entry_does_not_cover_this_one() {
    let dir = repo("other", &b("src/other.rs", "docs/specs/gate.md"));
    let (v, c, o) = brief(&dir, "src/gate.rs");
    assert_eq!((v.as_str(), c), ("not-covered", 0), "{o}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn dotdot_escape_is_not_covered() {
    let dir = repo("dotdot", &b("src/gate.rs", "docs/specs/../specs/gate.md"));
    let (v, c, o) = brief(&dir, "src/gate.rs");
    assert_eq!((v.as_str(), c), ("not-covered", 0), "{o}");
    let _ = fs::remove_dir_all(&dir);
}

/// A bound doc whose contents cannot be read must never yield `covered`,
/// even when a valid binding for the same entry also exists elsewhere is NOT
/// the case here: the ONLY binding is the unreadable one.
#[cfg(unix)]
#[test]
fn unreadable_bound_doc_is_undetermined_not_covered() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc_geteuid() } == 0 {
        return;
    }
    let dir = repo("locked", &b("src/gate.rs", "docs/specs/locked.md"));
    let p = dir.join("docs/specs/locked.md");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
    let (v, c, o) = brief(&dir, "src/gate.rs");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!((v.as_str(), c), ("undetermined", 10), "{o}");
}

#[cfg(unix)]
extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}
