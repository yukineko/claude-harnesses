#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Negative pin for backlog 536e2811 (K2 subject variants): a commit whose
//! subject is `test: alpha parser RED repro (backlog <X> RED repro)` is a
//! RED-repro commit, NOT a fix. `backlog next` must still hand out X.
//!
//! Why it exists: the pending fix to `evidence::fixed_task_ids` will accept
//! `(backlog <id> wave 2)` / `(backlog <id>; y)` as fixing `<id>`. A naive
//! "first whitespace token" parse would also count 38a44b30 from the real
//! history subject `Merge branch 'red-prepush-test-compile' (backlog 38a44b30
//! RED repro)` and silently drop that ticket from `backlog next`. This test
//! fails under such a parse (mutation-checked, see the commit report).
//!
//! Passing means: `backlog next` exits 0 AND stdout contains X's id (X, p0,
//! is the highest-priority pending task; control Y is p2). Fixture mirrors
//! backlog_20261113_k2_fixed_subject_variants.rs.
//!
//! Observed, deliberately NOT asserted (intent undecided): for a multi-id
//! subject `(backlog <X>, <Y>; 0.2.395)` current main takes the whole string
//! `X, Y; 0.2.395` (everything up to the next `)`) as ONE id, so neither X nor
//! Y is counted as fixed. Read from the code at rev c3e816b4, not executed.
//!
//! Measurement point: rev c3e816b4, 2026-11-16.
//!
//! Does NOT prove: that any other suffix (`wave N`, `; y`, multi-id) is or is
//! not a fix; which suffix words mark a non-fix beyond `RED repro`; `next
//! --claim`; ids that are prefixes of one another.
//!
//! Written by an independent test author, not the implementer.

mod common;

#[test]
fn red_repro_subject_does_not_count_as_fix() {
    let fx = common::Fixture::new("k2-red-repro");
    let (ox, x) = fx.add_extra(
        "alpha parser drops trailing newline",
        &["--priority", "p0", "--repro-test", common::REPRO_YES],
    );
    let (oy, y) = fx.add_extra(
        "zulu renderer misaligns footer",
        &["--priority", "p2", "--repro-test", common::REPRO_YES],
    );
    let x = x.unwrap_or_else(|| panic!("setup: X not filed: {}", ox.both()));
    let _y = y.unwrap_or_else(|| panic!("setup: Y not filed: {}", oy.both()));
    fx.write("src/repro.txt", "red\n");
    fx.commit_paths(
        &["src/repro.txt"],
        &format!("test: alpha parser RED repro (backlog {x} RED repro)"),
    );
    let o = fx.run(&["next"]);
    assert_eq!(
        o.code,
        0,
        "`backlog next` should still dispatch X ({x}) after a RED-repro commit; got exit {}: {}",
        o.code,
        o.both()
    );
    assert!(
        o.stdout.contains(&x),
        "RED-repro subject was treated as a fix: `backlog next` did not hand out X ({x}). stdout: {}",
        o.stdout
    );
}
