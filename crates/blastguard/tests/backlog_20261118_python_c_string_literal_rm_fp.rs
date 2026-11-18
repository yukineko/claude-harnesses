#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED pin (false positive, observed 2026-11-18 in a subagent of session
//! fcf56f45, blastguard 0.2.706): a `python3 -I -c '<program>' <file>` whose
//! program only READS a log file and filters its lines, and whose Python
//! string literals held "rm -rf" and r"rm -r" as SEARCH PATTERNS, was refused
//! with the verbatim reason
//!
//! > recursive rm (-r) can delete an entire directory tree
//!
//! Sibling (same class, heredoc form, backtick trigger): backlog b424eaed,
//! `backlog_20261118_python_heredoc_string_literal_rm_home_fp.rs`.
//!
//! MINIMISED by the test author with the debug binary of this rev (fixture
//! HOME, cwd = fixture project; main / subagent gave the same verdict in every
//! probe below). Unlike the heredoc sibling, NO BACKTICKS are needed:
//!   * `python3 -c 'print("rm -rf")'` — deny, the reason above. `-I` is
//!     irrelevant (`python3 -I -c` gives the same); quoting is irrelevant
//!     (`"..."`, `'...'`, `r"..."`, `"""..."""`); `print(...)` is irrelevant
//!     (`s = "rm -rf"` alone is deny); `"rm -r"`, `"rm -R"`, `"rm -rfv"`,
//!     `"rm --recursive"` are all deny;
//!   * `"rm -rf ~"` — deny with "... [system harm: refused on every request,
//!     never handed back as a question]"; `"rm -rf /etc"` — deny, "... deletes
//!     from a system directory: `/etc` ...";
//!   * the text must sit at the START of the literal or after a `;` inside
//!     it: `"a; rm -rf"` deny, but `"xrm -rf"`, `"echo rm -rf"`,
//!     `"grep rm -rf"`, `"use rm -rf carefully"` allow;
//!   * with a harmless operand the literal is allow (`"rm -rf x"`,
//!     `"rm -r x"`, `"rm -rf /tmp/x"`): the literal is judged as a real `rm`
//!     invocation of its operand, and the operand-less form is refused like an
//!     undetermined target;
//!   * the same text in a Python comment (`x = 1  # rm -rf`) is allow;
//!   * the same programs with the literal content changed to `ls -l` are
//!     allow (that is the parity twin pinned below).
//!   * ALSO OBSERVED, NOT PINNED HERE: a shell positional argument handed to
//!     the program (`python3 -c 'print(1)' "rm -rf"`) is deny with the same
//!     reason — it is sys.argv data, but it is not a Python string literal,
//!     so it is outside the S5 parser's scope.
//!
//! INFERRED, NOT TRACED in source: the `-c` program text is split like shell
//! words/commands and a segment that begins with `rm` is judged as an `rm`
//! command.
//!
//! WHAT IT PROVES (when the RED cases pass): for each probed literal, the
//! program's verdict in main and in subagent mode equals the verdict of the
//! SAME program with only that literal's content changed to `ls -l`, and its
//! reason does not cite recursive rm. This includes the observed read-and-
//! filter shape (sys.argv log path in the fixture project).
//!
//! WHAT IT DOES NOT PROVE: any particular verdict (the twin's verdict is the
//! reference, whatever it is); that a literal LATER EXECUTED (passed to
//! `os.system`, `subprocess`, `exec`, written to a script and run) is safe —
//! the controls pin that real execution stays refused, nothing more; the
//! positional-argument shape above; heredoc / `python3 <file>` forms (see the
//! sibling); other interpreters (`node -e`, `perl -e`, `ruby -e`); harmful
//! texts other than recursive rm; the exact reason text after the fix.
//!
//! Controls (not ignored, pass today): the `ls -l` twins are allow;
//! `os.system('rm -rf ~')` stays Deny; `subprocess.run(['rm','-rf','/etc'])`
//! stays Deny (reason quoted at the control).
//!
//! Grounds: memory `false-positives-not-tolerated`; the user's standing
//! ruling that python program text is read by the legacy reader until stage
//! S5 (a Python parser) lands — this RED is an S5 acceptance criterion, to be
//! fixed structurally there, not by a one-off patch. Parity assertions were
//! requested by the S5 owner.
//!
//! Probed at main a304bf1c8 (blastguard 0.2.709) with the debug binary and
//! re-run after merging main f3de4a343 (blastguard 0.2.711); the observed
//! session's hook was 0.2.706.
//!
//! Probe caveat recorded for the next reader: with a fixture HOME under
//! `/private/tmp` (a temp safe root), `subprocess.run(['rm','-rf',<HOME>])`
//! measured allow; with this file's neutral-base fixture HOME it measured
//! deny ("... [system harm: ...]") in both modes. The temp root, not the
//! subprocess path, decided that allow; not pinned here.
//!
//! RED written by an independent test author, not the implementer.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn proj(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    /// The log file the observed program filtered.
    fn log(&self) -> PathBuf {
        self.proj().join("run.log")
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "py-c-str-rm-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    let fx = Fx { base, home };
    std::fs::write(fx.log(), "start\nran rm -rf build\ndone\n").unwrap();
    fx
}

fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "py-c-str-rm-20261118",
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
        .env("HOME", &fx.home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(
        out.status.success(),
        "hook exited {:?} for {command:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout for {command:?} is not JSON ({e}): {stdout:?}"));
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

/// (subagent, decision, reason) for `command`, main first then subagent.
fn both_modes(fx: &Fx, command: &str) -> Vec<(bool, String, String)> {
    [false, true]
        .into_iter()
        .map(|subagent| {
            let (d, r) = run_as(fx, &fx.proj(), command, subagent);
            (subagent, d, r)
        })
        .collect()
}

const RM_REASON: &str = "recursive rm (-r)";

/// The harmless replacement for the literal's content.
const HARMLESS: &str = "ls -l";

/// The observed read-and-filter program: `-c` program (single-quoted for the
/// shell) whose two search patterns are `"{p1}"` and `r"{p2}"`, run on the
/// fixture log.
fn observed_shape(fx: &Fx, isolated: bool, p1: &str, p2: &str) -> String {
    let flag = if isolated { "-I -c" } else { "-c" };
    format!(
        "python3 {flag} 'import sys; pats=[\"{p1}\", r\"{p2}\"]; \
         print([l for l in open(sys.argv[1]) if any(p in l for p in pats)])' {}",
        fx.log().display()
    )
}

/// Minimal shapes. Each takes the literal CONTENT and returns the command;
/// the shell quoting of the `-c` program is part of the shape.
type Shape = fn(&str) -> String;

fn print_dq(s: &str) -> String {
    format!("python3 -c 'print(\"{s}\")'")
}
fn print_sq(s: &str) -> String {
    format!("python3 -c \"print('{s}')\"")
}
fn print_raw(s: &str) -> String {
    format!("python3 -c 'print(r\"{s}\")'")
}
fn print_triple(s: &str) -> String {
    format!("python3 -c 'print(\"\"\"{s}\"\"\")'")
}
fn assign_only(s: &str) -> String {
    format!("python3 -c 's = \"{s}\"'")
}
fn isolated_print(s: &str) -> String {
    format!("python3 -I -c 'print(\"{s}\")'")
}

const SHAPES: &[(&str, Shape)] = &[
    ("print_dq", print_dq),
    ("print_sq", print_sq),
    ("print_raw", print_raw),
    ("print_triple", print_triple),
    ("assign_only", assign_only),
    ("isolated_print", isolated_print),
];

/// Parity: `cmd`'s verdict equals `twin`'s in both modes, and `cmd`'s reason
/// does not cite recursive rm.
fn assert_parity(fx: &Fx, cmd: &str, twin: &str) {
    let got = both_modes(fx, cmd);
    let want = both_modes(fx, twin);
    for ((subagent, d, r), (_, wd, wr)) in got.iter().zip(want.iter()) {
        assert!(
            !r.contains(RM_REASON),
            "subagent={subagent}: rm text inside a python string literal is data, \
             not a recursive rm; got {d} ({r}); cmd={cmd:?}"
        );
        assert_eq!(
            d, wd,
            "subagent={subagent}: verdict must equal the harmless-literal twin's \
             ({wd}: {wr}); got ({r}); cmd={cmd:?}; twin={twin:?}"
        );
    }
}

// ------------------------------------------------------------------- RED

#[test]
#[ignore = "RED(f281b0b1): 2026-11-18 observed python3 -I -c read-and-filter program with \"rm -rf\" / r\"rm -r\" search-pattern literals is Deny (recursive rm); S5 (python parser) acceptance criterion"]
fn observed_filter_program_rm_patterns_match_harmless_twin() {
    let fx = fixture("red-observed");
    for isolated in [true, false] {
        assert_parity(
            &fx,
            &observed_shape(&fx, isolated, "rm -rf", "rm -r"),
            &observed_shape(&fx, isolated, HARMLESS, HARMLESS),
        );
    }
}

#[test]
#[ignore = "RED(f281b0b1): 2026-11-18 python3 -c string literal holding operand-less \"rm -rf\" / \"rm -r\" is Deny (recursive rm) in every quoting; S5 (python parser) acceptance criterion"]
fn operandless_rm_literal_matches_harmless_twin() {
    let fx = fixture("red-operandless");
    for lit in ["rm -rf", "rm -r", "rm -R", "rm --recursive"] {
        for (name, shape) in SHAPES {
            let cmd = shape(lit);
            let twin = shape(HARMLESS);
            eprintln!("shape={name} lit={lit:?}");
            assert_parity(&fx, &cmd, &twin);
        }
    }
}

#[test]
#[ignore = "RED(f281b0b1): 2026-11-18 python3 -c string literal \"rm -rf ~\" is Deny as system harm though never executed; S5 (python parser) acceptance criterion"]
fn rm_home_literal_matches_harmless_twin() {
    let fx = fixture("red-home");
    assert_parity(&fx, &print_dq("rm -rf ~"), &print_dq(HARMLESS));
    assert_parity(&fx, &print_sq("rm -rf ~"), &print_sq(HARMLESS));
}

#[test]
#[ignore = "RED(f281b0b1): 2026-11-18 python3 -c string literal \"rm -rf /etc\" is Deny (system directory) though never executed; S5 (python parser) acceptance criterion"]
fn rm_etc_literal_matches_harmless_twin() {
    let fx = fixture("red-etc");
    assert_parity(&fx, &print_dq("rm -rf /etc"), &print_dq(HARMLESS));
}

#[test]
#[ignore = "RED(f281b0b1): 2026-11-18 python3 -c string literal \"a; rm -rf\" (rm after `;` inside the literal) is Deny (recursive rm); S5 (python parser) acceptance criterion"]
fn rm_after_semicolon_inside_literal_matches_harmless_twin() {
    let fx = fixture("red-semicolon");
    assert_parity(&fx, &print_dq("a; rm -rf"), &print_dq("a; ls -l"));
}

// --------------------------------------------------------------- controls

/// Control: the harmless twins every RED compares against are Allow today,
/// so the REDs measure the literal's content and nothing else.
#[test]
fn control_harmless_twins_are_allow() {
    let fx = fixture("ctl-twins");
    let mut cmds: Vec<String> = SHAPES.iter().map(|(_, s)| s(HARMLESS)).collect();
    cmds.push(print_dq("a; ls -l"));
    cmds.push(observed_shape(&fx, true, HARMLESS, HARMLESS));
    cmds.push(observed_shape(&fx, false, HARMLESS, HARMLESS));
    for cmd in cmds {
        for (subagent, d, r) in both_modes(&fx, &cmd) {
            assert_eq!(d, "allow", "subagent={subagent}: cmd={cmd:?}; reason={r}");
        }
    }
}

/// Control: a `-c` program that actually runs `rm -rf ~` stays Deny.
#[test]
fn control_os_system_rm_home_stays_deny() {
    let fx = fixture("ctl-os-system");
    let cmd = "python3 -c \"import os; os.system('rm -rf ~')\"";
    for (subagent, d, r) in both_modes(&fx, cmd) {
        assert_eq!(d, "deny", "subagent={subagent}: cmd={cmd:?}; reason={r}");
    }
}

/// Control: a `-c` program that actually runs `rm -rf /etc` through
/// `subprocess.run` with an argv list stays Deny. Observed reason at this
/// rev: "the program runs `rm` on `/etc` through `subprocess.run` — a file in
/// a system directory; ...".
#[test]
fn control_subprocess_run_rm_etc_stays_deny() {
    let fx = fixture("ctl-subprocess");
    let cmd = "python3 -c \"import subprocess; subprocess.run(['rm','-rf','/etc'])\"";
    for (subagent, d, r) in both_modes(&fx, cmd) {
        assert_eq!(d, "deny", "subagent={subagent}: cmd={cmd:?}; reason={r}");
        assert!(
            r.contains("subprocess.run"),
            "subagent={subagent}: expected the subprocess.run refusal; got {r}"
        );
    }
}
