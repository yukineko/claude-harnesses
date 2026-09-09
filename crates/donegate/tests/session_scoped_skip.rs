// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end: donegate's one-shot escape hatch must belong to the session
//! that asked for it.
//!
//! ## The defect this pins shut
//!
//! `gate_run` and `refuse` both consumed `.donegate-skip` from **the project
//! root** (`consume_skip(&root, ".donegate-skip")`). The project root is shared
//! by every session working in that checkout and the file carries no
//! attribution, so whichever session's Stop hook fired next consumed it — its
//! own legitimate gate waved through on an exception it never asked for, and
//! the session that created the marker left without its exception. CLAUDE.md §5
//! forbids exactly this: 「並行セッションがあるときは project root の共有 skip
//! ファイルを使わない (一度だけ消費されるため、他セッションの正当なゲートを
//! 素通りさせる)」.
//!
//! ## Why the hatch cannot simply be deleted
//!
//! The documented alternatives do not exist in practice. `DONEGATE_DISABLE`
//! is read from the hook process's environment, and a Stop hook is a child of
//! the Claude Code app — it inherits the app's environment, not the one a Bash
//! tool call exported into. `~/.claude/settings.json` is permission-denied for
//! editing. So the shared file was the only in-session way out, which is the
//! structural reason gate work degrades into getting past the gate. The
//! replacement is therefore a *narrowing*, not a removal: attributed instead of
//! anonymous, reason-required instead of silent, recorded instead of invisible,
//! and limited to one session instead of applying to all of them.
//!
//! ## Which tests can fail, and how
//!
//! Every gate-behaviour assertion here is paired with a control, because a
//! suite in which the gate always blocks would satisfy most of these
//! vacuously: `a_red_check_with_no_skip_at_all_still_blocks` and
//! `a_green_check_allows_with_no_skip_involved` are those controls.
//!
//! Written by a DISINTERESTED author (CLAUDE.md §2(a)); the implementer of the
//! fix did not write these.
//!
//! ## Isolation
//!
//! Every test builds its own `$HOME` under `std::env::temp_dir()` and passes it
//! to the child process with `.env("HOME", …)`. donegate's default state dir is
//! `$HOME/.donegate/state` and its trust list is `$HOME/.harness/trust.toml`, so
//! a per-child `HOME` moves *all* of donegate's durable state into the scratch
//! dir. No process-global environment variable is mutated, and neither the live
//! `~/.donegate`, `~/.harness/trust.toml` nor `~/.overwatch` is read or written.
//! (Idiom reused from `tests/giveup_sentinel.rs`.)

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("donegate-skip-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

/// A trusted project rooted under an isolated `$HOME`, carrying exactly one
/// required check running `cmd`. Returns (home, project root).
///
/// The project MUST be trusted or `Config::load` ignores `donegate.toml`
/// outright and the gate has no checks at all — every "the gate allowed"
/// assertion would then pass against a gate that never ran anything.
fn project(tag: &str, cmd: &str) -> (PathBuf, PathBuf) {
    let home = scratch(tag);
    let root = home.join("project");
    std::fs::create_dir_all(&root).expect("create project root");

    std::fs::write(
        root.join("donegate.toml"),
        format!("max_attempts = 50\n\n[[check]]\nname = \"typecheck\"\ncmd = \"{cmd}\"\n"),
    )
    .expect("write donegate.toml");

    // `trust::is_trusted` canonicalizes before comparing, so the seeded entry
    // must be the canonical form (on macOS $TMPDIR resolves through /private).
    let canon = std::fs::canonicalize(&root).expect("canonicalize project root");
    std::fs::create_dir_all(home.join(".harness")).expect("create ~/.harness");
    std::fs::write(
        home.join(".harness/trust.toml"),
        format!("trusted = [\"{}\"]\n", canon.display()),
    )
    .expect("write trust.toml");

    (home, root)
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    /// The Stop-hook block protocol: a `{"decision":"block",…}` line on stdout.
    fn blocked(&self) -> bool {
        self.stdout.contains("\"decision\"") && self.stdout.contains("block")
    }
}

/// Run `donegate <args>` in `root` under an isolated `$HOME`, with `stdin` on
/// stdin and `CLAUDE_CODE_SESSION_ID` set to `session` (unless `session` is
/// `None`, in which case the variable is explicitly removed).
fn run(home: &Path, root: &Path, args: &[&str], session: Option<&str>, stdin: &str) -> Run {
    let bin = env!("CARGO_BIN_EXE_donegate");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(root)
        .env("HOME", home)
        .env_remove("DONEGATE_DISABLE")
        .env_remove("HARNESS_TRUST_ALL");
    match session {
        Some(s) => {
            cmd.env("CLAUDE_CODE_SESSION_ID", s);
        }
        None => {
            cmd.env_remove("CLAUDE_CODE_SESSION_ID");
        }
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("donegate spawns");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("donegate runs");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Drive one Stop as `session`. Built with a real JSON encoder so a `\` or a
/// quote in the temp path cannot corrupt the payload — a payload that fails to
/// parse collapses `session_key()` onto the shared `_local` bucket, which would
/// silently merge the two distinct sessions these tests exist to keep apart.
fn stop(home: &Path, root: &Path, session: &str) -> Run {
    let payload = serde_json::json!({
        "session_id": session,
        "cwd": root.to_string_lossy(),
        "hook_event_name": "Stop",
    })
    .to_string();
    // The env var is set to the same id: a real Stop hook carries the id in the
    // payload, and pinning both keeps the test from depending on which of the
    // two the gate happens to read.
    run(home, root, &["gate"], Some(session), &payload)
}

/// Ask donegate for a one-shot skip, as `session` would from its own shell.
fn issue_skip(home: &Path, root: &Path, session: Option<&str>, reason: Option<&str>) -> Run {
    let mut args: Vec<&str> = vec!["skip"];
    if let Some(r) = reason {
        args.push("--reason");
        args.push(r);
    }
    run(home, root, &args, session, "")
}

/// Concatenated contents of every `log.jsonl` under the isolated `$HOME`.
/// Read straight off disk rather than through donegate's own code, so the
/// observation does not depend on the thing under test.
fn gate_log(home: &Path) -> String {
    fn walk(dir: &Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "log.jsonl") {
                out.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    let mut out = String::new();
    walk(home, &mut out);
    out
}

// ===========================================================================
// ANTI-VACUITY CONTROLS — run these first when reading the file. Without them
// every "must block" assertion below would also hold against a gate that
// blocks unconditionally, and every "must allow" against one that never runs.
// ===========================================================================

/// CONTROL 1: a red check with no skip anywhere blocks. Establishes that the
/// gate under test is actually enforcing, so a later "blocked" is meaningful.
#[test]
fn a_red_check_with_no_skip_at_all_still_blocks() {
    let (home, root) = project("control-red", "exit 1");
    let r = stop(&home, &root, "sess-control");
    assert_eq!(r.code, 0, "a Stop hook always exits 0 toward Claude");
    assert!(
        r.blocked(),
        "a failing required check with no skip must block the stop; stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );
}

/// CONTROL 2: a green check allows, with no skip involved. Establishes that an
/// "allowed" outcome is reachable at all, so a later "allowed" is meaningful.
#[test]
fn a_green_check_allows_with_no_skip_involved() {
    let (home, root) = project("control-green", "exit 0");
    let r = stop(&home, &root, "sess-control");
    assert_eq!(r.code, 0);
    assert!(
        !r.blocked(),
        "a passing check must allow the stop; stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );
}

// ===========================================================================
// THE HEADLINE
// ===========================================================================

/// An unattributed marker in the SHARED project root must not wave anybody's
/// stop through.
///
/// This is the defect stated as a single observation, and it needs no second
/// session to demonstrate: the file names no session, so any session that stops
/// consumes it. Concurrency is what makes it *harmful*, not what makes it
/// *true*. Against the pre-fix binary the gate consumes `.donegate-skip` and
/// exits 0 with no decision, so this assertion fires.
#[test]
fn an_unattributed_project_root_marker_no_longer_waves_a_stop_through() {
    let (home, root) = project("shared-marker", "exit 1");
    std::fs::write(
        root.join(".donegate-skip"),
        "left here by some other session\n",
    )
    .expect("write the legacy shared marker");

    let r = stop(&home, &root, "sess-victim");
    assert_eq!(r.code, 0, "a Stop hook always exits 0 toward Claude");
    assert!(
        r.blocked(),
        "a file in the SHARED project root waved this session's stop through. The marker names no \
         session, so it applies to whichever session stops next — CLAUDE.md §5 forbids precisely \
         this. stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );
}

/// The two-session form of the same defect, driven through the supported CLI:
/// session A asks for a skip, session B stops. B must still be gated, and A's
/// exception must still be waiting for A.
///
/// Both halves matter. "B was blocked" alone would also hold if issuing had
/// simply failed; the second half proves the hatch works and is merely
/// invisible to B.
#[test]
fn a_skip_issued_by_another_session_does_not_wave_my_gate_through() {
    let (home, root) = project("cross-session", "exit 1");

    let issued = issue_skip(&home, &root, Some("sess-A"), Some("landing a doc-only fix"));
    assert_eq!(
        issued.code, 0,
        "`donegate skip --reason …` must succeed for the issuing session; stdout={:?} stderr={:?}",
        issued.stdout, issued.stderr
    );

    let b = stop(&home, &root, "sess-B");
    assert!(
        b.blocked(),
        "session B's legitimate gate was waved through by a skip session A asked for. stdout={:?} \
         stderr={:?}",
        b.stdout,
        b.stderr
    );

    let a = stop(&home, &root, "sess-A");
    assert!(
        !a.blocked(),
        "session A's own skip did not survive session B's stop — a hatch another session can \
         consume out from under you is a race, not an escape hatch. stdout={:?} stderr={:?}",
        a.stdout,
        a.stderr
    );
}

// ===========================================================================
// The hatch still works, and only once, for its own session
// ===========================================================================

/// The issuing session gets its exception, and the reason it gave is surfaced
/// so the bypass is legible in the transcript.
#[test]
fn my_own_skip_allows_my_stop_and_names_the_reason() {
    let (home, root) = project("same-session", "exit 1");
    let issued = issue_skip(&home, &root, Some("sess-mine"), Some("bisecting a flake"));
    assert_eq!(
        issued.code, 0,
        "issuing must succeed; stderr={:?}",
        issued.stderr
    );

    let r = stop(&home, &root, "sess-mine");
    assert!(
        !r.blocked(),
        "the session that issued the skip must be let through; stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );
    assert!(
        r.stderr.contains("bisecting a flake"),
        "the consumed skip's reason must be surfaced, so the bypass is legible rather than silent; \
         stderr={:?}",
        r.stderr
    );
}

/// ONE-SHOT: the exception covers one stop. The very next stop in the same
/// session is gated again, or the hatch is not an exception but an off switch.
#[test]
fn a_skip_covers_exactly_one_stop() {
    let (home, root) = project("one-shot", "exit 1");
    issue_skip(&home, &root, Some("sess-mine"), Some("one stop only"));

    let first = stop(&home, &root, "sess-mine");
    assert!(
        !first.blocked(),
        "apparatus: the first stop must be allowed, else the block below is vacuous; stdout={:?} \
         stderr={:?}",
        first.stdout,
        first.stderr
    );

    let second = stop(&home, &root, "sess-mine");
    assert!(
        second.blocked(),
        "the skip was still in effect on the next stop: a one-stop exception silently became an \
         open-ended bypass of a still-red check. stdout={:?} stderr={:?}",
        second.stdout,
        second.stderr
    );
}

// ===========================================================================
// Refusals
// ===========================================================================

/// A skip must say WHY. Refusal has to be observable in the exit code AND
/// effective — an "error" message followed by a working skip is not a refusal.
///
/// The apparatus block is load-bearing. Without it this test passes against a
/// binary that has no `skip` subcommand at all: every invocation "fails" and
/// every stop blocks, so "the reasonless skip was refused" is indistinguishable
/// from "nothing is implemented". Measured, not assumed — that is exactly how
/// it behaved against the pre-fix binary.
#[test]
fn a_skip_without_a_reason_is_refused() {
    let (home, root) = project("no-reason-apparatus", "exit 1");
    let ok = issue_skip(&home, &root, Some("sess-mine"), Some("a real reason"));
    assert_eq!(
        ok.code, 0,
        "apparatus: `donegate skip --reason …` must work, else the refusals below measure nothing; \
         stdout={:?} stderr={:?}",
        ok.stdout, ok.stderr
    );
    assert!(
        !stop(&home, &root, "sess-mine").blocked(),
        "apparatus: a skip WITH a reason must actually let the stop through, else 'refused' below \
         is indistinguishable from 'not implemented'"
    );

    for (tag, reason) in [
        ("missing", None),
        ("empty", Some("")),
        ("blank", Some("   ")),
    ] {
        let (home, root) = project(&format!("no-reason-{tag}"), "exit 1");
        let issued = issue_skip(&home, &root, Some("sess-mine"), reason);
        assert_ne!(
            issued.code, 0,
            "`donegate skip` with reason {reason:?} must fail: an unexplained bypass is invisible \
             to review even when it is recorded. stdout={:?} stderr={:?}",
            issued.stdout, issued.stderr
        );

        let r = stop(&home, &root, "sess-mine");
        assert!(
            r.blocked(),
            "a refused skip still let the stop through — the refusal was cosmetic. stdout={:?} \
             stderr={:?}",
            r.stdout,
            r.stderr
        );
    }
}

/// No session id means the skip cannot be attributed to anyone, which is
/// exactly the shared marker again. CLAUDE.md §3: the undeterminable case
/// resolves to the restrictive side — no skip — never to a shared placeholder
/// bucket that every unattributed session would collide in.
///
/// The apparatus block is load-bearing for the same reason as above: against a
/// binary with no `skip` subcommand, "refused" and "not implemented" produce
/// identical observations.
#[test]
fn a_skip_issued_without_a_session_id_is_refused() {
    let (home, root) = project("no-session-apparatus", "exit 1");
    let ok = issue_skip(&home, &root, Some("sess-real"), Some("a real reason"));
    assert_eq!(
        ok.code, 0,
        "apparatus: an attributed skip must succeed, else the refusal below measures nothing; \
         stdout={:?} stderr={:?}",
        ok.stdout, ok.stderr
    );

    let (home, root) = project("no-session", "exit 1");
    let issued = issue_skip(&home, &root, None, Some("who am I"));
    assert_ne!(
        issued.code, 0,
        "an unattributable skip must be refused; stdout={:?} stderr={:?}",
        issued.stdout, issued.stderr
    );

    let r = stop(&home, &root, "sess-anyone");
    assert!(
        r.blocked(),
        "a refused unattributed skip still waved a stop through. stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );
}

// ===========================================================================
// The `_local` fallback bucket
// ===========================================================================

/// Plant a `_local` skip in donegate's state dir, as something other than the
/// `skip` CLI could leave it (the CLI refuses `_local`; a hand-written file, a
/// migration or a future caller would not).
fn plant_local_skip(home: &Path, reason: &str) -> PathBuf {
    let p = home.join(".donegate/state/skips/_local.skip");
    std::fs::create_dir_all(p.parent().expect("skips dir")).expect("create skips dir");
    std::fs::write(&p, format!("{reason}\n")).expect("plant _local skip");
    p
}

/// A Stop payload with NO `session_id` collapses onto the shared `_local` key,
/// and that key must not carry an exception.
///
/// This is the fail-open route the session-scoped design could still have had.
/// `HookInput::session_key()` returns `"_local"` whenever the payload carries
/// no session id, so EVERY concurrent session whose payload degrades that way
/// lands on one key — the shared project-root marker rebuilt inside the state
/// dir, where the mirror-gap scan would never see it. A payload missing the
/// field still PARSES, so the gate is in real hook mode: this is the realistic
/// shape of the failure, not a contrived one.
#[test]
fn a_payload_with_no_session_id_does_not_consume_a_shared_local_skip() {
    let (home, root) = project("local-bucket", "exit 1");
    let planted = plant_local_skip(&home, "planted in the shared bucket");

    let payload = serde_json::json!({
        "cwd": root.to_string_lossy(),
        "hook_event_name": "Stop",
    })
    .to_string();
    let r = run(&home, &root, &["gate"], None, &payload);

    assert_eq!(
        r.code, 0,
        "a parseable Stop payload keeps the gate in hook mode"
    );
    assert!(
        r.blocked(),
        "a skip in the shared `_local` bucket waved this stop through. Every session whose \
         payload lacks an id collapses onto that key, so this is the cross-session leak restored \
         inside the state dir. stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );
    assert!(
        !r.stderr.contains("skip consumed"),
        "the gate reported consuming a skip it must not be able to see; stderr={:?}",
        r.stderr
    );
    assert!(
        planted.exists(),
        "the `_local` marker was deleted by a session that could not use it — a gate that eats an \
         unusable marker silently destroys whatever left it there"
    );
}

/// The same, for stdin that does not parse at all. Here the gate falls into
/// manual/interactive mode, where a block is a non-zero exit and a human report
/// on stderr rather than a decision JSON — so the assertion is written against
/// "did NOT wave the stop through" in a mode-agnostic way.
#[test]
fn wholly_malformed_stdin_does_not_consume_a_shared_local_skip() {
    let (home, root) = project("local-bucket-malformed", "exit 1");
    let planted = plant_local_skip(&home, "planted in the shared bucket");

    let r = run(&home, &root, &["gate"], None, "not json at all");

    let waved_through = r.code == 0 && !r.blocked() && r.stderr.contains("skip consumed");
    assert!(
        !waved_through,
        "unparseable stdin collapsed onto `_local` and consumed a shared skip; code={} \
         stdout={:?} stderr={:?}",
        r.code, r.stdout, r.stderr
    );
    assert!(
        r.stderr.contains("typecheck") || r.blocked(),
        "apparatus: the gate must actually have evaluated the failing check here, else 'not waved \
         through' is vacuous; code={} stdout={:?} stderr={:?}",
        r.code,
        r.stdout,
        r.stderr
    );
    assert!(
        planted.exists(),
        "the `_local` marker was consumed or deleted by a run that must not be able to see it"
    );
}

/// ANTI-VACUITY CONTROL for both `_local` tests: with the SAME fixture and an
/// attributed session, the hatch does work. Without this, "the stop was
/// blocked" above would also hold against a gate whose skip path is dead.
#[test]
fn the_same_fixture_still_honours_an_attributed_skip() {
    let (home, root) = project("local-bucket-control", "exit 1");
    plant_local_skip(&home, "planted in the shared bucket");

    issue_skip(
        &home,
        &root,
        Some("sess-real"),
        Some("a real attributed skip"),
    );
    let r = stop(&home, &root, "sess-real");
    assert!(
        !r.blocked(),
        "apparatus: an attributed skip must still be honoured in this very fixture; stdout={:?} \
         stderr={:?}",
        r.stdout,
        r.stderr
    );
}

// ===========================================================================
// The bypass leaves a record
// ===========================================================================

/// A bypass nobody can see afterwards is indistinguishable from a gate that
/// passed. The record must name the session and carry the reason, and must not
/// be written when no skip was consumed (the control below).
#[test]
fn a_consumed_skip_is_recorded_with_its_session_and_reason() {
    let (home, root) = project("recorded", "exit 1");
    issue_skip(
        &home,
        &root,
        Some("sess-rec"),
        Some("waiting on backlog 1234"),
    );

    let r = stop(&home, &root, "sess-rec");
    assert!(
        !r.blocked(),
        "apparatus: the skip must have been consumed, else the log assertions are vacuous; \
         stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );

    let log = gate_log(&home);
    assert!(
        log.contains("sess-rec"),
        "the bypass record must name the session that was let through; log={log:?}"
    );
    assert!(
        log.contains("waiting on backlog 1234"),
        "the bypass record must carry the reason — an untracked bypass is invisible to review, \
         i.e. indistinguishable from the gate having passed; log={log:?}"
    );
}

/// CONTROL 3 for the test above: the reason text is not written on every run,
/// so the assertion above is measuring the consumption and not the mere
/// existence of a log file.
#[test]
fn a_stop_with_no_skip_records_no_skip_reason() {
    let (home, root) = project("recorded-control", "exit 1");
    let r = stop(&home, &root, "sess-rec");
    assert!(r.blocked(), "apparatus: this stop must be a plain block");
    let log = gate_log(&home);
    assert!(
        !log.contains("waiting on backlog 1234"),
        "a stop with no skip must not carry a skip reason; log={log:?}"
    );
}
