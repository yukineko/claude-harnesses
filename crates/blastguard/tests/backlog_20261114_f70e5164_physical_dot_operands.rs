#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! GH #1661 / task f70e5164: a recursive `rm` operand spelled with `.`/`..`
//! components is judged by the deletion principle only after those components
//! are resolved PHYSICALLY (`SafeRoots::physical_absolute`). These tests pin
//! the refusals that make that safe, through the real hook binary, with the
//! cwd inside a Claude session scratchpad under `/private/tmp/claude-502`:
//!
//!  * a `..` after a symlinked directory pointing OUT of the scratchpad is not
//!    Allow (its lexical collapse lands on an existing dir in the scratchpad,
//!    which a lexical model would Allow);
//!  * a symlink between the temp root and a `./`-spelled operand still
//!    withholds the Allow, even when it points back inside the scratchpad;
//!  * a `..` that climbs above `/private/tmp` into a path outside every allow
//!    class is not Allow;
//!  * an operand that resolves to the temp root `/private/tmp` itself is the
//!    determined `Refused` Deny (reason names the root "itself");
//!  * a `..` after a NONEXISTENT prefix, and a `..` above `/`, are
//!    undetermined: not Allow, although their lexical collapse is Allow;
//!  * positive: the same existing directory spelled `./proj/target` and
//!    `../sbx/proj/target` is Allow for the main session.
//!
//! "Not Allow" is asserted for BOTH the main session and a subagent payload.
//! The hook runs with `env_clear` (no `TMPDIR`), so the fixture's `home`
//! under the OS temp dir (`/private/var/folders/...`) is not a temp root here.
//!
//! DOES NOT PROVE: anything about Linux (macOS only: the fixture lives under
//! `/private/tmp/claude-502`), about git work-tree classes reached through a
//! `..` spelling, or about TOCTOU between this judgement and the `rm`.
#![cfg(target_os = "macos")]

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CLAUDE_TMP: &str = "/private/tmp/claude-502";

struct Scratch {
    top: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
    created_claude_tmp: bool,
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.top);
        let _ = std::fs::remove_dir_all(&self.home);
        if self.created_claude_tmp {
            // Fails (harmlessly) if another test/session still uses it.
            let _ = std::fs::remove_dir(CLAUDE_TMP);
        }
    }
}

fn scratch(tag: &str) -> Scratch {
    let created_claude_tmp = !Path::new(CLAUDE_TMP).exists();
    let uniq = format!("bgtest-f70e-{tag}-{}", std::process::id());
    let top = Path::new(CLAUDE_TMP).join(&uniq);
    let cwd = top.join("3f2c1b0a-1111-4222-8333-444455556666/scratchpad/sbx");
    std::fs::create_dir_all(cwd.join("proj/target/debug")).unwrap();
    std::fs::write(cwd.join("proj/target/debug/x.o"), "x").unwrap();
    let home = std::env::temp_dir().join(format!("bg-f70e-home-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let home = std::fs::canonicalize(&home).unwrap();
    Scratch {
        top,
        home,
        cwd,
        created_claude_tmp,
    }
}

/// PreToolUse hook; returns (decision, reason). Empty stdout with exit 0 =
/// allow; a non-zero exit fails the test (never read as allow).
fn run_as(command: &str, s: &Scratch, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "f70e5164-20261114",
        "cwd": s.cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = "test-subagent".into();
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &s.home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("BLASTGUARD_APPROVALS_DIR", s.home.join("approvals"))
        .current_dir(&s.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

/// `command` is not Allow for the main session nor for a subagent.
fn assert_not_allow(command: &str, s: &Scratch) {
    for subagent in [false, true] {
        let (d, reason) = run_as(command, s, subagent);
        assert_ne!(
            d, "allow",
            "`{command}` (subagent={subagent}) must not be Allow; reason: {reason}"
        );
    }
}

/// `../` repeated once per component of the cwd: from the cwd to `/`.
fn up_to_root(s: &Scratch) -> String {
    let depth = s
        .cwd
        .to_str()
        .unwrap()
        .split('/')
        .filter(|c| !c.is_empty())
        .count();
    "../".repeat(depth)
}

#[test]
fn dot_and_dotdot_spellings_of_an_existing_scratch_dir_are_allow() {
    let s = scratch("pos");
    for c in ["rm -rf ./proj/target", "rm -rf ../sbx/proj/target"] {
        let (d, reason) = run_as(c, &s, false);
        assert_eq!(d, "allow", "main `{c}` must be Allow; reason: {reason}");
    }
}

#[test]
fn dotdot_through_a_symlink_out_of_the_scratchpad_is_not_allow() {
    let s = scratch("lnkout");
    let outside = s.home.join("outside");
    std::fs::create_dir_all(outside.join("d")).unwrap();
    std::fs::create_dir_all(outside.join("victim")).unwrap();
    // The lexical collapse of `lnk/../victim` is this existing scratch dir.
    std::fs::create_dir_all(s.cwd.join("victim")).unwrap();
    std::os::unix::fs::symlink(outside.join("d"), s.cwd.join("lnk")).unwrap();
    assert_not_allow("rm -rf lnk/../victim", &s);
    assert_not_allow("rm -rf ./lnk/../victim", &s);
}

#[test]
fn a_symlink_below_the_temp_root_withholds_allow_for_a_dot_spelling() {
    let s = scratch("lnkin");
    // Points back INSIDE the scratchpad: physically `proj/target`, but a
    // symlink between the temp root and the operand still withholds Allow.
    std::os::unix::fs::symlink(s.cwd.join("proj"), s.cwd.join("plink")).unwrap();
    assert_not_allow("rm -rf ./plink/target", &s);
    assert_not_allow("rm -rf plink/../proj/target", &s);
    // And one pointing out, spelled with `./`.
    let outside = s.home.join("out2");
    std::fs::create_dir_all(outside.join("x")).unwrap();
    std::os::unix::fs::symlink(&outside, s.cwd.join("olink")).unwrap();
    assert_not_allow("rm -rf ./olink/x", &s);
}

#[test]
fn dotdot_climbing_above_private_tmp_out_of_every_class_is_not_allow() {
    let s = scratch("climb");
    std::fs::create_dir_all(s.home.join("junk")).unwrap();
    let home_rel = s.home.to_str().unwrap().trim_start_matches('/');
    let op = format!("{}{home_rel}/junk", up_to_root(&s));
    assert_not_allow(&format!("rm -rf {op}"), &s);
    assert_not_allow(&format!("rm -rf ./{op}"), &s);
}

#[test]
fn an_operand_resolving_to_the_temp_root_itself_is_refused() {
    let s = scratch("root");
    // From `/private/tmp/<5 components>` five `..` land on `/private/tmp`.
    let below_tmp = s
        .cwd
        .strip_prefix("/private/tmp")
        .unwrap()
        .components()
        .count();
    let op = "../".repeat(below_tmp);
    for subagent in [false, true] {
        let (d, reason) = run_as(&format!("rm -rf {op}"), &s, subagent);
        assert_eq!(
            d, "deny",
            "`rm -rf {op}` (subagent={subagent}) must be Deny; reason: {reason}"
        );
        assert!(
            reason.contains("/private/tmp itself"),
            "the Deny must be the determined temp-root refusal; reason: {reason}"
        );
    }
}

#[test]
fn dotdot_after_a_nonexistent_prefix_is_not_allow() {
    let s = scratch("nope");
    // Lexically `proj/target` (Allow); physically the kernel fails on `nope`.
    assert_not_allow("rm -rf nope/../proj/target", &s);
    assert_not_allow("rm -rf ./proj/nope/../target", &s);
}

#[test]
fn dotdot_above_the_filesystem_root_is_not_allow() {
    let s = scratch("above");
    let cwd_rel = s.cwd.to_str().unwrap().trim_start_matches('/');
    // Three `..` too many: the kernel would stay at `/`, this model refuses.
    let op = format!("{}../../../{cwd_rel}/proj/target", up_to_root(&s));
    assert_not_allow(&format!("rm -rf {op}"), &s);
}
