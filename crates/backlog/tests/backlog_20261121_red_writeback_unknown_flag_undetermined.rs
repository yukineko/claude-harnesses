#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for red_writeback: an unknown cargo flag with no `=` has an
//! arity that cannot be determined (CargoArgs::parse treats it as a no-value
//! flag, so the next word becomes a libtest name filter). Undetermined must be
//! refused, not guessed. Written by an independent test author, base 644e22189.
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

const SOURCE_ONE: &str = ONE;

/// Control: a known value flag (`--features f`) keeps working, single RED tagged.
#[test]
fn control_known_value_flag_tags_single_red() {
    let (c, e, b, id) = run_case(
        "c-known",
        "cargo test -p x --test red_one --features f -- --include-ignored",
        SOURCE_ONE,
    );
    assert_eq!(c, 0, "{e}");
    assert!(b.contains(&format!("RED({id})")), "{b}");
}

/// Control: an inline value (`--frobnicate=v`) has determined arity (one word,
/// consumes nothing else), so it is not refused for arity and the single RED
/// is tagged.
#[test]
fn control_inline_value_unknown_flag_tags_single_red() {
    let (c, e, b, id) = run_case(
        "c-inline",
        "cargo test -p x --test red_one --frobnicate=v -- --include-ignored",
        SOURCE_ONE,
    );
    assert_eq!(c, 0, "{e}");
    assert!(b.contains(&format!("RED({id})")), "{b}");
}

#[test]
#[ignore = "RED(9a1a5129): unknown flag of undetermined arity is guessed, not refused"]
fn unknown_flag_without_equals_is_refused_as_undetermined() {
    let (c, e, b, id) = run_case(
        "f-undet",
        "cargo test -p x --test red_one --frobnicate red_one -- --include-ignored",
        SOURCE_ONE,
    );
    assert!(
        c != 0 && !b.contains(&format!("RED({id})")) && b == SOURCE_ONE,
        "unknown flag arity guessed: exit {c}, stderr={e}\nbody=\n{b}"
    );
}
