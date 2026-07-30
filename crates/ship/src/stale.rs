use std::fs;
use std::path::Path;

use harness_core::verdict::Determination;

/// Find crate names whose src/ is newer than their bin/<name>-linux-x86_64 binary.
///
/// For each `crates/<name>` directory that has BOTH a `src/` dir and a `bin/<name>-linux-x86_64` file,
/// returns `<name>` if the newest mtime under `src/` is more recent than the mtime of the binary.
/// Skips crates missing either `src/` or the binary.
///
/// A missing `crates/` dir is a legitimate absence (not this repo layout) and
/// yields `Known(vec![])`; any OTHER `read_dir` error (permission denied, IO)
/// is a cannot-determine — it must not be silently read as "nothing stale",
/// so it comes back `Undetermined` instead.
#[allow(dead_code)]
pub fn stale_crates(repo: &Path) -> Determination<Vec<String>> {
    let crates_dir = repo.join("crates");

    let mut stale = vec![];

    let entries = match fs::read_dir(&crates_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Determination::Known(Vec::new())
        }
        Err(e) => return Determination::undetermined(format!("{}: {e}", crates_dir.display())),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(crate_name) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(true) = check_stale_crate(&path, crate_name) {
                    stale.push(crate_name.to_string());
                }
            }
        }
    }

    Determination::Known(stale)
}

/// Check if a single crate is stale.
/// Returns Some(true) if stale, Some(false) if not stale, None if check couldn't be performed.
#[allow(dead_code)]
fn check_stale_crate(crate_path: &Path, crate_name: &str) -> Option<bool> {
    let src_dir = crate_path.join("src");
    let bin_file = crate_path.join(format!("bin/{}-linux-x86_64", crate_name));

    // Both src/ and bin must exist
    if !src_dir.exists() || !bin_file.exists() {
        return None;
    }

    // Get the newest mtime in src/
    let newest_src_mtime = get_newest_mtime_in_dir(&src_dir).ok()?;

    // Get the mtime of the binary
    let bin_mtime = fs::metadata(&bin_file).ok()?.modified().ok()?;

    // Return true if src is newer than bin
    Some(newest_src_mtime > bin_mtime)
}

/// Get the newest mtime of any file in a directory (recursively).
#[allow(dead_code)]
fn get_newest_mtime_in_dir(dir: &Path) -> std::io::Result<std::time::SystemTime> {
    let mut newest = std::time::SystemTime::UNIX_EPOCH;

    fn walk_dir(dir: &Path, newest: &mut std::time::SystemTime) -> std::io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::metadata(&path)?;
            if let Ok(modified) = metadata.modified() {
                if modified > *newest {
                    *newest = modified;
                }
            }
            if path.is_dir() {
                walk_dir(&path, newest)?;
            }
        }
        Ok(())
    }

    walk_dir(dir, &mut newest)?;
    Ok(newest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    fn known(d: Determination<Vec<String>>) -> Vec<String> {
        match d {
            Determination::Known(v) => v,
            Determination::Undetermined(why) => panic!("expected Known, got Undetermined: {why}"),
        }
    }

    #[test]
    fn test_stale_crates_newer_src() {
        let temp_repo = TempDir::new().unwrap();
        let repo_path = temp_repo.path();

        // Create crates/foo directory
        let foo_crate = repo_path.join("crates/foo");
        fs::create_dir_all(&foo_crate).unwrap();

        // Create src/lib.rs
        let src_dir = foo_crate.join("src");
        fs::create_dir(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), "code").unwrap();

        // Create bin/foo-linux-x86_64
        let bin_dir = foo_crate.join("bin");
        fs::create_dir(&bin_dir).unwrap();
        let bin_file = bin_dir.join("foo-linux-x86_64");
        fs::write(&bin_file, "binary").unwrap();

        // Make sure src is newer than bin by sleeping and touching src
        thread::sleep(Duration::from_millis(10));
        fs::write(src_dir.join("lib.rs"), "newer code").unwrap();

        // Check that foo is detected as stale
        let stale = known(stale_crates(repo_path));
        assert!(stale.contains(&"foo".to_string()));
    }

    #[test]
    fn test_stale_crates_newer_bin() {
        let temp_repo = TempDir::new().unwrap();
        let repo_path = temp_repo.path();

        // Create crates/bar directory
        let bar_crate = repo_path.join("crates/bar");
        fs::create_dir_all(&bar_crate).unwrap();

        // Create src/lib.rs
        let src_dir = bar_crate.join("src");
        fs::create_dir(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), "code").unwrap();

        // Create bin/bar-linux-x86_64
        let bin_dir = bar_crate.join("bin");
        fs::create_dir(&bin_dir).unwrap();
        let bin_file = bin_dir.join("bar-linux-x86_64");
        fs::write(&bin_file, "binary").unwrap();

        // Make sure bin is newer than src by sleeping and touching bin
        thread::sleep(Duration::from_millis(10));
        fs::write(&bin_file, "newer binary").unwrap();

        // Check that bar is NOT detected as stale
        let stale = known(stale_crates(repo_path));
        assert!(!stale.contains(&"bar".to_string()));
    }

    #[test]
    fn test_stale_crates_missing_src() {
        let temp_repo = TempDir::new().unwrap();
        let repo_path = temp_repo.path();

        // Create crates/baz directory without src
        let baz_crate = repo_path.join("crates/baz");
        fs::create_dir_all(&baz_crate).unwrap();

        // Create bin/baz-linux-x86_64
        let bin_dir = baz_crate.join("bin");
        fs::create_dir(&bin_dir).unwrap();
        fs::write(bin_dir.join("baz-linux-x86_64"), "binary").unwrap();

        // Check that baz is NOT detected (missing src)
        let stale = known(stale_crates(repo_path));
        assert!(!stale.contains(&"baz".to_string()));
    }

    #[test]
    fn test_stale_crates_missing_bin() {
        let temp_repo = TempDir::new().unwrap();
        let repo_path = temp_repo.path();

        // Create crates/qux directory without bin
        let qux_crate = repo_path.join("crates/qux");
        fs::create_dir_all(&qux_crate).unwrap();

        // Create src/lib.rs
        let src_dir = qux_crate.join("src");
        fs::create_dir(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), "code").unwrap();

        // Check that qux is NOT detected (missing bin)
        let stale = known(stale_crates(repo_path));
        assert!(!stale.contains(&"qux".to_string()));
    }

    #[test]
    fn test_stale_crates_empty_repo() {
        let temp_repo = TempDir::new().unwrap();
        let repo_path = temp_repo.path();

        // Check that empty repo returns empty vec
        let stale = known(stale_crates(repo_path));
        assert!(stale.is_empty());
    }

    /// CA-ship-stale-01: a `crates/` dir that EXISTS but is unreadable
    /// (permission denied) must resolve to `Undetermined`, not the same
    /// `Known(vec![])` as a legitimately-absent dir — collapsing both into
    /// "nothing stale" makes an incomplete scan look like a clean one.
    #[test]
    #[cfg(unix)]
    fn unreadable_existing_crates_dir_is_undetermined_not_known_empty() {
        use std::os::unix::fs::PermissionsExt;
        let temp_repo = TempDir::new().unwrap();
        let crates_dir = temp_repo.path().join("crates");
        fs::create_dir_all(&crates_dir).unwrap();
        let mut perms = fs::metadata(&crates_dir).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&crates_dir, perms.clone()).unwrap();

        let result = stale_crates(temp_repo.path());

        // Restore permissions so the temp dir can be cleaned up.
        perms.set_mode(0o755);
        let _ = fs::set_permissions(&crates_dir, perms);

        assert!(
            matches!(result, Determination::Undetermined(_)),
            "an unreadable existing crates/ dir must be Undetermined, got {result:?}"
        );
    }
}
