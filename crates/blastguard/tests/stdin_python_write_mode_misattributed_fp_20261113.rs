#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 2026-11-13 false positive: in a Python program read from a quoted
//! here-document (`python3 -I - <<'PYEOF'`), the `"w"` mode of one `open(...)`
//! is attributed to an UNRELATED absolute-path string literal elsewhere in the
//! body.
//!
//! Observed in a session (deny), then reproduced on the deployed 0.2.496 binary
//! with `p = "$HOME/.cargo/env"` (an existing file outside any git tree):
//! - deny: `python3 -I - <<'PYEOF'` / `p = "<ro>"` / `print(open(p).read())` /
//!   `open("<project file>", "w").write("y")` / `PYEOF`, reason
//!   "`python3 -I - <<'PYEOF'` takes its program from stdin, and it writes
//!   `<ro>` (`, \"w\")`) — it exists, and it is not inside a git work tree —
//!   nothing holds a second copy of these bytes".
//! - deny (minimal): the `open(p)` read is not needed — `p = "<ro>"` plus any
//!   `open("<project file>", "w")` is enough; so is `print("<ro>")`, and so is
//!   `q = "<project file>"` / `open(q, "w")`.
//! - allow: the same body with no `"w"` open; `open("<project file>", "w")`
//!   alone.
//!
//! The only file the program writes is `<project file>`, inside the project
//! tree, so the verdict a correct attribution gives is the verdict of the write
//! alone: Allow.
//!
//! RED: `r1`/`r2` expect Allow and fail at 1a519d22. Controls: `c1` (no `"w"`)
//! and `c2` (the project write alone) are Allow at HEAD; `c3`/`c4` (a real
//! `open("/etc/hosts", "w")`, with and without the read-only literal) must stay
//! NOT Allow, so a fix that simply stops scanning stdin programs is caught.
//!
//! In this fixture (neutral base) the RED verdict is Ask, not Deny, but the
//! reason still names `home/notes.txt` (the READ-ONLY file) as the write
//! target — the misattribution is the same.
//!
//! DOES NOT PROVE: unquoted here-docs, piped programs (`echo ... | python3`),
//! `open(..., mode="w")` keyword spelling, or `pathlib` writes.

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
            .join("stdin_python_write_mode_misattributed_fp_20261113")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("notes.txt"), "keep me\n").unwrap();
        std::fs::create_dir_all(proj.join("scratch")).unwrap();
        Fx { base, home, proj }
    }

    /// An existing file outside any git work tree, only ever READ.
    fn ro(&self) -> String {
        self.home.join("notes.txt").display().to_string()
    }

    /// The file the program actually writes: inside the project tree.
    fn out(&self) -> String {
        self.proj.join("scratch/out.txt").display().to_string()
    }

    fn bash(&self, command: &str) -> &'static str {
        let payload = serde_json::json!({
            "session_id": "stdin-python-write-mode-misattributed-fp-20261113",
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

fn py(body: &str) -> String {
    format!("python3 -I - <<'PYEOF'\n{body}\nPYEOF")
}

#[test]
#[ignore = "RED 2026-11-13: stdin python program's \"w\" mode is attributed to an unrelated read-only path literal"]
fn r1_observed_shape_read_one_file_write_another_is_allowed() {
    let f = Fx::new("r1");
    let cmd = py(&format!(
        "p = \"{}\"\nprint(open(p).read())\nopen(\"{}\", \"w\").write(\"y\")",
        f.ro(),
        f.out()
    ));
    assert_eq!(
        f.bash(&cmd),
        "allow",
        "only the project file is written: {cmd}"
    );
}

#[test]
#[ignore = "RED 2026-11-13: stdin python program's \"w\" mode is attributed to an unrelated read-only path literal"]
fn r2_minimal_unrelated_path_literal_plus_project_write_is_allowed() {
    let f = Fx::new("r2");
    let cmd = py(&format!(
        "p = \"{}\"\nopen(\"{}\", \"w\").write(\"y\")",
        f.ro(),
        f.out()
    ));
    assert_eq!(
        f.bash(&cmd),
        "allow",
        "only the project file is written: {cmd}"
    );
}

#[test]
fn c1_control_same_read_without_any_write_is_allowed() {
    let f = Fx::new("c1");
    let cmd = py(&format!("p = \"{}\"\nprint(open(p).read())", f.ro()));
    assert_eq!(f.bash(&cmd), "allow", "control must be Allow: {cmd}");
}

#[test]
fn c2_control_project_write_alone_is_allowed() {
    let f = Fx::new("c2");
    let cmd = py(&format!("open(\"{}\", \"w\").write(\"y\")", f.out()));
    assert_eq!(f.bash(&cmd), "allow", "control must be Allow: {cmd}");
}

#[test]
fn c3_control_real_system_write_stays_not_allowed() {
    let f = Fx::new("c3");
    let cmd = py("open(\"/etc/hosts\", \"w\").write(\"y\")");
    let v = f.bash(&cmd);
    assert_ne!(v, "allow", "a write to /etc/hosts must not be Allow: {cmd}");
    assert_ne!(v, "other", "unrecognised hook output: {cmd}");
}

#[test]
fn c4_control_real_system_write_beside_read_only_literal_stays_not_allowed() {
    let f = Fx::new("c4");
    let cmd = py(&format!(
        "p = \"{}\"\nprint(open(p).read())\nopen(\"/etc/hosts\", \"w\").write(\"y\")",
        f.ro()
    ));
    let v = f.bash(&cmd);
    assert_ne!(v, "allow", "a write to /etc/hosts must not be Allow: {cmd}");
    assert_ne!(v, "other", "unrecognised hook output: {cmd}");
}
