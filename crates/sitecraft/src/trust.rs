//! Content-pinned trust for `<root>/sitecraft.toml` (the webqa scheme).
//!
//! `sitecraft.toml` declares a `command` that `serve start` (and the Stop gate)
//! spawns. A config the user never approved must not run. `sitecraft trust`
//! records the sha256 of the config bytes, keyed by the canonicalized project
//! root, in `<state>/trust.json` (written to a temp file, then renamed). The
//! command runs only when the current bytes hash to the recorded value.
//!
//! Three answers, not two ([`Trust`]): `Trusted`, `Untrusted` (no record, or
//! the hash changed), `Undetermined` (the store or the config cannot be read
//! or parsed). Both non-`Trusted` answers block; no error maps to `Trusted`.

use std::path::Path;

/// File name of the trust store inside the state dir.
pub const STORE: &str = "trust.json";

/// Result of a trust check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// The bytes hash to the value recorded for this root.
    Trusted,
    /// No record for this root, or the content changed. Carries a message that
    /// tells the user to review the file and run `sitecraft trust`.
    Untrusted(String),
    /// The store or the config could not be read/parsed. Carries why.
    Undetermined(String),
}

/// Record trust for the current bytes of `config` under `root` in
/// `<state>/trust.json`; returns the recorded sha256 hex.
///
/// Contract: creates `state` if needed; a missing store is created; a store
/// that exists but cannot be read or parsed as JSON is **not overwritten** and
/// yields `Err` (it may hold other roots' records); an unreadable `config` or
/// uncanonicalizable `root` yields `Err`.
pub fn record(state: &Path, root: &Path, config: &Path) -> Result<String, String> {
    Err(format!(
        "sitecraft trust: not implemented yet (state {}, root {}, config {})",
        state.display(),
        root.display(),
        config.display()
    ))
}

/// Are `bytes` (the content of `config`, as already read by the caller) the
/// content recorded for `root` in `<state>/trust.json`?
///
/// Contract: no record → `Untrusted`; recorded hash differs → `Untrusted`;
/// equal → `Trusted`; store unreadable / not JSON, or `root` cannot be
/// canonicalized → `Undetermined`.
#[must_use]
pub fn check_bytes(state: &Path, root: &Path, config: &Path, bytes: &[u8]) -> Trust {
    let _ = (state, root, bytes);
    Trust::Undetermined(format!(
        "trust check for {} is not implemented yet",
        config.display()
    ))
}
