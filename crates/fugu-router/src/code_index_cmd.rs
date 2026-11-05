//! `code-index build|search` subcommands (per-repo lexical symbol index).
//! Split out of `main.rs` (file-growth cap).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::json;

use super::code_index_fingerprint;

/// Path convention for the per-repo code index: `<root>/.fugu/code-index.jsonl`.
/// Deliberately **per-repo** (not the machine-global `~/.fugu-router/...`
/// episode-store convention) because a code index's symbols/file-paths are
/// only meaningful relative to the repo they were extracted from — unlike
/// routing episodes or lessons, which are cross-project by design. `.fugu/`
/// sits alongside the repo (like `.git/`), namespaced under the plugin so it
/// doesn't collide with other tools' dotfiles.
fn code_index_path(root: &Path) -> PathBuf {
    root.join(".fugu").join("code-index.jsonl")
}

/// Sidecar meta path next to the index (code-RAG slice-3). Holds the
/// staleness fingerprint so `build --if-stale` can skip an unchanged rebuild
/// without touching the pure Symbol-per-line `.jsonl` body.
fn code_index_meta_path(root: &Path) -> PathBuf {
    root.join(".fugu").join("code-index.meta.json")
}

/// Enumerate git-tracked `.rs` files under `root` as repo-relative paths (via
/// `git ls-files`, so untracked/ignored files are skipped deterministically
/// the same way the rest of the repo treats "tracked"). Shared by the rebuild
/// and the `--if-stale` fingerprint paths so both see the identical file set.
fn code_index_rs_files(root: &Path) -> Result<Vec<String>> {
    let out = std::process::Command::new("git")
        .args(["-C", &root.to_string_lossy(), "ls-files"])
        .output()
        .with_context(|| format!("running git ls-files under {}", root.display()))?;
    anyhow::ensure!(
        out.status.success(),
        "git ls-files failed under {}: {}",
        root.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = String::from_utf8_lossy(&out.stdout);
    Ok(listing
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && l.ends_with(".rs"))
        .map(|l| l.to_string())
        .collect())
}

/// `code-index build [--root] [--if-stale]`: extract symbols from every
/// git-tracked `.rs` file and rebuild the index wholesale.
///
/// With `--if-stale`, first compute the cheap fingerprint of the current `.rs`
/// set; if it matches the sidecar meta from the last build **and** the index
/// file still exists, skip the rebuild and report `rebuilt:false`. Otherwise
/// (or without the flag) rebuild the index and refresh the sidecar meta,
/// reporting `rebuilt:true`. Plain `build` (no flag) always rebuilds —
/// back-compat.
pub(crate) fn cmd_code_index_build(root: Option<PathBuf>, if_stale: bool) -> Result<()> {
    use harness_core::code_index::{
        extract_symbols, read_meta, write_index, write_meta, IndexMeta,
    };

    let root = root.unwrap_or_else(|| PathBuf::from("."));
    let rel_files = code_index_rs_files(&root)?;
    let fingerprint = code_index_fingerprint(&root, &rel_files);
    let index_path = code_index_path(&root);
    let meta_path = code_index_meta_path(&root);

    // --if-stale fast path: unchanged source set → no-op (index untouched).
    if if_stale {
        if let Some(meta) = read_meta(&meta_path) {
            if meta.fingerprint == fingerprint && index_path.exists() {
                println!(
                    "{}",
                    json!({
                        "rebuilt": false,
                        "files_scanned": meta.files,
                        "symbols_indexed": meta.symbols,
                        "index_path": index_path.to_string_lossy(),
                    })
                );
                return Ok(());
            }
        }
    }

    let mut symbols = Vec::new();
    let mut files_scanned = 0usize;
    for rel in &rel_files {
        let Ok(contents) = std::fs::read_to_string(root.join(rel)) else {
            // Fail-soft: an unreadable tracked file (deleted-but-staged,
            // binary misdetection, permissions) is skipped, not fatal.
            continue;
        };
        symbols.extend(extract_symbols(&contents, rel));
        files_scanned += 1;
    }

    // Fail-loud: a build whose writes failed must not print `rebuilt: true`.
    // The error propagates, so the command exits non-zero with the IO error on
    // stderr and nothing on stdout.
    write_index(&index_path, &symbols)
        .with_context(|| format!("writing code index {}", index_path.display()))?;
    write_meta(
        &meta_path,
        &IndexMeta {
            fingerprint,
            files: files_scanned,
            symbols: symbols.len(),
        },
    )
    .with_context(|| format!("writing code index meta {}", meta_path.display()))?;

    println!(
        "{}",
        json!({
            "rebuilt": true,
            "files_scanned": files_scanned,
            "symbols_indexed": symbols.len(),
            "index_path": index_path.to_string_lossy(),
        })
    );
    Ok(())
}

/// Exit code of `code-index search` when no index exists at `<root>`.
const CODE_INDEX_EXIT_ABSENT: i32 = 3;
/// Exit code of `code-index search` when the index exists but cannot be
/// trusted (IO error, a line that does not parse as a symbol, or an empty
/// body the build sidecar does not vouch for).
const CODE_INDEX_EXIT_UNREADABLE: i32 = 4;

/// Strict tri-state load of the code index at `root` — the shared
/// [`harness_core::code_index::load_index_checked`] over this crate's body and
/// meta paths. Absent (exit 3) and Unreadable (exit 4: IO error, a line that is
/// not a symbol record, an unreadable build meta, a body/meta symbol-count
/// mismatch, or an empty body no meta vouches for) are distinct from a
/// genuine no-match, which is a successful `[]`.
fn load_code_index_strict(
    root: &Path,
) -> std::result::Result<
    Vec<harness_core::code_index::Symbol>,
    harness_core::code_index::IndexLoadError,
> {
    harness_core::code_index::load_index_checked(
        &code_index_path(root),
        &code_index_meta_path(root),
    )
}

/// `code-index search --query <q> [--root] [--k]`: load the index for `root`
/// and run the deterministic lexical search.
///
/// Contract (three outcomes, not two):
/// * index read → JSON array of hits on stdout, exit 0. `[]` means the index
///   was read and nothing matched — never "there is no index".
/// * index absent → nothing on stdout, diagnostic on stderr, exit 3.
/// * index unreadable/corrupt → nothing on stdout, diagnostic on stderr,
///   exit 4.
///
/// Callers that do `$(... 2>/dev/null || true)` therefore get an empty
/// string (no context) for absent/unreadable, not a false "nothing found".
pub(crate) fn cmd_code_index_search(query: String, root: Option<PathBuf>, k: usize) -> Result<()> {
    use harness_core::code_index::{search, IndexLoadError};

    let root = root.unwrap_or_else(|| PathBuf::from("."));
    let symbols = match load_code_index_strict(&root) {
        Ok(s) => s,
        Err(IndexLoadError::Absent(path)) => {
            eprintln!(
                "fugu-router: code-index search: no index at {} (run `fugu-router code-index build`)",
                path.display()
            );
            std::process::exit(CODE_INDEX_EXIT_ABSENT);
        }
        Err(IndexLoadError::Unreadable(why)) => {
            eprintln!("fugu-router: code-index search: index unreadable: {why}");
            std::process::exit(CODE_INDEX_EXIT_UNREADABLE);
        }
    };
    let hits = search(&symbols, &query, k);
    let arr: Vec<serde_json::Value> = hits
        .iter()
        .map(|h| {
            let mut v = serde_json::to_value(&h.symbol).unwrap_or_else(|_| json!({}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert("score".into(), json!(h.score));
            }
            v
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&arr)?);
    Ok(())
}
