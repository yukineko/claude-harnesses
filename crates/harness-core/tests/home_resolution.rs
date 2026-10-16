#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Contract tests for `harness_core::config::resolve_home`.
//!
//! Written by an independent test writer (CLAUDE.md §2(a)) against the
//! contract, not against the implementation. `resolve_home` is pure, so no
//! process env var is mutated here: every input is passed directly.
#![cfg(unix)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use harness_core::config::{resolve_home, UNDETERMINED_HOME};

const ACCOUNT: &str = "/Users/account-home-x";

fn os(s: &str) -> Option<&OsStr> {
    Some(OsStr::new(s))
}

fn undetermined() -> PathBuf {
    PathBuf::from(UNDETERMINED_HOME)
}

// ---- Clause 1 -------------------------------------------------------------

#[test]
fn c1_no_sandbox_home_set_returns_home_verbatim() {
    let got = resolve_home(os("/tmp/some-test-home"), None, Some(Path::new(ACCOUNT)));
    assert_eq!(
        got,
        PathBuf::from("/tmp/some-test-home"),
        "clause 1: without a sandbox, a non-empty HOME is returned verbatim"
    );
}

#[test]
fn c1_no_sandbox_home_equal_to_account_home_returns_home() {
    let got = resolve_home(os(ACCOUNT), None, Some(Path::new(ACCOUNT)));
    assert_eq!(
        got,
        PathBuf::from(ACCOUNT),
        "clause 1: without a sandbox, HOME is returned verbatim even when it equals the account home"
    );
}

// ---- Clause 2 -------------------------------------------------------------

#[test]
fn c2_no_sandbox_home_unset_is_undetermined_not_account_home() {
    let got = resolve_home(None, None, Some(Path::new(ACCOUNT)));
    assert_eq!(
        got,
        undetermined(),
        "clause 2: HOME unset without sandbox must resolve to UNDETERMINED_HOME, \
         never to the passwd account home (silent redirection is forbidden)"
    );
    assert_ne!(
        got,
        PathBuf::from("."),
        "clause 2: must never resolve to '.'"
    );
}

#[test]
fn c2_no_sandbox_home_empty_is_undetermined_not_account_home() {
    let got = resolve_home(os(""), None, Some(Path::new(ACCOUNT)));
    assert_eq!(
        got,
        undetermined(),
        "clause 2: HOME=\"\" is treated as unset and must resolve to UNDETERMINED_HOME, \
         never to the passwd account home"
    );
    assert_ne!(
        got,
        PathBuf::from("."),
        "clause 2: must never resolve to '.'"
    );
}

#[test]
fn c2_no_sandbox_home_unset_no_account_is_undetermined() {
    let got = resolve_home(None, None, None);
    assert_eq!(
        got,
        undetermined(),
        "clause 2: HOME unset and no account home must resolve to UNDETERMINED_HOME, not '.'"
    );
}

#[test]
fn c2_no_sandbox_home_empty_no_account_is_undetermined() {
    let got = resolve_home(os(""), None, None);
    assert_eq!(
        got,
        undetermined(),
        "clause 2: HOME=\"\" and no account home must resolve to UNDETERMINED_HOME, not '.'"
    );
}

// ---- Clause 3 -------------------------------------------------------------

#[test]
fn c3_sandbox_home_unset_returns_sandbox() {
    let got = resolve_home(None, os("/tmp/sandbox-home"), Some(Path::new(ACCOUNT)));
    assert_eq!(
        got,
        PathBuf::from("/tmp/sandbox-home"),
        "clause 3: with a sandbox and HOME unset, the sandbox path is returned"
    );
}

#[test]
fn c3_sandbox_home_empty_returns_sandbox() {
    let got = resolve_home(os(""), os("/tmp/sandbox-home"), Some(Path::new(ACCOUNT)));
    assert_eq!(
        got,
        PathBuf::from("/tmp/sandbox-home"),
        "clause 3: with a sandbox and HOME=\"\", the sandbox path is returned"
    );
}

// ---- Clause 4 -------------------------------------------------------------

#[test]
fn c4_sandbox_home_equals_account_home_returns_sandbox() {
    let got = resolve_home(
        os(ACCOUNT),
        os("/tmp/sandbox-home"),
        Some(Path::new(ACCOUNT)),
    );
    assert_eq!(
        got,
        PathBuf::from("/tmp/sandbox-home"),
        "clause 4: inside a sandbox, HOME equal to the real account home is off-limits; \
         the sandbox path must be returned"
    );
}

#[test]
fn c4_sandbox_home_equals_account_home_modulo_trailing_slash_returns_sandbox() {
    let home_with_slash = format!("{ACCOUNT}/");
    let got = resolve_home(
        os(&home_with_slash),
        os("/tmp/sandbox-home"),
        Some(Path::new(ACCOUNT)),
    );
    assert_eq!(
        got,
        PathBuf::from("/tmp/sandbox-home"),
        "clause 4: HOME equal to the account home modulo a trailing slash is still the real home; \
         sandbox must be returned"
    );
}

#[test]
fn c4_sandbox_account_home_trailing_slash_returns_sandbox() {
    let account_with_slash = format!("{ACCOUNT}/");
    let got = resolve_home(
        os(ACCOUNT),
        os("/tmp/sandbox-home"),
        Some(Path::new(&account_with_slash)),
    );
    assert_eq!(
        got,
        PathBuf::from("/tmp/sandbox-home"),
        "clause 4: account home with a trailing slash vs HOME without one is the same directory; \
         sandbox must be returned"
    );
}

// ---- Clause 5 -------------------------------------------------------------

#[test]
fn c5_sandbox_home_is_symlink_to_account_home_returns_sandbox() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real-home");
    std::fs::create_dir(&real).unwrap();
    let link = tmp.path().join("link-home");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let got = resolve_home(
        Some(link.as_os_str()),
        os("/tmp/sandbox-home"),
        Some(real.as_path()),
    );
    assert_eq!(
        got,
        PathBuf::from("/tmp/sandbox-home"),
        "clause 5: HOME that resolves (via symlink) to the account home is the real home; \
         sandbox must be returned"
    );
}

// ---- Clause 6 -------------------------------------------------------------

#[test]
fn c6_sandbox_home_is_distinct_test_dir_returns_home() {
    let tmp = tempfile::tempdir().unwrap();
    let got = resolve_home(
        Some(tmp.path().as_os_str()),
        os("/tmp/sandbox-home"),
        Some(Path::new(ACCOUNT)),
    );
    assert_eq!(
        got,
        tmp.path().to_path_buf(),
        "clause 6: an explicit HOME different from the account home is honored even in a sandbox"
    );
}

#[test]
fn c6_sandbox_home_is_distinct_nonexistent_dir_returns_home() {
    let got = resolve_home(
        os("/tmp/other-explicit-home"),
        os("/tmp/sandbox-home"),
        Some(Path::new(ACCOUNT)),
    );
    assert_eq!(
        got,
        PathBuf::from("/tmp/other-explicit-home"),
        "clause 6: an explicit HOME different from the account home is honored even in a sandbox"
    );
}

// ---- Clause 7 -------------------------------------------------------------

#[test]
fn c7_sandbox_home_set_account_unknown_returns_sandbox() {
    let got = resolve_home(os("/tmp/some-home"), os("/tmp/sandbox-home"), None);
    assert_eq!(
        got,
        PathBuf::from("/tmp/sandbox-home"),
        "clause 7: when the passwd lookup failed it cannot be proven HOME is not the real home; \
         inside a sandbox this resolves to the restrictive side (sandbox)"
    );
}

// ---- Clause 8 -------------------------------------------------------------

#[test]
fn c8_empty_sandbox_with_home_set_behaves_as_no_sandbox() {
    let got = resolve_home(os(ACCOUNT), os(""), Some(Path::new(ACCOUNT)));
    assert_eq!(
        got,
        PathBuf::from(ACCOUNT),
        "clause 8: HARNESS_SANDBOX_HOME=\"\" is no sandbox, so HOME is returned verbatim"
    );
}

#[test]
fn c8_empty_sandbox_with_home_unset_is_undetermined() {
    let got = resolve_home(None, os(""), Some(Path::new(ACCOUNT)));
    assert_eq!(
        got,
        undetermined(),
        "clause 8: HARNESS_SANDBOX_HOME=\"\" is no sandbox, so HOME unset resolves to UNDETERMINED_HOME"
    );
}

#[test]
fn c8_empty_sandbox_with_home_empty_is_undetermined() {
    let got = resolve_home(os(""), os(""), Some(Path::new(ACCOUNT)));
    assert_eq!(
        got,
        undetermined(),
        "clause 8: empty sandbox and empty HOME resolve to UNDETERMINED_HOME"
    );
}

// ---- Property ---------------------------------------------------------------

/// For every combination of inputs, the result is the account home only when
/// HOME was explicitly that value and no (non-empty) sandbox is set.
#[test]
fn property_account_home_only_when_explicit_and_unsandboxed() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real-home");
    std::fs::create_dir(&real).unwrap();
    let link = tmp.path().join("link-home");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let other = tmp.path().join("other-home");
    std::fs::create_dir(&other).unwrap();
    let real_slash = format!("{}/", real.display());

    let real_s = real.to_str().unwrap().to_string();
    let link_s = link.to_str().unwrap().to_string();
    let other_s = other.to_str().unwrap().to_string();

    let homes: Vec<Option<&str>> = vec![
        None,
        Some(""),
        Some(&real_s),
        Some(&real_slash),
        Some(&link_s),
        Some(&other_s),
    ];
    let sandboxes: Vec<Option<&str>> = vec![None, Some(""), Some("/tmp/sandbox-home")];
    let accounts: Vec<Option<&Path>> = vec![None, Some(real.as_path())];

    for &h in &homes {
        for &s in &sandboxes {
            for &a in &accounts {
                let got = resolve_home(h.map(OsStr::new), s.map(OsStr::new), a);
                let sandboxed = matches!(s, Some(v) if !v.is_empty());
                let home_explicit_real = h.map(PathBuf::from) == Some(got.clone());
                let is_account = got == real || got == Path::new(&real_slash);
                if is_account {
                    assert!(
                        home_explicit_real && !sandboxed,
                        "property: result {got:?} is the account home but HOME={h:?}, \
                         sandbox={s:?}, account={a:?}; the account home may only be returned \
                         when HOME is explicitly that value and no sandbox is set"
                    );
                }
                assert_ne!(
                    got,
                    PathBuf::from("."),
                    "property: never '.' (HOME={h:?}, sandbox={s:?}, account={a:?})"
                );
            }
        }
    }
}
