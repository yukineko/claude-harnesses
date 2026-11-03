//! Low-level access to the fde state directory: per-target directories,
//! three-valued JSON reads and atomic JSON writes.
//!
//! Layout: `<state_dir>/targets/<state_key>/<file>.json`. Nothing here ever
//! writes inside a target; callers pass the state directory resolved by
//! [`crate::target::state_dir`].
//!
//! Every IO or parse failure is `Undetermined`; a file that does not exist is
//! the determined answer `Known(None)` (absent), never conflated with an
//! unreadable one.

use std::io::Write;
use std::path::{Path, PathBuf};

use harness_core::verdict::Determination;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Directory holding all fde state for the target with `state_key`.
pub fn target_dir(state_dir: &Path, state_key: &str) -> PathBuf {
    state_dir.join("targets").join(state_key)
}

/// Read and parse the JSON file `path`.
///
/// * `Known(None)` — the file does not exist (`NotFound`).
/// * `Known(Some(v))` — read and parsed.
/// * `Undetermined` — any other read error (permissions, IO) or a parse error.
#[track_caller]
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Determination<Option<T>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Determination::known(None),
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot read state file {}: {e}",
                path.display()
            ))
        }
    };
    match serde_json::from_slice::<T>(&bytes) {
        Ok(v) => Determination::known(Some(v)),
        Err(e) => Determination::undetermined(format!(
            "state file {} is corrupt (not the expected JSON): {e}",
            path.display()
        )),
    }
}

/// Atomically write `value` as JSON to `path`: the bytes go to a temp file in
/// the same directory, are fsynced, and the temp file is renamed over `path`.
/// The parent directory is created when missing. `Known(())` only when the
/// rename succeeded; every failure is `Undetermined` (and the temp file is
/// removed on a best-effort basis — its removal failing does not change the
/// answer, which is already `Undetermined`).
#[track_caller]
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Determination<()> {
    let Some(dir) = path.parent() else {
        return Determination::undetermined(format!(
            "state file {} has no parent directory",
            path.display()
        ));
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        return Determination::undetermined(format!(
            "cannot create state directory {}: {e}",
            dir.display()
        ));
    }
    let json = match serde_json::to_vec_pretty(value) {
        Ok(j) => j,
        Err(e) => {
            return Determination::undetermined(format!(
                "cannot serialize state for {}: {e}",
                path.display()
            ))
        }
    };
    let Some(name) = path.file_name() else {
        return Determination::undetermined(format!(
            "state file {} has no file name",
            path.display()
        ));
    };
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".tmp.{}", std::process::id()));
    let tmp = dir.join(tmp_name);

    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&json)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    match written {
        Ok(()) => Determination::known(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Determination::undetermined(format!("cannot write state file {}: {e}", path.display()))
        }
    }
}

/// Remove `path`. `Known(())` when it was removed or did not exist; any
/// other failure is `Undetermined`.
#[track_caller]
pub fn remove_if_present(path: &Path) -> Determination<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Determination::known(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Determination::known(()),
        Err(e) => {
            Determination::undetermined(format!("cannot remove state file {}: {e}", path.display()))
        }
    }
}
