//! Locate a built `blastguard` binary for overwatch's ask-learn tests.
//!
//! `CARGO_BIN_EXE_blastguard` is only defined inside the blastguard crate, and
//! overwatch cannot dev-depend on blastguard (blastguard already depends on the
//! overwatch library). So:
//!
//!   1. `ASK_LEARN_BLASTGUARD_BIN=<path>` — use that binary as-is (fast path,
//!      e.g. `target/debug/blastguard` after `cargo build -p blastguard`).
//!   2. Otherwise run `cargo build -p blastguard --bin blastguard` ONCE per test
//!      binary, into a SEPARATE target dir (`<workspace>/target/ask-learn-blastguard`)
//!      so the nested cargo never waits on the build-directory lock held by the
//!      outer `cargo test`, and read the executable path from
//!      `--message-format=json`. The first run compiles blastguard's dependency
//!      tree into that dir (slow once, cached after).
//!
//! A build failure panics with cargo's stderr: a missing binary is never
//! silently treated as "nothing to learn".

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root")
}

pub fn blastguard_bin() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Some(p) = std::env::var_os("ASK_LEARN_BLASTGUARD_BIN") {
            let p = PathBuf::from(p);
            assert!(p.is_file(), "ASK_LEARN_BLASTGUARD_BIN={} is not a file", p.display());
            return p;
        }
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let root = workspace_root();
        let out = Command::new(cargo)
            .current_dir(&root)
            .env("CARGO_TARGET_DIR", root.join("target").join("ask-learn-blastguard"))
            .args([
                "build",
                "-p",
                "blastguard",
                "--bin",
                "blastguard",
                "--message-format=json-render-diagnostics",
            ])
            .output()
            .expect("spawn cargo build -p blastguard");
        assert!(
            out.status.success(),
            "cargo build -p blastguard failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let exe = stdout
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["reason"] == "compiler-artifact" && v["target"]["name"] == "blastguard")
            .filter_map(|v| v["executable"].as_str().map(PathBuf::from))
            .last()
            .unwrap_or_else(|| panic!("no blastguard executable in cargo output:\n{stdout}"));
        assert!(exe.is_file(), "built blastguard missing at {}", exe.display());
        exe
    })
    .clone()
}
