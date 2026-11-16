//! Durable atomic file replacement for overwatch's stores.
//!
//! Every overwatch store rewrite (leases / presence / subagents / changesets /
//! the JSONL compaction rewrites) used to do `fs::write(tmp)` + `fs::rename(tmp,
//! path)` with no fsync in between. On a crash the rename could be committed
//! while the temp's data blocks were not, leaving the final file at its full
//! size but all NUL (observed 2026-11-16: `presence.json` was 32322 NUL bytes).
//!
//! [`write_durable`] is the single path all of them now go through. It does NOT
//! reuse `harness_core::store`'s helper, which swallows rename errors.

use anyhow::Result;
use std::io::Write;
use std::path::Path;

/// Replace `path` with `bytes` via the caller-chosen sibling temp `tmp`:
/// create `tmp`, `write_all`, `sync_all` (fsync the temp's data), close,
/// `rename(tmp, path)`, then fsync the parent directory so the rename itself is
/// durable. Every step's error is propagated — none is ignored.
///
/// If any step after `tmp` was created fails, `tmp` is removed (only that
/// cleanup's own error is ignored, since the original error is the one being
/// reported) and the original error is returned. After a successful rename the
/// temp no longer exists, so a failing directory fsync still returns its error
/// and the cleanup is a no-op.
pub(crate) fn write_durable(path: &Path, tmp: &Path, bytes: &[u8]) -> Result<()> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(tmp)?;
    let res = write_sync_rename(file, path, tmp, bytes);
    if res.is_err() {
        // Cleanup only; the original error below is what the caller sees.
        let _ = std::fs::remove_file(tmp);
    }
    res
}

fn write_sync_rename(mut file: std::fs::File, path: &Path, tmp: &Path, bytes: &[u8]) -> Result<()> {
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, path)?;
    sync_parent_dir(path)?;
    Ok(())
}

/// fsync the directory containing `path` so the rename's directory entry is
/// durable. Failure is returned, not ignored.
#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Non-unix: directories cannot be opened and fsynced portably (Windows
/// `File::open` on a directory fails), so the directory fsync is skipped. The
/// temp file's own data fsync above still happens.
#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn write_durable_writes_content_and_replaces_existing() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("store.json");
        std::fs::write(&path, b"old contents that are longer").unwrap();
        let tmp = d.path().join("store.json.tmp.1");
        write_durable(&path, &tmp, b"{\"k\":1}").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"k\":1}");
    }

    #[test]
    fn write_durable_leaves_no_tmp_on_success() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("store.json");
        let tmp = d.path().join("store.json.tmp.1");
        write_durable(&path, &tmp, b"x").unwrap();
        assert!(!tmp.exists());
        assert_eq!(entries(d.path()), vec!["store.json".to_string()]);
    }

    #[test]
    fn write_durable_errors_when_parent_missing_and_leaves_no_tmp() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("missing").join("store.json");
        let tmp = d.path().join("missing").join("store.json.tmp.1");
        assert!(write_durable(&path, &tmp, b"x").is_err());
        assert!(!tmp.exists());
        assert!(!path.exists());
        assert!(entries(d.path()).is_empty());
    }

    #[test]
    fn write_durable_rename_failure_errors_and_removes_tmp() {
        let d = tempfile::tempdir().unwrap();
        // Destination is a non-empty directory: rename(file -> dir) fails
        // after the temp was created, written and fsynced.
        let path = d.path().join("dest");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("occupant"), b"y").unwrap();
        let tmp = d.path().join("dest.tmp.1");
        assert!(write_durable(&path, &tmp, b"x").is_err());
        assert!(
            !tmp.exists(),
            "temp must be cleaned up after a failed rename"
        );
        assert!(path.is_dir());
    }
}
