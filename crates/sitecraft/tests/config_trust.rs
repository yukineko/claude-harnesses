#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Black-box tests for `sitecraft.toml` loading and content-pinned trust.
//! Every test pins SITECRAFT_STATE_DIR to a tempdir (under a lock, since the
//! environment is process-wide) and derives the state dir via `state_dir()`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sitecraft::config::{self, Loaded};
use sitecraft::trust::{self, Trust};
use sitecraft::{state_dir, Determination, ENV_STATE_DIR};

static ENV_LOCK: Mutex<()> = Mutex::new(());

const VALID: &str = r#"
command = ["npm", "run", "dev", "--", "--port", "5173"]
port = 5173
ready_path = "/health"
ready_timeout_secs = 30
pages = ["/", "/about"]
build_dir = "dist"
web_globs = ["src/**", "index.html"]
"#;

/// Pin the state dir to a fresh tempdir; returns (guard, tempdir, state, root).
fn setup() -> (
    std::sync::MutexGuard<'static, ()>,
    tempfile::TempDir,
    PathBuf,
    PathBuf,
) {
    let g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let td = tempfile::tempdir().unwrap();
    let st = td.path().join("state");
    std::env::set_var(ENV_STATE_DIR, &st);
    let root = td.path().join("proj");
    fs::create_dir_all(&root).unwrap();
    let state = match state_dir() {
        Determination::Known(p) => p,
        other => panic!("state_dir not pinned: {other:?}"),
    };
    assert_eq!(state, st);
    (g, td, state, root)
}

fn write_cfg(root: &Path, s: &str) -> PathBuf {
    let p = config::path_for(root);
    fs::write(&p, s).unwrap();
    p
}

fn undetermined(root: &Path) -> bool {
    matches!(config::load(root), Loaded::Undetermined(_))
}

#[test]
fn config_missing_file_is_absent() {
    let (_g, _td, _st, root) = setup();
    assert!(matches!(config::load(&root), Loaded::Absent));
}

#[test]
fn config_valid_toml_is_config_with_fields_and_bytes() {
    let (_g, _td, _st, root) = setup();
    write_cfg(&root, VALID);
    match config::load(&root) {
        Loaded::Config(c, bytes) => {
            assert_eq!(c.command[0], "npm");
            assert_eq!(c.command.len(), 6);
            assert_eq!(c.port, 5173);
            assert_eq!(c.ready_path, "/health");
            assert_eq!(c.ready_timeout_secs, 30);
            assert_eq!(c.pages, vec!["/", "/about"]);
            assert_eq!(c.build_dir, "dist");
            assert_eq!(c.web_globs, vec!["src/**", "index.html"]);
            assert_eq!(c.max_attempts, 3);
            assert_eq!(c.reset_after_secs, 1800);
            assert_eq!(bytes, VALID.as_bytes());
        }
        other => panic!("expected Config, got {other:?}"),
    }
}

#[test]
fn config_defaults_apply() {
    let (_g, _td, _st, root) = setup();
    write_cfg(
        &root,
        "command=[\"x\"]\nport=1\npages=[\"/\"]\nbuild_dir=\"d\"\nweb_globs=[\"*\"]\n",
    );
    match config::load(&root) {
        Loaded::Config(c, _) => {
            assert_eq!(c.ready_path, "/");
            assert_eq!(c.ready_timeout_secs, 60);
        }
        other => panic!("expected Config, got {other:?}"),
    }
}

#[test]
fn config_syntax_error_is_undetermined() {
    let (_g, _td, _st, root) = setup();
    write_cfg(&root, "command = [\"x\"\nport = = 1\n");
    assert!(undetermined(&root));
}

#[test]
fn config_empty_command_is_undetermined() {
    let (_g, _td, _st, root) = setup();
    write_cfg(
        &root,
        &VALID.replace(
            r#"command = ["npm", "run", "dev", "--", "--port", "5173"]"#,
            "command = []",
        ),
    );
    assert!(undetermined(&root));
}

#[test]
fn config_empty_pages_is_undetermined() {
    let (_g, _td, _st, root) = setup();
    write_cfg(
        &root,
        &VALID.replace(r#"pages = ["/", "/about"]"#, "pages = []"),
    );
    assert!(undetermined(&root));
}

#[test]
fn config_unreadable_file_is_undetermined_not_absent() {
    let (_g, _td, _st, root) = setup();
    // A directory where the file should be: exists, but read fails (not NotFound).
    fs::create_dir(config::path_for(&root)).unwrap();
    assert!(undetermined(&root));
}

#[test]
fn trust_unrecorded_is_untrusted() {
    let (_g, _td, st, root) = setup();
    let p = write_cfg(&root, VALID);
    let r = trust::check_bytes(&st, &root, &p, VALID.as_bytes());
    assert!(matches!(r, Trust::Untrusted(_)), "got {r:?}");
}

#[test]
fn trust_recorded_same_bytes_is_trusted() {
    let (_g, _td, st, root) = setup();
    let p = write_cfg(&root, VALID);
    trust::record(&st, &root, &p).expect("record should succeed");
    assert_eq!(
        trust::check_bytes(&st, &root, &p, VALID.as_bytes()),
        Trust::Trusted
    );
}

#[test]
fn trust_one_byte_change_is_untrusted() {
    let (_g, _td, st, root) = setup();
    let p = write_cfg(&root, VALID);
    trust::record(&st, &root, &p).expect("record should succeed");
    let mut changed = VALID.as_bytes().to_vec();
    changed.push(b' ');
    let r = trust::check_bytes(&st, &root, &p, &changed);
    assert!(matches!(r, Trust::Untrusted(_)), "got {r:?}");
}

#[test]
fn trust_corrupt_store_is_undetermined_and_record_does_not_overwrite() {
    let (_g, _td, st, root) = setup();
    let p = write_cfg(&root, VALID);
    fs::create_dir_all(&st).unwrap();
    let store = st.join(trust::STORE);
    let garbage = b"{ this is not json";
    fs::write(&store, garbage).unwrap();

    let r = trust::check_bytes(&st, &root, &p, VALID.as_bytes());
    assert!(matches!(r, Trust::Undetermined(_)), "got {r:?}");

    assert!(trust::record(&st, &root, &p).is_err(), "record must Err");
    assert_eq!(
        fs::read(&store).unwrap(),
        garbage,
        "store must not be overwritten"
    );
}
