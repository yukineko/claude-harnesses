// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Trust-on-first-use approval memory: the anti-vacuity controls.
//!
//! # What this file is for
//!
//! The memory exists because `Ask` was firing on every repetition of a command
//! a human had already approved — the usability harm that retired `taintguard`
//! (backlog `9e0e3881`). A memory that removes asking is only worth having if
//! it can be shown to remove asking AND to NOT remove too much of it, so the
//! two failure directions are tested against each other here:
//!
//! * (i) the second run of the same command, same parameters, unchanged target,
//!   does not ask — the memory works at all;
//! * (ii) same command with CHANGED PARAMETERS asks again — parameters change
//!   where the effect lands, so an approval cannot span them;
//! * (iii) same command and parameters with a CHANGED TARGET asks again —
//!   「過去に実行されても変更があったときは再度判断すべき」;
//! * (iv) an effect reaching OUTSIDE the project is never approvable at all,
//!   so no amount of prior running can silence it;
//! * (v) the positive controls that (i) is actually reading the store: the same
//!   sequence against a DIFFERENT store asks, and the same sequence with the
//!   PostToolUse step omitted asks. Without these, (i) could pass for the
//!   vacuous reason that nothing ever asked in the first place — which is why
//!   [`ask_without_the_memory_is_the_baseline`] runs first and asserts the ask.
//!
//! # Level
//!
//! End-to-end through the real built binary, because the approval crosses TWO
//! invocations (PreToolUse stashes a pending fingerprint, PostToolUse promotes
//! it) and a library-level test could not observe that boundary at all.
//!
//! # Why not a `TempDir`
//!
//! `/tmp` is a blastguard SAFE ROOT ([`blastguard::scope`]'s `TEMP_ROOTS`), so a
//! project built under it has every sibling directory "inside a safe root" too
//! — and control (iv) needs a directory that is genuinely OUTSIDE. Cargo's
//! `CARGO_TARGET_TMPDIR` sits under `target/`, which is not a safe root, so the
//! outside/inside distinction survives there.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A hermetic project tree, plus the store the binary will be pointed at.
struct Fixture {
    /// `<base>/proj` — the session cwd AND `CLAUDE_PROJECT_DIR`.
    proj: PathBuf,
    /// `<base>/outside` — a sibling under no safe root.
    outside: PathBuf,
    /// `<base>/store` — `BLASTGUARD_APPROVALS_DIR`.
    store: PathBuf,
    /// `<base>/home` — `$HOME`, kept distinct from `proj` because
    /// `SafeRoots::new` refuses to treat the home directory as a root.
    home: PathBuf,
    base: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("approval_memory")
            .join(name);
        // Deterministic: a leftover store from a previous run would make (i)
        // pass without the current binary ever writing anything.
        let _ = std::fs::remove_dir_all(&base);
        let f = Fixture {
            proj: base.join("proj"),
            outside: base.join("outside"),
            store: base.join("store"),
            home: base.join("home"),
            base,
        };
        std::fs::create_dir_all(f.proj.join(".claude")).unwrap();
        std::fs::create_dir_all(f.proj.join("sub")).unwrap();
        std::fs::create_dir_all(f.outside.join(".claude")).unwrap();
        std::fs::create_dir_all(&f.home).unwrap();
        std::fs::write(f.proj.join(".claude/settings.json"), "{\"v\":1}\n").unwrap();
        std::fs::write(f.outside.join(".claude/settings.json"), "{\"v\":1}\n").unwrap();
        f
    }

    /// The approval entries on disk.
    ///
    /// An absent directory is an EMPTY store; anything else that stops the read
    /// panics. Returning `Vec::new()` on any error would make control (iv)'s
    /// `assert!(approved_entries().is_empty())` pass for the wrong reason — the
    /// empty-collection fallback this repository's `fail-open-guard` names, in a
    /// test whose whole job is to not be vacuous.
    fn approved_entries(&self) -> Vec<String> {
        let dir = self.store.join("approved");
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => panic!("cannot list {}: {e}", dir.display()),
        };
        let mut out: Vec<String> = rd
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    /// PreToolUse. Returns the hook's stdout — empty means Allow.
    fn pre(&self, command: &str) -> String {
        self.pre_with_store(command, &self.store)
    }

    fn pre_with_store(&self, command: &str, store: &Path) -> String {
        let payload = format!(
            r#"{{"session_id":"approval-memory","cwd":"{cwd}","tool_name":"Bash","tool_input":{{"command":"{cmd}"}}}}"#,
            cwd = self.proj.display(),
            cmd = command,
        );
        self.invoke(&[], &payload, store)
    }

    /// PostToolUse — the evidence that a human answered the ask with "yes".
    fn post(&self, command: &str) -> String {
        let payload = format!(
            r#"{{"session_id":"approval-memory","cwd":"{cwd}","tool_name":"Bash","tool_input":{{"command":"{cmd}"}},"tool_response":{{"stdout":"","stderr":"","interrupted":false}}}}"#,
            cwd = self.proj.display(),
            cmd = command,
        );
        self.invoke(&["record-approval"], &payload, &self.store)
    }

    fn invoke(&self, args: &[&str], payload: &str, store: &Path) -> String {
        let bin = env!("CARGO_BIN_EXE_blastguard");
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.proj)
            .env("BLASTGUARD_APPROVALS_DIR", store)
            .env("HOME", &self.home)
            // `interactive::ask_available` is a POSITIVE gate: without an
            // affirmatively-interactive entrypoint every `Ask` hardens to a
            // `Deny` and the memo's own effect would be invisible.
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .current_dir(&self.base);
        let mut child = cmd.spawn().expect("binary spawns");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        let out = child.wait_with_output().expect("binary runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "the hook must always exit 0; stderr was: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn asks(out: &str) -> bool {
    out.contains(r#""permissionDecision":"ask""#)
}

fn allows(out: &str) -> bool {
    out.trim().is_empty()
}

/// Two commands that blastguard answers `Ask` for, measured against the real
/// binary at `0.2.52` before this feature existed:
///
/// * `mytool .claude/settings.json` — "`mytool` is a command blastguard has no
///   rule for, and .claude/settings.json is a protected gate/config path";
/// * `chmod -R 755 sub` — "recursive chmod (-R) is confined to <root> … The
///   blast radius is bounded, so this is a question rather than a refusal".
///
/// Both are the shape the user complained about: an in-project effect that gets
/// asked about on every single repetition.
const UNKNOWN_VERB: &str = "mytool .claude/settings.json";
const RECURSIVE_CHMOD: &str = "chmod -R 755 sub";

/// Control 0 — the baseline the other controls are measured against.
///
/// If this ever stops asking, every "does not ask" assertion below becomes
/// vacuous, so it is asserted explicitly rather than assumed.
#[test]
fn ask_without_the_memory_is_the_baseline() {
    let f = Fixture::new("baseline");
    assert!(
        asks(&f.pre(UNKNOWN_VERB)),
        "unknown verb on a protected path"
    );
    assert!(asks(&f.pre(RECURSIVE_CHMOD)), "recursive chmod in-project");
    assert!(
        f.approved_entries().is_empty(),
        "a PreToolUse consult must never create an approval by itself"
    );
}

/// (i) Second run, same command, same parameters, unchanged target → no ask.
#[test]
fn approved_command_is_not_asked_again() {
    let f = Fixture::new("i-second-run");
    assert!(asks(&f.pre(RECURSIVE_CHMOD)), "first run must ask");
    f.post(RECURSIVE_CHMOD);
    assert_eq!(
        f.approved_entries().len(),
        1,
        "the PostToolUse must promote exactly one pending fingerprint"
    );
    assert!(
        allows(&f.pre(RECURSIVE_CHMOD)),
        "second run must be silent (allow)"
    );
}

/// (ii) Same command, CHANGED PARAMETERS → the ask returns.
///
/// `755` → `777` keeps the verb and the target and changes only what the
/// command DOES to it, which is precisely the case the user named:
/// 「もちろんパラメータが違えば害にもなる」.
#[test]
fn changed_parameters_ask_again() {
    let f = Fixture::new("ii-parameters");
    assert!(asks(&f.pre(RECURSIVE_CHMOD)), "first run must ask");
    f.post(RECURSIVE_CHMOD);
    assert!(allows(&f.pre(RECURSIVE_CHMOD)), "the approval must apply");
    assert!(
        asks(&f.pre("chmod -R 777 sub")),
        "a different mode is a different effect and must ask"
    );
    assert!(
        asks(&f.pre("chmod -R 755 sub other")),
        "an extra operand is a different effect and must ask"
    );
}

/// (iii) Same command and parameters, CHANGED TARGET CONTENT → the ask returns.
///
/// 「あと過去に実行されても変更があったときは再度判断すべきである」.
#[test]
fn changed_target_content_asks_again() {
    let f = Fixture::new("iii-content");
    assert!(asks(&f.pre(UNKNOWN_VERB)), "first run must ask");
    f.post(UNKNOWN_VERB);
    assert!(allows(&f.pre(UNKNOWN_VERB)), "the approval must apply");
    std::fs::write(f.proj.join(".claude/settings.json"), "{\"v\":2}\n").unwrap();
    assert!(
        asks(&f.pre(UNKNOWN_VERB)),
        "the approved target changed under the approval; it must be re-judged"
    );
}

/// (iv) An effect reaching OUTSIDE the project is never approvable.
///
/// 「parameter があっても影響が project 内部ならよいが外部におよぶのであれば
/// Ask するか禁止を報告する」. The store must not even acquire an entry: an
/// approval that exists but "does not apply" is one refactor away from
/// applying.
#[test]
fn outside_the_project_is_never_approvable() {
    let f = Fixture::new("iv-outside");
    let escaping = "mytool ../outside/.claude/settings.json";
    assert!(asks(&f.pre(escaping)), "first run must ask");
    f.post(escaping);
    assert!(
        f.approved_entries().is_empty(),
        "an outside-reaching effect must never be recorded as approved"
    );
    assert!(
        asks(&f.pre(escaping)),
        "an outside-reaching effect must keep asking however often it runs"
    );
}

/// (v-a) Positive control: (i) is reading the store, not merely not-asking.
#[test]
fn a_different_store_does_not_carry_the_approval() {
    let f = Fixture::new("v-a-other-store");
    assert!(asks(&f.pre(RECURSIVE_CHMOD)), "first run must ask");
    f.post(RECURSIVE_CHMOD);
    assert!(allows(&f.pre(RECURSIVE_CHMOD)), "the approval must apply");
    let elsewhere = f.base.join("store-elsewhere");
    assert!(
        asks(&f.pre_with_store(RECURSIVE_CHMOD, &elsewhere)),
        "an empty store is a first use and must ask"
    );
}

/// (v-b) Positive control: the PostToolUse is what makes an approval.
///
/// A PreToolUse consult alone stashes a PENDING fingerprint. If a pending entry
/// were treated as an approval, blastguard would approve every command it had
/// merely asked about — including the ones the human said no to.
#[test]
fn an_unpromoted_pending_is_not_an_approval() {
    let f = Fixture::new("v-b-pending");
    assert!(asks(&f.pre(RECURSIVE_CHMOD)), "first run must ask");
    assert!(
        asks(&f.pre(RECURSIVE_CHMOD)),
        "without a PostToolUse there is no evidence a human said yes"
    );
    assert!(
        f.approved_entries().is_empty(),
        "nothing may be approved yet"
    );
}

/// A `Deny` is never downgraded, however often it has run.
///
/// The memory's whole contract is that it moves `Ask` → `Allow` and touches
/// nothing else. A `Deny` produces no PostToolUse in production (the tool never
/// runs), but a hand-planted pending must not be able to reach it either.
#[test]
fn deny_is_never_downgraded_by_the_memory() {
    let f = Fixture::new("deny-untouched");
    let out = f.pre("rm -rf /");
    assert!(
        out.contains(r#""permissionDecision":"deny""#),
        "expected a deny, got: {out}"
    );
    f.post("rm -rf /");
    let again = f.pre("rm -rf /");
    assert!(
        again.contains(r#""permissionDecision":"deny""#),
        "a deny must survive any number of recorded runs, got: {again}"
    );
}

/// A command whose text is not statically readable is never approvable.
///
/// An expansion's value only exists at run time, so two runs of the same TEXT
/// are not two runs of the same effect. `scope` already refuses to expand
/// `$VAR`; the memory must refuse to remember it.
#[test]
fn expansions_are_never_approvable() {
    let f = Fixture::new("expansion");
    let cmd = "chmod -R 755 $TARGET";
    let first = f.pre(cmd);
    assert!(
        asks(&first) || first.contains(r#""deny""#),
        "must not allow"
    );
    f.post(cmd);
    assert!(
        f.approved_entries().is_empty(),
        "a command containing an expansion must never be recorded"
    );
    let second = f.pre(cmd);
    assert!(
        asks(&second) || second.contains(r#""deny""#),
        "must still not allow, got: {second}"
    );
}
