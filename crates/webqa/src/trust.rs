//! Content-pinned trust for `<root>/webqa.toml`.
//!
//! `webqa.toml` declares a `command` the gate spawns. A config the user never
//! approved (a freshly cloned repo, or an edit after approval) must not run.
//! `webqa trust` records the sha256 of the config bytes keyed by the
//! canonical project root in `<state>/trust.json`; the gate runs the command
//! only when the current bytes hash to the recorded value.
//!
//! Three answers, not two ([`Trust`]): `Trusted`, `Untrusted` (no record, or
//! the hash changed), `Undetermined` (the store or the config cannot be read
//! or parsed). The gate treats both non-`Trusted` answers as a block; nothing
//! here maps an error to `Trusted`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const STORE: &str = "trust.json";

pub enum Trust {
    Trusted,
    Untrusted(String),
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

fn hash_file(p: &Path) -> Result<String, String> {
    let bytes = std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
    Ok(hash_bytes(&bytes))
}

/// `Ok(None)` only when the store file does not exist.
fn read_store(state: &Path) -> Result<Option<BTreeMap<String, String>>, String> {
    let p = store_path(state);
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read trust store {}: {e}", p.display())),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("cannot parse trust store {}: {e}", p.display()))
}

/// Record trust for the current bytes of `config` under `root`.
pub fn record(state: &Path, root: &Path, config: &Path) -> Result<String, String> {
    let k = key(root)?;
    let h = hash_file(config)?;
    // An unreadable/corrupt store is not silently replaced: the user must
    // look at it (it may hold other roots' records).
    let mut m = read_store(state)?.unwrap_or_default();
    m.insert(k, h.clone());
    std::fs::create_dir_all(state)
        .map_err(|e| format!("cannot create {}: {e}", state.display()))?;
    let text =
        serde_json::to_string_pretty(&m).map_err(|e| format!("cannot encode trust store: {e}"))?;
    let p = store_path(state);
    let tmp = state.join(format!("{STORE}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write {}: {e}", p.display())
    })?;
    Ok(h)
}

/// Are `bytes` (the content of `config`, as already read by the caller) the
/// content recorded for `root`? The caller parses the same bytes, so the
/// approved and the executed content are one read.
pub fn check_bytes(state: &Path, root: &Path, config: &Path, bytes: &[u8]) -> Trust {
    match recorded_hash(state, root, config) {
        Err(t) => t,
        Ok(recorded) if hash_bytes(bytes) == recorded => Trust::Trusted,
        Ok(_) => Trust::Untrusted(format!(
            "{} changed since it was trusted; review it and run `webqa trust` again",
            config.display()
        )),
    }
}

/// The recorded hash for `root`, or the non-`Trusted` answer.
fn recorded_hash(state: &Path, root: &Path, config: &Path) -> Result<String, Trust> {
    let k = key(root).map_err(Trust::Undetermined)?;
    let store = read_store(state).map_err(Trust::Undetermined)?;
    match store.as_ref().and_then(|m| m.get(&k)) {
        Some(h) => Ok(h.clone()),
        None => Err(Trust::Untrusted(format!(
            "{} has never been trusted; review it and run `webqa trust` in {}",
            config.display(),
            k
        ))),
    }
}

/// Is the current content of `config` the one recorded for `root`? (Reads
/// the file; an unreadable config is undetermined.)
#[cfg(test)]
pub fn check(state: &Path, root: &Path, config: &Path) -> Trust {
    let recorded = match recorded_hash(state, root, config) {
        Ok(h) => h,
        Err(t) => return t,
    };
    match hash_file(config) {
        Ok(h) if h == recorded => Trust::Trusted,
        Ok(_) => Trust::Untrusted(format!(
            "{} changed since it was trusted; review it and run `webqa trust` again",
            config.display()
        )),
        Err(e) => Trust::Undetermined(e),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("webqa-trust-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("root")).unwrap();
        p
    }

    #[test]
    fn corrupt_store_is_undetermined_not_trusted() {
        let d = tmp("corrupt");
        let root = d.join("root");
        let cfg = root.join("webqa.toml");
        std::fs::write(&cfg, "x").unwrap();
        let state = d.join("state");
        record(&state, &root, &cfg).unwrap();
        std::fs::write(store_path(&state), "{not json").unwrap();
        assert!(matches!(check(&state, &root, &cfg), Trust::Undetermined(_)));
        // record refuses to overwrite a corrupt store
        assert!(record(&state, &root, &cfg).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_config_after_trust_is_undetermined() {
        let d = tmp("gone");
        let root = d.join("root");
        let cfg = root.join("webqa.toml");
        std::fs::write(&cfg, "x").unwrap();
        let state = d.join("state");
        record(&state, &root, &cfg).unwrap();
        assert!(matches!(check(&state, &root, &cfg), Trust::Trusted));
        std::fs::remove_file(&cfg).unwrap();
        assert!(matches!(check(&state, &root, &cfg), Trust::Undetermined(_)));
        let _ = std::fs::remove_dir_all(&d);
    }
}
