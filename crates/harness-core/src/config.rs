//! Config primitives shared by every plugin: home/base-dir resolution, tilde
//! expansion, and env-var parsing. Each plugin defines its OWN `Config` struct
//! with its own fields and load/merge, composing these helpers — only the common
//! primitives live here.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Env var declaring a sandbox home: "this process tree must not touch the
/// account's real home". The repo's tracked `.cargo/config.toml` sets it via
/// `[env]` for every `cargo test`/`cargo run`, and cargo exports it to the test
/// binary and every child it spawns -- that is what keeps a test that inherits
/// the real `HOME` from writing the real `~/.overwatch`, `~/.harness`, ...
/// (backlog 099a5cbb). Production hooks are spawned by Claude Code, not cargo,
/// so they never see it. It grants nothing `HOME` itself does not already
/// grant: whoever can set it can set `HOME`.
pub const SANDBOX_HOME_ENV: &str = "HARNESS_SANDBOX_HOME";

/// What [`home`] returns when the home directory cannot be determined (unix
/// with `HOME` unset/empty and no sandbox). It sits under `/dev/null`, a
/// non-directory, so every create/write under it fails with `ENOTDIR` and
/// every read fails with an error that is NOT `NotFound` -- i.e. writers
/// surface an error and `boundary` readers resolve `Undetermined` instead of
/// "absent". It is never a real location, so nothing is silently written
/// "somewhere else" (CLAUDE.md §3). Callers that turn every IO error into a
/// default are not made safe by this; that erasure lives at their call site.
pub const UNDETERMINED_HOME: &str = "/dev/null/HOME-unset.harness-state-dir-undetermined";

fn non_empty(v: Option<&OsStr>) -> Option<&OsStr> {
    v.filter(|s| !s.is_empty())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// Pure home resolution (the env/passwd reads live in [`home`]).
///
/// * `home_env` -- `$HOME`; `sandbox_env` -- `$HARNESS_SANDBOX_HOME`
///   ([`SANDBOX_HOME_ENV`]); empty values count as unset.
/// * `account_home` -- the OS account's home from the passwd database
///   (`getpwuid_r`), deliberately NOT derived from `$HOME`.
///
/// Rules:
/// 1. Sandbox declared: return the sandbox, unless `HOME` is set to a
///    directory provably different from the account home (an explicit test
///    `HOME` such as a tempdir keeps working). `HOME` equal to the account
///    home (path-equal or same after symlink resolution), `HOME` unset, or the
///    account home undeterminable (cannot prove `HOME` is not the real home)
///    all resolve to the sandbox -- the restrictive side.
/// 2. No sandbox, `HOME` set: `HOME`.
/// 3. No sandbox, `HOME` unset: on unix, [`UNDETERMINED_HOME`]. A unix process
///    without `HOME` has had its environment scrubbed (`env -i`,
///    `Command::env_clear`), and guessing the passwd home is what made
///    `env_clear` test children write the real `~/.overwatch`. On non-unix,
///    where `HOME` is normally absent, `account_home` (`dirs::home_dir()`) is
///    the authoritative source; [`UNDETERMINED_HOME`] if even that is absent.
pub fn resolve_home(
    home_env: Option<&OsStr>,
    sandbox_env: Option<&OsStr>,
    account_home: Option<&Path>,
) -> PathBuf {
    let home = non_empty(home_env).map(PathBuf::from);
    if let Some(sandbox) = non_empty(sandbox_env) {
        return match (home, account_home) {
            (Some(h), Some(acct)) if !same_dir(&h, acct) => h,
            _ => PathBuf::from(sandbox),
        };
    }
    if let Some(h) = home {
        return h;
    }
    if cfg!(unix) {
        PathBuf::from(UNDETERMINED_HOME)
    } else {
        account_home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(UNDETERMINED_HOME))
    }
}

/// The OS account's home directory from the passwd database, independent of
/// `$HOME`. `None` when the lookup fails.
#[cfg(unix)]
fn account_home() -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let mut buf: Vec<libc::c_char> = vec![0; 4096];
    loop {
        // SAFETY: `passwd` is plain C data, valid when zeroed; getpwuid_r
        // writes only into `pwd` and `buf` (whose length we pass), and on
        // success `pw_dir` points into `buf`, which outlives its use below.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE && buf.len() < (1 << 20) {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        // SAFETY: non-null and NUL-terminated inside `buf` per getpwuid_r.
        let dir = unsafe { CStr::from_ptr(pwd.pw_dir) }.to_bytes();
        if dir.is_empty() {
            return None;
        }
        return Some(PathBuf::from(OsStr::from_bytes(dir)));
    }
}

#[cfg(not(unix))]
fn account_home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Resolves the user's home directory for every plugin's state/config, per
/// [`resolve_home`] fed from `$HOME`, `$HARNESS_SANDBOX_HOME` and the passwd
/// database. The account home is only looked up when a sandbox is declared
/// (or, off unix, when `HOME` is unset).
pub fn home() -> PathBuf {
    let home_env = std::env::var_os("HOME");
    let sandbox_env = std::env::var_os(SANDBOX_HOME_ENV);
    let need_account = non_empty(sandbox_env.as_deref()).is_some()
        || (!cfg!(unix) && non_empty(home_env.as_deref()).is_none());
    let acct = if need_account { account_home() } else { None };
    resolve_home(home_env.as_deref(), sandbox_env.as_deref(), acct.as_deref())
}

/// The `~/.<plugin>` base directory for a plugin's config/store/state.
pub fn base_dir(plugin: &str) -> PathBuf {
    home().join(format!(".{plugin}"))
}

/// Expand a leading `~` to the home directory.
pub fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        home().join(rest)
    } else if s == "~" {
        home()
    } else {
        PathBuf::from(s)
    }
}

/// Parse a `u64` env var, or None when unset/empty/unparseable.
pub fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.trim().parse::<u64>().ok()
}

/// Parse a boolean-ish env var: `0`/`false`/`no`/`off`/empty → false, else true.
pub fn env_bool(key: &str) -> Option<bool> {
    let v = std::env::var(key).ok()?;
    let v = v.trim().to_ascii_lowercase();
    Some(!matches!(v.as_str(), "" | "0" | "false" | "no" | "off"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // home() reads the process-wide HOME env var. The guard is
    // crate::test_env::lock() — crate-wide, not module-private: a lock here
    // would serialize only config's own cases, and HOME is read by trust too.

    #[test]
    fn base_dir_is_dotprefixed_under_home() {
        // Readers need the guard too, not just mutators: both sides of this
        // assertion resolve HOME independently (once inside base_dir, once in
        // home()), so a concurrent set_var between them fails a test that is
        // otherwise a tautology.
        let _guard = crate::test_env::lock();
        assert_eq!(base_dir("ctxrot"), home().join(".ctxrot"));
    }

    #[test]
    fn home_prefers_home_env_var_and_never_guesses_when_unset() {
        // The env-reading wrapper over resolve_home (whose contract is pinned
        // by tests/home_resolution.rs). Contract change (backlog 099a5cbb):
        // an empty/unset HOME used to fall back to dirs::home_dir(), i.e. the
        // REAL home, which is how env_clear'd test children polluted the live
        // ~/.overwatch. It now resolves to the sandbox when one is declared,
        // else to UNDETERMINED_HOME -- never to the passwd home.
        let _guard = crate::test_env::lock();
        let prev = std::env::var_os("HOME");
        let prev_sb = std::env::var_os(SANDBOX_HOME_ENV);

        std::env::set_var("HOME", "/tmp/harness-core-test-home");
        std::env::remove_var(SANDBOX_HOME_ENV);
        assert_eq!(home(), PathBuf::from("/tmp/harness-core-test-home"));

        std::env::set_var("HOME", "");
        assert_eq!(home(), PathBuf::from(UNDETERMINED_HOME));
        assert_ne!(home(), dirs::home_dir().unwrap_or_default());

        std::env::set_var(SANDBOX_HOME_ENV, "/tmp/harness-core-test-sandbox");
        assert_eq!(home(), PathBuf::from("/tmp/harness-core-test-sandbox"));

        // An explicit HOME distinct from the account home keeps winning.
        std::env::set_var("HOME", "/tmp/harness-core-test-home");
        assert_eq!(home(), PathBuf::from("/tmp/harness-core-test-home"));

        // HOME equal to the real account home is redirected to the sandbox.
        if let Some(acct) = account_home() {
            std::env::set_var("HOME", &acct);
            assert_eq!(home(), PathBuf::from("/tmp/harness-core-test-sandbox"));
        }

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_sb {
            Some(v) => std::env::set_var(SANDBOX_HOME_ENV, v),
            None => std::env::remove_var(SANDBOX_HOME_ENV),
        }
    }

    #[test]
    fn expand_tilde_handles_home_forms() {
        // Same reason as base_dir_is_dotprefixed_under_home: two independent
        // HOME resolutions per assertion.
        let _guard = crate::test_env::lock();
        assert_eq!(expand_tilde("~"), home());
        assert_eq!(expand_tilde("~/store"), home().join("store"));
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn env_parsers_are_lenient() {
        // Distinct keys are not a reason to skip the guard. `setenv` may
        // reallocate the whole `environ` block, so a write here can corrupt a
        // concurrent `getenv` for an *unrelated* key -- which is exactly what
        // the guarded HOME tests are doing. (That hazard is why edition 2024
        // makes `std::env::set_var` unsafe.) The invariant this lock buys is
        // "every env mutation in this crate's tests holds it"; one exemption
        // makes the invariant unenforceable and ungreppable.
        let _guard = crate::test_env::lock();
        std::env::set_var("HARNESS_TEST_U64", " 42 ");
        std::env::set_var("HARNESS_TEST_BOOL", "off");
        assert_eq!(env_u64("HARNESS_TEST_U64"), Some(42));
        assert_eq!(env_bool("HARNESS_TEST_BOOL"), Some(false));
        assert_eq!(env_u64("HARNESS_TEST_UNSET_XYZ"), None);
        std::env::remove_var("HARNESS_TEST_U64");
        std::env::remove_var("HARNESS_TEST_BOOL");
    }
}
