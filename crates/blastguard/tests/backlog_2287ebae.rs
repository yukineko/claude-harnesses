//! Backlog 2287ebae (dup 725dd715 / eb93be49): `redirect_target_suffix_resolution`'s
//! two symlink rows are red on macOS, where `/etc/fstab` does not exist.
//!
//! Re-measured 2026-10-15 at main e1aca620: the two rows fail for TWO
//! different reasons, and only one of them is the fixture.
//!
//!   * `symlink_before_dotdot_in_a_suffix_must_not_be_resolved` panics inside
//!     its fixture check (`canonicalize` of `esc/../../etc/fstab` -> ENOENT):
//!     a host-file dependency.
//!   * `symlink_without_a_dotdot_is_caught_by_the_symlink_axis` gets a real
//!     `Allow` from the hook. That row passes on Linux only because
//!     `/etc/fstab` EXISTS there: a redirect through an escaping symlink to an
//!     ABSENT file in a system directory is Allow, while the literal spelling
//!     of the same path is Deny ("a NEW file there is itself the change").
//!     So "make the fixture create its own target" would hide a fail-open.
//!
//! This file pins the second reason without depending on any host file: it
//! names a file under `/etc` that the test first proves absent.
//!
//! Repro: `cargo test -p blastguard --test backlog_2287ebae` (was `#[ignore]`d
//! RED until the redirect axes judged the real landing, `detect::physical_landing`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::io::Write as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::json;

const ABSENT_SYSTEM_FILE: &str = "/etc/blastguard-backlog-2287ebae.conf";

struct Fx {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("backlog_2287ebae")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let f = Fx {
            home: base.join("home"),
            proj: base.join("src/proj"),
            base,
        };
        std::fs::create_dir_all(&f.home).unwrap();
        std::fs::create_dir_all(&f.proj).unwrap();
        // `esc` -> `/`: `<proj>/esc/etc/x` IS `/etc/x` to the kernel.
        symlink("/", f.proj.join("esc")).unwrap();
        f
    }

    /// Empty stdout is this hook's `Allow`.
    fn verdict(&self, command: &str) -> String {
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut child = Command::new(env!("CARGO_BIN_EXE_blastguard"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.proj)
            .env("HOME", &self.home)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base)
            .spawn()
            .expect("binary spawns");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        let out = child.wait_with_output().expect("binary runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// Control: the literal spelling of the absent system file is refused, and an
/// EXISTING system file through the same symlink is refused — so the fixture
/// is wired and the hook does treat a new `/etc` file as a change.
#[test]
fn controls_literal_absent_system_file_and_existing_file_through_symlink_are_refused() {
    let f = Fx::new("control");
    assert!(
        !Path::new(ABSENT_SYSTEM_FILE).exists(),
        "fixture: {ABSENT_SYSTEM_FILE} must not exist on this host"
    );
    let literal = f.verdict(&format!(": > {ABSENT_SYSTEM_FILE}"));
    assert!(
        literal.contains("\"deny\"") || literal.contains("\"ask\""),
        "literal `: > {ABSENT_SYSTEM_FILE}` must be refused; got {literal:?}"
    );
    let existing = f.verdict(&format!(": > {}/esc/etc/hosts", f.proj.display()));
    assert!(
        !existing.trim().is_empty(),
        "an existing /etc file through the symlink must be refused; got Allow"
    );
}

/// THE DEFECT: a new file in `/etc` named through an escaping symlink is Allow.
#[test]
fn absent_system_file_through_escaping_symlink_is_not_allowed() {
    let f = Fx::new("hazard");
    let through = f
        .proj
        .join("esc")
        .join(ABSENT_SYSTEM_FILE.trim_start_matches('/'));
    assert!(
        !through.exists(),
        "fixture: {} must not exist",
        through.display()
    );
    assert_eq!(
        std::fs::canonicalize(through.parent().unwrap()).unwrap(),
        std::fs::canonicalize("/etc").unwrap(),
        "fixture: the parent must really be /etc to the kernel"
    );
    let cmd = format!(": > {}", through.display());
    let out = f.verdict(&cmd);
    assert!(
        !out.trim().is_empty(),
        "`{cmd}` creates {ABSENT_SYSTEM_FILE} through a symlink; the literal \
         spelling is refused, so this must NOT be Allow; got empty output (= Allow)"
    );
}

/// The APPEND twin: `>>` through the same symlink creates the same new file.
#[test]
fn append_to_absent_system_file_through_escaping_symlink_is_not_allowed() {
    let f = Fx::new("append");
    let through = f
        .proj
        .join("esc")
        .join(ABSENT_SYSTEM_FILE.trim_start_matches('/'));
    assert!(
        !through.exists(),
        "fixture: {} must not exist",
        through.display()
    );
    let cmd = format!("echo x >> {}", through.display());
    let out = f.verdict(&cmd);
    assert!(
        !out.trim().is_empty(),
        "`{cmd}` appends a new {ABSENT_SYSTEM_FILE} through a symlink; got Allow"
    );
}

/// Control: an ordinary new file inside the project (no symlink) stays Allow,
/// so the rows above are not passing because every new file is now refused.
#[test]
fn control_new_file_inside_the_project_stays_allowed() {
    let f = Fx::new("ordinary");
    for cmd in [
        format!(": > {}/new.log", f.proj.display()),
        format!("echo x >> {}/new.log", f.proj.display()),
    ] {
        let out = f.verdict(&cmd);
        assert!(
            out.trim().is_empty(),
            "`{cmd}` must stay Allow; got {out:?}"
        );
    }
}

/// Undetermined real landing: an ancestor that cannot be searched means the
/// kernel's answer is unknown, which must not read as "not a system directory".
#[test]
fn unresolvable_real_landing_is_not_allowed() {
    use std::os::unix::fs::PermissionsExt as _;
    let f = Fx::new("locked");
    let locked = f.proj.join("locked");
    std::fs::create_dir_all(locked.join("sub")).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let probe = std::fs::canonicalize(locked.join("sub/new.log"));
    let cmd = format!(": > {}/sub/new.log", locked.display());
    let out = f.verdict(&cmd);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let kind = probe.as_ref().err().map(std::io::Error::kind);
    assert_eq!(
        kind,
        Some(std::io::ErrorKind::PermissionDenied),
        "fixture: the locked ancestor must make resolution fail with EACCES (running as root?)"
    );
    assert!(
        !out.trim().is_empty(),
        "`{cmd}`: where it lands could not be resolved; got Allow"
    );
}
