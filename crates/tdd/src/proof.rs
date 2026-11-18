//! Test-first proof: make "the test was written before the implementation"
//! a *verifiable* artifact instead of a claim.
//!
//!   `tdd red`   runs the tests and REQUIRES them to fail (≥1 red). It records
//!               `<proof_dir>/<task>.red.json`. If they already pass, that's not
//!               test-first — it errors.
//!   `tdd green` REQUIRES a prior RED proof, runs the tests, and REQUIRES them to
//!               pass. It records `<proof_dir>/<task>.green.json`.
//!   `tdd verify` succeeds iff both proofs exist (RED then GREEN happened).
//!
//! The file names above are the FIRST test command's slot; every further
//! command of the task gets its own slot (see the slots section below), and
//! `tdd green --cmd X` needs the RED slot of the same command `X`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use harness_core::verdict::{Determination, Required};
use serde_json::json;

use crate::config::Config;
use crate::runner;

fn safe(task: &str) -> String {
    task.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

pub fn proof_dir(root: &Path, cfg: &Config) -> PathBuf {
    root.join(&cfg.proof_dir)
}

pub fn artifact_path(root: &Path, cfg: &Config, task: &str, kind: &str) -> PathBuf {
    proof_dir(root, cfg).join(format!("{}.{kind}.json", safe(task)))
}

// ── slots (backlog 519f089e) ────────────────────────────────────────────────
//
// One proof file per (task, kind, test command). The FIRST command recorded
// for a task and kind lives at the legacy name `<task>.<kind>.json` (so a
// reader that knows only one slot still finds it, and a single-command task
// is byte-for-byte the old layout). Every further command lives at
// `<task>.<16 hex of fnv1a64(cmd)>.<kind>.json`. A slot is identified by the
// `cmd` recorded INSIDE the file, never by its name.

/// What the `cmd` of a proof file could be determined to be.
enum SlotCmd {
    Absent,
    Cmd(String),
    /// Valid JSON object that records no `cmd` at all: a proof written before
    /// slots existed. At the legacy-named path it is the task's one slot
    /// whatever the command (the pre-slot behaviour); anywhere else it is
    /// `Unknown`.
    CmdLess,
    /// Exists but its `cmd` cannot be read: not "absent", not "another command".
    Unknown(String),
}

fn peek_cmd(path: &Path) -> SlotCmd {
    match harness_core::boundary::read_to_string(path) {
        Determination::Known(None) => SlotCmd::Absent,
        Determination::Known(Some(text)) => {
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(v) => match v.get("cmd") {
                    Some(c) => match c.as_str() {
                        Some(c) => SlotCmd::Cmd(c.to_string()),
                        None => SlotCmd::Unknown(format!(
                            "{} records a `cmd` that is not a string",
                            path.display()
                        )),
                    },
                    None if v.is_object() => SlotCmd::CmdLess,
                    None => SlotCmd::Unknown(format!("{} is not a JSON object", path.display())),
                },
                Err(_) => SlotCmd::Unknown(format!("{} is not valid JSON", path.display())),
            }
        }
        Determination::Undetermined(why) => SlotCmd::Unknown(why.as_str().to_string()),
    }
}

fn hashed_path(root: &Path, cfg: &Config, task: &str, kind: &str, cmd: &str) -> PathBuf {
    let h = harness_core::hash::fnv1a64(cmd.as_bytes());
    proof_dir(root, cfg).join(format!("{}.{h:016x}.{kind}.json", safe(task)))
}

/// The slot file already holding `cmd`'s proof, if any. A candidate whose
/// command cannot be determined is an error: guessing "other command" could
/// overwrite evidence, guessing "same command" could pair the wrong proofs.
fn find_slot(
    root: &Path,
    cfg: &Config,
    task: &str,
    kind: &str,
    cmd: &str,
) -> Result<Option<PathBuf>> {
    let legacy = artifact_path(root, cfg, task, kind);
    for p in [legacy.clone(), hashed_path(root, cfg, task, kind, cmd)] {
        match peek_cmd(&p) {
            SlotCmd::Absent => {}
            SlotCmd::Cmd(c) if c == cmd => return Ok(Some(p)),
            SlotCmd::Cmd(_) => {}
            SlotCmd::CmdLess if p == legacy => return Ok(Some(p)),
            SlotCmd::CmdLess => bail!(
                "{} records no `cmd` but is not the legacy-named slot; refusing to guess — fix \
                 or remove that file and run again.",
                p.display()
            ),
            SlotCmd::Unknown(why) => bail!(
                "cannot tell which test command {} records ({why}); refusing to guess — fix or \
                 remove that file and run again.",
                p.display()
            ),
        }
    }
    Ok(None)
}

/// The file `cmd`'s proof is written to: its existing slot, else the legacy
/// name when free, else the hashed name when free.
fn write_slot(root: &Path, cfg: &Config, task: &str, kind: &str, cmd: &str) -> Result<PathBuf> {
    if let Some(p) = find_slot(root, cfg, task, kind, cmd)? {
        return Ok(p);
    }
    for p in [
        artifact_path(root, cfg, task, kind),
        hashed_path(root, cfg, task, kind, cmd),
    ] {
        if matches!(peek_cmd(&p), SlotCmd::Absent) {
            return Ok(p);
        }
    }
    bail!("no free {kind} proof slot for this command (hash collision with another command)")
}

/// Every proof file of `kind` for `task`: the legacy-named one plus the
/// hashed-name slots, sorted. A missing proof dir is "none"; any other read
/// failure is an `Err` ("could not look" is not "nothing there").
pub fn slot_files(
    root: &Path,
    cfg: &Config,
    task: &str,
    kind: &str,
) -> Result<Vec<PathBuf>, String> {
    let dir = proof_dir(root, cfg);
    let mut out = Vec::new();
    let primary = artifact_path(root, cfg, task, kind);
    match std::fs::symlink_metadata(&primary) {
        Ok(_) => out.push(primary),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot stat {}: {e}", primary.display())),
    }
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(format!("cannot read {}: {e}", dir.display())),
    };
    let prefix = format!("{}.", safe(task));
    let suffix = format!(".{kind}.json");
    let mut hashed = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| format!("cannot read an entry of {}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(mid) = name
            .strip_prefix(&prefix)
            .and_then(|n| n.strip_suffix(&suffix))
        else {
            continue;
        };
        if mid.len() == 16
            && mid
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            hashed.push(entry.path());
        }
    }
    hashed.sort();
    out.extend(hashed);
    Ok(out)
}

/// Read every slot of `kind` for the oracle: cmd and passed of each file.
pub fn read_slot_proofs(
    root: &Path,
    cfg: &Config,
    task: &str,
    kind: &str,
) -> Determination<Vec<crate::transition::SlotProof>> {
    let files = match slot_files(root, cfg, task, kind) {
        Ok(f) => f,
        Err(e) => return Determination::undetermined(e),
    };
    let mut out = Vec::new();
    for p in files {
        out.push(match harness_core::boundary::read_to_string(&p) {
            Determination::Known(Some(text)) => {
                let v: Option<serde_json::Value> = serde_json::from_str(&text).ok();
                crate::transition::SlotProof {
                    cmd: Determination::known(
                        v.as_ref()
                            .and_then(|v| v.get("cmd")?.as_str().map(str::to_string)),
                    ),
                    passed: Determination::known(
                        v.as_ref().and_then(|v| v.get("passed")?.as_bool()),
                    ),
                }
            }
            // Listed a moment ago and gone now: not "no proof".
            Determination::Known(None) => crate::transition::SlotProof {
                cmd: Determination::undetermined(format!("{} vanished while reading", p.display())),
                passed: Determination::undetermined(format!(
                    "{} vanished while reading",
                    p.display()
                )),
            },
            Determination::Undetermined(why) => crate::transition::SlotProof {
                cmd: Determination::undetermined(why.as_str()),
                passed: Determination::undetermined(why.as_str()),
            },
        });
    }
    Determination::known(out)
}

/// The fields every proof carries about the run itself: identity of the tree
/// it saw, per-test results, counts, and the output tail.
fn run_evidence(root: &Path, cfg: &Config, out: &runner::Outcome) -> serde_json::Value {
    json!({
        "tree_identity": runner::evidence::tree_identity(root, &cfg.proof_dir),
        "tests": out.tests,
        "counts": out.counts,
        "output_tail": out.output_tail,
    })
}

/// The names the paired RED proof recorded as `"fail"`, sorted, copied into
/// the GREEN proof so a consumer needs only `green.json` to ask "did GREEN
/// pass everything RED failed". `null` (NOT an empty list) when the RED proof
/// has no readable `tests` map (an older tdd, or an unreadable file): "RED's
/// failing set is unknown" must not read as "RED failed nothing".
fn red_failed_tests(red_path: &Path) -> serde_json::Value {
    let tests = match harness_core::boundary::read_to_string(red_path) {
        Determination::Known(Some(text)) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("tests").and_then(|t| t.as_object().cloned())),
        _ => None,
    };
    match tests {
        Some(map) => serde_json::Value::Array(
            map.iter()
                .filter(|(_, v)| v.as_str() == Some("fail"))
                .map(|(k, _)| serde_json::Value::String(k.clone()))
                .collect(),
        ),
        None => serde_json::Value::Null,
    }
}

fn merge_into(base: &mut serde_json::Value, extra: serde_json::Value) {
    if let (Some(b), serde_json::Value::Object(e)) = (base.as_object_mut(), extra) {
        b.extend(e);
    }
}

/// Decide whether a RED run is acceptable: the tests MUST have failed.
fn judge_red(passed: bool) -> Result<()> {
    if passed {
        bail!(
            "tests passed on `tdd red` — that is not test-first. Write a test that FAILS \
             against the not-yet-written behaviour first, then run `tdd red` again."
        );
    }
    Ok(())
}

/// Which proof a run was meant to produce; only selects the wording of
/// [`judge_ran`]'s refusal so each phase tells the user which command to re-run.
#[derive(Clone, Copy)]
enum Phase {
    Red,
    Green,
}

/// Refuse to treat a run as evidence when the test command never ran
/// (`spawn_error` set): the runner reports that as `passed: false`, which
/// `judge_red` would otherwise read as "tests fail as expected" and
/// `judge_green` as "tests still failing". Both `red()` and `green()` call
/// this right after `run_cmd`, so a run that never happened is reported as
/// such (naming the spawn error) and records no artifact in either phase.
fn judge_ran(out: &runner::Outcome, phase: Phase) -> Result<()> {
    if let Some(e) = &out.spawn_error {
        let (which, again) = match phase {
            Phase::Red => ("RED", "tdd red"),
            Phase::Green => ("GREEN", "tdd green"),
        };
        bail!(
            "test command never ran ({e}) — no {which} recorded. Fix the environment/command \
             and run `{again}` again."
        );
    }
    Ok(())
}

/// Refuse to treat a run as evidence when it produced no exit code: the
/// command timed out (`timed_out`), or it ended without an exit code —
/// `exit_code` is `None` with no `spawn_error` and `timed_out` false, which
/// the harness-core runner produces both when the child was killed by a
/// signal and when waiting on the child failed (its `wait_timeout` `Err`
/// arm). The runner reports all of these as `passed: false`, which
/// `judge_red` would read as "tests fail as expected" — but no assertion
/// failure was observed, so the outcome is undetermined, not RED. Only a
/// command that ran and exited with a code (even non-zero) is evidence.
/// `red()` and `green()` call this right after [`judge_ran`]; neither records
/// an artifact for such a run. (GREEN was already refused via `passed: false`;
/// this only makes the message name the real cause.)
fn judge_exited(out: &runner::Outcome, phase: Phase, timeout_secs: u64) -> Result<()> {
    let (which, again) = match phase {
        Phase::Red => ("RED", "tdd red"),
        Phase::Green => ("GREEN", "tdd green"),
    };
    if out.timed_out {
        bail!(
            "test command timed out after {timeout_secs} s — no {which} recorded: no assertion \
             failure was observed, so the result is undetermined. Fix the hang or raise the \
             timeout and run `{again}` again."
        );
    }
    if out.exit_code.is_none() {
        bail!(
            "test command produced no exit code (killed by a signal, or waiting on it failed) — \
             no {which} recorded: no assertion failure was observed, so the result is \
             undetermined. Find out why the process ended without an exit code and run \
             `{again}` again."
        );
    }
    Ok(())
}

/// Decide whether a RED proof exists — the single canonical definition of the
/// "no RED proof found" judgment. Shared by [`judge_green`] (the exhaustive
/// GREEN-run judgment) and by `green()`'s own early check, which MUST run
/// this same judgment *before* the `strict_separation` identity check (see
/// the comment at that call site): calling through this one function keeps
/// the error text and behaviour identical at both call sites instead of two
/// independently maintained copies of the same bail.
fn judge_has_red(has_red: bool) -> Result<()> {
    if !has_red {
        bail!("no RED proof found — run `tdd red --task <id>` before implementing.");
    }
    Ok(())
}

/// Decide whether a GREEN run is acceptable: a RED proof must exist and the
/// tests MUST now pass.
fn judge_green(passed: bool, has_red: bool) -> Result<()> {
    judge_has_red(has_red)?;
    if !passed {
        bail!(
            "tests still failing — keep implementing until they pass, then run `tdd green` again."
        );
    }
    Ok(())
}

/// Canonicalize an author identity for comparison: trimmed + lowercased, so
/// `"Agent-A"` and `" agent-a "` are recognised as the same identity. Mirrors
/// `condukt::verify::canonical` used for the worker/verifier-model invariant.
fn canonical_author(id: &str) -> String {
    id.trim().to_lowercase()
}

/// Bind the recorded identity to something real instead of an honor-system
/// bare string: when `--author` is not explicitly given, default to the
/// current Claude Code session id (`CLAUDE_CODE_SESSION_ID`), falling back to
/// a stable, shared `"_local"` bucket when unset (matches
/// `harness_core::hook::HookInput::session_key`'s convention for manual/CLI
/// runs outside a hook). This makes the *common* case — the same agent/session
/// runs both `tdd red` and `tdd green` — actually caught under
/// `strict_separation`, rather than depending entirely on a caller-supplied
/// `--author` string.
///
/// Residual limitation (document plainly, this is HOTL not a hard security
/// boundary): a determined single agent can still defeat this by passing two
/// *different* `--author` overrides explicitly (or by forging
/// `CLAUDE_CODE_SESSION_ID`) — nothing here cryptographically authenticates
/// the identity. `strict_separation` raises the bar for the common/accidental
/// case; it is not a hard security boundary against a deliberately adversarial
/// single agent.
fn default_author() -> String {
    default_author_from(std::env::var("CLAUDE_CODE_SESSION_ID").ok())
}

/// Testable core of [`default_author`]: given what
/// `env::var("CLAUDE_CODE_SESSION_ID")` would have returned, resolve the
/// default identity — trimmed session id, or the shared `"_local"` bucket
/// when unset/blank. Split out so tests can exercise the fallback logic
/// deterministically without mutating real process-global env state.
fn default_author_from(session_id: Option<String>) -> String {
    session_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "_local".to_string())
}

/// Resolve the identity to record for a RED/GREEN step: an explicit
/// `--author` always wins; otherwise fall back to [`default_author`] (the
/// session id, or `"_local"`). Never `None` — there is always *some* recorded
/// identity now, even without `--author`.
fn resolve_author(author: &Option<String>) -> String {
    resolve_author_from(author, default_author)
}

/// Testable core of [`resolve_author`]: an explicit `--author` always wins;
/// otherwise calls `default` (normally [`default_author`], overridable in
/// tests) to resolve the fallback identity.
fn resolve_author_from(author: &Option<String>, default: impl FnOnce() -> String) -> String {
    author
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string)
        .unwrap_or_else(default)
}

/// Pure gate: strict test/impl author separation (opt-in, fail-closed under
/// strict mode). Mirrors condukt's `resolve_verifier_model`/`same_model`
/// invariant that the verifier model must never equal the worker model — here
/// the RED (test-authoring) identity must never equal the GREEN
/// (implementation) identity, so one agent can't write a wrong implementation
/// and a matching wrong test (reward hacking).
///
/// - `strict == false` (default): always allowed, regardless of identities —
///   fully backward compatible.
/// - `strict == true`: allowed iff both identities are present and differ
///   (case-insensitive, trimmed). Missing/empty identities are also rejected
///   under strict mode, since separation can't be verified without them. In
///   practice `green()` always resolves a non-empty identity via
///   [`resolve_author`] (session id default), so this branch now only fires
///   for a RED proof written *before* this identity-default existed (no
///   `author` field at all) or when called directly with `None` (e.g. tests).
pub fn judge_separation(
    strict: bool,
    test_author: Option<&str>,
    impl_author: Option<&str>,
) -> Result<()> {
    if !strict {
        return Ok(());
    }
    let test_author = test_author.map(str::trim).filter(|s| !s.is_empty());
    let impl_author = impl_author.map(str::trim).filter(|s| !s.is_empty());
    let (Some(test_author), Some(impl_author)) = (test_author, impl_author) else {
        bail!(
            "strict_separation is enabled but the test-author and/or impl-author identity is \
             missing — pass `--author <id>` to both `tdd red` and `tdd green` so separation can \
             be verified."
        );
    };
    if canonical_author(test_author) == canonical_author(impl_author) {
        bail!(
            "strict_separation: the RED (test-author=\"{test_author}\") and GREEN \
             (impl-author=\"{impl_author}\") identities are the same — the same agent must not \
             both write the failing test and its implementation (this defeats test-first and \
             enables reward hacking). Have a different agent/session run `tdd green`, or disable \
             strict_separation if this is intentional."
        );
    }
    Ok(())
}

fn write_artifact(path: &Path, value: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn resolve_cmd<'a>(cmd: &'a Option<String>, cfg: &'a Config) -> &'a str {
    match cmd {
        Some(c) if !c.trim().is_empty() => c,
        _ => &cfg.test_cmd,
    }
}

/// `tdd red`: run the tests, require failure, record the RED proof.
///
/// RED is recorded only for a command that ran and exited non-zero. A run
/// that timed out, or produced no exit code (killed by a signal, or the wait
/// on the child failed), is refused and records no proof: no assertion
/// failure was observed, so the outcome is undetermined (see
/// [`judge_exited`]). A run whose tests already pass is refused too.
///
/// `author` is an optional identity (agent/session id) for the test-authoring
/// step, recorded into the proof so a later `tdd green --author <id>` can be
/// checked against it under `strict_separation` (see [`judge_separation`]).
/// When omitted, the recorded identity defaults to the current session id
/// (see [`default_author`]) rather than being left empty — see
/// [`resolve_author`].
pub fn red(
    root: &Path,
    cfg: &Config,
    task: &str,
    cmd: &Option<String>,
    author: &Option<String>,
) -> Result<()> {
    let cmdline = resolve_cmd(cmd, cfg);
    let tmp = cfg.state_dir.join("tmp");
    let out = runner::run_cmd(
        cmdline,
        root,
        cfg.default_timeout_secs,
        cfg.output_tail_lines,
        &tmp,
    );
    judge_ran(&out, Phase::Red)?;
    judge_exited(&out, Phase::Red, cfg.default_timeout_secs)?;
    judge_red(out.passed)?;
    let path = write_slot(root, cfg, task, "red", cmdline)?;
    let author = resolve_author(author);
    let mut proof = json!({
        "task": task,
        "phase": "red",
        "cmd": cmdline,
        "passed": out.passed,
        "exit_code": out.exit_code,
        "ts": chrono::Local::now().to_rfc3339(),
        "author": author,
    });
    merge_into(&mut proof, run_evidence(root, cfg, &out));
    write_artifact(&path, &proof)?;
    println!(
        "🔴 RED recorded for `{task}` — tests fail as expected ({}).\n   {}",
        out.status_str(),
        path.display()
    );
    Ok(())
}

/// Read a proof artifact's `author` field through the harness boundary.
///
/// `Known(None)` if the artifact is missing, corrupt, or the field is
/// absent/not a string; `Known(Some(author))` when present. An IO failure other
/// than not-found is `Undetermined` — it is NOT folded into `Known(None)`:
/// [`green`] refuses on it with the read error named, so an unreadable RED
/// proof is reported as unreadable rather than as a missing `--author`.
#[cfg(test)]
fn read_author(root: &Path, cfg: &Config, task: &str, kind: &str) -> Determination<Option<String>> {
    read_author_at(&artifact_path(root, cfg, task, kind))
}

fn read_author_at(path: &Path) -> Determination<Option<String>> {
    harness_core::boundary::read_to_string(path).map(|text| {
        let value: serde_json::Value = serde_json::from_str(&text?).ok()?;
        value
            .get("author")?
            .as_str()
            .map(std::string::ToString::to_string)
    })
}

/// `tdd green`: require a RED proof, run the tests, require success, record GREEN.
///
/// GREEN is recorded only for a command that ran and exited zero. A run that
/// timed out, or produced no exit code (killed by a signal, or the wait on
/// the child failed), is refused and records no proof (see [`judge_exited`]).
///
/// `author` is an optional identity for the implementation step. When
/// omitted, it defaults to the current session id (see [`default_author`] /
/// [`resolve_author`]) rather than being left empty — so the *common* case
/// (the same agent/session runs both `red` and `green` without ever passing
/// `--author`) is actually observable under `strict_separation`, instead of
/// silently passing because no identity was recorded. Under
/// `cfg.strict_separation` the resolved identity is compared against the RED
/// proof's recorded author and rejected (fail-closed) if they are the same
/// identity — see [`judge_separation`]. When `strict_separation` is off (the
/// default) this check is skipped entirely, so behaviour is unchanged.
pub fn green(
    root: &Path,
    cfg: &Config,
    task: &str,
    cmd: &Option<String>,
    author: &Option<String>,
) -> Result<()> {
    // The RED that counts is the one recorded for THIS command (its slot): a
    // RED of command A is no RED for command B (backlog 519f089e / dc37a4da).
    let cmdline = resolve_cmd(cmd, cfg);
    let red_slot = match find_slot(root, cfg, task, "red", cmdline) {
        Ok(s) => s,
        // Under strict_separation an unreadable RED proof is, above all, a RED
        // author that cannot be read: refuse with that reason (still a refusal).
        Err(e) if cfg.strict_separation => bail!(
            "strict_separation is enabled but the RED proof's author could not be read, so \
             separation cannot be verified (refusing GREEN): {e}"
        ),
        Err(e) => return Err(e),
    };
    let has_red = red_slot.is_some();
    if !has_red {
        if let Ok(others) = slot_files(root, cfg, task, "red") {
            if !others.is_empty() {
                bail!(
                    "no RED proof found for this test command `{cmdline}` — RED proofs exist for \
                     other command(s) of `{task}`, and a RED of one command is no RED for \
                     another. Run `tdd red --task {task} --cmd <this command>` first."
                );
            }
        }
    }
    // Check RED-proof existence FIRST, before the strict_separation identity
    // check: otherwise a `tdd green --author X` run *without* a prior `tdd
    // red` gets misdiagnosed as "identity is missing" (there's no RED
    // artifact to read an author from) instead of the correct "no RED proof
    // found" — even though `--author` was passed correctly. This mirrors the
    // same early bail `judge_green` performs, just moved ahead of the
    // identity check so the more fundamental precondition is reported first.
    // `strict_separation`'s own job — rejecting a RED proof that *does*
    // exist but has a missing/matching author identity — is unchanged below.
    //
    // Delegates to `judge_has_red` (the same judgment `judge_green` applies
    // later, once `passed` is known) rather than a second hand-rolled bail,
    // so there is exactly one definition of "no RED proof found".
    judge_has_red(has_red)?;
    let author = resolve_author(author);
    if cfg.strict_separation {
        let red_file = red_slot.clone().unwrap_or_default();
        let test_author = match read_author_at(&red_file).require() {
            Required::Determined(author) => author,
            Required::Blocked(why) => {
                let verdict = why.into_verdict();
                bail!(
                    "strict_separation is enabled but the RED proof's author could not be \
                     read, so separation cannot be verified (refusing GREEN): {}",
                    verdict.reason().map_or("undetermined", |r| r.as_str())
                );
            }
        };
        judge_separation(true, test_author.as_deref(), Some(&author))?;
    }
    let tmp = cfg.state_dir.join("tmp");
    let out = runner::run_cmd(
        cmdline,
        root,
        cfg.default_timeout_secs,
        cfg.output_tail_lines,
        &tmp,
    );
    judge_ran(&out, Phase::Green)?;
    judge_exited(&out, Phase::Green, cfg.default_timeout_secs)?;
    judge_green(out.passed, has_red)?;
    let red_path = red_slot.unwrap_or_default();
    let path = write_slot(root, cfg, task, "green", cmdline)?;
    let mut proof = json!({
        "task": task,
        "phase": "green",
        "cmd": cmdline,
        "passed": out.passed,
        "exit_code": out.exit_code,
        "ts": chrono::Local::now().to_rfc3339(),
        "red_proof": red_path.display().to_string(),
        "author": author,
    });
    merge_into(&mut proof, run_evidence(root, cfg, &out));
    proof["red_failed_tests"] = red_failed_tests(&red_path);
    write_artifact(&path, &proof)?;
    println!(
        "🟢 GREEN recorded for `{task}` — tests pass after RED.\n   {}",
        path.display()
    );
    Ok(())
}

/// `tdd verify`: true iff the task has at least one RED proof and at least one
/// GREEN proof (any slot). A proof dir that cannot be listed is "no" (§3).
pub fn verify(root: &Path, cfg: &Config, task: &str) -> bool {
    let any = |kind: &str| slot_files(root, cfg, task, kind).is_ok_and(|f| !f.is_empty());
    any("red") && any("green")
}

/// Read a proof artifact's `passed` field through the harness boundary.
///
/// - `Known(None)`: the artifact is missing, not valid JSON, or lacks a
///   boolean `passed` field — "no proof".
/// - `Known(Some(passed))`: the recorded result.
/// - `Undetermined`: the artifact exists but could not be read (an IO error
///   other than not-found). This is NOT folded into `Known(None)`:
///   `transition::oracle_report` reports it as `transition="undetermined"`,
///   `valid_fp_oracle=false`, and `tdd oracle` exits non-zero
///   ([`crate::transition::ORACLE_EXIT_UNDETERMINED`]).
///
/// Reads only the legacy-named slot `<task>.<kind>.json`. `tdd oracle` no
/// longer calls this (it reads every slot through [`read_slot_proofs`]); it is
/// kept because `tests/fault_injection.rs` pins its fail-closed behaviour.
#[allow(dead_code)]
pub fn read_passed(
    root: &Path,
    cfg: &Config,
    task: &str,
    kind: &str,
) -> Determination<Option<bool>> {
    let path = artifact_path(root, cfg, task, kind);
    harness_core::boundary::read_to_string(&path).map(|text| {
        let value: serde_json::Value = serde_json::from_str(&text?).ok()?;
        value.get("passed")?.as_bool()
    })
}

impl runner::Outcome {
    fn status_str(&self) -> String {
        if self.timed_out {
            "timed out".to_string()
        } else if let Some(e) = &self.spawn_error {
            e.clone()
        } else {
            match self.exit_code {
                Some(c) => format!("exit {c}"),
                None => "killed".to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn red_requires_failure() {
        assert!(judge_red(false).is_ok()); // failed → good
        assert!(judge_red(true).is_err()); // passed → not test-first
    }

    #[test]
    fn green_requires_red_then_pass() {
        assert!(judge_green(true, true).is_ok());
        assert!(judge_green(true, false).is_err()); // no RED proof
        assert!(judge_green(false, true).is_err()); // still failing
    }

    // ── strict test/impl author separation (Item C) ─────────────────────────
    //
    // Pure gate over (test_author, impl_author, strict_flag) — no LLM in the
    // decision path, mirroring condukt's verifier-model≠worker-model
    // invariant (`resolve_verifier_model`/`same_model`).

    #[test]
    fn separation_allows_when_not_strict_regardless_of_identity() {
        // Backward compatible: strict mode is opt-in. Even identical (or
        // missing) identities are allowed when `strict` is false.
        assert!(judge_separation(false, Some("agent-a"), Some("agent-a")).is_ok());
        assert!(judge_separation(false, None, None).is_ok());
    }

    #[test]
    fn separation_rejects_same_identity_under_strict_mode() {
        assert!(judge_separation(true, Some("agent-a"), Some("agent-a")).is_err());
        // Case-insensitive / whitespace-insensitive identity comparison.
        assert!(judge_separation(true, Some("Agent-A"), Some(" agent-a ")).is_err());
    }

    #[test]
    fn separation_allows_different_identity_under_strict_mode() {
        assert!(judge_separation(true, Some("agent-a"), Some("agent-b")).is_ok());
    }

    #[test]
    fn separation_rejects_missing_identity_under_strict_mode() {
        // Can't verify separation without both identities — fail-closed.
        assert!(judge_separation(true, None, Some("agent-b")).is_err());
        assert!(judge_separation(true, Some("agent-a"), None).is_err());
        assert!(judge_separation(true, None, None).is_err());
        assert!(judge_separation(true, Some(""), Some("agent-b")).is_err());
    }

    // ── identity default (bind to something real; Item 1) ───────────────────
    //
    // Pure, deterministic — no real env mutation, so these are safe under
    // test parallelism (see `default_author_from`/`resolve_author_from`).

    #[test]
    fn default_author_falls_back_to_local_bucket_when_session_id_unset() {
        assert_eq!(default_author_from(None), "_local");
        assert_eq!(default_author_from(Some("".to_string())), "_local");
        assert_eq!(default_author_from(Some("   ".to_string())), "_local");
    }

    #[test]
    fn default_author_uses_trimmed_session_id_when_set() {
        assert_eq!(
            default_author_from(Some("sess-123".to_string())),
            "sess-123"
        );
        assert_eq!(
            default_author_from(Some("  sess-123  ".to_string())),
            "sess-123"
        );
    }

    #[test]
    fn resolve_author_prefers_explicit_author_over_default() {
        // Explicit `--author` always wins, even if it differs from the
        // session-id default — the override still works.
        assert_eq!(
            resolve_author_from(&Some("agent-a".to_string()), || "sess-xyz".to_string()),
            "agent-a"
        );
    }

    #[test]
    fn resolve_author_falls_back_to_default_when_author_absent_or_blank() {
        assert_eq!(
            resolve_author_from(&None, || "sess-xyz".to_string()),
            "sess-xyz"
        );
        assert_eq!(
            resolve_author_from(&Some("   ".to_string()), || "sess-xyz".to_string()),
            "sess-xyz"
        );
    }

    #[test]
    fn same_session_auto_id_rejected_under_strict_separation() {
        // The identity-default gate's actual purpose: two `--author`-less
        // calls from the *same* session (the common case — one agent runs
        // both `tdd red` and `tdd green` without ever passing `--author`)
        // resolve to the SAME identity, and must be rejected under strict
        // separation — this is the "bind to something real" fix, not just an
        // honor-system string.
        let same_session = || "sess-same".to_string();
        let red_author = resolve_author_from(&None, same_session);
        let green_author = resolve_author_from(&None, same_session);
        assert_eq!(red_author, green_author);
        assert!(judge_separation(true, Some(&red_author), Some(&green_author)).is_err());
    }

    #[test]
    fn distinct_session_auto_ids_allowed_under_strict_separation() {
        let red_author = resolve_author_from(&None, || "sess-a".to_string());
        let green_author = resolve_author_from(&None, || "sess-b".to_string());
        assert_ne!(red_author, green_author);
        assert!(judge_separation(true, Some(&red_author), Some(&green_author)).is_ok());
    }

    #[test]
    fn default_off_unchanged_even_with_same_auto_session_id() {
        // strict_separation is opt-in: with it off, same-session auto ids are
        // still allowed (fully backward compatible with pre-existing
        // behaviour/tests).
        let same_session = || "sess-same".to_string();
        let red_author = resolve_author_from(&None, same_session);
        let green_author = resolve_author_from(&None, same_session);
        assert!(judge_separation(false, Some(&red_author), Some(&green_author)).is_ok());
    }

    #[test]
    fn green_rejects_same_author_end_to_end_under_strict_mode() {
        // Exercises the real `green()` wiring: a RED proof recorded with
        // author "agent-a", then `green()` called with the SAME author under
        // strict_separation must be rejected — even before the test command
        // itself would run (fail-closed, checked first).
        let base = std::env::temp_dir().join(format!("tdd-sep-same-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            strict_separation: true,
            ..Config::default()
        };
        write_artifact(
            &artifact_path(&base, &cfg, "t1", "red"),
            &json!({"passed": false, "author": "agent-a", "cmd": "true"}),
        )
        .unwrap();

        let err = green(
            &base,
            &cfg,
            "t1",
            &Some("true".to_string()),
            &Some("agent-a".to_string()),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("strict_separation"),
            "expected a strict_separation rejection, got: {err}"
        );
        // No GREEN proof should have been written since the gate rejected
        // before the test command ran.
        assert!(!artifact_path(&base, &cfg, "t1", "green").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn green_allows_different_author_end_to_end_under_strict_mode() {
        let base = std::env::temp_dir().join(format!("tdd-sep-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            strict_separation: true,
            ..Config::default()
        };
        write_artifact(
            &artifact_path(&base, &cfg, "t1", "red"),
            &json!({"passed": false, "author": "agent-a", "cmd": "true"}),
        )
        .unwrap();

        // "true" always exits 0, satisfying judge_green's pass requirement.
        green(
            &base,
            &cfg,
            "t1",
            &Some("true".to_string()),
            &Some("agent-b".to_string()),
        )
        .unwrap();
        assert!(artifact_path(&base, &cfg, "t1", "green").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn green_honors_explicit_author_override_end_to_end_default_off() {
        // Explicit `--author` override still works end-to-end even with
        // strict_separation OFF (default-off unchanged): both red() and
        // green() honor an explicit author and never touch CLAUDE_CODE_SESSION_ID.
        let base = std::env::temp_dir().join(format!("tdd-sep-override-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            strict_separation: false,
            ..Config::default()
        };

        // Fails (exit 1) until ok.flag exists, then passes: the SAME command
        // for RED and GREEN, as a slot requires.
        let flag_cmd = "test -f ok.flag".to_string();
        red(
            &base,
            &cfg,
            "t1",
            &Some(flag_cmd.clone()),
            &Some("agent-a".to_string()),
        )
        .unwrap();
        std::fs::write(base.join("ok.flag"), "x").unwrap();
        assert_eq!(
            read_author(&base, &cfg, "t1", "red")
                .require()
                .expect("proof artifact readable")
                .as_deref(),
            Some("agent-a")
        );

        green(
            &base,
            &cfg,
            "t1",
            &Some(flag_cmd),
            &Some("agent-a".to_string()),
        )
        .unwrap();
        assert_eq!(
            read_author(&base, &cfg, "t1", "green")
                .require()
                .expect("proof artifact readable")
                .as_deref(),
            Some("agent-a")
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn env_session_id_default_end_to_end_via_real_red_green() {
        // Serialized in a SINGLE #[test] (mirrors config.rs's HOME-mutation
        // test) because CLAUDE_CODE_SESSION_ID is process-global env state and
        // `cargo test` runs tests in parallel by default.
        //
        // Exercises the real `red()`/`green()` wiring end-to-end with NO
        // `--author` passed at all: the recorded identity must default to
        // CLAUDE_CODE_SESSION_ID. Same session id for both RED and GREEN
        // (the common "one agent, no --author" case) is rejected under
        // strict_separation; a different session id for GREEN is allowed.
        let base = std::env::temp_dir().join(format!("tdd-sep-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            strict_separation: true,
            ..Config::default()
        };

        let saved = std::env::var("CLAUDE_CODE_SESSION_ID").ok();

        // Same session for RED and GREEN → rejected (no --author given at all).
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "sess-same-e2e");
        let flag_cmd = Some("test -f ok.flag".to_string());
        red(&base, &cfg, "t1", &flag_cmd, &None).unwrap();
        std::fs::write(base.join("ok.flag"), "x").unwrap();
        assert_eq!(
            read_author(&base, &cfg, "t1", "red")
                .require()
                .expect("proof artifact readable")
                .as_deref(),
            Some("sess-same-e2e")
        );
        let err = green(&base, &cfg, "t1", &flag_cmd, &None).unwrap_err();
        assert!(
            err.to_string().contains("strict_separation"),
            "expected same-session auto id to be rejected under strict_separation, got: {err}"
        );
        assert!(!artifact_path(&base, &cfg, "t1", "green").exists());

        // Different session for GREEN → allowed.
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "sess-other-e2e");
        green(&base, &cfg, "t1", &flag_cmd, &None).unwrap();
        assert_eq!(
            read_author(&base, &cfg, "t1", "green")
                .require()
                .expect("proof artifact readable")
                .as_deref(),
            Some("sess-other-e2e")
        );

        // Explicit --author override still works, taking priority over env.
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "sess-ignored");
        red(
            &base,
            &cfg,
            "t2",
            &Some("false".to_string()),
            &Some("explicit-author".to_string()),
        )
        .unwrap();
        assert_eq!(
            read_author(&base, &cfg, "t2", "red")
                .require()
                .expect("proof artifact readable")
                .as_deref(),
            Some("explicit-author"),
            "explicit --author must override the CLAUDE_CODE_SESSION_ID default"
        );

        match saved {
            Some(v) => std::env::set_var("CLAUDE_CODE_SESSION_ID", v),
            None => std::env::remove_var("CLAUDE_CODE_SESSION_ID"),
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── finding 9: RED-proof existence must be checked before the
    // strict_separation identity check ──────────────────────────────────────
    //
    // Regression coverage for the ordering bug: `tdd green --author X` run
    // without a prior `tdd red` must report "no RED proof found", not the
    // misleading "identity is missing" (even though `--author` was passed).

    #[test]
    fn green_reports_missing_red_proof_not_identity_error_when_red_absent() {
        // (a) No RED proof at all + `--author` given + strict_separation on:
        // must fail with the RED-proof-not-found message, never the identity
        // error (the author WAS supplied correctly).
        let base = std::env::temp_dir().join(format!("tdd-finding9-nored-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            strict_separation: true,
            ..Config::default()
        };

        let err = green(
            &base,
            &cfg,
            "t1",
            &Some("true".to_string()),
            &Some("agent-a".to_string()),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no RED proof found"),
            "expected 'no RED proof found', got: {msg}"
        );
        assert!(
            !msg.contains("identity"),
            "must not misreport as an identity error when the real cause is a missing RED proof, got: {msg}"
        );
        assert!(!artifact_path(&base, &cfg, "t1", "green").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn green_still_reports_identity_error_when_red_exists_but_author_mismatched() {
        // (b) RED proof DOES exist but the author identity is missing/mismatched:
        // strict_separation's own job is unchanged — it must still fail with
        // the identity error (not the RED-proof-not-found message).
        let base =
            std::env::temp_dir().join(format!("tdd-finding9-mismatch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            strict_separation: true,
            ..Config::default()
        };
        write_artifact(
            &artifact_path(&base, &cfg, "t1", "red"),
            &json!({"passed": false, "author": "agent-a", "cmd": "true"}),
        )
        .unwrap();

        let err = green(
            &base,
            &cfg,
            "t1",
            &Some("true".to_string()),
            &Some("agent-a".to_string()),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("strict_separation"),
            "expected a strict_separation identity rejection, got: {msg}"
        );
        assert!(!artifact_path(&base, &cfg, "t1", "green").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn read_passed_is_fail_soft() {
        let base = std::env::temp_dir().join(format!("tdd-passed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            ..Config::default()
        };
        std::fs::create_dir_all(&base).unwrap();

        // missing artifact → None
        assert_eq!(
            read_passed(&base, &cfg, "t1", "red")
                .require()
                .expect("proof artifact readable"),
            None
        );

        // well-formed proofs → the recorded `passed` value
        write_artifact(
            &artifact_path(&base, &cfg, "t1", "red"),
            &json!({"passed": false}),
        )
        .unwrap();
        write_artifact(
            &artifact_path(&base, &cfg, "t1", "green"),
            &json!({"passed": true}),
        )
        .unwrap();
        assert_eq!(
            read_passed(&base, &cfg, "t1", "red")
                .require()
                .expect("proof artifact readable"),
            Some(false)
        );
        assert_eq!(
            read_passed(&base, &cfg, "t1", "green")
                .require()
                .expect("proof artifact readable"),
            Some(true)
        );

        // corrupt JSON → None (no panic)
        std::fs::write(artifact_path(&base, &cfg, "t2", "red"), "{not json").unwrap();
        assert_eq!(
            read_passed(&base, &cfg, "t2", "red")
                .require()
                .expect("proof artifact readable"),
            None
        );

        // valid JSON but no boolean `passed` → None
        write_artifact(&artifact_path(&base, &cfg, "t3", "red"), &json!({"x": 1})).unwrap();
        assert_eq!(
            read_passed(&base, &cfg, "t3", "red")
                .require()
                .expect("proof artifact readable"),
            None
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn verify_needs_both_artifacts() {
        let base = std::env::temp_dir().join(format!("tdd-proof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let cfg = Config {
            proof_dir: ".tdd".to_string(),
            ..Config::default()
        };
        std::fs::create_dir_all(&base).unwrap();
        assert!(!verify(&base, &cfg, "t1"));
        write_artifact(&artifact_path(&base, &cfg, "t1", "red"), &json!({"x":1})).unwrap();
        assert!(!verify(&base, &cfg, "t1"));
        write_artifact(&artifact_path(&base, &cfg, "t1", "green"), &json!({"x":1})).unwrap();
        assert!(verify(&base, &cfg, "t1"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
