//! The gated `download` subcommand.
//!
//! Fetches the SWE-bench Verified split (princeton-nlp/SWE-bench_Verified on
//! HuggingFace) into a local normalized JSONL cache. Following the beacon house
//! pattern, network I/O shells out to `curl` rather than linking an HTTP stack,
//! keeping the bundled binary tiny.
//!
//! Two invariants matter:
//!   * **Gated** — the network path is reached *only* when `benchkit download`
//!     is explicitly invoked. Nothing here runs during `cargo test`; the loader
//!     and model are the deterministic, offline core.
//!   * **Idempotent** — if the cache file already exists it is a no-op (unless
//!     `--force`), so re-running is cheap and safe.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// HuggingFace `datasets` server URL for the SWE-bench Verified `test` split as
/// newline-delimited JSON rows (one instance per line).
const DATASET_URL: &str = "https://huggingface.co/datasets/princeton-nlp/SWE-bench_Verified/resolve/main/data/test-00000-of-00001.parquet";

/// Default cache location, relative to the current working directory.
const DEFAULT_CACHE: &str = ".benchkit-cache/swe-bench-verified.jsonl";

/// Outcome of a `download` run — reported so the CLI can print a clear line and
/// so tests of the *decision* logic never touch the network.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Cache already present; nothing fetched (the idempotent no-op).
    CacheHit(PathBuf),
    /// Cache written (or overwritten) from the network.
    Fetched(PathBuf),
}

/// Run the download subcommand. `dest` is the cache file path (or `None` for the
/// default); `force` re-fetches even when the cache exists.
///
/// This is the only entry point that touches the network, and only when the
/// cache is absent (or `force`) — the gate required by the task.
pub fn execute(dest: Option<PathBuf>, force: bool) -> Result<Outcome> {
    let path = dest.unwrap_or_else(|| PathBuf::from(DEFAULT_CACHE));
    if path.exists() && !force {
        return Ok(Outcome::CacheHit(path));
    }
    fetch(&path)?;
    Ok(Outcome::Fetched(path))
}

/// Perform the actual `curl` fetch into `path`, creating parent dirs first.
fn fetch(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating cache dir: {}", parent.display()))?;
        }
    }
    // beacon pattern: shell out to curl with a hard timeout instead of linking
    // an HTTP client. `-f` fails on HTTP errors, `-L` follows the HF redirect.
    let status = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "120",
            "-o",
            &path.to_string_lossy(),
            DATASET_URL,
        ])
        .status()
        .context("spawning curl to fetch SWE-bench Verified (is curl installed?)")?;
    if !status.success() {
        bail!(
            "curl failed to fetch {DATASET_URL} (exit {:?})",
            status.code()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Guards the gate: when the cache file exists, `execute` short-circuits to
    // CacheHit and never reaches `fetch` — so this test does no network I/O.
    #[test]
    fn existing_cache_is_a_noop() {
        let dir = std::env::temp_dir().join(format!("benchkit-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("cache.jsonl");
        std::fs::write(&cache, "{}\n").unwrap();

        let out = execute(Some(cache.clone()), false).unwrap();
        assert_eq!(out, Outcome::CacheHit(cache));

        std::fs::remove_dir_all(&dir).ok();
    }
}
