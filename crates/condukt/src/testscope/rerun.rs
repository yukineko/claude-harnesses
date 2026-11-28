//! The rerun ledger of `condukt testscope`: `ledger.tsv` (the legacy 3-column
//! rows), `ledger.v2.tsv` (append-only records), the confirm / invalidate
//! records, and [`check_rerun`], which judges a same-tree rerun (rule 3 of the
//! parent module doc, and its "Confirmed runs" paragraph).
//!
//! Moved here verbatim from `testscope.rs` to keep that file under the
//! file-growth cap (backlog 0c301e8b split, 2026-11-17). Behaviour is unchanged.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::{
    parse_tests, tree_identity, untagged, Decision, Parsed, Words, TDD_GREEN_ROUTE, TDD_RED_ROUTE,
};

/// One rerun-ledger record: the tree a test ran in and the command.
///
/// Line format: `<top>\t<head:dirty>\t<cmd>`. Lines written by condukt
/// 0.7.413-0.7.418 may carry `@from=<top>:<head:dirty>\t` before the command
/// (the payload cwd's tree); that context is read and ignored, so such a
/// record collides with every later run of the same command on the same tree.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LedgerKey {
    top: String,
    tree: String,
    cmd: String,
}

impl LedgerKey {
    fn line(&self) -> String {
        format!("{}\t{}\t{}", self.top, self.tree, self.cmd)
    }

    fn parse(line: &str) -> Option<Self> {
        let mut it = line.splitn(3, '\t');
        let top = it.next()?.to_string();
        let tree = it.next()?.to_string();
        let rest = it.next()?;
        let cmd = match rest.strip_prefix("@from=") {
            Some(r) => r.split_once('\t')?.1.to_string(),
            None => rest.to_string(),
        };
        Some(LedgerKey { top, tree, cmd })
    }

    /// Same command on the same tree. Where it was launched from (the payload
    /// cwd) plays no part (user ruling 2026-11-15: the fingerprint is taken
    /// only from the tree the test actually runs in).
    fn collides(&self, other: &LedgerKey) -> bool {
        self.top == other.top && self.tree == other.tree && self.cmd == other.cmd
    }
}

/// The call being judged, as the ledger records it.
pub(super) struct Attempt<'a> {
    /// The PreToolUse `tool_use_id`; empty when the payload had none (such a
    /// row can never be promoted: [`confirm`] never matches an empty id).
    pub(super) tool_use_id: &'a str,
    /// The payload's raw `tool_input.command`.
    pub(super) raw_command: &'a str,
    /// The payload cwd.
    pub(super) cwd: &'a Path,
}

// ── ledger.v2.tsv ───────────────────────────────────────────────────────────

/// One record of `ledger.v2.tsv`, an append-only TSV (no row is ever
/// rewritten or removed; a promotion or an invalidation is a new row).
///
/// * `pending <id> <top> <tree> <cmd> <raw> <cwd>`: PreToolUse judged this
///   test (key = `top`, `tree`, normalized `cmd`) for the call `id`; `raw` and
///   `cwd` are the payload's `tool_input.command` and cwd.
/// * `confirm <id>`: the call `id` is known to have executed (a Post or a
///   PostToolUseFailure arrived, or `state test` ran it itself).
/// * `invalidate <id>`: the call `id` did not run (`state test` could not
///   spawn it); its pending rows count for nothing.
/// * `unmatched <raw> <cwd>`: a Post arrived for a test command that had no
///   pending row for its `tool_use_id`.
///
/// Every field is escaped by [`esc`] so a tab or newline inside a command
/// cannot split a row.
#[derive(Debug, Clone, PartialEq, Eq)]
enum V2Row {
    Pending {
        id: String,
        key: LedgerKey,
        raw: String,
        cwd: String,
    },
    Confirm {
        id: String,
    },
    Invalidate {
        id: String,
    },
    Unmatched {
        raw: String,
        cwd: String,
    },
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\t' => o.push_str("\\t"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            c => o.push(c),
        }
    }
    o
}

fn unesc(s: &str) -> Option<String> {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next()? {
            '\\' => o.push('\\'),
            't' => o.push('\t'),
            'n' => o.push('\n'),
            'r' => o.push('\r'),
            _ => return None,
        }
    }
    Some(o)
}

impl V2Row {
    fn line(&self) -> String {
        match self {
            V2Row::Pending { id, key, raw, cwd } => format!(
                "pending\t{}\t{}\t{}\t{}\t{}\t{}",
                esc(id),
                esc(&key.top),
                esc(&key.tree),
                esc(&key.cmd),
                esc(raw),
                esc(cwd)
            ),
            V2Row::Confirm { id } => format!("confirm\t{}", esc(id)),
            V2Row::Invalidate { id } => format!("invalidate\t{}", esc(id)),
            V2Row::Unmatched { raw, cwd } => format!("unmatched\t{}\t{}", esc(raw), esc(cwd)),
        }
    }

    /// `None` for any line that is not exactly one well-formed record.
    fn parse(line: &str) -> Option<Self> {
        let f: Vec<&str> = line.split('\t').collect();
        let u = |i: usize| f.get(i).and_then(|x| unesc(x));
        match (f.first().copied(), f.len()) {
            (Some("pending"), 7) => Some(V2Row::Pending {
                id: u(1)?,
                key: LedgerKey {
                    top: u(2)?,
                    tree: u(3)?,
                    cmd: u(4)?,
                },
                raw: u(5)?,
                cwd: u(6)?,
            }),
            (Some("confirm"), 2) => Some(V2Row::Confirm { id: u(1)? }),
            (Some("invalidate"), 2) => Some(V2Row::Invalidate { id: u(1)? }),
            (Some("unmatched"), 3) => Some(V2Row::Unmatched {
                raw: u(1)?,
                cwd: u(2)?,
            }),
            _ => None,
        }
    }
}

/// `ledger.v2.tsv` next to the legacy ledger (`ledger.tsv` -> `ledger.v2.tsv`;
/// any other file name -> `<name>.v2`).
fn v2_path(ledger: &Path) -> PathBuf {
    match ledger.file_name().and_then(|n| n.to_str()) {
        Some("ledger.tsv") => ledger.with_file_name("ledger.v2.tsv"),
        Some(n) => ledger.with_file_name(format!("{n}.v2")),
        None => ledger.with_extension("v2"),
    }
}

/// Read `ledger.v2.tsv`. A missing file is no rows; a file that cannot be read,
/// is not UTF-8, or holds a row that is not a well-formed record is an `Err`
/// (undetermined, never "no prior": CLAUDE.md §3).
fn read_v2(path: &Path) -> Result<Vec<V2Row>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let mut rows = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        match V2Row::parse(line) {
            Some(r) => rows.push(r),
            None => {
                return Err(format!(
                    "{} line {} is not a well-formed record",
                    path.display(),
                    i + 1
                ));
            }
        }
    }
    Ok(rows)
}

/// Append `lines` (one `write_all`, so rows of concurrent hooks do not
/// interleave) to `path`, creating it and its directory.
fn append_lines(path: &Path, lines: &[String]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut buf = String::new();
    for l in lines {
        buf.push_str(l);
        buf.push('\n');
    }
    f.write_all(buf.as_bytes())
}

/// What the two ledgers say once confirmations and invalidations are applied.
#[derive(Default)]
struct Priors {
    /// Keys of runs known to have executed.
    confirmed: Vec<LedgerKey>,
    /// Keys of attempts never confirmed (pending v2 rows, legacy rows nobody
    /// confirmed).
    pending: Vec<LedgerKey>,
    /// `(raw command, cwd)` of Posts that matched no pending row.
    unmatched: Vec<(String, String)>,
}

fn priors(v2: &[V2Row], legacy: &[LedgerKey]) -> Priors {
    let ids = |want_confirm: bool| -> std::collections::HashSet<&str> {
        v2.iter()
            .filter_map(|r| match r {
                V2Row::Confirm { id } if want_confirm => Some(id.as_str()),
                V2Row::Invalidate { id } if !want_confirm => Some(id.as_str()),
                _ => None,
            })
            .collect()
    };
    let confirmed_ids = ids(true);
    let invalid_ids = ids(false);
    let mut p = Priors::default();
    for r in v2 {
        match r {
            V2Row::Pending { id, key, .. } => {
                if !id.is_empty() && confirmed_ids.contains(id.as_str()) {
                    p.confirmed.push(key.clone());
                } else if id.is_empty() || !invalid_ids.contains(id.as_str()) {
                    p.pending.push(key.clone());
                }
            }
            V2Row::Unmatched { raw, cwd } => p.unmatched.push((raw.clone(), cwd.clone())),
            V2Row::Confirm { .. } | V2Row::Invalidate { .. } => {}
        }
    }
    // A legacy row is the old binaries' "ran" record; without a confirmation
    // behind it (the legacy row a confirmation itself appends has one) it
    // proves nothing, so it is pending.
    for k in legacy {
        if !p.confirmed.iter().any(|c| c.collides(k)) {
            p.pending.push(k.clone());
        }
    }
    p
}

/// The legacy `ledger.tsv`: unreadable is an `Err`; a line that does not parse
/// as a key never matched a key before either (the old check was exact line
/// equality), so it is skipped.
fn read_legacy(ledger: &Path) -> Result<Vec<LedgerKey>, String> {
    match std::fs::read_to_string(ledger) {
        Ok(s) => Ok(s.lines().filter_map(LedgerKey::parse).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!(
            "condukt testscope cannot read its rerun ledger {}: {e}",
            ledger.display()
        )),
    }
}

const ASK_TAIL: &str = "no PostToolUse / PostToolUseFailure has confirmed that an earlier attempt \
     actually executed, so a call that was stopped before it ran cannot be told from one that \
     ran; confirm that this test should run again";

/// Rule 3. `by_dir` maps each directory a test runs in (the payload cwd, or
/// the target of a leading `cd`) to its tests. The key is the identity of the
/// tree each test actually runs in plus the command, nothing else (backlog
/// 214b39b5: it used to be the payload cwd's tree whatever `cd` preceded the
/// test; user ruling 2026-11-15: the payload cwd's tree must not contribute
/// either, so editing only the payload cwd does not permit a rerun on an
/// unchanged run tree, pinned by `backlog_20261114_testscope_cd_tree_identity.rs`
/// `rerun_after_editing_only_the_payload_cwd_tree_is_refused_when_test_runs_in_cd_target`).
///
/// A key confirmed on this tree is denied. Otherwise a pending prior of the
/// key, or an unmatched Post for this raw command and cwd, asks. Both the
/// allowed and the asked attempt record their own pending row (an asked call
/// that is rejected fires no Post and so stays pending: the next rerun asks
/// again).
///
/// `tdd green --cmd <c>` (backlog 8f82f09d), at any nesting depth: its checked
/// key is `<c>` tagged once ([`green_tagged`]), so it collides only with an
/// earlier `tdd green` of `<c>` on this tree, not a bare run (that would leave
/// the F->P oracle no way to record GREEN). It also checks the key of a
/// `tdd red` of `<c>` ([`TDD_RED_ROUTE`], backlog 0c301e8b): a confirmed one on
/// this tree is denied, a pending one asks. It records the fully [`untagged`]
/// bare key, so a later bare run is refused.
///
/// `tdd red --cmd <c>` (backlog fb4f650d) is checked against its red-tagged key
/// and an earlier `tdd green` of `<c>`, not the bare key, so a bare run on the
/// tree cannot block the RED proof; it records both the bare key and its
/// red-tagged key.
pub(super) fn check_rerun(
    by_dir: &[(PathBuf, Vec<Words>)],
    ledger: &Path,
    at: &Attempt,
) -> Decision {
    let mut keys: Vec<LedgerKey> = Vec::new();
    let mut rows_keys: Vec<LedgerKey> = Vec::new();
    for (dir, tests) in by_dir {
        let (top, tree) = match tree_identity(dir) {
            Ok(t) => t,
            Err(e) => {
                return Decision::Deny(format!(
                    "condukt testscope cannot determine the tree this test runs against in {} \
                     ({e}), so whether it re-runs a test already run on the same tree is \
                     undetermined",
                    dir.display()
                ));
            }
        };
        for w in tests {
            let key = |cmd: String| LedgerKey {
                top: top.clone(),
                tree: tree.clone(),
                cmd,
            };
            let (bare, green, red) = untagged(w);
            let bare_key = key(bare.join(" "));
            let red_key = key(format!("{} {}", TDD_RED_ROUTE.join(" "), bare.join(" ")));
            if green {
                // Checked against an earlier green of this command, and against
                // an earlier red of it (a red then green on one tree is refused).
                // A green records its green key, not a red key of its own (a
                // red nested inside it is recorded by the `red` arm below).
                let green_key = key(w.join(" "));
                keys.push(green_key.clone());
                keys.push(red_key.clone());
                rows_keys.push(green_key);
            } else if red {
                // backlog fb4f650d (mirror of 8f82f09d): a `tdd red` is checked
                // only against an earlier `tdd red` of this command, so a bare
                // run cannot block the RED evidence the F->P oracle requires.
                // It is still refused after a `tdd green` of it (a red after a
                // green on one unchanged tree proves nothing), and it records
                // the bare key below, so a bare run after it is refused.
                keys.push(red_key.clone());
                keys.push(key(format!(
                    "{} {}",
                    TDD_GREEN_ROUTE.join(" "),
                    bare.join(" ")
                )));
            } else {
                keys.push(bare_key.clone());
            }
            rows_keys.push(bare_key);
            if red {
                rows_keys.push(red_key);
            }
        }
    }
    let v2p = v2_path(ledger);
    let legacy = match read_legacy(ledger) {
        Ok(l) => l,
        Err(e) => return Decision::Deny(e),
    };
    let v2 = match read_v2(&v2p) {
        Ok(r) => r,
        Err(e) => {
            return Decision::Deny(format!(
                "condukt testscope cannot read its v2 rerun ledger ({e}), so whether this test \
                 already ran is undetermined"
            ));
        }
    };
    let prior = priors(&v2, &legacy);
    for k in &keys {
        if prior.confirmed.iter().any(|r| r.collides(k)) {
            return Decision::Deny(format!(
                "rerun refused: `{}` already ran on this exact tree ({} at {}); running the same \
                 test twice is forbidden (user ruling 2026-11-07). Change the code first, or \
                 move on",
                k.cmd, k.tree, k.top
            ));
        }
    }
    let asked = keys
        .iter()
        .find(|k| prior.pending.iter().any(|r| r.collides(k)))
        .map(|k| {
            format!(
                "`{}` was attempted before on this exact tree ({} at {}), but {ASK_TAIL}",
                k.cmd, k.tree, k.top
            )
        })
        .or_else(|| {
            let cwd = at.cwd.to_string_lossy();
            prior
                .unmatched
                .iter()
                .any(|(r, c)| r == at.raw_command && c.as_str() == cwd)
                .then(|| {
                    format!(
                        "a PostToolUse arrived for `{}` in {} that matched no recorded attempt, \
                         so whether it ran on this tree is unknown, and {ASK_TAIL}",
                        at.raw_command, cwd
                    )
                })
        });
    let rows: Vec<String> = rows_keys
        .iter()
        .map(|k| {
            V2Row::Pending {
                id: at.tool_use_id.to_string(),
                key: k.clone(),
                raw: at.raw_command.to_string(),
                cwd: at.cwd.to_string_lossy().into_owned(),
            }
            .line()
        })
        .collect();
    let write = append_lines(&v2p, &rows);
    match (asked, write) {
        (None, Ok(())) => Decision::Allow,
        (None, Err(e)) => Decision::Deny(format!(
            "condukt testscope cannot record this test run in {} ({e}), so a later rerun could \
             not be detected",
            v2p.display()
        )),
        (Some(why), Ok(())) => Decision::Ask(format!("condukt testscope: {why}")),
        // Asking is already the restricted side; the unwritten pending row only
        // means this asked attempt is not remembered, which the prior still is.
        (Some(why), Err(e)) => Decision::Ask(format!(
            "condukt testscope: {why} (this attempt could not be recorded in {}: {e})",
            v2p.display()
        )),
    }
}

/// The fault named by `CONDUKT_TESTSCOPE_FAULT`, if it is `want`. Test-only
/// seam, read from the environment and nowhere else; see the module doc.
fn fault(want: &str) -> bool {
    std::env::var("CONDUKT_TESTSCOPE_FAULT").is_ok_and(|v| v == want)
}

/// What [`confirm`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Confirmed {
    /// The pending rows of the call are now confirmed.
    Promoted,
    /// No pending row carried the call's id; an `unmatched` row was appended
    /// (only when the caller asked for it).
    Unmatched,
    /// The tree of at least one key changed during the call: those keys stay
    /// pending (a same-tree rerun asks) and their post-call tree is recorded
    /// pending as well; any unchanged key is promoted (backlog bdc30aa7).
    TreeMoved,
}

/// Promote every pending row of the call `id` to confirmed: append `confirm
/// <id>` to `ledger.v2.tsv`, then the old 3-column row of each key to
/// `ledger.tsv` (so an older cached binary also refuses the rerun). With no
/// pending row for `id` (or an empty `id`) append an `unmatched` row for
/// `raw_command` / `cwd` instead. A key whose tree the call itself changed is
/// not promoted (see [`Confirmed::TreeMoved`]). `Err` means the confirmation (or a part of
/// it) was not recorded; the rows then stay pending, which asks on a rerun.
pub fn confirm(
    ledger: &Path,
    id: &str,
    raw_command: &str,
    cwd: &Path,
) -> Result<Confirmed, String> {
    let v2p = v2_path(ledger);
    let rows = read_v2(&v2p)?;
    let keys: Vec<&LedgerKey> = if id.is_empty() {
        Vec::new()
    } else {
        rows.iter()
            .filter_map(|r| match r {
                V2Row::Pending { id: i, key, .. } if i == id => Some(key),
                _ => None,
            })
            .collect()
    };
    if keys.is_empty() {
        append_lines(
            &v2p,
            &[V2Row::Unmatched {
                raw: raw_command.to_string(),
                cwd: cwd.to_string_lossy().into_owned(),
            }
            .line()],
        )
        .map_err(|e| format!("cannot append to {}: {e}", v2p.display()))?;
        return Ok(Confirmed::Unmatched);
    }
    // A key whose tree is no longer the one PreToolUse measured was changed by
    // the call itself (`git checkout <rev> -- f && tdd red ...`), so the test
    // may have run on another tree: it is not promoted (stays pending = asks),
    // and the post-call tree gets a pending row too (backlog bdc30aa7). A tree
    // that cannot be re-read keeps the old promotion (the restricted side).
    let (mut stable, mut moved) = (Vec::new(), Vec::new());
    for k in keys {
        match tree_identity(Path::new(&k.top)) {
            Ok((top, tree)) if top == k.top && tree != k.tree => moved.push(LedgerKey {
                top,
                tree,
                cmd: k.cmd.clone(),
            }),
            _ => stable.push(k),
        }
    }
    let confirm_id = if moved.is_empty() {
        id.to_string()
    } else {
        format!("{id}#stable")
    };
    let pending = |rid: &str, k: &LedgerKey| {
        V2Row::Pending {
            id: rid.to_string(),
            key: k.clone(),
            raw: raw_command.to_string(),
            cwd: cwd.to_string_lossy().into_owned(),
        }
        .line()
    };
    let mut rows: Vec<String> = Vec::new();
    if !moved.is_empty() {
        rows.extend(stable.iter().map(|k| pending(&confirm_id, k)));
        rows.extend(moved.iter().map(|k| pending(&format!("{id}#post"), k)));
    }
    if !stable.is_empty() {
        rows.push(V2Row::Confirm { id: confirm_id }.line());
    }
    append_lines(&v2p, &rows).map_err(|e| format!("cannot append to {}: {e}", v2p.display()))?;
    if stable.is_empty() {
        return Ok(Confirmed::TreeMoved);
    }
    let legacy: Vec<String> = stable.iter().map(|k| k.line()).collect();
    append_lines(ledger, &legacy).map_err(|e| {
        format!(
            "the confirmation is in {} but the legacy row could not be appended to {}: {e}",
            v2p.display(),
            ledger.display()
        )
    })?;
    Ok(if moved.is_empty() {
        Confirmed::Promoted
    } else {
        Confirmed::TreeMoved
    })
}

/// Mark the call `id` as not having run: append `invalidate <id>`. The caller
/// (`state test`, after a failed spawn) uses it so its own pending row does not
/// make a later rerun ask. `Err` leaves the row pending (the restricted side).
pub fn invalidate(ledger: &Path, id: &str) -> Result<(), String> {
    let v2p = v2_path(ledger);
    if fault("invalidate_append_fail") {
        return Err(format!(
            "cannot append to {}: injected fault CONDUKT_TESTSCOPE_FAULT=invalidate_append_fail",
            v2p.display()
        ));
    }
    append_lines(&v2p, &[V2Row::Invalidate { id: id.to_string() }.line()])
        .map_err(|e| format!("cannot append to {}: {e}", v2p.display()))
}

/// Wrapper-refusal markers emitted by `tdd` itself (`crates/tdd/src/config.rs`
/// `untrusted_reason` / `invalid_reason`): both carry `REFUSING TO JUDGE`, and
/// the untrusted one the ``Remedy: run `tdd trust` `` line.
const WRAPPER_REFUSAL_MARKERS: &[&str] = &["REFUSING TO JUDGE", "Remedy: run `tdd trust`"];
/// Output that only appears once cargo / libtest actually started.
const RUNNER_START_MARKERS: &[&str] = &["Running ", "Compiling", "test result:", "running "];

/// True only when a FAILED Bash call's output POSITIVELY shows that the test
/// runner never started: it contains a known `tdd` wrapper-refusal marker
/// ([`WRAPPER_REFUSAL_MARKERS`]) AND none of the cargo/libtest start markers
/// ([`RUNNER_START_MARKERS`]). A real cargo failure (exit 101, "test result:
/// FAILED", compile errors) stays a run; anything unclear returns false, i.e.
/// the restrictive side (the call is confirmed). NOT detected: refusals by
/// wrappers other than tdd, a refusal whose text lacks the markers, and a
/// refusal in a chain where an earlier cargo step already printed start
/// markers (all stay confirmed).
pub fn failure_shows_runner_never_started(output: &str) -> bool {
    WRAPPER_REFUSAL_MARKERS.iter().any(|m| output.contains(m))
        && !RUNNER_START_MARKERS.iter().any(|m| output.contains(m))
}

/// PostToolUse / PostToolUseFailure of a Bash call. Does nothing (`Ok(None)`)
/// unless the parser finds a test command in `command`; then [`confirm`]s the
/// call. A panic injected by `CONDUKT_TESTSCOPE_FAULT=confirm_panic` happens
/// here, after a test command was found.
pub fn confirm_post(
    command: &str,
    cwd: &Path,
    home: Option<&Path>,
    ledger: &Path,
    tool_use_id: Option<&str>,
) -> Result<Option<Confirmed>, String> {
    match parse_tests(command, cwd, home) {
        Parsed::Final(_) => return Ok(None),
        Parsed::Tests(_) => {}
    }
    if fault("confirm_panic") {
        // The seam exists to prove the confirm hook surfaces a panic as a
        // `systemMessage` instead of blocking; the panic is the point.
        #[allow(clippy::panic)]
        let crash = || panic!("injected fault CONDUKT_TESTSCOPE_FAULT=confirm_panic");
        crash();
    }
    confirm(ledger, tool_use_id.unwrap_or(""), command, cwd).map(Some)
}

/// Settle a run that `condukt state test` made itself (it has no Post hook):
/// a spawned test (any exit status) is confirmed, a failed spawn is
/// invalidated. A confirmation that cannot be recorded is reported on stderr
/// (the row stays pending, so a rerun asks); an invalidation that cannot be
/// recorded is an `Err` that says so (the row stays pending too).
pub fn settle_self_run(
    ledger: &Path,
    id: &str,
    cmd: &str,
    cwd: &Path,
    spawned: std::io::Result<std::process::ExitStatus>,
) -> Result<std::process::ExitStatus, String> {
    match spawned {
        Ok(status) => {
            if let Err(e) = confirm(ledger, id, cmd, cwd) {
                eprintln!(
                    "condukt: the run of '{cmd}' could not be confirmed in the rerun ledger \
                     ({e}); a rerun will ask instead of being refused"
                );
            }
            Ok(status)
        }
        Err(e) => match invalidate(ledger, id) {
            Ok(()) => Err(format!("failed to run '{cmd}': {e}")),
            Err(ie) => Err(format!(
                "failed to run '{cmd}': {e}; and the attempt could not be invalidated ({ie}), \
                 so its row stays pending and a rerun will ask"
            )),
        },
    }
}

/// A fresh call id for a run that is not a hook call (`condukt state test`).
pub fn self_attempt_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("state-test-{}-{nanos}", std::process::id())
}

/// The ledger path: `$CONDUKT_TESTSCOPE_LEDGER`, else
/// `$HOME/.condukt/testscope/ledger.tsv`.
pub fn ledger_path(home: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CONDUKT_TESTSCOPE_LEDGER") {
        return Some(PathBuf::from(p));
    }
    home.map(|h| h.join(".condukt").join("testscope").join("ledger.tsv"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A v2 row survives tabs, newlines and backslashes in its fields, and a
    /// line that is not exactly one record does not parse (R9 relies on this).
    #[test]
    fn v2_rows_round_trip_and_malformed_lines_do_not_parse() {
        let rows = [
            V2Row::Pending {
                id: "a\tb".into(),
                key: LedgerKey {
                    top: "/t".into(),
                    tree: "h:0".into(),
                    cmd: "cargo test".into(),
                },
                raw: "cargo  test\n&& echo \\x".into(),
                cwd: "/c".into(),
            },
            V2Row::Confirm { id: "1".into() },
            V2Row::Invalidate { id: "2".into() },
            V2Row::Unmatched {
                raw: "r".into(),
                cwd: "/c".into(),
            },
        ];
        for r in rows {
            let line = r.line();
            assert!(!line.contains('\n'));
            assert_eq!(V2Row::parse(&line), Some(r));
        }
        for bad in ["", "zzz\tx", "confirm", "confirm\t1\t2", "confirm\t\\q"] {
            assert_eq!(V2Row::parse(bad), None, "{bad:?}");
        }
    }
}
