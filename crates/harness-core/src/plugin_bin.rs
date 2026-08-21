//! harness-core::plugin_bin — locate a sibling harness plugin's executable
//! without trusting the ambient `$PATH`.
//!
//! # Why this exists (measured, not hypothetical)
//!
//! Hook processes do **not** inherit the plugin `bin/` directories on `$PATH`.
//! Claude Code prepends them inside the *Bash tool's* shell (via the sourced
//! shell snapshot); the `claude` process environment itself has none of them.
//! Measured 2026-08-21 on this host: `tr '\0' '\n' < /proc/<claude pid>/environ
//! | grep -c plugins/cache/yukineko` → `0`, while the same lookup from the Bash
//! tool resolves every plugin binary. Every hook-spawned `Command::new("backlog")`
//! therefore resolves against the *user's login* `$PATH` only.
//!
//! That went unnoticed for as long as stale standalone copies happened to sit in
//! `~/.cargo/bin` — which is on the login `$PATH`. Backlog `91fa24df` measured
//! the harm of those copies (a 2026-07-23 `backlog` shadowing the rolled-out one,
//! reporting a different store) and they were deleted on 2026-08-20 (`bb046648`).
//! The deletion was correct, and it removed the accident that had been holding
//! the hook path up: from the next session on, `overwatch status` — the
//! SessionStart banner — reported all four of its sources as `(unknown: … No such
//! file or directory (os error 2))`.
//!
//! # Resolution order: cache first, `$PATH` second
//!
//! Deliberately the reverse of the ad-hoc resolver this generalises
//! (`autoflow::backlog::find_backlog_binary`, which probes `$PATH` first).
//! `scripts/rollout-plugins.sh` is the only sanctioned distributor of these
//! binaries (CLAUDE.md forbids hand-`cp`), so the plugin cache is the copy whose
//! version is *known*; anything on `$PATH` is of unknown provenance and was
//! measured to be four weeks stale. Probing `$PATH` first is what let the stale
//! copy win. `$PATH` stays as a fallback so a standalone install with no plugin
//! cache still works.
//!
//! # The three answers
//!
//! - `Known(Some(path))` — an executable was found. Callers spawn this path.
//! - `Known(None)` — the plugin is genuinely **not installed** here: no cache
//!   directory and nothing on `$PATH`. An observation.
//! - `Undetermined(reason)` — we did not get to look: the cache directory exists
//!   but could not be enumerated, an entry could not be read, or a candidate's
//!   existence could not be tested. Collapsing this into `Known(None)` is the
//!   fail-open this module exists to refuse (CLAUDE.md §3) — "I could not tell
//!   whether backlog is installed" is not "backlog is not installed", and one
//!   level up it is not "the backlog is empty".

use std::path::{Path, PathBuf};

use crate::config::home;
use crate::verdict::Determination;

/// The plugin-cache root that `scripts/rollout-plugins.sh` writes:
/// `~/.claude/plugins/cache/yukineko`.
pub fn cache_root() -> PathBuf {
    home()
        .join(".claude")
        .join("plugins")
        .join("cache")
        .join("yukineko")
}

/// Sort key for a plugin cache version directory name.
///
/// Cache dirs are semver-ish (`0.2.27`). A plain string sort — what the ad-hoc
/// resolvers used — orders them lexicographically, so `0.1.9` sorts ABOVE
/// `0.1.12` and the resolver picks a superseded binary the moment a minor series
/// reaches ten releases. Compare the dot-separated numeric components instead.
/// A component that is not a plain integer contributes `None`, which sorts below
/// every integer, so `0.2.27` outranks `0.2.27-rc1`; the raw name is the final
/// tiebreak so the result is a total order (a deterministic pick).
fn version_key(name: &str) -> (Vec<Option<u64>>, String) {
    let parts = name.split('.').map(|p| p.parse::<u64>().ok()).collect();
    (parts, name.to_string())
}

/// Pick the highest version directory name, by [`version_key`] order.
fn pick_highest(mut names: Vec<String>) -> Option<String> {
    names.sort_by_key(|n| version_key(n));
    names.pop()
}

/// Look for `<root>/<name>/<version>/bin/<name>`, newest version first.
///
/// `root` is a parameter so the enumeration — including both of its failure
/// arms — is testable without touching the real `$HOME`.
pub fn cache_lookup_in(root: &Path, name: &str) -> Determination<Option<PathBuf>> {
    let base = root.join(name);

    let dir = match std::fs::read_dir(&base) {
        Ok(d) => d,
        // No directory for this plugin ⇒ it was never rolled out here. An
        // observation, and the one case where the caller should try `$PATH`.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Determination::Known(None),
        // Permission denied, IO error, … ⇒ we did not get to look.
        Err(e) => {
            return Determination::undetermined(format!(
                "could not enumerate {}: {e}",
                base.display()
            ))
        }
    };

    let mut versions: Vec<String> = Vec::new();
    for entry in dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not read an entry of {}: {e}",
                    base.display()
                ))
            }
        };
        let candidate = entry.path().join("bin").join(name);
        // `exists()` folds "not there" and "cannot tell" into one `false`;
        // `try_exists()` keeps them apart.
        match candidate.try_exists() {
            Ok(true) => match entry.file_name().to_str() {
                Some(v) => versions.push(v.to_string()),
                None => {
                    return Determination::undetermined(format!(
                        "a version directory name under {} is not UTF-8",
                        base.display()
                    ))
                }
            },
            Ok(false) => {}
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not test {}: {e}",
                    candidate.display()
                ))
            }
        }
    }

    Determination::Known(pick_highest(versions).map(|v| base.join(v).join("bin").join(name)))
}

/// Is `name` spawnable through the ambient `$PATH`?
///
/// Spawn success is the whole test; the child's exit status is irrelevant (a
/// plugin whose `--version` exits non-zero is still present). `Err` here is
/// `ENOENT` in every practical case, i.e. "not on `$PATH`".
fn on_path(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .output()
        .is_ok()
}

/// Resolve a harness plugin's executable: plugin cache first, `$PATH` second.
///
/// See the module docs for why that order, and for what each of the three
/// answers means.
pub fn resolve(name: &str) -> Determination<Option<PathBuf>> {
    match cache_lookup_in(&cache_root(), name) {
        Determination::Known(Some(p)) => Determination::Known(Some(p)),
        // No cache copy — a standalone install on `$PATH` is still a real find.
        Determination::Known(None) => {
            if on_path(name) {
                Determination::Known(Some(PathBuf::from(name)))
            } else {
                Determination::Known(None)
            }
        }
        // We could not look in the cache. A `$PATH` hit here would be a binary of
        // unknown provenance chosen *because* the known-good copy was unreadable
        // — exactly the stale-shadow shape this module refuses. Stay undetermined.
        Determination::Undetermined(r) => Determination::Undetermined(r),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build `<root>/<name>/<v>/bin/<name>` for each `v`.
    fn plant(root: &Path, name: &str, versions: &[&str]) {
        for v in versions {
            let bin = root.join(name).join(v).join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join(name), b"#!/bin/sh\n").unwrap();
        }
    }

    fn expect_known(d: Determination<Option<PathBuf>>) -> Option<PathBuf> {
        match d {
            Determination::Known(v) => v,
            Determination::Undetermined(r) => panic!("expected Known, got undetermined: {r:?}"),
        }
    }

    #[test]
    fn ten_or_more_patches_in_a_series_still_picks_the_newest() {
        let tmp = tempfile::tempdir().unwrap();
        plant(tmp.path(), "hypothesis", &["0.1.9", "0.1.12"]);
        assert_eq!(
            expect_known(cache_lookup_in(tmp.path(), "hypothesis")),
            Some(tmp.path().join("hypothesis/0.1.12/bin/hypothesis")),
            "0.1.12 is newer than 0.1.9; a lexicographic sort picks 0.1.9"
        );
    }

    #[test]
    fn minor_series_compares_numerically_too() {
        let tmp = tempfile::tempdir().unwrap();
        plant(tmp.path(), "condukt", &["0.7.138", "0.10.0", "0.9.4"]);
        assert_eq!(
            expect_known(cache_lookup_in(tmp.path(), "condukt")),
            Some(tmp.path().join("condukt/0.10.0/bin/condukt"))
        );
    }

    /// Anti-vacuity control: the ordering fix must not break the ordinary case.
    #[test]
    fn single_version_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        plant(tmp.path(), "backlog", &["0.3.1"]);
        assert_eq!(
            expect_known(cache_lookup_in(tmp.path(), "backlog")),
            Some(tmp.path().join("backlog/0.3.1/bin/backlog"))
        );
    }

    #[test]
    fn a_missing_plugin_dir_is_known_absent_not_undetermined() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            expect_known(cache_lookup_in(tmp.path(), "nosuchplugin")),
            None
        );
    }

    /// A version dir with no `bin/<name>` inside is not a candidate — and its
    /// absence is an observation, not an unknown.
    #[test]
    fn a_version_dir_without_the_binary_is_not_a_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("backlog/0.3.1/skills")).unwrap();
        assert_eq!(expect_known(cache_lookup_in(tmp.path(), "backlog")), None);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_plugin_dir_is_undetermined_not_absent() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        plant(tmp.path(), "backlog", &["0.3.1"]);
        let base = tmp.path().join("backlog");
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o000)).unwrap();
        let got = cache_lookup_in(tmp.path(), "backlog");
        // Restore before asserting so the tempdir can always be cleaned up.
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755)).unwrap();
        match got {
            Determination::Undetermined(r) => {
                assert!(r.as_str().contains("could not enumerate"), "reason: {r:?}")
            }
            other => panic!("an unreadable cache dir must not read as absent: {other:?}"),
        }
    }

    #[test]
    fn version_key_orders_numerically_and_ranks_prerelease_below_release() {
        assert!(version_key("0.1.12") > version_key("0.1.9"));
        assert!(version_key("0.10.0") > version_key("0.9.4"));
        assert!(version_key("0.2.27") > version_key("0.2.27-rc1"));
    }
}
