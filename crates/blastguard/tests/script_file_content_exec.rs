// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling R3 (2026-10-14): executing a TEXT script file is judged by the
//! file's CONTENT.
//!
//! `bash f.sh`, `sh f.sh`, `zsh f`, `./f.sh` (text file with a shebang and the
//! executable bit), `source f.sh`, `. f.sh`, `python3 f.py`, `node f.js`:
//! blastguard must read the file at PreToolUse time (a relative path resolved
//! against the payload `cwd`) and judge it like the same text run inline by
//! that interpreter:
//!
//!   * content that would destroy something outside the session repo / tmp
//!     (`rm -rf ~`, `rm -rf ~/Documents`, python
//!     `shutil.rmtree(os.path.expanduser('~'))`) -> Ask carrying
//!     [`RECOMMEND_NO`] (the user ruled Ask recommending not running, NOT a
//!     hard Deny);
//!   * content that cannot be determined — file unreadable / not found / not
//!     UTF-8 text, content itself undeterminable (`rm -rf "$x"`), or the script
//!     runs another script -> Ask carrying [`RECOMMEND_NO`];
//!   * content that only touches the repo / tmp or is harmless (`echo hi`,
//!     `rm -rf ./target`, `rm -rf /tmp/foo`, `cargo build`) -> Allow.
//!
//! Out of scope: BINARY executables (Mach-O / ELF). Their verdict is pinned
//! at what it was measured to be before the feature, so the feature cannot
//! silently change it. Inline commands are unchanged (`rm -rf ~` stays Deny).
//!
//! DEFERRED (user ruling 2026-10-14): blastguard does not read script-file
//! content yet, so every content-reading test below is `#[ignore]`d; run with
//! `--ignored` to reproduce the write-then-run gap. The controls stay active.
//!
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.
//!
//! Fixture: a session repo `<base>/src/proj` (a git repo) is the payload
//! `cwd` and `CLAUDE_PROJECT_DIR`; `HOME` is a synthetic `<base>/home` (so
//! `~` in a script names a directory OUTSIDE the session repo and outside any
//! temp root). `<base>` is the calibrated [`neutral_base::neutral_base`], so
//! the fixture location itself is neither inside a worktree storage root nor
//! inside a temp safe root. The hook process runs with its OWN current
//! directory set to `<base>` (not the repo), so an implementation that
//! resolves the script path against the process cwd instead of the payload
//! `cwd` does not find the file.

#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Marker every Ask produced under the user's 2026-10-14 rulings must carry in
/// `permissionDecisionReason` (ruling R0: the Ask recommends the negative
/// choice).
const RECOMMEND_NO: &str = "Recommended: do not run this.";

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("script_file_content_exec")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let f = Fixture {
            home: base.join("home"),
            proj: base.join("src/proj"),
            base,
        };
        std::fs::create_dir_all(f.home.join("Documents")).unwrap();
        std::fs::write(f.home.join("Documents/keep.txt"), "x").unwrap();
        std::fs::create_dir_all(f.proj.join("target")).unwrap();
        let st = git_env::pinned(&f.proj)
            .args(["init", "-q"])
            .status()
            .expect("git runs");
        assert!(st.success(), "git init failed in {}", f.proj.display());
        f
    }

    /// Write a text script into the session repo (simulates a prior Write
    /// tool call). `exec` sets mode 0755.
    fn script(&self, rel: &str, content: &str, exec: bool) -> PathBuf {
        let p = self.proj.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
        if exec {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    /// `(permissionDecision, permissionDecisionReason)`; `"allow"` when the
    /// hook printed nothing.
    fn verdict(&self, command: &str) -> (String, String) {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDECODE", "1")
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDE_PROJECT_DIR", &self.proj)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let mut child = cmd.spawn().expect("binary spawns");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        let out = child.wait_with_output().expect("binary runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0 for {command:?}; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if stdout.trim().is_empty() {
            return ("allow".to_string(), String::new());
        }
        let doc: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout is not JSON for {command:?} ({e}): {stdout}"));
        let hso = doc.get("hookSpecificOutput");
        let decision = hso
            .and_then(|h| h.get("permissionDecision"))
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("no permissionDecision for {command:?}, got: {stdout}"))
            .to_string();
        let reason = hso
            .and_then(|h| h.get("permissionDecisionReason"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        (decision, reason)
    }
}

/// Collects every command whose verdict is not exactly Ask + [`RECOMMEND_NO`].
fn not_ask_with_marker(f: &Fixture, cmds: &[String]) -> Vec<String> {
    let mut bad = Vec::new();
    for c in cmds {
        let (d, reason) = f.verdict(c);
        if d != "ask" || !reason.contains(RECOMMEND_NO) {
            bad.push(format!("{c:?} -> {d} ({reason})"));
        }
    }
    bad
}

/// Collects every command whose verdict is not Allow.
fn not_allow(f: &Fixture, cmds: &[String]) -> Vec<String> {
    let mut bad = Vec::new();
    for c in cmds {
        let (d, reason) = f.verdict(c);
        if d != "allow" {
            bad.push(format!("{c:?} -> {d} ({reason})"));
        }
    }
    bad
}

#[track_caller]
fn assert_none(bad: &[String], what: &str) {
    assert!(bad.is_empty(), "{what}:\n{}", bad.join("\n"));
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_string()).collect()
}

// ------------------------------------------------- destructive content -> Ask

/// A shell script whose content destroys `~` (outside repo/tmp), run through
/// every shell spelling the ruling names: Ask + marker.
#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn shell_script_destroying_home_asks_with_marker() {
    let f = Fixture::new("destroy_home_sh");
    f.script("f.sh", "#!/bin/sh\nrm -rf ~\n", true);
    let bad = not_ask_with_marker(
        &f,
        &s(&[
            "bash f.sh",
            "sh f.sh",
            "zsh f.sh",
            "./f.sh",
            "source f.sh",
            ". f.sh",
        ]),
    );
    assert_none(&bad, "script `rm -rf ~` must be Ask + marker");
}

/// Same with `~/Documents`, an extension-less file for `zsh f`, a script in a
/// subdirectory, and an ABSOLUTE script path.
#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn shell_script_destroying_home_subdir_asks_with_marker() {
    let f = Fixture::new("destroy_docs_sh");
    f.script("f", "rm -rf ~/Documents\n", false);
    let abs = f.script(
        "sub/g.sh",
        "#!/bin/bash\necho start\nrm -rf ~/Documents\n",
        true,
    );
    let bad = not_ask_with_marker(
        &f,
        &[
            "zsh f".to_string(),
            "bash f".to_string(),
            "bash sub/g.sh".to_string(),
            "./sub/g.sh".to_string(),
            format!("bash {}", abs.display()),
            abs.display().to_string(),
        ],
    );
    assert_none(&bad, "script `rm -rf ~/Documents` must be Ask + marker");
}

/// Python file whose content removes the home directory: Ask + marker.
#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn python_file_destroying_home_asks_with_marker() {
    let f = Fixture::new("destroy_home_py");
    f.script(
        "d.py",
        "import os, shutil\nshutil.rmtree(os.path.expanduser('~'))\n",
        false,
    );
    f.script("e.py", "import os\nos.system('rm -rf ~')\n", false);
    let bad = not_ask_with_marker(&f, &s(&["python3 d.py", "python3 e.py"]));
    assert_none(&bad, "python file destroying ~ must be Ask + marker");
}

/// Node file whose content runs `rm -rf ~` through a child process: Ask +
/// marker.
#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn node_file_destroying_home_asks_with_marker() {
    let f = Fixture::new("destroy_home_js");
    f.script(
        "d.js",
        "require('child_process').execSync('rm -rf ~');\n",
        false,
    );
    let bad = not_ask_with_marker(&f, &s(&["node d.js"]));
    assert_none(&bad, "node file running `rm -rf ~` must be Ask + marker");
}

/// Two-step attack: the destructive script was written by an EARLIER tool call
/// (here: the fixture writes it before the hook sees anything), then the
/// innocuous-looking `bash x.sh` arrives. Ask + marker.
#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn two_step_write_then_run_asks_with_marker() {
    let f = Fixture::new("two_step");
    // Step 1 (prior Write tool call): nothing to judge in the Bash hook.
    f.script("x.sh", "set -e\ncd /\nrm -rf ~/Documents ~\n", false);
    // Step 2: the run.
    let bad = not_ask_with_marker(&f, &s(&["bash x.sh"]));
    assert_none(&bad, "two-step write-then-run must be Ask + marker");
}

/// User ruling (2026-10-14, follow-up 2): arguments and interpreter flags do
/// not take the call out of scope, and a text `./f` WITHOUT a shebang is still
/// a script. Each destructive file -> Ask + marker.
#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn destructive_script_with_args_and_flags_asks_with_marker() {
    let f = Fixture::new("args_flags");
    f.script("f.sh", "rm -rf ~\n", true);
    f.script("noshebang", "rm -rf ~/Documents\n", true);
    f.script(
        "d.py",
        "import os, shutil\nshutil.rmtree(os.path.expanduser('~'))\n",
        false,
    );
    f.script(
        "d.js",
        "require('child_process').execSync('rm -rf ~');\n",
        false,
    );
    let bad = not_ask_with_marker(
        &f,
        &s(&[
            "bash -x f.sh a b",
            "sh -e f.sh x",
            "./f.sh a b",
            "./noshebang",
            "python3 d.py arg",
            "python3 -u d.py",
            "node d.js arg",
        ]),
    );
    assert_none(
        &bad,
        "destructive script with args/flags must be Ask + marker",
    );
}

/// User ruling (2026-10-14, follow-up 2): `./f` is judged by its shebang. A
/// python shebang on a file whose python program removes `~` -> Ask + marker.
#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn shebang_selects_interpreter_for_direct_exec_asks_with_marker() {
    let f = Fixture::new("shebang_python");
    f.script(
        "f",
        "#!/usr/bin/env python3\nimport os, shutil\nshutil.rmtree(os.path.expanduser('~'))\n",
        true,
    );
    let bad = not_ask_with_marker(&f, &s(&["./f"]));
    assert_none(&bad, "python-shebang `./f` removing ~ must be Ask + marker");
}

/// User ruling (2026-10-14, follow-up): a file containing system destruction
/// (`rm -rf /`) is also Ask + marker (file content -> Ask, inline stays Deny).
#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn script_destroying_root_asks_with_marker() {
    let f = Fixture::new("destroy_root_sh");
    f.script("r.sh", "#!/bin/sh\nrm -rf /\n", true);
    let bad = not_ask_with_marker(&f, &s(&["bash r.sh", "./r.sh", ". r.sh"]));
    assert_none(&bad, "script `rm -rf /` must be Ask + marker");
}

// ---------------------------------------------- undeterminable content -> Ask

#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn missing_script_file_asks_with_marker() {
    let f = Fixture::new("missing");
    let bad = not_ask_with_marker(
        &f,
        &s(&[
            "bash nope.sh",
            "sh nope.sh",
            "source nope.sh",
            ". nope.sh",
            "python3 nope.py",
            "node nope.js",
        ]),
    );
    assert_none(&bad, "a script that cannot be found must be Ask + marker");
}

#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn unreadable_script_file_asks_with_marker() {
    let f = Fixture::new("unreadable");
    let p = f.script("noperm.sh", "echo hi\n", false);
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Precondition: the file really is unreadable for this process (not root).
    assert!(
        std::fs::read(&p).is_err(),
        "precondition: fixture must be unreadable"
    );
    let bad = not_ask_with_marker(&f, &s(&["bash noperm.sh", "source noperm.sh"]));
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_none(&bad, "an unreadable script must be Ask + marker");
}

#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn non_utf8_script_file_asks_with_marker() {
    let f = Fixture::new("non_utf8");
    let p = f.proj.join("bin.sh");
    std::fs::write(&p, [b'e', b'c', b'h', b'o', b' ', 0xff, 0xfe, 0x80, b'\n']).unwrap();
    let bad = not_ask_with_marker(&f, &s(&["bash bin.sh", "sh bin.sh"]));
    assert_none(&bad, "a non-UTF-8 script must be Ask + marker");
}

/// The text is readable but itself undeterminable: `rm -rf "$x"`.
#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn script_with_undeterminable_content_asks_with_marker() {
    let f = Fixture::new("undet_content");
    f.script("v.sh", "#!/bin/sh\nrm -rf \"$x\"\n", true);
    let bad = not_ask_with_marker(&f, &s(&["bash v.sh", "./v.sh", ". v.sh"]));
    assert_none(&bad, "script `rm -rf \"$x\"` must be Ask + marker");
}

/// A script that runs ANOTHER script is undeterminable by ruling, even when
/// the inner script is harmless.
#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn script_running_another_script_asks_with_marker() {
    let f = Fixture::new("nested_script");
    f.script("inner.sh", "#!/bin/sh\necho hi\n", true);
    f.script("outer1.sh", "bash inner.sh\n", false);
    f.script("outer2.sh", "./inner.sh\n", false);
    f.script("outer3.sh", ". ./inner.sh\n", false);
    let bad = not_ask_with_marker(
        &f,
        &s(&["bash outer1.sh", "bash outer2.sh", "bash outer3.sh"]),
    );
    assert_none(
        &bad,
        "a script that runs another script must be Ask + marker",
    );
}

// ------------------------------------------------- harmless content -> Allow

#[test]
fn harmless_shell_script_is_allowed() {
    let f = Fixture::new("harmless_sh");
    f.script("a.sh", "#!/bin/sh\necho hi\n", true);
    f.script("c.sh", "#!/bin/sh\nrm -rf /tmp/foo\n", true);
    f.script("d.sh", "#!/bin/sh\ncargo build\n", true);
    let mut cmds = Vec::new();
    for name in ["a.sh", "c.sh", "d.sh"] {
        for pre in ["bash ", "sh ", "./", "source ", ". "] {
            cmds.push(format!("{pre}{name}"));
        }
    }
    let bad = not_allow(&f, &cmds);
    assert_none(&bad, "a harmless / tmp-only script must be Allow");
}

/// User ruling (2026-10-14, follow-up 1): a script's content gets the SAME
/// verdict as that text run inline. Inline `rm -rf ./target` in the session
/// repo is a confined Ask, so the script is an Ask too. The marker is NOT
/// required: this is a repo-confined question, not a destruction-risk Ask.
#[test]
#[ignore = "deferred by user ruling 2026-10-14: blastguard does not read script-file content yet; run with --ignored to reproduce the write-then-run gap"]
fn repo_confined_rm_script_asks_same_as_inline() {
    let f = Fixture::new("repo_confined_sh");
    f.script("b.sh", "#!/bin/sh\nrm -rf ./target\n", true);
    let mut bad = Vec::new();
    for pre in ["bash ", "sh ", "./", "source ", ". "] {
        let c = format!("{pre}b.sh");
        let (d, reason) = f.verdict(&c);
        if d != "ask" {
            bad.push(format!("{c:?} -> {d} ({reason})"));
        }
    }
    assert_none(
        &bad,
        "a repo-confined `rm -rf ./target` script must be Ask (same as inline)",
    );
}

#[test]
fn harmless_interpreter_files_are_allowed() {
    let f = Fixture::new("harmless_interp");
    f.script("h.py", "print('hi')\n", false);
    f.script("h.js", "console.log('hi');\n", false);
    let bad = not_allow(&f, &s(&["python3 h.py", "node h.js"]));
    assert_none(&bad, "a harmless interpreter file must be Allow");
}

// ------------------------------------------------------------------ controls

/// Inline commands are unchanged by the feature: `rm -rf ~` typed inline is
/// still a hard Deny (the Ask downgrade is ONLY for content read from a file).
#[test]
fn control_inline_destruction_stays_denied() {
    let f = Fixture::new("inline_control");
    let (d, reason) = f.verdict("rm -rf ~");
    assert_eq!(d, "deny", "inline `rm -rf ~` must stay Deny ({reason})");
}

/// Baseline for the Allow expectations above: the same harmless text typed
/// inline in this fixture is Allow. If THIS fails, the fixture (not the
/// feature) is what decides the harmless-script verdicts.
#[test]
fn control_inline_harmless_text_is_allowed_in_this_fixture() {
    let f = Fixture::new("inline_harmless_control");
    let bad = not_allow(&f, &s(&["echo hi", "rm -rf /tmp/foo", "cargo build"]));
    assert_none(&bad, "inline harmless baseline must be Allow");
}

/// Inline commands are unchanged (ruling R3). Measured 2026-10-14: an INLINE
/// `rm -rf ./target` inside the session repo is a confined Ask ("recursive rm
/// (-r) is confined to <repo> ... this is a question rather than a refusal").
/// Pairs with `repo_confined_rm_script_asks_same_as_inline`.
#[test]
fn control_inline_recursive_rm_in_repo_stays_confined_ask() {
    let f = Fixture::new("inline_rm_target_control");
    let (d, reason) = f.verdict("rm -rf ./target");
    assert_eq!(
        d, "ask",
        "inline `rm -rf ./target` verdict changed ({reason})"
    );
}

/// OUT OF SCOPE: a binary executable (Mach-O / ELF magic). Its verdict is
/// pinned at the value measured before the feature existed so the feature
/// cannot change it. Measured 2026-10-14 (this test, run against blastguard
/// 0.2.109 + the uncommitted here-string change in `detect.rs`, before any
/// script-content reading existed): `./echo hi` with `echo` a copy of
/// `/bin/echo` was Allow.
const BINARY_EXEC_VERDICT_TODAY: &str = "allow";

fn copy_binary(f: &Fixture, name: &str) -> PathBuf {
    let src = Path::new("/bin/echo");
    let dst = f.proj.join(name);
    std::fs::copy(src, &dst).unwrap();
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).unwrap();
    let head = std::fs::read(&dst).unwrap();
    let magic = &head[..4];
    let is_binary = magic == [0x7f, b'E', b'L', b'F']
        || magic == [0xcf, 0xfa, 0xed, 0xfe]
        || magic == [0xca, 0xfe, 0xba, 0xbe]
        || magic == [0xfe, 0xed, 0xfa, 0xcf];
    assert!(
        is_binary,
        "precondition: /bin/echo copy has a binary magic, got {magic:?}"
    );
    dst
}

#[test]
fn control_binary_executable_verdict_is_unchanged() {
    let f = Fixture::new("binary_exec");
    copy_binary(&f, "echo");
    let (d, reason) = f.verdict("./echo hi");
    assert_eq!(
        d, BINARY_EXEC_VERDICT_TODAY,
        "binary executables are out of scope; verdict must stay as measured ({reason})"
    );
}
