//! Content-pinned trust for `<root>/sitecraft.toml` (the webqa scheme).
//!
//! `sitecraft.toml` declares a `command` that `serve start` (and the Stop gate)
//! spawns. A config the user never approved must not run. `sitecraft trust`
//! records the sha256 of the config bytes, keyed by the canonicalized project
//! root, in `<state>/trust.json` (written to a temp file, then renamed). The
//! command runs only when the current bytes hash to the recorded value.
//!
//! D2-trust (rulings D2): `sitecraft trust` also records, under the key
//! `<canonical root>#capabilities`, the digest of the `command` capabilities'
//! argv ([`crate::decide::capability::command_digest`]); with no `command`
//! capability the key is removed (none is required). `check capability`
//! runs a `command` only when [`check_capabilities`] is `Trusted`.
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

/// Suffix of the D2-trust key: `<canonical root>#capabilities` holds the
/// digest of the `command` capabilities' argv
/// ([`crate::decide::capability::command_digest`]).
pub const CAPABILITIES_SUFFIX: &str = "#capabilities";

/// What to do with the `<root>#capabilities` key when recording.
enum CapsUpdate<'a> {
    /// Leave it as it is (plain [`record`]).
    Keep,
    /// Store this digest.
    Set(&'a str),
    /// Remove it (no `command` capability: no digest is required).
    Remove,
}

/// Record trust for the current bytes of `config` under `root` in
/// `<state>/trust.json`; returns the recorded sha256 hex. The
/// `<root>#capabilities` key is left untouched (see [`record_with_capabilities`]).
///
/// Contract: creates `state` if needed; a missing store is created; a store
/// that exists but cannot be read or parsed as JSON is **not overwritten** and
/// yields `Err` (it may hold other roots' records); an unreadable `config` or
/// uncanonicalizable `root` yields `Err`.
pub fn record(state: &Path, root: &Path, config: &Path) -> Result<String, String> {
    record_inner(state, root, config, &CapsUpdate::Keep)
}

/// [`record`] plus D2-trust in the same store write: `Some(digest)` stores
/// it under `<canonical root>#capabilities`, `None` (no `command`
/// capability) removes that key. Same error contract as [`record`]; on `Err`
/// nothing is written.
pub fn record_with_capabilities(
    state: &Path,
    root: &Path,
    config: &Path,
    digest: Option<&str>,
) -> Result<String, String> {
    let update = match digest {
        Some(d) => CapsUpdate::Set(d),
        None => CapsUpdate::Remove,
    };
    record_inner(state, root, config, &update)
}

fn record_inner(
    state: &Path,
    root: &Path,
    config: &Path,
    caps: &CapsUpdate<'_>,
) -> Result<String, String> {
    let k = key(root)?;
    let bytes =
        std::fs::read(config).map_err(|e| format!("cannot read {}: {e}", config.display()))?;
    let h = hash_bytes(&bytes);
    let mut m = read_store(state)?;
    let caps_key = format!("{k}{CAPABILITIES_SUFFIX}");
    match caps {
        CapsUpdate::Keep => {}
        CapsUpdate::Set(d) => {
            m.insert(caps_key, (*d).to_string());
        }
        CapsUpdate::Remove => {
            m.remove(&caps_key);
        }
    }
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

/// D2-trust: is `digest` (of the current `command` capabilities) the one
/// recorded under `<canonical root>#capabilities`?
///
/// Contract: no record → `Untrusted`; a different digest → `Untrusted`
/// (both say to review the argv and run `sitecraft trust`); equal →
/// `Trusted`; store unreadable / not JSON, or `root` cannot be canonicalized
/// → `Undetermined`.
#[must_use]
pub fn check_capabilities(state: &Path, root: &Path, digest: &str) -> Trust {
    let k = match key(root) {
        Ok(k) => format!("{k}{CAPABILITIES_SUFFIX}"),
        Err(e) => return Trust::Undetermined(e),
    };
    let store = match read_store(state) {
        Ok(s) => s,
        Err(e) => return Trust::Undetermined(e),
    };
    match store.get(&k) {
        None => Trust::Untrusted(
            "the `command` capabilities' argv have never been trusted; a human reviews `verify.argv` in sitecraft/decisions.toml and runs `sitecraft trust`".to_string(),
        ),
        Some(recorded) if recorded == digest => Trust::Trusted,
        Some(_) => Trust::Untrusted(
            "the `command` capabilities' argv changed since they were trusted; a human reviews `verify.argv` in sitecraft/decisions.toml and runs `sitecraft trust` again".to_string(),
        ),
    }
}
