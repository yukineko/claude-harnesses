#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED: blastguard false positive observed 2026-11-20 (rev c434e41e5 area,
//! deployed ~0.2.8xx). A SUBAGENT with cwd = the repo root ran
//!
//! ```text
//! mkdir -p D && cd D && python3 - <<'EOF'
//! import json,re
//! raw=open('<abs path>').read()
//! ...json.dump(d,open('decomposition-4items.json','w'),...)
//! EOF
//! condukt validate --file decomposition-4items.json; echo "validate=$?"
//! ```
//!
//! where D = `/private/tmp/claude-502/<project>/<session>/scratchpad` (the
//! session scratchpad). Verbatim deny reason:
//!
//! > the Python program imports `json`, `re`, and a command before it on the
//! > line writes into the directory it runs in (D, as D) an entry whose name
//! > is not read here, which may replace one of them — blastguard cannot
//! > judge what the Python program would do (S5: Undetermined)
//!
//! MAIN vs SUBAGENT: the same command byte-for-byte was ALLOWED in the main
//! session and DENIED in the subagent. A subagent cannot answer an Ask, so
//! the hook hardens Ask to Deny; the underlying main verdict is therefore
//! most likely Ask-shaped, and the observed "Allow in main" is itself
//! unexplained by this harness.
//!
//! OBSERVED IN THIS HARNESS (2026-11-20, worktree of c434e41e5, binary built
//! from that tree): main mode = Ask with the same S5 reason; subagent mode =
//! Deny (Ask hardened). The real main-session "Allow" was therefore likely the
//! user approving the Ask prompt (NOT verified).
//!
//! TWO READINGS (not decided here):
//!  (a) FP: `mkdir -p D` creates D itself (an entry in D's PARENT), not an
//!      entry inside D that could shadow `json.py` / `re.py`; D is the
//!      session-managed scratchpad. Telling it apart from (b): the verdict
//!      for `mkdir -p D` must not depend on writes "into D".
//!  (b) Intended: `mkdir -p` on an existing D is a no-op, and blastguard
//!      cannot know whether D pre-existed with planted `json.py`. Telling it
//!      apart from (a): a rule that allows only a D that is a session-managed
//!      scratchpad AND still treats a real `printf x > json.py` into D as
//!      non-allow (see `control_*`).
//!
//! The RED asserts main-mode verdict is allow and the subagent verdict is
//! allow. DOES NOT PROVE: that a fix keeps planted shadow modules caught
//! (that is `control_*`), nor anything off macOS.
#![cfg(target_os = "macos")]

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CLAUDE_TMP: &str = "/private/tmp/claude-502";

struct Scratch {
    top: PathBuf,
    home: PathBuf,
    proj: PathBuf,
    sp: PathBuf,
    created_claude_tmp: bool,
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.top);
        let _ = std::fs::remove_dir_all(&self.home);
        if self.created_claude_tmp {
            let _ = std::fs::remove_dir(CLAUDE_TMP);
        }
    }
}

/// `create_sp`: whether the scratchpad dir exists before the command runs.
fn scratch(tag: &str, create_sp: bool) -> Scratch {
    let created_claude_tmp = !Path::new(CLAUDE_TMP).exists();
    let top = Path::new(CLAUDE_TMP).join(format!("bgtest-mkcd-{tag}-{}", std::process::id()));
    let sp = top.join("62e92d16-0aef-402b-ad58-39425ad07ded/scratchpad");
    let tasks = top.join("62e92d16-0aef-402b-ad58-39425ad07ded/tasks");
    std::fs::create_dir_all(&tasks).unwrap();
    std::fs::write(tasks.join("a2e029d5cc5734dc6.output"), "{}\n").unwrap();
    if create_sp {
        std::fs::create_dir_all(&sp).unwrap();
    }
    let home = std::env::temp_dir().join(format!("bg-mkcd-home-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let proj = home.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    Scratch {
        top,
        home,
        proj,
        sp,
        created_claude_tmp,
    }
}

fn run_as(command: &str, s: &Scratch, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "62e92d16-0aef-402b-ad58-39425ad07ded",
        "cwd": s.proj,
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
        .env("CLAUDE_PROJECT_DIR", &s.proj)
        .env("BLASTGUARD_APPROVALS_DIR", s.home.join("approvals"))
        .current_dir(&s.proj)
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

fn observed_command(s: &Scratch) -> String {
    let d = s.sp.display();
    let t =
        s.sp.parent()
            .unwrap()
            .join("tasks/a2e029d5cc5734dc6.output");
    format!(
        "mkdir -p {d} && cd {d} && python3 - <<'EOF'\nimport json,re\nraw=open('{t}').read()\nlines=[json.loads(l) for l in raw.splitlines() if l.strip()]\nd=json.loads(\"{{}}\")\njson.dump(d,open('decomposition-4items.json','w'),ensure_ascii=False,indent=2)\nprint(len(d))\nEOF\ncondukt validate --file decomposition-4items.json; echo \"validate=$?\"\n",
        t = t.display()
    )
}

/// RED. Scratchpad exists (as in the real session).
#[test]
fn red_subagent_mkdir_cd_scratchpad_python_stdin_heredoc_not_blocked() {
    let s = scratch("red", true);
    let cmd = observed_command(&s);
    let main = run_as(&cmd, &s, false);
    let sub = run_as(&cmd, &s, true);
    assert_eq!(
        main.0, "allow",
        "main-mode verdict (observed Allow in the real main session); got {} reason: {}",
        main.0, main.1
    );
    assert_eq!(
        sub.0, "allow",
        "subagent verdict must equal main (allow); got {} reason: {}",
        sub.0, sub.1
    );
}

/// Control (green): a real shadowing module written into the cwd stays blocked.
#[test]
fn control_planted_json_py_then_python_stdin_stays_non_allow() {
    let s = scratch("ctl", true);
    let d = s.sp.display();
    let cmd = format!("cd {d} && printf x > json.py && python3 - <<'EOF'\nimport json\nEOF\n");
    for sub in [false, true] {
        let (v, r) = run_as(&cmd, &s, sub);
        assert_ne!(
            v, "allow",
            "planted json.py + python3 stdin must not be allowed (subagent={sub}); reason: {r}"
        );
    }
}
