//! Integration test: the vendored fixture loads into typed instances with the
//! right field values, entirely offline (no network, no download subcommand).

use std::path::PathBuf;

use benchkit::load_instances;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("instances.jsonl")
}

#[test]
fn loads_vendored_fixture_into_instances() {
    let instances = load_instances(fixture_path()).expect("fixture should load");
    assert!(
        instances.len() >= 2,
        "expected at least 2 vendored rows, got {}",
        instances.len()
    );

    // Row 0: astropy — check the renamed upper-case test-set fields decoded as
    // plain lists of strings, plus the core identity fields.
    let astropy = &instances[0];
    assert_eq!(astropy.instance_id, "astropy__astropy-12907");
    assert_eq!(astropy.repo, "astropy/astropy");
    assert_eq!(
        astropy.base_commit,
        "d16bfe05a744909de4b27f5875fe0d4ed41ce607"
    );
    assert_eq!(astropy.version, "4.3");
    assert_eq!(astropy.fail_to_pass.len(), 2);
    assert_eq!(
        astropy.fail_to_pass[0],
        "astropy/modeling/tests/test_separable.py::test_separable[compound_model6-result6]"
    );
    assert_eq!(astropy.pass_to_pass.len(), 2);
    assert_eq!(
        astropy.environment_setup_commit,
        "298ccb478e6bf092953bca67a3d29dc6c35f6752"
    );
    assert!(astropy.patch.contains("separable.py"));
    assert!(astropy.test_patch.contains("test_separable.py"));
    assert!(!astropy.problem_statement.is_empty());

    // Row 1: django — sanity on a second, differently-shaped row.
    let django = &instances[1];
    assert_eq!(django.instance_id, "django__django-11099");
    assert_eq!(django.repo, "django/django");
    assert_eq!(django.fail_to_pass.len(), 2);
    assert_eq!(django.pass_to_pass.len(), 1);
}
