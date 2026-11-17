#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED amendment for t3 (backlog 3d5285d5), written by the independent t3
//! verifier (sonnet), not the implementer. Observed 2026-11-17 at b6f2b4ec8:
//! red_writeback rewrites an #[ignore] the repro filter does not name (a
//! single untagged RED next to the named test, or a RED mention inside a
//! comment) and exits 0. The four exits-1 tests are controls that already
//! pass at b6f2b4ec8. The fake cargo ignores the filter and always fails.
//! DOES NOT PROVE: -p crate selection, the fn-name scan, package_is on
//! non-[package] tables, idempotence (all UNVERIFIED in the verifier report).
mod common;
use common::*;

const FAKE_CARGO_RED: &str = r#"#!/bin/bash
m="$(cat marker 2>/dev/null)"
ignored=0
after_dd=0
skip=0
for a in "$@"; do
  if [ "$skip" = 1 ]; then skip=0; continue; fi
  if [ "$after_dd" = 0 ]; then
    case "$a" in
      --) after_dd=1;;
      -p|--package|--test|--bin|--lib) skip=1;;
    esac
  else
    case "$a" in
      --ignored|--include-ignored) ignored=1;;
    esac
  fi
done
echo "running 1 test"
if [ "$ignored" = 0 ]; then
  echo "test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out"
  exit 0
fi
if [ "$m" = broken ]; then
  echo "test red_one ... FAILED"
  echo "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out"
  exit 101
fi
echo "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
exit 0
"#;

const TEST_REL: &str = "crates/x/tests/red_one.rs";
const REPRO: &str = "cargo test -p x --test red_one -- --include-ignored";

fn run_case(
    tag: &str,
    repro: &str,
    content: &str,
    chmod_ro: bool,
) -> (i32, String, String, String) {
    let f = Fixture::new(tag);
    f.write_exec_shim("cargo", FAKE_CARGO_RED);
    f.write("marker", "broken");
    f.write(TEST_REL, content);
    f.commit_paths(&["marker", TEST_REL], "test: RED repro");
    if chmod_ro {
        use std::os::unix::fs::PermissionsExt;
        let p = f.repo.join(TEST_REL);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    let p = f.project();
    let o = f.run(&[
        "add",
        "--title",
        "t",
        "--project",
        &p,
        "--repro-test",
        repro,
    ]);
    let id = parse_added(&o.stdout);
    let st = f.status(&id);
    let body = std::fs::read_to_string(f.repo.join(TEST_REL)).unwrap();
    eprintln!(
        "[{tag}] code={} status={st}\nstderr={}\nbody=\n{body}",
        o.code, o.stderr
    );
    (o.code, o.stderr.clone(), body, id)
}

const ONE: &str = "#[test]\n#[ignore = \"RED: x\"]\nfn red_one() {\n    panic!(\"bug\");\n}\n";

#[test]
fn no_test_flag_exits_1() {
    let (c, e, b, _) = run_case(
        "v-notest",
        "cargo test -p x -- --include-ignored",
        ONE,
        false,
    );
    assert_eq!(c, 1, "{e}");
    assert!(!b.contains("RED("));
}
#[test]
fn missing_file_exits_1() {
    let (c, _e, _b, _) = run_case(
        "v-missing",
        "cargo test -p x --test nope -- --include-ignored",
        ONE,
        false,
    );
    assert_eq!(c, 1);
}
#[test]
fn readonly_file_exits_1() {
    let (c, _e, b, _) = run_case("v-ro", REPRO, ONE, true);
    assert_eq!(c, 1);
    assert!(!b.contains("RED("));
}
#[test]
fn non_red_repro_file_exits_1() {
    let (c, _e, _b, _) = run_case(
        "v-nonred",
        REPRO,
        "#[test]\nfn red_one() {\n    panic!(\"bug\");\n}\n",
        false,
    );
    assert_eq!(c, 1);
}
/// repro filter targets red_a (already tagged with ANOTHER id); red_b is untagged.
#[test]
// test-weakening-justified: new RED for an unfixed defect (2026-11-17).
#[ignore = "RED: 2026-11-17 t3 verifier - red_writeback rewrites the wrong #[ignore] and exits 0 - remove when fixed"]
fn single_untagged_other_test_is_not_rewritten() {
    let body = "#[test]\n#[ignore = \"RED(deadbeef): a\"]\nfn red_a() { panic!(); }\n#[test]\n#[ignore = \"RED: b\"]\nfn red_b() { panic!(); }\n";
    let (c, _e, b, id) = run_case("v-wrong", &format!("{REPRO} red_a"), body, false);
    assert!(
        !b.contains(&format!("RED({id}): b")),
        "WRONG attr rewritten (exit {c}):\n{b}"
    );
}
/// repro filter targets red_a (plain #[ignore = "other"]); red_b has RED.
#[test]
// test-weakening-justified: new RED for an unfixed defect (2026-11-17).
#[ignore = "RED: 2026-11-17 t3 verifier - red_writeback rewrites the wrong #[ignore] and exits 0 - remove when fixed"]
fn filter_names_nonred_test_but_other_red_gets_tagged() {
    let body = "#[test]\n#[ignore = \"slow\"]\nfn red_a() { panic!(); }\n#[test]\n#[ignore = \"RED: b\"]\nfn red_b() { panic!(); }\n";
    let (c, _e, b, id) = run_case("v-wrong2", &format!("{REPRO} red_a"), body, false);
    assert!(
        !b.contains(&format!("RED({id}): b")),
        "WRONG attr rewritten (exit {c}):\n{b}"
    );
}
/// RED text inside a comment before the real attribute.
#[test]
// test-weakening-justified: new RED for an unfixed defect (2026-11-17).
#[ignore = "RED: 2026-11-17 t3 verifier - red_writeback rewrites the wrong #[ignore] and exits 0 - remove when fixed"]
fn comment_mention_not_rewritten() {
    let body = "// was #[ignore = \"RED: old\"]\n#[test]\nfn red_one() { panic!(); }\n";
    let (c, _e, b, id) = run_case("v-comment", REPRO, body, false);
    assert!(
        c == 1 && !b.contains(&format!("RED({id})")),
        "comment rewritten, exit {c}:\n{b}"
    );
}
