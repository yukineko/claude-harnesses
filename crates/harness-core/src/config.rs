//! Config primitives shared by every plugin: home/base-dir resolution, tilde
//! expansion, and env-var parsing. Each plugin defines its OWN `Config` struct
//! with its own fields and load/merge, composing these helpers — only the common
//! primitives live here.

use std::path::PathBuf;

/// Resolves the user's home directory. Prefers the `HOME` env var (when set
/// and non-empty) over `dirs::home_dir()`. On Unix `HOME` is already the
/// authoritative source, so this is a no-op there; on Windows,
/// `dirs::home_dir()` resolves via `USERPROFILE`/`SHGetKnownFolderPath` and
/// ignores `HOME`, which breaks the repo's `HOME`-swap test-isolation pattern
/// (`std::env::set_var("HOME", tmpdir)` for state isolation in tests).
pub fn home() -> PathBuf {
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
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
    use std::sync::Mutex;

    // home() reads the process-wide HOME env var, so tests that mutate it must
    // not run concurrently with each other (mirrors beacon::config's precedent).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn base_dir_is_dotprefixed_under_home() {
        assert_eq!(base_dir("ctxrot"), home().join(".ctxrot"));
    }

    #[test]
    fn home_prefers_home_env_var_over_dirs_home_dir() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("HOME");

        std::env::set_var("HOME", "/tmp/harness-core-test-home");
        assert_eq!(home(), PathBuf::from("/tmp/harness-core-test-home"));

        std::env::set_var("HOME", "");
        assert_eq!(
            home(),
            dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
        );

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn expand_tilde_handles_home_forms() {
        assert_eq!(expand_tilde("~"), home());
        assert_eq!(expand_tilde("~/store"), home().join("store"));
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn env_parsers_are_lenient() {
        std::env::set_var("HARNESS_TEST_U64", " 42 ");
        std::env::set_var("HARNESS_TEST_BOOL", "off");
        assert_eq!(env_u64("HARNESS_TEST_U64"), Some(42));
        assert_eq!(env_bool("HARNESS_TEST_BOOL"), Some(false));
        assert_eq!(env_u64("HARNESS_TEST_UNSET_XYZ"), None);
        std::env::remove_var("HARNESS_TEST_U64");
        std::env::remove_var("HARNESS_TEST_BOOL");
    }
}
