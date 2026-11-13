#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 2026-11-13 FAIL-OPEN: a `cp` / `mv` / `install` DESTINATION spelled through
//! a variable is not judged at all.
//!
//! Observed on the deployed 0.2.496 binary:
//! - deny: `cp /dev/null /etc/hosts`, `cp README.md /etc/hosts`,
//!   `mv README.md /etc/hosts`, `install README.md /etc/hosts`, each with
//!   "'<cmd> destination /etc/hosts' writes into a system directory - a NEW
//!   file there is itself the change ... [system harm: ...".
//! - allow: `L=/etc/hosts; cp /dev/null $L`, `L=/etc/hosts; cp README.md $L`,
//!   `L=/etc/hosts; cp README.md "$L"`, `L=/etc; cp README.md $L/hosts`,
//!   `L=/etc/hosts; mv README.md $L`, `L=/etc/hosts; install README.md $L`,
//!   and the unresolvable `cp README.md $L` / `mv README.md $L`.
//!
//! The same variable IS resolved for a redirect (`L=/etc/hosts; echo x > $L`
//! is denied) and for `rm` (`L=/etc/hosts; rm -f $L` is denied, a bare
//! `rm -f $L` is denied as unresolvable). So the system-path overwrite the
//! literal form refuses is allowed as soon as the destination is `$L`.
//!
//! RED: `r1`..`r6` expect NOT Allow and fail at 8002d4c5. Controls: `c1` (the
//! literal forms) stay not Allow; `c2`/`c3` (a project scratch destination,
//! through a variable and literally) are Allow, so the RED is about the
//! variable destination, not about `cp` being refused wholesale.
//!
//! DOES NOT PROVE: `cp -t $DIR`, `rsync`, `dd of=$L`, `ln -sf`, or `$L`
//! set by `export` / `read` / command substitution.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let base = neutral_base::neutral_base()
            .join("cp_variable_destination_fail_open_20261113")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(proj.join("scratch")).unwrap();
        std::fs::write(proj.join("notes.txt"), "n\n").unwrap();
        Fx { base, home, proj }
    }

    fn safe(&self) -> String {
        self.proj.join("scratch/out.txt").display().to_string()
    }

    fn bash(&self, command: &str) -> &'static str {
        let payload = serde_json::json!({
            "session_id": "cp-variable-destination-fail-open-20261113",
            "hook_event_name": "PreToolUse",
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.proj)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDECODE", "1")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        let v = verdict(&s);
        println!("[{command}] => {v} {:?}", s.trim());
        v
    }

    fn assert_not_allowed(&self, cmd: &str, why: &str) {
        let v = self.bash(cmd);
        assert_ne!(v, "allow", "{why}: {cmd}");
        assert_ne!(v, "other", "unrecognised hook output: {cmd}");
    }
}

fn verdict(out: &str) -> &'static str {
    if out.trim().is_empty() {
        "allow"
    } else if out.contains(r#""permissionDecision":"deny""#) {
        "deny"
    } else if out.contains(r#""permissionDecision":"ask""#) {
        "ask"
    } else {
        "other"
    }
}

const RED: &str = "the literal form of this overwrite of /etc/hosts is refused";

#[test]
#[ignore = "RED 2026-11-13: cp destination through a variable is not judged; overwrites /etc/hosts"]
fn r1_cp_into_variable_system_path_is_not_allowed() {
    let f = Fx::new("r1");
    f.assert_not_allowed("L=/etc/hosts; cp notes.txt $L", RED);
}

#[test]
#[ignore = "RED 2026-11-13: cp destination through a variable is not judged; overwrites /etc/hosts"]
fn r2_cp_dev_null_into_variable_system_path_is_not_allowed() {
    let f = Fx::new("r2");
    f.assert_not_allowed("L=/etc/hosts; cp /dev/null $L", RED);
}

#[test]
#[ignore = "RED 2026-11-13: cp destination through a variable is not judged; overwrites /etc/hosts"]
fn r3_cp_into_variable_system_dir_plus_suffix_is_not_allowed() {
    let f = Fx::new("r3");
    f.assert_not_allowed("L=/etc; cp notes.txt $L/hosts", RED);
}

#[test]
#[ignore = "RED 2026-11-13: mv destination through a variable is not judged; overwrites /etc/hosts"]
fn r4_mv_into_variable_system_path_is_not_allowed() {
    let f = Fx::new("r4");
    f.assert_not_allowed("L=/etc/hosts; mv notes.txt $L", RED);
}

#[test]
#[ignore = "RED 2026-11-13: install destination through a variable is not judged; overwrites /etc/hosts"]
fn r5_install_into_variable_system_path_is_not_allowed() {
    let f = Fx::new("r5");
    f.assert_not_allowed("L=/etc/hosts; install notes.txt $L", RED);
}

#[test]
#[ignore = "RED 2026-11-13: an unresolvable cp destination is allowed (bare `rm -f $L` is not)"]
fn r6_cp_into_unresolvable_variable_is_not_allowed() {
    let f = Fx::new("r6");
    f.assert_not_allowed(
        "cp notes.txt $L",
        "the destination cannot be resolved, so the verdict cannot be Allow",
    );
}

#[test]
fn c1_control_literal_system_destinations_stay_not_allowed() {
    let f = Fx::new("c1");
    for cmd in [
        "cp notes.txt /etc/hosts",
        "cp /dev/null /etc/hosts",
        "mv notes.txt /etc/hosts",
        "install notes.txt /etc/hosts",
    ] {
        f.assert_not_allowed(cmd, "a literal overwrite of /etc/hosts must not be Allow");
    }
}

#[test]
fn c2_control_safe_variable_destination_is_allowed() {
    let f = Fx::new("c2");
    let cmd = format!("L={}; cp notes.txt $L", f.safe());
    assert_eq!(f.bash(&cmd), "allow", "control must be Allow: {cmd}");
}

#[test]
fn c3_control_safe_literal_destination_is_allowed() {
    let f = Fx::new("c3");
    let cmd = format!("cp notes.txt {}", f.safe());
    assert_eq!(f.bash(&cmd), "allow", "control must be Allow: {cmd}");
}
