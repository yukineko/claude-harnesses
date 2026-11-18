#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-11-19: for every SUBSET.md Table B row, an unquoted glob
//! whose prefix up to the last `/` is a literal directory (no `..`, no
//! expansion) with the glob only in the FINAL component is placed by the
//! parent directory's Table C class. Written by an agent that did not write
//! the implementation (CLAUDE.md 2(a)).
//!
//! `allow_*` pin the ruling; `deny_*` are controls (glob in the middle, `..`,
//! expansion, short head, protected parents) that must stay not-Allow.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use blastguard::detect::detect_scoped;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

fn real_path(p: &str) -> Option<String> {
    std::fs::canonicalize(p)
        .ok()
        .map(|c| c.to_string_lossy().into_owned())
}

struct Fx {
    _dir: tempfile::TempDir,
    cwd: String,
}

impl Fx {
    fn new() -> Fx {
        let dir = tempfile::Builder::new()
            .prefix("bg-lit-glob-")
            .tempdir_in(neutral_base::neutral_base())
            .unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), b"").unwrap();
        std::fs::write(root.join("x.o"), b"").unwrap();
        Fx {
            cwd: root.to_string_lossy().into_owned(),
            _dir: dir,
        }
    }

    fn run(&self, cmd: &str) -> Decision {
        let scope = SafeRoots::new(
            Some(&self.cwd),
            Some(&self.cwd),
            None,
            None,
            Some(real_path),
        );
        detect_scoped("Bash", Some(&json!({ "command": cmd })), &scope)
    }

    fn allow(&self, cmd: &str) {
        let d = self.run(cmd);
        assert_eq!(d, Decision::Allow, "expected Allow for `{cmd}`, got {d:?}");
    }

    fn not_allow(&self, cmd: &str) {
        let d = self.run(cmd);
        assert_ne!(d, Decision::Allow, "expected NOT Allow for `{cmd}`");
    }
}

#[test]
fn allow_rm_tmp_long_head_glob() {
    Fx::new().allow("rm -rf /tmp/zqxv1234*");
}

#[test]
fn allow_chown_src_glob() {
    Fx::new().allow("chown yuki src/*.rs");
}

#[test]
fn allow_chmod_src_glob() {
    Fx::new().allow("chmod 644 src/*.rs");
}

#[test]
fn allow_rm_glob_in_cwd() {
    Fx::new().allow("rm *.o");
}

#[test]
fn allow_cp_src_glob_to_tmp_dir() {
    Fx::new().allow("cp src/*.rs /tmp/zqxvout/");
}

#[test]
fn deny_glob_in_middle_component() {
    Fx::new().not_allow("rm -rf /tmp/*/x");
}

#[test]
fn deny_dotdot_glob() {
    Fx::new().not_allow("rm -rf ../*");
}

#[test]
fn deny_expansion_dir_glob() {
    Fx::new().not_allow("rm -rf $D/*");
}

#[test]
fn deny_short_head_glob() {
    Fx::new().not_allow("rm -rf /tmp/ab*");
}

#[test]
fn deny_chmod_etc_glob() {
    Fx::new().not_allow("chmod 777 /etc/*");
}

#[test]
fn deny_rm_home_glob() {
    Fx::new().not_allow("rm -rf ~/*");
}

#[test]
fn deny_rm_root_glob() {
    Fx::new().not_allow("rm -rf /*");
}
