#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! S5b follow-ups: user rulings 2026-11-19 on the S5 (`python-parser`)
//! stage's known gaps, pinned by an independent test author (not the
//! implementer). Every test that failed on the base (main 2177fadf1,
//! blastguard 0.2.735) carries `#[ignore = "RED s5b 2026-11-19: ..."]`; the
//! un-ignored ones passed there and are controls / regression guards.
//!
//! (a) Import shadowing: `python3 -c` puts the cwd first on `sys.path`, so a
//!     file `M.py` or a package dir `M/` in the hook cwd replaces the module
//!     `M` the program imports → not Allow (Ask in main). Control: the same
//!     program in a cwd without such an entry → Allow.
//! (b) `open(p, 'r+')` is an in-place write judged by target path like
//!     `'a'`: scratch (/tmp/...) → Allow, System (/etc/bg-sentinel) → Deny,
//!     the same verdict as the `'a'` twin.
//! (c) python wrapped by `bash -c "..."`, `env`, `timeout 5`, `nice`, `nohup`
//!     is judged by S5 exactly like the direct spelling. `sudo python3 -c`
//!     is out of scope and not asserted.
//! (d) `sh -c 'echo "$HOME"'` / `bash -c 'echo "$HOME"'` → Allow; control
//!     `sh -c 'rm -rf "$HOME"'` → Deny.
//!
//! All paths are non-existent sentinels or tempdir fixtures; nothing here is
//! executed by a shell or an interpreter, only judged by the hook binary.
//! The child env is cleared (`env_clear`), so `CLAUDE_CODE_SESSION_ID` never
//! reaches it.
//!
//! (b) observed on the base: verdicts already hold (r+ is read as Truncate,
//! spelled `: > P`; Truncate on /tmp is Allow and on /etc is Deny, the same
//! as `'a'`), so its tests are regression guards, not RED. The `'w'` reason
//! does not claim the contents are destroyed ("redirect ... writes into a
//! system directory"), so no reason text is asserted.
//!
//! DOES NOT PROVE: reason wording (verdicts only); that `r+` is spelled as an
//! append (`: >> P`) rather than a truncate in the reason; modules other than the
//! ones named; shadowing by a `.pth`, `PYTHONPATH` or site-packages entry;
//! wrappers other than the ones listed; nested wrappers
//! (`env timeout 5 python3 ...`); real interpreter behaviour (CPython preloads
//! `os` at startup, so an `os.py` in the cwd does not actually replace `os` —
//! the ruling's not-Allow for `os.py` is pinned as given, and a second module
//! that is not preloaded, `shutil`, is pinned too).

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Home(PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Home {
    fn wt(&self) -> PathBuf {
        self.0.join("wt")
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-s5b-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// (decision, reason) of the hook for `command` run with hook cwd `cwd`.
fn run_in(command: &str, home: &Home, cwd: &Path, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "s5b-followups-20261119",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
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

fn main_of(command: &str, tag: &str) -> (String, String) {
    let home = unique_home(tag);
    let wt = home.wt();
    run_in(command, &home, &wt, false)
}

fn both_of(command: &str, tag: &str) -> ((String, String), (String, String)) {
    let home = unique_home(tag);
    let wt = home.wt();
    (
        run_in(command, &home, &wt, false),
        run_in(command, &home, &wt, true),
    )
}

// ---------------------------------------------------------------------------
// (a) import shadowing by an entry of the hook cwd
// ---------------------------------------------------------------------------

const IMPORT_OS: &str = "python3 -c 'import os; print(1)'";
const IMPORT_SHUTIL: &str = "python3 -c 'import shutil; print(1)'";

fn shadowed(prog: &str, setup: impl Fn(&Path), tag: &str) -> ((String, String), (String, String)) {
    let home = unique_home(tag);
    let wt = home.wt();
    setup(&wt);
    (
        run_in(prog, &home, &wt, false),
        run_in(prog, &home, &wt, true),
    )
}

fn assert_ask_main_not_allow_sub(r: ((String, String), (String, String)), what: &str) {
    let ((m, mr), (s, sr)) = r;
    assert_eq!(m, "ask", "{what}: main must ask; got {m} reason={mr}");
    assert_ne!(s, "allow", "{what}: subagent must not allow; reason={sr}");
}

// user ruling 2026-11-19 (S5b a): a module file in the cwd shadows the import → not Allow.
#[test]
fn import_os_with_os_py_in_cwd_is_ask() {
    let r = shadowed(
        IMPORT_OS,
        |wt| std::fs::write(wt.join("os.py"), "").unwrap(),
        "a-ospy",
    );
    assert_ask_main_not_allow_sub(r, "os.py in cwd");
}

// user ruling 2026-11-19 (S5b a): a package dir in the cwd shadows the import → not Allow.
#[test]
fn import_os_with_os_dir_in_cwd_is_ask() {
    let r = shadowed(
        IMPORT_OS,
        |wt| {
            std::fs::create_dir_all(wt.join("os")).unwrap();
            std::fs::write(wt.join("os").join("__init__.py"), "").unwrap();
        },
        "a-osdir",
    );
    assert_ask_main_not_allow_sub(r, "os/ in cwd");
}

// user ruling 2026-11-19 (S5b a), with a module CPython does not preload.
#[test]
fn import_shutil_with_shutil_py_in_cwd_is_ask() {
    let r = shadowed(
        IMPORT_SHUTIL,
        |wt| std::fs::write(wt.join("shutil.py"), "").unwrap(),
        "a-shpy",
    );
    assert_ask_main_not_allow_sub(r, "shutil.py in cwd");
}

// Control for (a): no shadowing entry in the cwd → Allow in both modes.
#[test]
fn control_import_without_shadow_in_cwd_is_allow() {
    for prog in [IMPORT_OS, IMPORT_SHUTIL] {
        let ((m, mr), (s, sr)) = shadowed(
            prog,
            |wt| std::fs::write(wt.join("unrelated.txt"), "").unwrap(),
            "a-ctl",
        );
        assert_eq!(m, "allow", "main must allow {prog:?}; reason={mr}");
        assert_eq!(s, "allow", "subagent must allow {prog:?}; reason={sr}");
    }
}

// ---------------------------------------------------------------------------
// (b) open(p, 'r+') judged like 'a'
// ---------------------------------------------------------------------------

const SCRATCH: &str = "/tmp/bg-s5b-sentinel-scratch";
const SYSTEM: &str = "/etc/bg-sentinel";

fn open_prog(path: &str, mode: &str) -> String {
    format!(r#"python3 -c 'open("{path}","{mode}")'"#)
}

// user ruling 2026-11-19 (S5b b): r+ on a scratch path → Allow, like 'a'.
#[test]
fn rplus_on_scratch_path_is_allow_like_append() {
    let a = both_of(&open_prog(SCRATCH, "a"), "b-sa");
    let r = both_of(&open_prog(SCRATCH, "r+"), "b-sr");
    assert_eq!(a.0 .0, "allow", "'a' twin main; reason={}", a.0 .1);
    assert_eq!(r.0 .0, "allow", "r+ main must allow; reason={}", r.0 .1);
    assert_eq!(
        r.1 .0, a.1 .0,
        "r+ subagent must match 'a'; r+={} a={}",
        r.1 .1, a.1 .1
    );
}

// user ruling 2026-11-19 (S5b b): r+ on a System path → Deny, like 'a'.
#[test]
fn rplus_on_system_path_is_deny_like_append() {
    let a = both_of(&open_prog(SYSTEM, "a"), "b-ya");
    let r = both_of(&open_prog(SYSTEM, "r+"), "b-yr");
    assert_eq!(a.0 .0, "deny", "'a' twin main; reason={}", a.0 .1);
    assert_eq!(a.1 .0, "deny", "'a' twin subagent; reason={}", a.1 .1);
    assert_eq!(r.0 .0, "deny", "r+ main must deny; reason={}", r.0 .1);
    assert_eq!(r.1 .0, "deny", "r+ subagent must deny; reason={}", r.1 .1);
}

// ---------------------------------------------------------------------------
// (c) wrappers are judged by S5 like the direct spelling
// ---------------------------------------------------------------------------

/// A program that only prints a string containing `rm -rf ~`.
const LITERAL_DIRECT: &str = r#"python3 -c 'print("rm -rf ~")'"#;
/// A program that opens /etc/bg-sentinel for writing.
const WRITE_DIRECT: &str = r#"python3 -c 'open("/etc/bg-sentinel","w")'"#;

const LITERAL_BASH_C: &str = r#"bash -c "python3 -c 'print(\"rm -rf ~\")'""#;
const WRITE_BASH_C: &str = r#"bash -c "python3 -c 'open(\"/etc/bg-sentinel\",\"w\")'""#;

const PREFIXES: &[&str] = &["env ", "timeout 5 ", "nice ", "nohup "];

// Control: the direct spelling of the literal-only program is Allow (S5 retires
// the legacy reading of its string literal).
#[test]
fn control_direct_literal_program_is_allow() {
    let ((m, mr), (s, sr)) = both_of(LITERAL_DIRECT, "c-ld");
    assert_eq!(m, "allow", "main; reason={mr}");
    assert_eq!(s, "allow", "subagent; reason={sr}");
}

// Control: the direct spelling of the /etc write is Deny.
#[test]
fn control_direct_system_write_is_deny() {
    let ((m, mr), (s, sr)) = both_of(WRITE_DIRECT, "c-wd");
    assert_eq!(m, "deny", "main; reason={mr}");
    assert_eq!(s, "deny", "subagent; reason={sr}");
}

// user ruling 2026-11-19 (S5b c): bash -c "python3 -c ..." judged like direct.
#[test]
fn bash_c_literal_program_is_allow() {
    let ((m, mr), (s, sr)) = both_of(LITERAL_BASH_C, "c-lb");
    assert_eq!(m, "allow", "main; cmd={LITERAL_BASH_C:?} reason={mr}");
    assert_eq!(s, "allow", "subagent; cmd={LITERAL_BASH_C:?} reason={sr}");
}

#[test]
fn bash_c_system_write_is_deny() {
    let ((m, mr), (s, sr)) = both_of(WRITE_BASH_C, "c-wb");
    assert_eq!(m, "deny", "main; cmd={WRITE_BASH_C:?} reason={mr}");
    assert_eq!(s, "deny", "subagent; cmd={WRITE_BASH_C:?} reason={sr}");
}

// user ruling 2026-11-19 (S5b c): env / timeout 5 / nice / nohup judged like direct.
#[test]
fn prefixed_literal_program_is_allow() {
    let mut bad = Vec::new();
    for p in PREFIXES {
        let cmd = format!("{p}{LITERAL_DIRECT}");
        let ((m, mr), (s, sr)) = both_of(&cmd, "c-lp");
        if m != "allow" || s != "allow" {
            bad.push(format!("{cmd:?}: main={m} ({mr}) sub={s} ({sr})"));
        }
    }
    assert!(bad.is_empty(), "must allow:\n{}", bad.join("\n"));
}

#[test]
fn prefixed_system_write_is_deny() {
    let mut bad = Vec::new();
    for p in PREFIXES {
        let cmd = format!("{p}{WRITE_DIRECT}");
        let ((m, mr), (s, sr)) = both_of(&cmd, "c-wp");
        if m != "deny" || s != "deny" {
            bad.push(format!("{cmd:?}: main={m} ({mr}) sub={s} ({sr})"));
        }
    }
    assert!(bad.is_empty(), "must deny:\n{}", bad.join("\n"));
}

// ---------------------------------------------------------------------------
// (d) sh -c 'echo "$HOME"' false positive
// ---------------------------------------------------------------------------

// user ruling 2026-11-19 (S5b d): echoing $HOME through sh -c / bash -c → Allow.
#[test]
fn sh_c_echo_home_is_allow() {
    let mut bad = Vec::new();
    for cmd in [r#"sh -c 'echo "$HOME"'"#, r#"bash -c 'echo "$HOME"'"#] {
        let (m, mr) = main_of(cmd, "d-echo");
        if m != "allow" {
            bad.push(format!("{cmd:?}: main={m} ({mr})"));
        }
    }
    assert!(bad.is_empty(), "must allow:\n{}", bad.join("\n"));
}

// Control for (d): removing $HOME through sh -c stays Deny.
#[test]
fn control_sh_c_rm_rf_home_is_deny() {
    let ((m, mr), (s, sr)) = both_of(r#"sh -c 'rm -rf "$HOME"'"#, "d-rm");
    assert_eq!(m, "deny", "main; reason={mr}");
    assert_eq!(s, "deny", "subagent; reason={sr}");
}
