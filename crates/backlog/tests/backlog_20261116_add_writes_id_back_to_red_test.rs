#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `backlog add --repro-test CMD` mints an id but never writes it
//! back into the `#[ignore = "RED..."]` test that CMD points at.
//!
//! Observation (2026-11-16, main 202efa14a): of the files containing
//! `#[ignore = "RED`, 62 files, 51 carry no backlog id that exists in the
//! file body. Cause is ordering: the test is committed first, then `backlog
//! add` issues the id, so nothing ever puts the id back into the test.
//! (Measured: files with `#[ignore = "RED` counted by grep over the tree at
//! that rev; the count is not inherited here, re-measure before reuse.)
//!
//! User decision (2026-11-16): after a successful `backlog add`, rewrite the
//! pointed-at test's `#[ignore = "RED..."]` to `#[ignore = "RED(<id>): ..."]`.
//!
//! Two readings of the observation:
//!   (a) defect: `add` should write the id back; the missing id is `add`'s
//!       omission. Under (a) this test FAILS today and passes once fixed.
//!   (b) intended: ids live only in the store and the test needs no id.
//!       The user's decision above rules (b) out; this test pins (a).
//! How to tell them apart: after a successful `add`, the pointed-at file
//! either contains `RED(<id>)` or it does not. Observable; nothing to judge.
//!
//! Shape: linked-worktree fixture, committed `crates/x/tests/red_one.rs` with
//! `#[ignore = "RED: x"]`, fake cargo that fails the ignored test, then `add
//! --repro-test "cargo test -p x --test red_one -- --include-ignored"`.
//! CONTROL: `add` itself must succeed (exit 0, row pending). The RED assert
//! is only the write-back.
//!
//! DOES NOT PROVE: that the write-back is committed or staged (only the
//! working-tree file is read); behaviour for repros that name no `--test`
//! file, several files, or a non-RED ignore text; that the right test is
//! chosen when the file holds several `#[ignore]` tests; idempotence on
//! re-add; the 51-of-62 figure (taken from the task statement, not re-run).
//!
//! Written by an independent test author, not the implementer.

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

#[test]
// test-weakening-justified: new RED for an unfixed defect (2026-11-16).
#[ignore = "RED: 2026-11-16 backlog add does not write the minted id back into the #[ignore = \"RED...\"] test"]
fn add_writes_minted_id_back_into_red_test() {
    let f = Fixture::new("add-writeback");
    f.write_exec_shim("cargo", FAKE_CARGO_RED);
    f.write("marker", "broken");
    f.write(
        TEST_REL,
        "#[test]\n#[ignore = \"RED: x\"]\nfn red_one() {\n    panic!(\"bug\");\n}\n",
    );
    f.commit_paths(&["marker", TEST_REL], "test: RED repro");

    let p = f.project();
    let o = f.run(&[
        "add",
        "--title",
        "t",
        "--project",
        &p,
        "--repro-test",
        REPRO,
    ]);

    // CONTROL: add itself succeeds and files a pending row.
    assert_eq!(o.code, 0, "control: add must succeed: {}", o.both());
    let id = parse_added(&o.stdout);
    assert_eq!(f.status(&id), "pending", "control: row pending");

    let body = std::fs::read_to_string(f.repo.join(TEST_REL)).unwrap();
    assert!(
        body.contains(&format!("RED({id})")),
        "add minted id {id} but did not write it back; expected `RED({id})` in {TEST_REL}, file is:\n{body}"
    );
}
