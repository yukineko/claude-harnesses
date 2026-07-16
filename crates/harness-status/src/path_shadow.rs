//! PATH-shadowing detector: several plugin READMEs document a standalone
//! install step (`cp target/release/<bin> ~/.cargo/bin/`) so SKILL.md files
//! can invoke the CLI by bare name (e.g. `condukt state ...`). If that copy
//! is never refreshed, it silently shadows the up-to-date plugin-cache binary
//! (`~/.claude/plugins/cache/yukineko/<name>/<version>/bin/<name>`) for every
//! bare-name PATH lookup — `scripts/rollout-plugins.sh` keeps the cache copy
//! current, but has no way to touch a stray `~/.cargo/bin` copy. This module
//! flags that drift so it doesn't go unnoticed indefinitely. Purely
//! diagnostic: fail-soft throughout, never blocks a turn.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// One binary whose bare-name PATH resolution does not point at the
/// plugin-cache copy.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ShadowedBinary {
    pub name: String,
    pub shadowing_path: String,
    pub cache_path: String,
}

/// Split a `$PATH`-style colon-separated string into directories, in order.
fn split_path(path_env: &str) -> Vec<PathBuf> {
    path_env
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Walk `path_dirs` in order and return the first `dir/name` for which
/// `exists` reports true. Pure/testable: `exists` is injected so tests never
/// touch the real filesystem.
fn resolve_path_first<F: Fn(&Path) -> bool>(
    name: &str,
    path_dirs: &[PathBuf],
    exists: &F,
) -> Option<PathBuf> {
    for dir in path_dirs {
        let candidate = dir.join(name);
        if exists(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Pure core: given the ordered `$PATH` directories and the set of known
/// plugin-cache binaries (`name`, `cache_path`), report every binary whose
/// first PATH match is NOT its cache path. `exists` is injected for testing.
fn detect_with<F: Fn(&Path) -> bool>(
    path_dirs: &[PathBuf],
    cache_bins: &[(String, PathBuf)],
    exists: F,
) -> Vec<ShadowedBinary> {
    let mut out = Vec::new();
    for (name, cache_path) in cache_bins {
        if let Some(first) = resolve_path_first(name, path_dirs, &exists) {
            if &first != cache_path {
                out.push(ShadowedBinary {
                    name: name.clone(),
                    shadowing_path: first.display().to_string(),
                    cache_path: cache_path.display().to_string(),
                });
            }
        }
        // No PATH match at all → nothing shadows the cache copy; not flagged.
    }
    out
}

/// The plugin-cache root: `~/.claude/plugins/cache/yukineko`.
fn cache_root() -> PathBuf {
    harness_core::config::home()
        .join(".claude")
        .join("plugins")
        .join("cache")
        .join("yukineko")
}

/// Pick the highest-sorting version dir's `bin/` under a plugin dir (mirrors
/// the sort+pop "current version" resolution used elsewhere in the harness,
/// e.g. `autoflow::backlog::find_backlog_binary`).
fn latest_bin_dir(plugin_dir: &Path) -> Option<PathBuf> {
    let mut versions: Vec<PathBuf> = std::fs::read_dir(plugin_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    versions.sort();
    let bin = versions.pop()?.join("bin");
    bin.is_dir().then_some(bin)
}

/// Non-recursive list of file names directly inside `dir`.
fn list_binary_names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Enumerate every `(name, cache_path)` pair across all plugin-cache dirs.
/// Fail-soft: a missing/unreadable cache root yields an empty list.
fn scan_cache_bins(root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut plugin_dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    plugin_dirs.sort();

    let mut out = Vec::new();
    for plugin_dir in plugin_dirs {
        let Some(bin_dir) = latest_bin_dir(&plugin_dir) else {
            continue;
        };
        for name in list_binary_names(&bin_dir) {
            let cache_path = bin_dir.join(&name);
            out.push((name, cache_path));
        }
    }
    out
}

/// Live detection: reads `$PATH`, the real plugin-cache root, and the real
/// filesystem. Fail-soft throughout — a missing `$PATH` env var or missing
/// cache root yields an empty report, never panics.
pub fn detect() -> Vec<ShadowedBinary> {
    let Ok(path_env) = std::env::var("PATH") else {
        return Vec::new();
    };
    let path_dirs = split_path(&path_env);
    let cache_bins = scan_cache_bins(&cache_root());
    detect_with(&path_dirs, &cache_bins, |p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pb(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn cache_dir_first_in_path_is_not_flagged() {
        let path_dirs = vec![pb("/cache/condukt/0.7.64/bin"), pb("/home/user/.cargo/bin")];
        let cache_bins = vec![(
            "condukt".to_string(),
            pb("/cache/condukt/0.7.64/bin/condukt"),
        )];
        // Both dirs "contain" a condukt binary, but the cache dir comes first.
        let shadowed = detect_with(&path_dirs, &cache_bins, |p| {
            p == Path::new("/cache/condukt/0.7.64/bin/condukt")
                || p == Path::new("/home/user/.cargo/bin/condukt")
        });
        assert!(shadowed.is_empty());
    }

    #[test]
    fn earlier_dir_with_same_name_is_flagged_as_shadowing() {
        let path_dirs = vec![pb("/home/user/.cargo/bin"), pb("/cache/condukt/0.7.64/bin")];
        let cache_bins = vec![(
            "condukt".to_string(),
            pb("/cache/condukt/0.7.64/bin/condukt"),
        )];
        let shadowed = detect_with(&path_dirs, &cache_bins, |p| {
            p == Path::new("/cache/condukt/0.7.64/bin/condukt")
                || p == Path::new("/home/user/.cargo/bin/condukt")
        });
        assert_eq!(shadowed.len(), 1);
        assert_eq!(shadowed[0].name, "condukt");
        assert_eq!(shadowed[0].shadowing_path, "/home/user/.cargo/bin/condukt");
        assert_eq!(shadowed[0].cache_path, "/cache/condukt/0.7.64/bin/condukt");
    }

    #[test]
    fn no_path_match_at_all_is_not_flagged() {
        let path_dirs = vec![pb("/home/user/.cargo/bin")];
        let cache_bins = vec![(
            "condukt".to_string(),
            pb("/cache/condukt/0.7.64/bin/condukt"),
        )];
        // Nothing on PATH actually has this binary — not a shadowing concern.
        let shadowed = detect_with(&path_dirs, &cache_bins, |_| false);
        assert!(shadowed.is_empty());
    }

    #[test]
    fn missing_cache_root_yields_empty_scan_never_panics() {
        let bins = scan_cache_bins(Path::new("/no/such/cache/root/at/all"));
        assert!(bins.is_empty());
    }

    #[test]
    fn empty_cache_bins_yields_no_findings() {
        let shadowed = detect_with(&[pb("/home/user/.cargo/bin")], &[], |_| true);
        assert!(shadowed.is_empty());
    }
}
