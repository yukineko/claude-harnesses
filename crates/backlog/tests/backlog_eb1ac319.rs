//! backlog eb1ac319 (dups 966da0dd / fb178a29 / 2f740852): the empty
//! `crates/backlog/src/lib.rs` is not rustfmt-clean — `rustfmt --check`
//! reports a diff at line 1, so `cargo fmt --all -- --check` is red on main.
use std::path::Path;
use std::process::Command;

#[test]
fn backlog_lib_rs_is_rustfmt_clean() {
    let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
    let out = Command::new("rustfmt")
        .args(["--check", "--edition", "2021"])
        .arg(&lib)
        .output()
        .expect("rustfmt runs (rustup toolchain on PATH)");
    assert!(
        out.status.code().is_some(),
        "rustfmt was killed by a signal"
    );
    assert!(
        out.status.success(),
        "rustfmt --check reports a diff in {}:\n{}{}",
        lib.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
