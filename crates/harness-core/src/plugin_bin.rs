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
//! Deliberately the reverse of the ad-hoc resolvers this replaced: autoflow's
//! `find_backlog_binary` used to probe `$PATH` first (with a lexical version
//! sort), as did ctxrot's and stuckguard's overwatch lookups. Since `43780aa1`
//! all three delegate to [`resolve`]; none probes `$PATH` first any more.
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
//!   but could not be enumerated, an entry could not be read or stat'ed, a
//!   candidate's existence could not be tested, or (no cache copy) a `$PATH`
//!   entry of that name exists but could not be spawned. Collapsing this into `Known(None)` is the
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
///
/// # How each entry of `<root>/<name>/` is classified
///
/// The plugin cache dir does not hold only version dirs: `rollout-plugins.sh`
/// also writes a plain file `.version-history.jsonl` there. Each entry is
/// classified by `std::fs::metadata` on its path — which **follows symlinks**,
/// so a symlink to a version dir is judged as the dir it points at — before
/// any `bin/<name>` probe:
///
/// - **directory** (or a symlink to one) → a version-dir candidate; probe
///   `<entry>/bin/<name>` with `try_exists`. `Ok(true)` keeps it, `Ok(false)`
///   drops it (observed: no binary there), `Err` (e.g. `EACCES` inside the
///   version dir) makes the whole lookup `Undetermined`.
/// - **not a directory** (a regular file such as `.version-history.jsonl`, a
///   symlink to a file, …) → observed to be unable to hold `bin/<name>`, so it
///   is not a candidate and is skipped. No probe is made.
/// - **metadata `NotFound`** (a dangling symlink, or an entry removed between
///   `read_dir` and `metadata`) → there is nothing there to hold a binary; not
///   a candidate, skipped.
/// - **any other metadata error** (`EACCES`, IO error, …) → we could not tell
///   what the entry is ⇒ `Undetermined`.
///
/// The skip decisions come only from that metadata classification; an error
/// from the `bin/<name>` probe itself is never reinterpreted as "absent".
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
        let entry_path = entry.path();
        // Follows symlinks on purpose (NOT `entry.file_type()`), so a symlinked
        // version dir is still a candidate. See the doc comment's table.
        match std::fs::metadata(&entry_path) {
            Ok(meta) if meta.is_dir() => {}
            // A plain file (e.g. rollout's `.version-history.jsonl`) or a
            // symlink to one: cannot contain `bin/<name>`. Observation, skip.
            Ok(_) => continue,
            // Dangling symlink / vanished entry: nothing there. Observation, skip.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not stat {}: {e}",
                    entry_path.display()
                ))
            }
        }
        let candidate = entry_path.join("bin").join(name);
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

/// Upper bound on how long [`on_path`] waits for `<name> --version` to exit.
///
/// [`resolve`] is called from hooks and inside lock-held critical sections, so
/// the probe must never block for as long as the child chooses to run.
const PATH_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Is `name` spawnable through the ambient `$PATH`?
///
/// Spawn success is the whole test; the child's exit status is irrelevant (a
/// plugin whose `--version` exits non-zero is still present). The child gets
/// null stdin/stdout/stderr and is waited on for at most `timeout`:
///
/// - spawn `Ok` → `Known(true)`, whether the child exits in time or not. On
///   timeout (or a `try_wait` error) the child is killed and then reaped with
///   `wait`, so no zombie is left behind. A slow `--version` does not make an
///   existing binary absent.
/// - spawn `Err(NotFound)` → `Known(false)`: nothing named `name` on `$PATH`.
/// - any other spawn error (e.g. `PermissionDenied` for a non-executable file
///   of that name on `$PATH`) → `Undetermined`: something is there but we could
///   not run it, which is neither "installed" nor an observed absence.
fn on_path(name: &str, timeout: std::time::Duration) -> Determination<bool> {
    use std::process::{Command, Stdio};
    let mut child = match Command::new(name)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Determination::Known(false),
        Err(e) => {
            return Determination::undetermined(format!("could not spawn {name} from $PATH: {e}"))
        }
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Determination::Known(true),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            // Timed out, or try_wait failed: the spawn already proved presence.
            // Kill and reap so the probe never outlives this call.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Determination::Known(true);
            }
        }
    }
}

/// Resolve a harness plugin's executable: plugin cache first, `$PATH` second.
///
/// See the module docs for why that order, and for what each of the three
/// answers means.
pub fn resolve(name: &str) -> Determination<Option<PathBuf>> {
    match cache_lookup_in(&cache_root(), name) {
        Determination::Known(Some(p)) => Determination::Known(Some(p)),
        // No cache copy — a standalone install on `$PATH` is still a real find.
        Determination::Known(None) => match on_path(name, PATH_PROBE_TIMEOUT) {
            Determination::Known(true) => Determination::Known(Some(PathBuf::from(name))),
            Determination::Known(false) => Determination::Known(None),
            Determination::Undetermined(r) => Determination::Undetermined(r),
        },
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

    /// Write an executable shell script at `<dir>/<file>` and return its path.
    #[cfg(unix)]
    fn script(dir: &Path, file: &str, body: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(file);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn on_path_is_bounded_and_counts_a_hung_binary_as_present() {
        let tmp = tempfile::tempdir().unwrap();
        let p = script(tmp.path(), "hang", "#!/bin/sh\nsleep 30\n", 0o755);
        let start = std::time::Instant::now();
        let got = on_path(p.to_str().unwrap(), std::time::Duration::from_millis(200));
        let took = start.elapsed();
        assert!(
            took < std::time::Duration::from_secs(5),
            "probe must be bounded, took {took:?}"
        );
        assert!(matches!(got, Determination::Known(true)), "got {got:?}");
    }

    #[cfg(unix)]
    #[test]
    fn on_path_present_even_if_version_exits_nonzero() {
        let tmp = tempfile::tempdir().unwrap();
        let p = script(tmp.path(), "bad", "#!/bin/sh\nexit 7\n", 0o755);
        let got = on_path(p.to_str().unwrap(), PATH_PROBE_TIMEOUT);
        assert!(matches!(got, Determination::Known(true)), "got {got:?}");
    }

    #[test]
    fn on_path_missing_binary_is_known_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("no-such-binary");
        let got = on_path(p.to_str().unwrap(), PATH_PROBE_TIMEOUT);
        assert!(matches!(got, Determination::Known(false)), "got {got:?}");
    }

    #[cfg(unix)]
    #[test]
    fn on_path_unspawnable_existing_file_is_undetermined() {
        let tmp = tempfile::tempdir().unwrap();
        let p = script(tmp.path(), "noexec", "#!/bin/sh\n", 0o644);
        let got = on_path(p.to_str().unwrap(), PATH_PROBE_TIMEOUT);
        assert!(
            matches!(got, Determination::Undetermined(_)),
            "a non-executable file is neither installed nor absent: {got:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_entry_is_skipped_not_undetermined() {
        let tmp = tempfile::tempdir().unwrap();
        plant(tmp.path(), "backlog", &["0.3.1"]);
        std::os::unix::fs::symlink(
            tmp.path().join("gone"),
            tmp.path().join("backlog").join("0.9.9"),
        )
        .unwrap();
        assert_eq!(
            expect_known(cache_lookup_in(tmp.path(), "backlog")),
            Some(tmp.path().join("backlog/0.3.1/bin/backlog"))
        );
    }

    #[test]
    fn version_key_orders_numerically_and_ranks_prerelease_below_release() {
        assert!(version_key("0.1.12") > version_key("0.1.9"));
        assert!(version_key("0.10.0") > version_key("0.9.4"));
        assert!(version_key("0.2.27") > version_key("0.2.27-rc1"));
    }
}
