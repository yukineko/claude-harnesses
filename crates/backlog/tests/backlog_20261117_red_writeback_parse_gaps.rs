#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repros for red_writeback parsing gaps, written by an independent
//! verifier (not the implementer). Observed 2026-11-17 at 1331e475d.
//! - unknown value flag (`--lockfile-path ci.lock`): its value becomes a libtest
//!   name filter (CargoArgs::parse VALUE_FLAGS), so the right test is refused,
//!   or an unrelated test is tagged.
//! - `#[ignore]` on a non-fn item is attached to the next fn (next_fn_name).
//! - `# [ignore = "RED..."]` / `#[ ignore = "RED..."]` (valid Rust) is not seen.
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
const REPRO_LOCKFILE: &str =
    "cargo test -p x --test red_one --lockfile-path ci.lock -- --include-ignored";

fn run_case(tag: &str, repro: &str, content: &str) -> (i32, String, String, String) {
    let f = Fixture::new(tag);
    f.write_exec_shim("cargo", FAKE_CARGO_RED);
    f.write("marker", "broken");
    f.write(TEST_REL, content);
    f.commit_paths(&["marker", TEST_REL], "test: RED repro");
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
    let body = std::fs::read_to_string(f.repo.join(TEST_REL)).unwrap();
    eprintln!(
        "[{tag}] code={}\nstderr={}\nbody=\n{body}",
        o.code, o.stderr
    );
    (o.code, o.stderr.clone(), body, id)
}

const ONE: &str = "#[test]\n#[ignore = \"RED: x\"]\nfn red_one() {\n    panic!(\"bug\");\n}\n";

/// Control: the same repro without the unknown flag tags the single RED.
#[test]
fn control_plain_repro_tags_single_red() {
    let (c, e, b, id) = run_case("c-plain", REPRO, ONE);
    assert_eq!(c, 0, "{e}");
    assert!(b.contains(&format!("RED({id})")), "{b}");
}

#[test]
fn unknown_value_flag_value_is_not_a_filter() {
    let (c, e, b, id) = run_case("f3-misreject", REPRO_LOCKFILE, ONE);
    assert!(
        c == 0 && b.contains(&format!("RED({id})")),
        "exit {c}, stderr={e}\nbody=\n{b}"
    );
}

#[test]
fn unknown_value_flag_value_does_not_pick_wrong_test() {
    let body = "#[test]\n#[ignore = \"RED: a\"]\nfn red_a() { panic!(); }\n#[test]\n#[ignore = \"RED: b\"]\nfn red_b() { panic!(); }\n";
    let (c, e, b, id) = run_case(
        "f3-wrong",
        "cargo test -p x --test red_one --lockfile-path red_b -- --include-ignored",
        body,
    );
    // No filter => two untagged REDs => must be refused, nothing rewritten.
    assert!(
        !b.contains(&format!("RED({id})")),
        "tagged by an unknown flag's value (exit {c}, stderr={e}):\n{b}"
    );
}

#[test]
#[ignore = "RED(c62740af): #[ignore = \"RED..\"] on a non-fn item (struct) is bound to the next fn"]
fn ignore_on_non_fn_item_is_not_bound_to_next_fn() {
    let body = "#[ignore = \"RED: s\"]\nstruct Helper;\n#[test]\nfn red_one() { panic!(); }\n";
    let (c, e, b, id) = run_case("f4", &format!("{REPRO} red_one"), body);
    assert!(
        !b.contains(&format!("RED({id})")),
        "struct's ignore tagged as red_one's (exit {c}, stderr={e}):\n{b}"
    );
}

#[test]
#[ignore = "RED(4bbf36c2): `# [ignore = \"RED..\"]` (valid Rust, space after #) is not seen, the add is refused"]
fn space_after_hash_ignore_is_seen() {
    let body = "#[test]\n# [ignore = \"RED: x\"]\nfn red_one() { panic!(); }\n";
    let (c, e, b, id) = run_case("f5-hash", REPRO, body);
    assert!(
        c == 0 && b.contains(&format!("RED({id})")),
        "exit {c}, stderr={e}\nbody=\n{b}"
    );
}

#[test]
#[ignore = "RED(4bbf36c2): `#[ ignore = \"RED..\"]` (valid Rust, space after [) is not seen, the add is refused"]
fn space_after_bracket_ignore_is_seen() {
    let body = "#[test]\n#[ ignore = \"RED: x\"]\nfn red_one() { panic!(); }\n";
    let (c, e, b, id) = run_case("f5-bracket", REPRO, body);
    assert!(
        c == 0 && b.contains(&format!("RED({id})")),
        "exit {c}, stderr={e}\nbody=\n{b}"
    );
}
