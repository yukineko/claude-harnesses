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
//!
//! Issue #1554 (user ruling 2026-11-08): where `chmod 000` does not make
//! resolution fail with EACCES (v9fs `/mnt/c`, root), the locked-ancestor row
//! neither passes nor skips; it fails with [`PRECONDITION_NOT_BUILDABLE`] and
//! the reason, distinct from its verdict assertion. That branch is exercised
//! here only with a simulated `NotFound`; a real v9fs run was not observed.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

#[path = "support/bounded.rs"]
mod bounded;

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::json;

const ABSENT_SYSTEM_FILE: &str = "/etc/blastguard-backlog-2287ebae.conf";

struct Fx {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
    /// Owns `base`: a unique directory per fixture instance, so concurrent
    /// runs of this binary never share (and clobber) one fixture path
    /// (backlog b6ce7f76). Removed on drop.
    _dir: tempfile::TempDir,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let parent = Path::new(env!("CARGO_TARGET_TMPDIR")).join("backlog_2287ebae");
        std::fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::Builder::new()
            .prefix(&format!("{name}-"))
            .tempdir_in(&parent)
            .unwrap();
        let base = dir.path().to_path_buf();
        let f = Fx {
            home: base.join("home"),
            proj: base.join("src/proj"),
            base,
            _dir: dir,
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
        let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        hook.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.proj)
            .env("HOME", &self.home)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDECODE", "1")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
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
    let precondition = precondition_failure(&probe);
    assert!(
        precondition.is_none(),
        "{}",
        precondition.unwrap_or_default()
    );
    assert!(
        !out.trim().is_empty(),
        "`{cmd}`: where it lands could not be resolved; got Allow"
    );
}

/// The fixed head of the failure message [`precondition_failure`] builds, so a
/// run that could not build the fixture reads differently from a run whose
/// verdict assertion failed (issue #1554, user ruling 2026-11-08).
const PRECONDITION_NOT_BUILDABLE: &str =
    "PRECONDITION NOT BUILDABLE HERE (this is not a blastguard verdict failure)";

/// Whether the "unsearchable ancestor" precondition of
/// [`unresolvable_real_landing_is_not_allowed`] was actually built, judged
/// from the result of resolving a path below a `chmod 000` directory.
#[derive(Debug, PartialEq, Eq)]
enum LockedAncestor {
    /// Resolution failed with `PermissionDenied`: the kernel really refused
    /// to search the directory, so the hook ran against the intended fixture.
    Built,
    /// Anything else, with why. Observed on v9fs (`/mnt/c`, WSL): `NotFound`,
    /// because the filesystem ignores `chmod 000` (issue #1554). As root the
    /// permission check is bypassed.
    NotBuildable(String),
}

fn classify_locked_ancestor(probe: &std::io::Result<PathBuf>) -> LockedAncestor {
    match probe {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => LockedAncestor::Built,
        Err(e) => LockedAncestor::NotBuildable(format!(
            "resolving a path below a `chmod 000` directory failed with {:?} ({e}) instead of \
             PermissionDenied, so the directory was still searchable: this filesystem does not \
             enforce POSIX permissions (e.g. v9fs under /mnt/c on WSL, issue #1554) or the test \
             runs as root. Put CARGO_TARGET_DIR on a POSIX filesystem and run as a normal user.",
            e.kind()
        )),
        Ok(p) => LockedAncestor::NotBuildable(format!(
            "resolving a path below a `chmod 000` directory succeeded ({}), so the directory was \
             still searchable: the test runs as root, or the filesystem does not enforce POSIX \
             permissions. Put CARGO_TARGET_DIR on a POSIX filesystem and run as a normal user.",
            p.display()
        )),
    }
}

/// Fail — never pass, never skip — when the precondition was not built, with a
/// message that says so instead of an assertion about the verdict.
/// `Some(message)` when the precondition was not built; the caller asserts it
/// is `None`, so such a run fails with this message instead of a verdict one.
fn precondition_failure(probe: &std::io::Result<PathBuf>) -> Option<String> {
    match classify_locked_ancestor(probe) {
        LockedAncestor::Built => None,
        LockedAncestor::NotBuildable(why) => Some(format!("{PRECONDITION_NOT_BUILDABLE}: {why}")),
    }
}

/// The v9fs result (#1554), simulated: this host cannot reproduce v9fs, so the
/// `NotFound` that `/mnt/c` produced is fed to the helper directly.
#[test]
fn simulated_v9fs_not_found_fails_as_precondition_not_buildable() {
    let probe: std::io::Result<PathBuf> = Err(std::io::ErrorKind::NotFound.into());
    let msg = precondition_failure(&probe).expect("NotFound must be a precondition failure");
    assert!(
        msg.starts_with("PRECONDITION NOT BUILDABLE HERE") && msg.contains("NotFound"),
        "{msg}"
    );
    let denied: std::io::Result<PathBuf> = Err(std::io::ErrorKind::PermissionDenied.into());
    assert_eq!(precondition_failure(&denied), None);
}

#[test]
fn locked_ancestor_classification() {
    let denied: std::io::Result<PathBuf> = Err(std::io::ErrorKind::PermissionDenied.into());
    assert_eq!(classify_locked_ancestor(&denied), LockedAncestor::Built);
    let not_found: std::io::Result<PathBuf> = Err(std::io::ErrorKind::NotFound.into());
    match classify_locked_ancestor(&not_found) {
        LockedAncestor::NotBuildable(why) => assert!(
            why.contains("NotFound") && why.contains("v9fs") && why.contains("root"),
            "the reason must name the observed error and the likely causes: {why}"
        ),
        LockedAncestor::Built => panic!("NotFound must not count as the precondition"),
    }
    let resolved: std::io::Result<PathBuf> = Ok(PathBuf::from("/x/locked/sub/new.log"));
    assert!(matches!(
        classify_locked_ancestor(&resolved),
        LockedAncestor::NotBuildable(_)
    ));
}
