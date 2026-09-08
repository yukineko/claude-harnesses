//! `ctxrot rescue` (PreCompact) — what happens when the transcript CANNOT BE READ.
//!
//! The rescue hook exists to save the conversation before `/compact` destroys
//! it. It decides whether there is anything to save from
//! `harness_core::transcript::recent_turns`, which today maps **every** failure
//! — `File::open` error, line read error, JSON parse error — onto `Vec::new()`
//! (`crates/harness-core/src/transcript.rs`, `recent_turns`). The consumer then
//! does `if turns.is_empty() { return; }` (`src/hooks/rescue.rs:71`).
//!
//! So "this transcript holds no user/assistant turns" and "this transcript could
//! not be read at all" produce the identical observable: **no note, no message,
//! exit 0**. That is CLAUDE.md §3 exactly — a cannot-determine resolved to the
//! permissive side, and §1's "silence is not an acceptable degrade": the user
//! loses the carryover this hook exists to preserve and is never told.
//!
//! The correct shape already sits a few lines ABOVE the defect in the same
//! function: the existing-note lookup receives
//! `harness_core::verdict::Determination::Undetermined(why)` and prints a
//! diagnostic to stderr instead of mistaking it for "none exists"
//! (`src/hooks/rescue.rs:61-66`). These tests pin the same treatment for the
//! transcript read, and — deliberately — pin it at the OBSERVABLE layer (the
//! real hook binary's stderr / notes on disk) rather than on `recent_turns`'s
//! return type, so they can be observed RED against the code as it stands today
//! and stay meaningful whatever type the fix lands on.
//!
//! Two unreadable shapes are pinned on purpose:
//!
//! * `unopenable` — `File::open` fails (mode 000). A fix could satisfy this one
//!   alone with an `open()` probe in `rescue.rs` that never touches the real
//!   defect.
//! * `undecodable` — `File::open` SUCCEEDS and the bytes are not UTF-8, so
//!   `BufRead::lines()` yields `Err` and every line is silently `continue`d.
//!   An `open()`-probe shim cannot see this one; only propagating the failure
//!   out of the transcript layer can.
//!
//! Anti-vacuity: the fix must not be reachable by making the hook noisy or by
//! breaking the working path. A transcript that genuinely contains no
//! user/assistant turns must stay silent and write nothing
//! (`no_turns_transcript_stays_silent_and_writes_no_note`), and a transcript
//! with turns must still produce its note
//! (`transcript_with_turns_still_writes_its_note`). Both pass today and must
//! keep passing.
//!
//! NOT pinned (open design question, deliberately left to the implementer): a
//! transcript that is PARTIALLY readable — some decodable turns plus an
//! undecodable line. Whether that should still write its note, warn, or both is
//! a judgement call these tests do not pre-empt.

#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// A sandboxed run of the real `ctxrot rescue` hook: `HOME` is swapped so the
/// note store/state land in the temp dir instead of the user's, and the
/// on-compact background distill is off so no model is ever spawned.
fn run_rescue(home: &Path, cwd: &Path, transcript: &Path, session: &str) -> Run {
    let payload = serde_json::json!({
        "session_id": session,
        "transcript_path": transcript.to_string_lossy(),
        "cwd": cwd.to_string_lossy(),
        "hook_event_name": "PreCompact",
        // NOT a `band-` trigger: the coalescing branch (and its own, already
        // correct, Undetermined arm) is out of scope here.
        "trigger": "manual",
    })
    .to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_ctxrot"))
        .arg("rescue")
        .env("HOME", home)
        .env("CTXROT_DISTILL_ON_COMPACT", "0")
        .env_remove("GUARD_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ctxrot");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("wait ctxrot");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Every rescue note written under the sandboxed `HOME`.
fn rescue_notes(home: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![home.join(".ctxrot").join("store")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with("rescue-") && n.ends_with(".md"))
            {
                found.push(p);
            }
        }
    }
    found
}

/// A sandbox: `(tempdir, home, project cwd)`.
fn sandbox() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("proj");
    std::fs::create_dir_all(&home).expect("mkdir home");
    std::fs::create_dir_all(&cwd).expect("mkdir cwd");
    (tmp, home, cwd)
}

/// A transcript file that exists but cannot be OPENED (mode 000).
///
/// The precondition is asserted, not assumed: running as a user who can read a
/// mode-000 file (root) would make this test vacuous, and a vacuous test that
/// silently passes is the very failure mode being pinned — so it fails loudly
/// instead.
fn unopenable_transcript(dir: &Path) -> PathBuf {
    let p = dir.join("unopenable.jsonl");
    std::fs::write(
        &p,
        b"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
    )
    .expect("write transcript");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
    assert!(
        std::fs::File::open(&p).is_err(),
        "precondition failed: {} is still openable after chmod 000 (running as root?). \
         This test cannot observe the unreadable-transcript path in this environment.",
        p.display()
    );
    p
}

/// A transcript file that OPENS fine but whose bytes are not UTF-8, so every
/// `BufRead::lines()` item is an `Err` that `recent_turns` silently skips.
fn undecodable_transcript(dir: &Path) -> PathBuf {
    let p = dir.join("undecodable.jsonl");
    std::fs::write(&p, b"\xff\xfe\xff not utf-8\n\xff\xfe\n").expect("write transcript");
    assert!(
        std::fs::File::open(&p).is_ok(),
        "precondition failed: {} should be openable",
        p.display()
    );
    assert!(
        std::fs::read_to_string(&p).is_err(),
        "precondition failed: {} decoded as UTF-8, so the read-error path is not exercised",
        p.display()
    );
    p
}

/// A well-formed transcript containing zero user/assistant turns.
fn no_turns_transcript(dir: &Path) -> PathBuf {
    let p = dir.join("noturns.jsonl");
    std::fs::write(
        &p,
        "{\"type\":\"system\",\"message\":{\"role\":\"system\",\"content\":\"boot\"}}\n\
         {\"type\":\"summary\",\"summary\":\"prior session\"}\n",
    )
    .expect("write transcript");
    p
}

/// A transcript with real user/assistant turns (the crate's own fixture).
fn good_transcript(dir: &Path) -> PathBuf {
    let p = dir.join("good.jsonl");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("transcript.jsonl"),
        &p,
    )
    .expect("copy fixture");
    p
}

// ---------------------------------------------------------------------------
// The defect (RED today)
// ---------------------------------------------------------------------------

/// A transcript that cannot be OPENED must not be reported as "nothing to save".
///
/// The hook stays exit 0 (it is an observability hook — it must not break the
/// compaction), but it must SAY that it could not read the transcript, naming
/// the path, exactly as the sibling `Determination::Undetermined` arm in the
/// same function already says it could not check for an existing note.
#[test]
fn unopenable_transcript_is_reported_not_silently_treated_as_no_turns() {
    let (tmp, home, cwd) = sandbox();
    let transcript = unopenable_transcript(tmp.path());

    let run = run_rescue(&home, &cwd, &transcript, "sess-unopenable");

    assert_eq!(
        run.code, 0,
        "rescue must stay exit 0; stderr={}",
        run.stderr
    );
    assert!(
        !run.stderr.trim().is_empty(),
        "an unreadable transcript produced NO diagnostic at all — indistinguishable \
         from a transcript with nothing worth saving. stdout={:?} stderr={:?}",
        run.stdout,
        run.stderr
    );
    assert!(
        run.stderr.contains(&*transcript.to_string_lossy()),
        "the diagnostic must name the transcript it could not read ({}); got stderr={:?}",
        transcript.display(),
        run.stderr
    );
}

/// A transcript that opens but whose CONTENT cannot be decoded must not be
/// reported as "nothing to save" either.
///
/// This is the shim-proof half: an `File::open()` probe bolted onto the
/// consumer passes the test above and still fails here, because the failure is
/// only visible inside `recent_turns`'s read loop.
#[test]
fn undecodable_transcript_is_reported_not_silently_treated_as_no_turns() {
    let (tmp, home, cwd) = sandbox();
    let transcript = undecodable_transcript(tmp.path());

    let run = run_rescue(&home, &cwd, &transcript, "sess-undecodable");

    assert_eq!(
        run.code, 0,
        "rescue must stay exit 0; stderr={}",
        run.stderr
    );
    assert!(
        !run.stderr.trim().is_empty(),
        "a transcript whose bytes could not be read produced NO diagnostic — the read \
         errors were swallowed line by line. stdout={:?} stderr={:?}",
        run.stdout,
        run.stderr
    );
    assert!(
        run.stderr.contains(&*transcript.to_string_lossy()),
        "the diagnostic must name the transcript it could not read ({}); got stderr={:?}",
        transcript.display(),
        run.stderr
    );
}

/// The contract stated as one comparison: "could not read it" and "there was
/// nothing in it" must not be the same observable event.
///
/// Kept separate from the two tests above so that the requirement survives even
/// if their exact assertions are ever re-negotiated: whatever the diagnostic
/// says, the two runs must differ, and the no-turns run must be the quiet one.
#[test]
fn unreadable_and_no_turns_are_distinguishable() {
    let (tmp, home, cwd) = sandbox();
    let unreadable = unopenable_transcript(tmp.path());
    let empty = no_turns_transcript(tmp.path());

    let unreadable_run = run_rescue(&home, &cwd, &unreadable, "sess-unreadable");
    let empty_run = run_rescue(&home, &cwd, &empty, "sess-empty");

    assert!(
        empty_run.stderr.trim().is_empty(),
        "a transcript with genuinely no turns must stay quiet (anti-vacuity: the fix \
         must not be 'always print something'); got stderr={:?}",
        empty_run.stderr
    );
    assert_ne!(
        unreadable_run.stderr.trim(),
        empty_run.stderr.trim(),
        "an unreadable transcript and an empty one are reported identically, so a \
         downstream reader cannot tell a failed rescue from an unnecessary one"
    );
}

// ---------------------------------------------------------------------------
// Anti-vacuity controls (GREEN today — must stay GREEN after the fix)
// ---------------------------------------------------------------------------

/// Today's correct behaviour for a genuinely empty conversation: no note, no
/// noise. The fix must not buy its diagnostic by warning about every transcript.
#[test]
fn no_turns_transcript_stays_silent_and_writes_no_note() {
    let (tmp, home, cwd) = sandbox();
    let transcript = no_turns_transcript(tmp.path());

    let run = run_rescue(&home, &cwd, &transcript, "sess-noturns");

    assert_eq!(
        run.code, 0,
        "rescue must stay exit 0; stderr={}",
        run.stderr
    );
    assert!(
        run.stderr.trim().is_empty(),
        "nothing to rescue must stay quiet; got stderr={:?}",
        run.stderr
    );
    assert!(
        run.stdout.trim().is_empty(),
        "PreCompact injects nothing on stdout; got stdout={:?}",
        run.stdout
    );
    assert!(
        rescue_notes(&home).is_empty(),
        "no note may be written when there are no turns; found {:?}",
        rescue_notes(&home)
    );
}

/// Today's correct behaviour for a real conversation: the note lands and the
/// hook says where. The fix must not break the path the hook exists for.
#[test]
fn transcript_with_turns_still_writes_its_note() {
    let (tmp, home, cwd) = sandbox();
    let transcript = good_transcript(tmp.path());

    let run = run_rescue(&home, &cwd, &transcript, "sess-good");

    assert_eq!(
        run.code, 0,
        "rescue must stay exit 0; stderr={}",
        run.stderr
    );
    let notes = rescue_notes(&home);
    assert_eq!(
        notes.len(),
        1,
        "exactly one rescue note expected; found {notes:?} (stderr={:?})",
        run.stderr
    );
    assert!(
        run.stderr.contains("rescue note saved"),
        "the hook must report the saved note; got stderr={:?}",
        run.stderr
    );
    let body = std::fs::read_to_string(&notes[0]).expect("read note");
    assert!(
        body.contains("trigger: manual"),
        "note must record the trigger; got:\n{body}"
    );
    assert!(
        body.contains("serde"),
        "note must carry the transcript's own turns; got:\n{body}"
    );
}
