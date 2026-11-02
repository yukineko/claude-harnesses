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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

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

fn store_path(state: &Path) -> PathBuf {
    state.join(STORE)
}

fn key(root: &Path) -> Result<String, String> {
    std::fs::canonicalize(root)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| format!("cannot canonicalize {}: {e}", root.display()))
}

fn hash_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// An empty map only when the store file does not exist (NotFound); any
/// other read or parse failure is `Err`.
fn read_store(state: &Path) -> Result<BTreeMap<String, String>, String> {
    let p = store_path(state);
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("cannot read trust store {}: {e}", p.display())),
    };
    serde_json::from_str(&text)
        .map_err(|e| format!("cannot parse trust store {}: {e}", p.display()))
}

/// Record trust for the current bytes of `config` under `root` in
/// `<state>/trust.json`; returns the recorded sha256 hex.
///
/// Contract: creates `state` if needed; a missing store is created; a store
/// that exists but cannot be read or parsed as JSON is **not overwritten** and
/// yields `Err` (it may hold other roots' records); an unreadable `config` or
/// uncanonicalizable `root` yields `Err`.
pub fn record(state: &Path, root: &Path, config: &Path) -> Result<String, String> {
    let k = key(root)?;
    let bytes =
        std::fs::read(config).map_err(|e| format!("cannot read {}: {e}", config.display()))?;
    let h = hash_bytes(&bytes);
    let mut m = read_store(state)?;
    m.insert(k, h.clone());
    std::fs::create_dir_all(state)
        .map_err(|e| format!("cannot create {}: {e}", state.display()))?;
    let text =
        serde_json::to_string_pretty(&m).map_err(|e| format!("cannot encode trust store: {e}"))?;
    let p = store_path(state);
    let tmp = state.join(format!("{STORE}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &p) {
        // Best-effort cleanup of our own tmp file; the rename error is reported.
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot write {}: {e}", p.display()));
    }
    Ok(h)
}

/// Are `bytes` (the content of `config`, as already read by the caller) the
/// content recorded for `root` in `<state>/trust.json`?
///
/// Contract: no record -> `Untrusted`; recorded hash differs -> `Untrusted`;
/// equal -> `Trusted`; store unreadable / not JSON, or `root` cannot be
/// canonicalized -> `Undetermined`.
#[must_use]
pub fn check_bytes(state: &Path, root: &Path, config: &Path, bytes: &[u8]) -> Trust {
    let k = match key(root) {
        Ok(k) => k,
        Err(e) => return Trust::Undetermined(e),
    };
    let store = match read_store(state) {
        Ok(s) => s,
        Err(e) => return Trust::Undetermined(e),
    };
    match store.get(&k) {
        None => Trust::Untrusted(format!(
            "{} has never been trusted; review it and run `sitecraft trust` in {k}",
            config.display()
        )),
        Some(recorded) if *recorded == hash_bytes(bytes) => Trust::Trusted,
        Some(_) => Trust::Untrusted(format!(
            "{} changed since it was trusted; review it and run `sitecraft trust` again",
            config.display()
        )),
    }
}
