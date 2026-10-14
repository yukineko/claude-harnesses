// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 984e3ed9 — `index_store::fresh_symbols` wraps
//! `code_index::load_index` in `Determination::known`, so a symbol index body
//! that will not load is served as `Known([])` (or a partial list).
//!
//! The module doc of `crates/harness-core/src/index_store.rs` states: "an index
//! that will not load ... resolve[s] to `Undetermined`. None of them resolve to
//! an empty result set". These tests drive the public façade
//! (`all_symbols` / `search_symbols`) against a real git repo whose symbol
//! index meta is current (fingerprint matches the tree) but whose body is
//! damaged, and assert that claim. The control pins that an intact index is
//! `Known` with the seeded symbol.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Stdio};

use harness_core::index_store::{self, IndexPaths};
use harness_core::verdict::Determination;

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

/// A git repo with one tracked `.rs` file and freshly built indexes.
fn built_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "t"]);
    std::fs::write(
        dir.path().join("lib.rs"),
        "pub fn alpha_helper() -> i32 {\n    0\n}\n\npub fn beta_helper() -> i32 {\n    1\n}\n",
    )
    .unwrap();
    git(dir.path(), &["add", "lib.rs"]);
    git(dir.path(), &["commit", "-q", "-m", "seed"]);
    match index_store::build_if_stale(dir.path()) {
        Determination::Known(stats) => assert!(stats.rebuilt, "fixture build must rebuild"),
        Determination::Undetermined(u) => panic!("fixture build undetermined: {}", u.as_str()),
    }
    dir
}

/// Make the body's bytes unreadable as UTF-8 and the containing dir read-only
/// so the one-shot repair rebuild cannot overwrite it either. The meta stays
/// current, so the façade's freshness check passes and only the body load can
/// notice.
fn damage_body_invalid_utf8(root: &Path) -> std::path::PathBuf {
    let paths = IndexPaths::new(root);
    std::fs::write(&paths.symbols_body, [0xff, 0xfe, 0xfd, b'\n']).unwrap();
    let dir = paths.symbols_body.parent().unwrap().to_path_buf();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&paths.symbols_body, std::fs::Permissions::from_mode(0o444)).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    dir
}

struct RestoreMode(std::path::PathBuf);
impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
#[ignore = "backlog 984e3ed9: RED repro"]
fn unreadable_symbol_body_is_undetermined_not_known_empty() {
    let repo = built_repo();
    let dir = damage_body_invalid_utf8(repo.path());
    let _restore = RestoreMode(dir);

    match index_store::all_symbols(repo.path()) {
        Determination::Undetermined(_) => {}
        Determination::Known(symbols) => panic!(
            "an index body that will not load (invalid UTF-8) must resolve to \
             Undetermined per the index_store module doc; got Known({symbols:?})"
        ),
    }
}

#[test]
#[ignore = "backlog 984e3ed9: RED repro"]
fn symbol_body_that_is_a_directory_is_undetermined_for_search() {
    let repo = built_repo();
    let paths = IndexPaths::new(repo.path());
    std::fs::remove_file(&paths.symbols_body).unwrap();
    std::fs::create_dir(&paths.symbols_body).unwrap();
    // Read-only index dir so the repair rebuild cannot replace the directory.
    let dir = paths.symbols_body.parent().unwrap().to_path_buf();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    let _restore = RestoreMode(dir);

    match index_store::search_symbols(repo.path(), "alpha_helper", 10) {
        Determination::Undetermined(_) => {}
        Determination::Known(hits) => panic!(
            "a symbol index body that is a directory (EISDIR) must resolve to \
             Undetermined, not a confident result set; got Known({hits:?})"
        ),
    }
}

#[test]
#[ignore = "backlog 984e3ed9: RED repro"]
fn partially_corrupt_symbol_body_is_not_served_as_complete() {
    let repo = built_repo();
    let paths = IndexPaths::new(repo.path());
    let body = std::fs::read_to_string(&paths.symbols_body).unwrap();
    // Replace the alpha_helper record with garbage; beta_helper stays valid.
    let damaged: String = body
        .lines()
        .map(|l| {
            if l.contains("alpha_helper") {
                "{not a symbol".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        damaged.contains("{not a symbol"),
        "fixture: alpha line replaced"
    );
    std::fs::write(&paths.symbols_body, damaged + "\n").unwrap();
    let dir = paths.symbols_body.parent().unwrap().to_path_buf();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&paths.symbols_body, std::fs::Permissions::from_mode(0o444))
            .unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    let _restore = RestoreMode(dir);

    match index_store::all_symbols(repo.path()) {
        Determination::Undetermined(_) => {}
        Determination::Known(symbols) => panic!(
            "a partially corrupt symbol index must not be served as Known (a lost \
             symbol would read as 'not there'); got Known({:?})",
            symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        ),
    }
}

/// Control: an intact index is Known and holds the seeded symbol.
#[test]
fn intact_index_is_known_with_symbol() {
    let repo = built_repo();
    match index_store::all_symbols(repo.path()) {
        Determination::Known(symbols) => {
            assert!(
                symbols.iter().any(|s| s.name == "alpha_helper"),
                "{symbols:?}"
            )
        }
        Determination::Undetermined(u) => panic!("intact index undetermined: {}", u.as_str()),
    }
}
