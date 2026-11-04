#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! PATH lookup for a bare hook command name takes the first *executable*
//! match, as the shell does — a non-executable file earlier on `$PATH` does
//! not shadow it (GH #1484). Own test binary with a single test: it mutates
//! the process-wide `$PATH`.

use std::os::unix::fs::PermissionsExt;

use harness_core::hook::{extract_binary_path, hook_target_status, HookTargetStatus};

fn file_with_mode(dir: &std::path::Path, name: &str, mode: u32) -> String {
    let p = dir.join(name);
    std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn path_lookup_takes_first_executable_match() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let name = "hs-path-lookup-probe-bin";
    file_with_mode(a.path(), name, 0o644);
    let exec = file_with_mode(b.path(), name, 0o755);
    let path = std::env::join_paths([a.path(), b.path()]).unwrap();
    std::env::set_var("PATH", &path);

    assert_eq!(extract_binary_path(name, None), Some(exec));
    assert_eq!(hook_target_status(name, None), HookTargetStatus::Present);

    // Only a non-executable match on PATH: flagged, not present.
    std::env::set_var("PATH", a.path());
    match hook_target_status(name, None) {
        HookTargetStatus::NotExecutable { binary_path } => {
            assert!(binary_path.starts_with(&*a.path().to_string_lossy()));
        }
        other => panic!("expected NotExecutable, got {other:?}"),
    }
}
