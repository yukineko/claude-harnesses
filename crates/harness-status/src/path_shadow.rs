//! PATH-shadowing detector: several plugin READMEs document a standalone
//! install step (`cp target/release/<bin> ~/.cargo/bin/`) so SKILL.md files
//! can invoke the CLI by bare name (e.g. `condukt state ...`). If that copy
//! is never refreshed, it silently shadows the up-to-date plugin-cache binary
//! (`~/.claude/plugins/cache/yukineko/<name>/<version>/bin/<name>`) for every
//! bare-name PATH lookup — `scripts/rollout-plugins.sh` keeps the cache copy
//! current, but has no way to touch a stray `~/.cargo/bin` copy. This module
//! flags that drift so it doesn't go unnoticed indefinitely. Diagnostic only
//! (never blocks a turn), but not silent: a scan that could not be completed
//! is reported as undetermined, never as "nothing shadowed".

use harness_core::verdict::Determination;
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

/// Non-recursive list of the file names directly inside `dir`, in three
/// answers. Each entry is classified with `std::fs::metadata` (follows
/// symlinks, so a symlinked binary counts): a file is listed, a non-file is
/// skipped, and a `NotFound` (dangling symlink / vanished entry) is skipped as
/// observed-nothing. Any other error — `dir` unreadable, an entry unreadable or
/// un-stat-able, a non-UTF-8 name — is `Undetermined`, never a shorter list.
fn list_binary_names(dir: &Path) -> Determination<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(e) => {
            return Determination::undetermined(format!(
                "could not enumerate {}: {e}",
                dir.display()
            ))
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not read an entry of {}: {e}",
                    dir.display()
                ))
            }
        };
        let path = entry.path();
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not stat {}: {e}",
                    path.display()
                ))
            }
        }
        match entry.file_name().into_string() {
            Ok(name) => names.push(name),
            Err(_) => {
                return Determination::undetermined(format!(
                    "a binary name under {} is not UTF-8",
                    dir.display()
                ))
            }
        }
    }
    Determination::Known(names)
}

/// Enumerate every `(name, cache_path)` pair across all plugin-cache dirs
/// under `root` (the live caller passes `harness_core::plugin_bin::cache_root()`).
///
/// A missing cache root is a legitimate absence (e.g. no plugins installed
/// yet) and yields `Known(vec![])`. Each plugin's current version is picked by
/// `harness_core::plugin_bin::cache_lookup_in` (numeric version order, the
/// newest version dir that holds `bin/<plugin>`), and every file in that
/// `bin/` dir is listed. A plugin with no `bin/<plugin>` in any version
/// (`Known(None)`: a skill-only plugin) has nothing to shadow and is skipped.
///
/// Every cannot-determine — the root or a plugin dir unreadable, an entry that
/// cannot be read or stat'ed, a non-UTF-8 name, an unreadable `bin/` dir —
/// makes the whole scan `Undetermined` (naming the plugin). It is never read as
/// "no shadowed binaries", and a plugin is never silently dropped from the scan.
fn scan_cache_bins(root: &Path) -> Determination<Vec<(String, PathBuf)>> {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Determination::Known(Vec::new())
        }
        Err(e) => return Determination::undetermined(format!("{}: {e}", root.display())),
    };
    let mut plugins: Vec<String> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not read an entry of {}: {e}",
                    root.display()
                ))
            }
        };
        let path = entry.path();
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            // A plain file in the cache root cannot be a plugin dir.
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not stat {}: {e}",
                    path.display()
                ))
            }
        }
        match entry.file_name().into_string() {
            Ok(name) => plugins.push(name),
            Err(_) => {
                return Determination::undetermined(format!(
                    "a plugin dir name under {} is not UTF-8",
                    root.display()
                ))
            }
        }
    }
    plugins.sort();

    let mut out = Vec::new();
    for plugin in plugins {
        let bin_dir = match harness_core::plugin_bin::cache_lookup_in(root, &plugin) {
            Determination::Known(Some(bin)) => match bin.parent() {
                Some(dir) => dir.to_path_buf(),
                None => {
                    return Determination::undetermined(format!(
                        "plugin {plugin}: resolved binary {} has no parent dir",
                        bin.display()
                    ))
                }
            },
            // No version dir holds `bin/<plugin>`: nothing of this plugin can
            // be shadowed. An observation, not a skip-on-error.
            Determination::Known(None) => continue,
            Determination::Undetermined(why) => {
                return Determination::undetermined(format!("plugin {plugin}: {why}"))
            }
        };
        let names = match list_binary_names(&bin_dir) {
            Determination::Known(names) => names,
            Determination::Undetermined(why) => {
                return Determination::undetermined(format!("plugin {plugin}: {why}"))
            }
        };
        for name in names {
            let cache_path = bin_dir.join(&name);
            out.push((name, cache_path));
        }
    }
    Determination::Known(out)
}

/// Live detection: reads `$PATH`, the real plugin-cache root
/// (`harness_core::plugin_bin::cache_root()`), and the real filesystem. A
/// missing cache root yields `Known` empty; an unset `$PATH` or any
/// cannot-determine in the cache scan (see [`scan_cache_bins`]) yields
/// `Undetermined`. Never panics.
pub fn detect() -> Determination<Vec<ShadowedBinary>> {
    let Ok(path_env) = std::env::var("PATH") else {
        return Determination::undetermined("$PATH is not set");
    };
    let path_dirs = split_path(&path_env);
    match scan_cache_bins(&harness_core::plugin_bin::cache_root()) {
        Determination::Known(cache_bins) => {
            Determination::Known(detect_with(&path_dirs, &cache_bins, |p| p.is_file()))
        }
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
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
    fn missing_cache_root_yields_known_empty_scan_never_panics() {
        let bins = scan_cache_bins(Path::new("/no/such/cache/root/at/all"));
        assert_eq!(bins, Determination::Known(Vec::new()));
    }

    #[test]
    fn empty_cache_bins_yields_no_findings() {
        let shadowed = detect_with(&[pb("/home/user/.cargo/bin")], &[], |_| true);
        assert!(shadowed.is_empty());
    }

    /// CA-harness-status-path-shadow-01: a cache root that EXISTS but is
    /// unreadable (permission denied) must resolve to `Undetermined`, not the
    /// same `Known(vec![])` as a legitimately-absent root — the old
    /// `let Ok(..) else { return Vec::new() }` collapsed both into "no
    /// shadowed binaries", which reads as clean even when the scan never ran.
    #[test]
    #[cfg(unix)]
    fn unreadable_existing_cache_root_is_undetermined_not_known_empty() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "harness-status-path-shadow-unreadable-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&dir, perms.clone()).unwrap();

        let result = scan_cache_bins(&dir);

        // Restore permissions so the temp dir can be cleaned up.
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(&dir, perms);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            matches!(result, Determination::Undetermined(_)),
            "an unreadable existing cache root must be Undetermined, got {result:?}"
        );
    }
}
