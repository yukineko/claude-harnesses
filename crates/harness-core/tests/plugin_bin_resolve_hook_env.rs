// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog abba6f0d — independent behavioural tests of
//! `harness_core::plugin_bin::resolve` through its PUBLIC surface, with `HOME`
//! pointing at a temp plugin cache and `PATH` replaced, the way a hook process
//! sees the world (no plugin `bin/` dirs on `PATH`).
//!
//! Each case was observed RED against the code before the change it pins
//! (`82ee4a78`, i.e. before `ab7ce91d`) and GREEN at the change:
//!
//! - a plugin cache dir holding only rollout's `.version-history.jsonl`, with
//!   nothing on `PATH`, is `Known(None)` (pre: `Undetermined`, ENOTDIR).
//! - a non-executable file of that name on `PATH` (no cache copy) is
//!   `Undetermined` (pre: `Known(None)` — "something is there but cannot be run"
//!   was read as "not installed").
//! - the `PATH` probe of a hanging binary is bounded and leaves no zombie child
//!   (pre: blocked for as long as the child ran).
//!
//! `resolve` reads the process-global `HOME`/`PATH`, so every test holds
//! `ENV_LOCK` while it has them repointed.
#![cfg(unix)]

use harness_core::plugin_bin::resolve;
use harness_core::verdict::Determination;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Repoints `HOME` and `PATH` for the guard's lifetime, restoring both on drop
/// (even if an assertion panics).
struct EnvGuard {
    home: Option<OsString>,
    path: Option<OsString>,
}

impl EnvGuard {
    fn set(home: &Path, path: &Path) -> Self {
        let g = EnvGuard {
            home: std::env::var_os("HOME"),
            path: std::env::var_os("PATH"),
        };
        std::env::set_var("HOME", home);
        std::env::set_var("PATH", path);
        g
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.home.take() {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match self.path.take() {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
    }
}

fn cache_dir(home: &Path, name: &str) -> PathBuf {
    home.join(".claude/plugins/cache/yukineko").join(name)
}

fn write_mode(path: &Path, body: &str, mode: u32) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn history_file_only_cache_dir_and_path_miss_is_known_absent() {
    let _l = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let empty_path = tmp.path().join("empty-path");
    std::fs::create_dir_all(&empty_path).unwrap();
    let name = "vabba-history-only";
    let dir = cache_dir(&home, name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(".version-history.jsonl"),
        b"{\"version\":\"0.1.0\"}\n",
    )
    .unwrap();

    let got = {
        let _g = EnvGuard::set(&home, &empty_path);
        resolve(name)
    };
    assert_eq!(
        got,
        Determination::Known(None),
        "rollout's ledger file is not a version dir and there is no binary on PATH: \
         this is an observed absence, not a failure to look"
    );
}

#[test]
fn non_executable_file_on_path_without_cache_copy_is_undetermined() {
    let _l = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let path_dir = tmp.path().join("path");
    std::fs::create_dir_all(&path_dir).unwrap();
    let name = "vabba-noexec";
    write_mode(&path_dir.join(name), "#!/bin/sh\nexit 0\n", 0o644);

    let got = {
        let _g = EnvGuard::set(&home, &path_dir);
        resolve(name)
    };
    match got {
        Determination::Undetermined(_) => {}
        Determination::Known(v) => panic!(
            "a file named {name} sits on PATH but cannot be spawned: that is neither \
             installed nor an observed absence, got Known({v:?})"
        ),
    }
}

/// Zombie children (state `Z`) of this test process, by pid.
fn zombie_children() -> Vec<String> {
    let me = std::process::id().to_string();
    let out = std::process::Command::new("/bin/ps")
        .args(["-A", "-o", "pid=,ppid=,stat="])
        .output()
        .expect("ps runs");
    assert!(out.status.success(), "ps failed: {out:?}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 3 && f[1] == me && f[2].starts_with('Z')).then(|| f[0].to_string())
        })
        .collect()
}

#[test]
fn hanging_binary_on_path_is_found_within_the_bound_and_reaped() {
    let _l = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let path_dir = tmp.path().join("path");
    std::fs::create_dir_all(&path_dir).unwrap();
    let name = "vabba-hang";
    // `exec` so the killed pid IS the sleeping process (no orphaned grandchild).
    write_mode(
        &path_dir.join(name),
        "#!/bin/sh\nexec /bin/sleep 30\n",
        0o755,
    );

    let before = zombie_children();
    let start = Instant::now();
    let got = {
        let _g = EnvGuard::set(&home, &path_dir);
        resolve(name)
    };
    let took = start.elapsed();

    assert!(
        took < Duration::from_secs(10),
        "the PATH probe must be bounded (3s), took {took:?}"
    );
    assert_eq!(
        got,
        Determination::Known(Some(PathBuf::from(name))),
        "a binary that spawned but did not answer --version in time is still present"
    );
    let after: Vec<String> = zombie_children()
        .into_iter()
        .filter(|p| !before.contains(p))
        .collect();
    assert!(
        after.is_empty(),
        "the timed-out probe child must be killed AND reaped; zombies left: {after:?}"
    );
}
