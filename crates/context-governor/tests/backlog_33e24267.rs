// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 33e24267 — `harness_core::code_index::load_index` silently skips
//! unparseable lines, so `context_governor::codesearch::load` serves a
//! partially corrupt index as a smaller *valid* index (`Ok`), indistinguishable
//! from an intact one.
//!
//! `codesearch::load`'s own doc: "Load the index for `root` as a tri-state, so
//! an unreadable store is distinguishable from an empty one." A store holding a
//! line that is not a symbol record is a store that could not be fully read;
//! it must surface as `Decline::Unreadable`, not `Ok(<the survivors>)`.

use context_governor::codesearch::{load, Decline};
use harness_core::code_index::{write_index, Symbol};

fn sym(name: &str, line: usize) -> Symbol {
    Symbol {
        name: name.to_string(),
        kind: "fn".to_string(),
        file: "src/lib.rs".to_string(),
        line,
        signature: format!("fn {name}() {{"),
    }
}

#[test]
fn partially_corrupt_index_is_unreadable_not_ok_subset() {
    let dir = tempfile::tempdir().unwrap();
    let index_dir = dir.path().join(".fugu");
    std::fs::create_dir_all(&index_dir).unwrap();
    let index_path = index_dir.join("code-index.jsonl");
    // drop(): write_index returns io::Result after the fix and () before it;
    // a failed write still surfaces below when the file is read back.
    drop(write_index(
        &index_path,
        &[sym("alpha_helper", 1), sym("beta_helper", 5)],
    ));

    // Corrupt the alpha_helper record; beta_helper survives.
    let body = std::fs::read_to_string(&index_path).unwrap();
    let damaged: String = body
        .lines()
        .map(|l| {
            if l.contains("alpha_helper") {
                "{\"name\":\"alpha_helper\",TRUNCATED".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&index_path, damaged + "\n").unwrap();

    match load(dir.path()) {
        Err(Decline::Unreadable(_)) => {}
        other => panic!(
            "a partially corrupt index must be Decline::Unreadable, not served as a \
             smaller valid index; got {other:?}"
        ),
    }
}

/// Control: an intact two-symbol index loads Ok with both symbols.
#[test]
fn intact_index_loads_both_symbols() {
    let dir = tempfile::tempdir().unwrap();
    let index_dir = dir.path().join(".fugu");
    std::fs::create_dir_all(&index_dir).unwrap();
    let index_path = index_dir.join("code-index.jsonl");
    let syms = [sym("alpha_helper", 1), sym("beta_helper", 5)];
    drop(write_index(&index_path, &syms));
    assert_eq!(load(dir.path()).unwrap(), syms.to_vec());
}
