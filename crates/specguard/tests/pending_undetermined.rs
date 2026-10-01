// このファイルは丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED-WRITER: written before the fix by an agent that does not implement it (backlog f49e4a72)
//!
//! `specguard pending` is the SessionStart hook. `pending()` in `src/main.rs`
//! is `let Ok(l) = load(cli) else { return EXIT_OK; };` — a config that EXISTS
//! but cannot be loaded makes it print nothing, which a human reads as "no
//! drift pending". That is "cannot determine" resolved to "fine" (CLAUDE.md
//! section 3). The sibling branch in `render_pending` states the rule itself:
//! "the one thing we must not do is print nothing".
//!
//! Asserted contract (deliberately loose, the implementer invents the wording):
//! stdout is non-empty and makes no clean claim. Exit code is not constrained.
//!
//! Control: NO config file at all. `load` returns the same `Err` for a missing
//! file as for a broken one (`Config::load` -> "reading config ..."), so the
//! code cannot tell them apart today; it prints nothing, exit 0. That case is
//! "not a specguard project" (the hook is installed globally-ish and fires in
//! repos that never opted in), so there is no pending state to determine and
//! silence is correct. A fix must separate "file absent" from "file present
//! but unloadable"; this control pins the former so it cannot change by accident.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn project(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("pending_undet_{name}"));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Runs exactly as the hook does: `specguard pending`, default `--config`.
fn pending(dir: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(dir)
        .arg("pending")
        .output()
        .expect("specguard spawns")
}

fn valid_config(template_line: &str) -> String {
    format!(
        r#"
[project]
name = "Demo"
root = "."

[agent]
command = "bash"
args = ["-c", "true"]

[prompt]
{template_line}

[[area]]
name = "src"
globs = ["src/**"]
canon = ["docs/spec.md"]
"#
    )
}

/// Non-empty stdout that does not claim a clean state.
fn assert_surfaces_undetermined(o: &Output, what: &str) {
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        !out.trim().is_empty(),
        "{what}: `specguard pending` printed NOTHING on stdout (exit {:?}, stderr {:?}); \
         silence reads as \"no drift pending\" — cannot-determine must be surfaced",
        o.status.code(),
        String::from_utf8_lossy(&o.stderr)
    );
    let lower = out.to_lowercase();
    for clean in ["no drift", "no pending", "問題なし", "ドリフトなし"] {
        assert!(
            !lower.contains(clean),
            "{what}: stdout claims a clean state ({clean:?}): {out}"
        );
    }
}

#[test]
fn unparseable_config_is_surfaced_not_silent() {
    let d = project("unparseable");
    fs::write(d.join("specguard.toml"), "this is = = not [ toml\n").unwrap();
    assert_surfaces_undetermined(&pending(&d), "unparseable config");
}

#[test]
fn unreadable_config_is_surfaced_not_silent() {
    let d = project("unreadable");
    // A directory where the file should be: read_to_string fails (EISDIR).
    fs::create_dir(d.join("specguard.toml")).unwrap();
    assert_surfaces_undetermined(&pending(&d), "unreadable config (directory)");
}

#[test]
fn unreadable_template_is_surfaced_not_silent() {
    let d = project("template");
    // `load` -> `load_template` propagates the read error, so this is a load
    // failure (not "not a specguard project"): the config is valid and present.
    fs::write(
        d.join("specguard.toml"),
        valid_config("template = \"missing-template.md\""),
    )
    .unwrap();
    assert_surfaces_undetermined(&pending(&d), "unreadable template");
}

#[test]
fn control_no_config_at_all_stays_silent_exit_ok() {
    let d = project("noconfig");
    let o = pending(&d);
    assert_eq!(o.status.code(), Some(0), "no config: exit must stay 0");
    assert!(
        o.stdout.is_empty(),
        "no config = not a specguard project; must stay silent, got: {}",
        String::from_utf8_lossy(&o.stdout)
    );
}
