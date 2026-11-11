// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Scout 2026-11-12 S2: an evidence CMD run by `fde evidence red` after
//! ratification can write outside the target root (e.g. into `$HOME`).
//!
//! The CMD is a plain `touch <abs path>` (no shell operators). fde runs it
//! with only cwd = target root (gate.rs:697, SKILL.md:109), nothing confines
//! the write. The fixture HOME is a temp dir outside the target root.

mod common;

use common::{show, Engagement, TempPlace};

#[test]
#[ignore = "scout 2026-11-12 S2: RED, open defect. Remove the ignore when fixed"]
fn evidence_red_cmd_must_not_write_outside_target_root() {
    let e = Engagement::ratified(&[("R1", "first")]);
    let home = TempPlace::new("fde-home-");

    // Control: a CMD touching a path INSIDE the root runs and creates its file.
    let inside = e.root().join("control-marker");
    let out = e.fde(&[
        "evidence",
        "red",
        "--req",
        "R1",
        "--cmd",
        &format!("touch {}", inside.display()),
    ]);
    assert!(
        inside.exists(),
        "control: CMD must have run (inside-root marker missing): {}",
        show(&out)
    );

    // RED: same shape of CMD aimed at a path under HOME, outside the root.
    let outside = home.path.join("escaped-marker");
    assert!(
        !outside.starts_with(e.root()),
        "fixture: marker is outside root"
    );
    let out = e.fde(&[
        "evidence",
        "red",
        "--req",
        "R1",
        "--cmd",
        &format!("touch {}", outside.display()),
    ]);
    assert!(
        !outside.exists(),
        "evidence CMD wrote outside the target root: {} exists\n{}",
        outside.display(),
        show(&out)
    );
}
