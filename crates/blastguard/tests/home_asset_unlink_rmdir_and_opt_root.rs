#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Coverage-gap tests written by an agent that did not write the code under
//! test (CLAUDE.md section 2(a)).
//!
//!   * `home_asset_deny` is reached from `analyze_unlink_rmdir` as well as from
//!     `analyze_rm`, but only `rm` spellings were pinned. `unlink` / `rmdir` of
//!     an irreplaceable `$HOME` asset must be Deny exactly like `rm`.
//!   * `system_path_block` exempts a target the session legitimately owns via
//!     `confined_root`: a project checked out at `/opt/myproj` is a safe root,
//!     while a sibling `/opt/other` is still a system-directory write.
//!
//! Every Deny test is paired with an Allow control on the same code path, so a
//! pass cannot be "everything is denied".
//!
//! Fixture note (user ruling 12, 2026-11-17, SUBSET.md): `~` / `$HOME` /
//! `${HOME}` resolve to the hook process env HOME. Production builds
//! `SafeRoots` home from that same HOME (`main.rs` `safe_roots`), so this
//! fixture takes HOME from the process env instead of a hard-coded
//! `/home/yuki` (which diverged from the process HOME and made `~/.gnupg`
//! not an asset of the fixture home). Assertions are unchanged.

use blastguard::detect;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

const OPT_PROJECT: &str = "/opt/myproj";

fn home() -> String {
    std::env::var("HOME").expect("HOME must be set: production safe_roots builds home from it")
}

fn project_dir() -> String {
    format!("{}/proj", home())
}

fn identity(p: &str) -> Option<String> {
    Some(p.to_string())
}

fn roots(project: &str) -> SafeRoots {
    let home = home();
    SafeRoots::new(
        Some(project),
        Some(project),
        Some(home.as_str()),
        None,
        Some(identity),
    )
}

fn scoped(project: &str, cmd: &str) -> Decision {
    detect::detect_scoped("Bash", Some(&json!({ "command": cmd })), &roots(project))
}

fn is_deny(d: &Decision) -> bool {
    matches!(d, Decision::Deny(_))
}

// ---------------------------------------------------------------- (a) ----

/// Baseline: `rm` of a home asset is Deny. If this fails the unlink/rmdir
/// tests below prove nothing about the twin.
#[test]
fn control_rm_home_assets_are_deny() {
    let h = home();
    for cmd in [
        format!("rm {h}/.ssh/id_rsa"),
        "rm ~/.ssh/id_rsa".to_string(),
        format!("rm {h}/.zshrc"),
        "rm $HOME/.gitconfig".to_string(),
    ] {
        let d = scoped(&project_dir(), &cmd);
        assert!(is_deny(&d), "control: `{cmd}` must be Deny; got {d:?}");
    }
}

/// Control: ordinary non-asset targets stay Allow on the unlink/rmdir path.
#[test]
fn control_unlink_rmdir_ordinary_targets_are_allow() {
    let h = home();
    for cmd in [
        format!("unlink {h}/proj/scratch.txt"),
        format!("rmdir {h}/proj/emptydir"),
        format!("unlink {h}/notes.txt"),
    ] {
        let d = scoped(&project_dir(), &cmd);
        assert!(
            matches!(d, Decision::Allow),
            "control: `{cmd}` must stay Allow; got {d:?}"
        );
    }
}

#[test]
fn unlink_home_asset_is_deny() {
    let h = home();
    for cmd in [
        format!("unlink {h}/.ssh/id_rsa"),
        "unlink ~/.ssh/id_rsa".to_string(),
        "unlink $HOME/.ssh/id_rsa".to_string(),
        "unlink ${HOME}/.gnupg/pubring.kbx".to_string(),
        format!("unlink {h}/.zshrc"),
        "unlink ~/.gitconfig".to_string(),
    ] {
        let d = scoped(&project_dir(), &cmd);
        assert!(
            is_deny(&d),
            "`{cmd}` deletes an irreplaceable home asset and must be Deny like `rm`; got {d:?}"
        );
    }
}

#[test]
fn rmdir_home_asset_dir_is_deny() {
    let h = home();
    for cmd in [
        format!("rmdir {h}/.ssh"),
        "rmdir ~/.gnupg".to_string(),
        "rmdir $HOME/.ssh/keys".to_string(),
        "rmdir ~/.config".to_string(),
        format!("rmdir {h}/.cache"),
    ] {
        let d = scoped(&project_dir(), &cmd);
        assert!(
            is_deny(&d),
            "`{cmd}` removes an irreplaceable home asset dir and must be Deny; got {d:?}"
        );
    }
}

// ---------------------------------------------------------------- (b) ----

/// The exempt side: with the session project at `/opt/myproj`, writes strictly
/// inside it are not a system-directory write.
#[test]
fn opt_project_write_inside_session_root_is_exempt() {
    for cmd in [
        "echo x > /opt/myproj/out.txt",
        "cp a.txt /opt/myproj/ok.txt",
        "echo x > /opt/myproj/sub/out.txt",
    ] {
        let d = scoped(OPT_PROJECT, cmd);
        assert!(
            !is_deny(&d),
            "`{cmd}` is inside the session's own /opt project and must not be \
             denied as a system-directory write; got {d:?}"
        );
    }
}

/// The non-exempt side: a sibling under `/opt`, or `/opt` itself, is still a
/// system-directory write even when the session lives at `/opt/myproj`.
#[test]
fn opt_sibling_write_is_still_system_dir_deny() {
    for cmd in [
        "echo x > /opt/other/out.txt",
        "cp a.txt /opt/other/ok.txt",
        "echo x > /opt/evil.sh",
        "echo x > /opt/myproj2/out.txt",
    ] {
        let d = scoped(OPT_PROJECT, cmd);
        assert!(
            is_deny(&d),
            "`{cmd}` is outside the session's /opt project and must be Deny; got {d:?}"
        );
    }
}

/// Without a session root at `/opt/myproj`, the same in-`/opt` write is Deny:
/// the exemption comes from the session's root, not from the path text.
#[test]
fn opt_write_without_session_root_is_deny() {
    let project = project_dir();
    for cmd in [
        "echo x > /opt/myproj/out.txt",
        "cp a.txt /opt/myproj/ok.txt",
    ] {
        let d = scoped(&project, cmd);
        assert!(
            is_deny(&d),
            "`{cmd}` with project at {project} must be Deny; got {d:?}"
        );
    }
}
