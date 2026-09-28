#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 10b72c4d: `trust::add` must fail closed on an unreadable / unparseable
//! trust file (not overwrite it with a list of one), and concurrent adds must
//! not lose entries.
//!
//! Integration test => its own process. HOME is pinned to a temp dir under a
//! local lock; the real home is never touched.

use harness_core::trust;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn pin_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());
    std::env::remove_var("HARNESS_TRUST_ALL");
    home
}

#[test]
fn add_on_unparseable_trust_file_errs_and_leaves_bytes_untouched() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _home = pin_home();
    let proj = tempfile::tempdir().unwrap();
    let path = trust::trust_path();
    assert!(path.starts_with(_home.path()), "apparatus: HOME pinned");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let garbage = b"trusted = [\"/keep/me\", \n <<< not toml";
    std::fs::write(&path, garbage).unwrap();

    let r = trust::add(proj.path());

    assert!(
        r.is_err(),
        "add over a corrupt trust file must fail, got {r:?}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        garbage,
        "a failed add must leave the trust file byte-identical"
    );
}

#[cfg(unix)]
#[test]
fn add_on_unreadable_trust_file_errs_and_leaves_bytes_untouched() {
    use std::os::unix::fs::PermissionsExt;
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _home = pin_home();
    let proj = tempfile::tempdir().unwrap();
    let path = trust::trust_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let valid = b"trusted = [\"/keep/me\"]\n";
    std::fs::write(&path, valid).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Apparatus: if we can still read it (e.g. root), the test proves nothing.
    if std::fs::read(&path).is_ok() {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        eprintln!("SKIPPED: file still readable with mode 000 (running as root?)");
        return;
    }

    let r = trust::add(proj.path());

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        r.is_err(),
        "add over an unreadable trust file must fail, got {r:?}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        valid,
        "file must be untouched"
    );
}

/// CONTROL: on a healthy / absent trust file add succeeds and records the key,
/// so the Err above is caused by the corruption, not by add always failing.
#[test]
fn control_add_on_absent_and_valid_trust_file_succeeds() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _home = pin_home();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let ka = trust::add(a.path()).expect("absent file: add ok");
    let kb = trust::add(b.path()).expect("valid file: add ok");
    let l = trust::list();
    assert!(
        l.contains(&ka) && l.contains(&kb),
        "both entries kept: {l:?}"
    );
}

#[test]
fn concurrent_adds_keep_every_entry() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _home = pin_home();
    const N: usize = 16;
    let projs: Vec<_> = (0..N).map(|_| tempfile::tempdir().unwrap()).collect();
    let barrier = std::sync::Barrier::new(N);
    let results: Vec<_> = std::thread::scope(|s| {
        let hs: Vec<_> = projs
            .iter()
            .map(|p| {
                let barrier = &barrier;
                s.spawn(move || {
                    barrier.wait();
                    trust::add(p.path())
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    // An add may legitimately Err (lock contention) but must not silently lose
    // an entry it reported as Ok.
    let listed = trust::list();
    for (p, r) in projs.iter().zip(&results) {
        if let Ok(k) = r {
            assert!(
                listed.contains(k),
                "add({}) returned Ok but the entry was lost (lost update); listed {} of {N}",
                p.path().display(),
                listed.len()
            );
        }
    }
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        N,
        "all {N} adds must succeed and be kept; results: {results:?}"
    );
    assert_eq!(
        listed.len(),
        N,
        "expected {N} entries, got {}",
        listed.len()
    );
}
