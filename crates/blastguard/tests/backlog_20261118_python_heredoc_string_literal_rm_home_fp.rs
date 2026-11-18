#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED pin (false positive, observed 2026-11-18 in the main session of
//! fcf56f45): a `python3 - <path> <<'EOF'` program whose ONLY occurrence of
//! `rm -rf ~` is BACKTICK-WRAPPED TEXT INSIDE A PYTHON STRING LITERAL, and
//! which uses no `os` / `subprocess` / `exec` / `eval`, is Deny with the
//! verbatim reason
//!
//! > recursive rm (-r) can delete an entire directory tree [system harm:
//! > refused on every request, never handed back as a question]
//!
//! Exact observed command (recovered from `blastguard log --interactive
//! --since 1d`: `decision=deny actor=interactive system_harm=true
//! rule=rm-recursive`): `python3 - /Users/yuki/Documents/vault/.../record.md
//! <<'EOF'` with `p=sys.argv[1]; s=open(p).read()`, a dict of triple-quoted
//! Japanese prose values, one of which holds the text
//! "（文字列中の `rm -rf ~` や `->` に反応）", then `s=s.replace(tag,v)`,
//! `open(p,'w').write(s)`, `EOF`, `grep -c 'fill:' <path>`.
//!
//! MINIMISED by the test author (probe with the debug binary of this rev, fixture
//! HOME, cwd = fixture project): the trigger is the BACKTICKS. Measured
//! (main / subagent):
//!   * no file write, `s = "prose `rm -rf ~` x"; print(len(s))` — and the
//!     same with `'...'` or `"""..."""` quoting — deny / deny, the reason above;
//!   * the same program WITHOUT the backticks (`"prose rm -rf ~ x"`) —
//!     allow / allow;
//!   * backticks around harmless text (`"""prose `ls -l` x"""`) — allow / allow;
//!   * the observed read-replace-write shape (argv path, or literal path to
//!     an existing .md) with backticks — deny / deny, the reason above;
//!     without the backticks — ask / deny for a DIFFERENT reason ("the
//!     program calls `open` on a path blastguard cannot resolve statically
//!     ..." for the argv path; "`,'w')` onto `<file>` is confined to ... a
//!     safe root ..." for the existing literal path).
//!
//! So the backtick text inside a string literal is judged as a real
//! `rm -rf ~`. INFERRED, NOT TRACED in source by this author: that the
//! backtick span is read as a shell command substitution (consistent with
//! `backlog_20261117_python_heredoc_marker_fp.rs`: "`" is in
//! `DYNAMIC_EXECUTION_MARKERS`, matched over the whole program text).
//!
//! WHAT IT PROVES (when the RED cases pass):
//!   * the no-write program holding backtick-wrapped `rm -rf ~` in a string
//!     is Allow in main and subagent mode (as its no-backtick twin is today);
//!   * for the observed read-replace-write shape (argv path under the fixture
//!     HOME, and a literal path to an EXISTING `.md` in the fixture project),
//!     the backtick-wrapped `rm -rf ~` does not change the verdict: it equals
//!     the verdict of the identical program without the backticks, and the
//!     reason does not cite recursive rm.
//!
//! Controls (not ignored, pass today): the no-backtick twins are not the rm
//! refusal; `os.system('rm -rf ~')` as real code stays Deny; the c1873b73
//! twin `os.__dict__['sys'+'tem']('rm -rf ~')` stays not-Allow.
//!
//! WHAT IT DOES NOT PROVE: that the read-replace-write shapes become Allow —
//! their no-backtick twins are Ask today for the separate reasons quoted
//! above (unresolved argv `open` path; write of an existing file in a safe
//! root), and those are not this defect; other harmful texts in backticks
//! (`rm -rf /etc`, `chmod 777 /etc/hosts`); `python3 -c` programs (the test
//! author's own `python3 -c` holding the string "rm -rf" was also refused
//! with "recursive rm (-r) can delete an entire directory tree" while
//! writing this test — observed, not pinned here); unquoted heredocs; the
//! `->` FP named in the same observed prose; programs that DO hold a real
//! execution path beside the string; a string later run by a following
//! command (`bash note.md`); the exact reason text after the fix.
//!
//! Expected verdict for the no-write program: Allow, following the
//! convention of the analogous data-only python-heredoc FP tests:
//!   * `backlog_20261115_python_string_literal_subprocess_chmod_data_fp.rs`
//!     pins `src = "..."; print(len(src))` as allow in main and subagent
//!     ("string literal is data");
//!   * `backlog_20261117_python_heredoc_marker_fp.rs` pins the backtick-in-
//!     literal shapes as not-Ask/not-Deny, with an Allow control for the
//!     marker-free twin.
//!
//! Grounds: memory `false-positives-not-tolerated`; the user's standing
//! ruling that python heredoc bodies are read by the legacy reader until
//! stage S5 (a Python parser) lands — this RED is an S5 acceptance criterion,
//! to be fixed structurally there, not by a one-off patch.
//!
//! Reproduced at main 79b2aaec3 (blastguard 0.2.706) with the debug test
//! binary; the observed session's hook version was not recorded.
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
    /// Stands in for the observed `~/Documents/vault/...` record note.
    fn vault(&self) -> PathBuf {
        self.home.join("Documents").join("vault")
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "py-str-rmhome-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    std::fs::create_dir_all(home.join("Documents").join("vault")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base, home }
}

fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "py-str-rmhome-20261118",
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

const RM_REASON: &str = "recursive rm (-r) can delete an entire directory tree";

/// The string literal of the observed prose, with the backticks.
const BACKTICK_TEXT: &str = "\"\"\"worker is stopped by text such as `rm -rf ~` in a string\"\"\"";
/// The same literal without the backticks (the twin).
const PLAIN_TEXT: &str = "\"\"\"worker is stopped by text such as rm -rf ~ in a string\"\"\"";

/// The observed program shape: read `argv[1]`, replace a marker with
/// `literal`, write it back, then `grep -c` the file.
fn observed_argv_shape(target: &Path, literal: &str) -> String {
    let t = target.display();
    format!(
        "python3 - {t} <<'EOF'\n\
         import sys\n\
         p=sys.argv[1]; s=open(p).read()\n\
         s=s.replace(\"<!-- fill: x -->\", {literal})\n\
         open(p,'w').write(s)\n\
         EOF\n\
         grep -c 'fill:' {t}"
    )
}

/// Read-replace-write of an existing `.md` file named by a literal path.
fn literal_md_shape(target: &Path, literal: &str) -> String {
    let t = target.display();
    format!(
        "python3 - <<'EOF'\n\
         s=open('{t}').read()\n\
         s=s.replace('<!-- fill: x -->', {literal})\n\
         open('{t}','w').write(s)\n\
         EOF"
    )
}

/// No file write: the string is only measured.
fn no_write_shape(literal: &str) -> String {
    format!("python3 - <<'EOF'\ns = {literal}\nprint(len(s))\nEOF")
}

/// The backtick program's verdict equals its no-backtick twin's in both
/// modes, and its reason does not cite recursive rm.
fn assert_backticks_do_not_change_verdict(fx: &Fx, with_bt: &str, twin: &str) {
    let got = both_modes(fx, with_bt);
    let want = both_modes(fx, twin);
    for ((subagent, d, r), (_, wd, wr)) in got.iter().zip(want.iter()) {
        assert!(
            !r.contains(RM_REASON),
            "subagent={subagent}: backtick text inside a python string literal is data, \
             not a recursive rm; got {d} ({r}); cmd={with_bt:?}"
        );
        assert_eq!(
            d, wd,
            "subagent={subagent}: verdict must equal the no-backtick twin's ({wd}: {wr}); \
             got ({r}); cmd={with_bt:?}"
        );
    }
}

// ------------------------------------------------------------------- RED

#[test]
#[ignore = "RED(b424eaed): 2026-11-18 backtick-wrapped `rm -rf ~` inside a python heredoc string literal (no write, no exec) is Deny as system harm; S5 (python parser) acceptance criterion"]
fn no_write_backtick_rm_home_in_string_is_allow() {
    let fx = fixture("red-nowrite");
    for lit in [
        BACKTICK_TEXT,
        "\"prose `rm -rf ~` x\"",
        "'prose `rm -rf ~` x'",
    ] {
        let cmd = no_write_shape(lit);
        for (subagent, d, r) in both_modes(&fx, &cmd) {
            assert_eq!(
                d, "allow",
                "subagent={subagent}: string literal is data; cmd={cmd:?}; reason={r}"
            );
        }
    }
}

#[test]
#[ignore = "RED(b424eaed): 2026-11-18 observed shape (python3 - <path> heredoc, argv read-replace-write) with backtick `rm -rf ~` in a string is Deny as system harm; S5 (python parser) acceptance criterion"]
fn observed_argv_shape_backtick_text_does_not_change_verdict() {
    let fx = fixture("red-observed");
    let target = fx.vault().join("record.md");
    std::fs::write(&target, "# record\n<!-- fill: x -->\n").unwrap();
    assert_backticks_do_not_change_verdict(
        &fx,
        &observed_argv_shape(&target, BACKTICK_TEXT),
        &observed_argv_shape(&target, PLAIN_TEXT),
    );
}

#[test]
#[ignore = "RED(b424eaed): 2026-11-18 literal-path read-replace-write of an existing .md with backtick `rm -rf ~` in a string is Deny as system harm; S5 (python parser) acceptance criterion"]
fn existing_md_literal_path_backtick_text_does_not_change_verdict() {
    let fx = fixture("red-md");
    let target = fx.proj().join("README.md");
    std::fs::write(&target, "# title\n<!-- fill: x -->\n").unwrap();
    assert_backticks_do_not_change_verdict(
        &fx,
        &literal_md_shape(&target, BACKTICK_TEXT),
        &literal_md_shape(&target, PLAIN_TEXT),
    );
}

// --------------------------------------------------------------- controls

/// Control: the no-write twins without backticks, and backticks around
/// harmless text, are Allow today.
#[test]
fn control_no_write_twins_are_allow() {
    let fx = fixture("ctl-nowrite");
    for lit in [PLAIN_TEXT, "\"\"\"prose `ls -l` x\"\"\""] {
        let cmd = no_write_shape(lit);
        for (subagent, d, r) in both_modes(&fx, &cmd) {
            assert_eq!(d, "allow", "subagent={subagent}: cmd={cmd:?}; reason={r}");
        }
    }
}

/// Control: the read-replace-write twins without backticks are not the
/// recursive-rm refusal today (their Ask has another reason, see header).
#[test]
fn control_read_replace_write_twins_are_not_the_rm_refusal() {
    let fx = fixture("ctl-twins");
    let vault = fx.vault().join("record.md");
    std::fs::write(&vault, "# record\n<!-- fill: x -->\n").unwrap();
    let md = fx.proj().join("README.md");
    std::fs::write(&md, "# title\n<!-- fill: x -->\n").unwrap();
    for cmd in [
        observed_argv_shape(&vault, PLAIN_TEXT),
        literal_md_shape(&md, PLAIN_TEXT),
    ] {
        for (subagent, d, r) in both_modes(&fx, &cmd) {
            assert!(
                !r.contains(RM_REASON),
                "subagent={subagent}: twin should not be the rm refusal; got {d} ({r}); cmd={cmd:?}"
            );
        }
    }
}

/// Control: a program that actually runs `rm -rf ~` stays Deny.
#[test]
fn control_os_system_rm_home_stays_deny() {
    let fx = fixture("ctl-os-system");
    let cmd = "python3 - <<'EOF'\nimport os\nos.system('rm -rf ~')\nEOF";
    for (subagent, d, r) in both_modes(&fx, cmd) {
        assert_eq!(d, "deny", "subagent={subagent}: cmd={cmd:?}; reason={r}");
    }
}

/// Control (c1873b73 twin): reaching `os.system` through `os.__dict__` with a
/// concatenated key must not be Allow.
#[test]
fn control_os_dict_concat_system_rm_home_is_not_allow() {
    let fx = fixture("ctl-os-dict");
    let cmd = "python3 - <<'EOF'\nimport os\nos.__dict__['sys'+'tem']('rm -rf ~')\nEOF";
    for (subagent, d, r) in both_modes(&fx, cmd) {
        assert_ne!(d, "allow", "subagent={subagent}: cmd={cmd:?}; reason={r}");
    }
}
