#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro (doc <-> code contradiction): `docs/specs/overwatch.md` says
//! `load_leases` treats a missing OR corrupt leases.json as an empty registry
//! (`unwrap_or_default`). The code returns `Err` for a corrupt file (only a
//! genuinely-absent file is an empty registry) precisely so a corrupt registry
//! is never silently clobbered by the next read-modify-write.
//!
//! The test observes the real behaviour, then asserts the spec line does not
//! claim the opposite.
use overwatch::store;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn spec_must_not_claim_corrupt_leases_json_loads_as_empty_registry() {
    let home = std::env::temp_dir().join(format!("ow-repro-spec-leases-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let cwd = home.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    std::env::set_var("HOME", &home);
    std::env::remove_var("HARNESS_SANDBOX_HOME");
    assert!(Command::new("git")
        .current_dir(&cwd)
        .args(["init", "-q"])
        .status()
        .unwrap()
        .success());

    // Observation: corrupt leases.json -> load_leases.
    let path = store::leases_path(&cwd).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{ this is not json").unwrap();
    let corrupt_is_err = store::load_leases(&cwd).is_err();
    // Control: an absent file IS an empty registry.
    std::fs::remove_file(&path).unwrap();
    assert!(
        store::load_leases(&cwd).unwrap().is_empty(),
        "control failed: absent leases.json must load as an empty registry"
    );
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        corrupt_is_err,
        "control failed: corrupt leases.json should be Err"
    );

    let spec_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/specs/overwatch.md");
    let spec = std::fs::read_to_string(&spec_path).unwrap();
    let line = spec
        .lines()
        .find(|l| l.contains("load_leases"))
        .unwrap_or_else(|| panic!("no spec line mentions load_leases in {spec_path:?}"));
    let claims_corrupt_is_empty = line.contains("破損") && line.contains("空レジストリ");
    assert!(
        !claims_corrupt_is_empty,
        "docs/specs/overwatch.md claims load_leases treats corrupt JSON as an empty registry, \
         but load_leases returns Err for a corrupt file. Spec line: {line}"
    );
}
