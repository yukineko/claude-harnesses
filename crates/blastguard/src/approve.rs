//! Trust-on-first-use approval memory: remember that a human already said yes
//! to THIS effect, so the same effect is not asked about again.
//!
//! # Why this module exists
//!
//! `Ask` is the crate's third answer — "blastguard cannot tell, so a human
//! must" ([`crate::model`]). It is correct and it is also repetitive: a command
//! a human approved five minutes ago is asked about again, and again, on every
//! repetition. That repetition is not a cosmetic annoyance; it is the failure
//! mode that got the sibling crate `taintguard` retired by user ruling on
//! 2026-08-24, because a gate that asks about ordinary work teaches its operator
//! to stop reading the question. This module removes the repetition WITHOUT
//! removing the question.
//!
//! # What is remembered, and what is not
//!
//! An approval is keyed on the EFFECT, never on the script. That distinction is
//! the whole design:
//!
//! * the command text, whitespace-normalised — so different PARAMETERS are a
//!   different key. `chmod -R 755 sub` being approved says nothing about
//!   `chmod -R 777 sub`, because the parameters are where the effect lives;
//! * every path-shaped token's RESOLVED REAL PATH — so an approval cannot be
//!   carried to another location, and re-pointing a symlink after the approval
//!   moves the key rather than inheriting it;
//! * every resolved target's CONTENT HASH — so a target that changed under a
//!   standing approval is re-judged (「過去に実行されても変更があったときは
//!   再度判断すべきである」).
//!
//! And it is bounded by WHERE the effect lands: an approval is only computable
//! when every path-shaped token resolves strictly INSIDE one of this session's
//! safe roots ([`crate::scope::Placement::Inside`], the one variant that module
//! permits to relax a verdict). An effect reaching outside the project is not
//! "approved with caveats", it is not representable in this store at all.
//!
//! # Direction: `Ask` → `Allow`, and nothing else
//!
//! The memory may only DOWNGRADE an `Ask`. It never touches a `Deny`, never
//! upgrades a blast radius, and never manufactures an approval on its own. A
//! `Deny`'d command never reaches the recorder in the first place, because a
//! denied tool call produces no `PostToolUse` — which is also the reason
//! recording lives on `PostToolUse` and not here: a `PreToolUse` hook cannot
//! know what the human answered, and the tool having actually RUN is the only
//! evidence available that they said yes.
//!
//! That is why the store has two tiers. `PreToolUse` stashes a PENDING
//! fingerprint (the state of the world at the moment the human was asked);
//! `PostToolUse` promotes it. A pending entry is never an approval — otherwise
//! blastguard would approve every command it had merely asked about, including
//! the ones that were refused.
//!
//! # Fail-closed (CLAUDE.md §3)
//!
//! Every "cannot determine" in this module resolves to "no approval", which
//! means the `Ask` stands — the restrictive side. The three-valued
//! [`Lookup`] is what makes that expressible: an unreadable store, an
//! unparseable entry and an absent entry are three different facts and none of
//! them is an approval. Concretely:
//!
//! * command text that is not statically readable (an expansion, a substitution,
//!   quoting this module cannot faithfully tokenise) → `Undetermined`;
//! * any token that does not resolve strictly inside a safe root → `Undetermined`;
//! * a target that exists but whose content cannot be hashed → `Undetermined`;
//! * store IO failure, or an entry that does not parse or does not name the
//!   fingerprint it is filed under → `Undetermined`;
//! * an empty store → `NotRecorded`, i.e. first use, i.e. ask.
//!
//! No path in this module returns `Approved` on the strength of something it
//! failed to read.
//!
//! # I/O
//!
//! Fingerprinting is PURE: the filesystem enters through an injected
//! [`TargetProbe`] that only the binary supplies, the same shape
//! [`crate::scope::RealPathResolver`] already uses. [`Store`] does touch the
//! filesystem — it is a store — but it is a thin wrapper whose every failure is
//! an `Undetermined`, and the analyser in [`crate::detect`] neither calls nor
//! knows about any of this.

use crate::scope::{Placement, SafeRoots};
use harness_core::verdict::Determination;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Hex SHA-256 of `bytes`.
///
/// Local rather than in `harness-core`: adding a helper there would bump the
/// shared crate's version, and `check-plugin-rollout.py` compares every
/// plugin's recorded `harness_core_version`, so a one-function addition would
/// require re-rolling 36 plugins that have no use for it. `sha2` is already a
/// workspace dependency (fugu-router, precommit-audit, stuckguard).
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Resolve a path to a stable description of WHAT IS THERE right now.
///
/// The binary's implementation answers with the target's kind and, for a
/// regular file, a hash of its contents. Injected rather than called directly so
/// every branch below — including "the target could not be probed" — is
/// reachable from a unit test without a filesystem.
///
/// `Undetermined` means "could not tell what is there", and this module then
/// declines to compute a fingerprint at all.
pub type TargetProbe = fn(&str) -> Determination<String>;

/// The answer to "is this exact effect on record as approved?".
///
/// Three answers, not two — for the reason [`crate::model::Decision`] gives at
/// length: a bool would force "the store could not be read" and "the store said
/// no" into the same value, and only one of those two is a fact about the
/// command.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Lookup {
    /// A human approved this exact fingerprint before. The only variant that
    /// may downgrade an `Ask`.
    Approved,
    /// Nothing on record: a first use, or the fingerprint moved because the
    /// parameters or the target changed.
    NotRecorded,
    /// The store could not answer. NOT an approval.
    Undetermined(String),
}

impl Lookup {
    /// True only for [`Lookup::Approved`].
    ///
    /// Deliberately not a `bool`-returning `is_ok`-shaped helper on the other
    /// variants: the callers that matter must be unable to write
    /// `unwrap_or(true)`, and there is exactly one question worth asking here.
    #[must_use]
    pub fn is_approved(&self) -> bool {
        matches!(self, Lookup::Approved)
    }
}

/// Characters whose presence means this module cannot read the command's effect
/// off the text.
///
/// * `$` and `` ` `` — an expansion or substitution whose value only exists at
///   run time, so the same TEXT is not the same EFFECT on two runs. `scope`
///   already refuses to expand these; remembering them would undo that.
/// * `'`, `"`, `\` — quoting. This module tokenises on whitespace, which is a
///   deliberate over-approximation (see [`tokens`]); quoting makes that
///   over-approximation WRONG rather than merely coarse, because a quoted
///   operand containing a space would be split into two tokens neither of which
///   is the real path. An imperfect tokenisation must degrade to "ask", so it
///   degrades here rather than being papered over with a hand-rolled parser
///   that would be a second, driftable copy of `detect`'s.
const UNREADABLE_CHARS: &[char] = &['$', '`', '\'', '"', '\\'];

/// Collapse whitespace runs to single spaces and trim.
///
/// The normalisation is intentionally minimal: anything that changes what the
/// command DOES must change this string. Reordering flags, changing a mode,
/// adding an operand — all of those survive into the fingerprint, and that is
/// the point of control (ii).
#[must_use]
pub fn normalize_command(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The whitespace-separated tokens of a normalised command.
///
/// EVERY token is treated as a candidate path operand, including the command
/// word and the flags. That is coarse on purpose, and it is coarse in the
/// restrictive direction: a token that is not a path (`chmod`, `-R`, `755`)
/// resolves against the session cwd and lands inside a safe root, contributing
/// only extra entropy to the fingerprint; a token that IS an absolute path
/// outside the tree makes the whole command unapprovable. The alternative — a
/// heuristic for "looks like a path" — could only ever err by SKIPPING an
/// operand, and a skipped operand is one whose change would not move the
/// fingerprint. So the coarse rule is the safe one.
fn tokens(normalized: &str) -> Vec<&str> {
    normalized.split(' ').filter(|t| !t.is_empty()).collect()
}

/// The key an approval is filed under: tool + command text + every token's
/// resolved path and current content.
///
/// `Undetermined` means no approval can be computed, hence no approval can
/// apply and none can be recorded — the `Ask` stands.
pub fn fingerprint(
    tool: &str,
    command: &str,
    roots: &SafeRoots,
    probe: TargetProbe,
) -> Determination<String> {
    let normalized = normalize_command(command);
    if normalized.is_empty() {
        return Determination::undetermined("empty command text");
    }
    if let Some(bad) = normalized.chars().find(|c| UNREADABLE_CHARS.contains(c)) {
        return Determination::undetermined(format!(
            "command contains `{bad}`, so its effect is not readable from its text"
        ));
    }
    // The command identity comes first so that two commands can never collide
    // by having the same operand descriptions.
    let mut material = format!("v1\ntool={tool}\ncmd={normalized}\n");
    let cwd = roots.session_cwd();
    for token in tokens(&normalized) {
        let placement = match roots.classify(token, cwd) {
            Determination::Known(p) => p,
            Determination::Undetermined(why) => {
                return Determination::undetermined(format!(
                    "operand `{token}` could not be located: {}",
                    why.as_str()
                ));
            }
        };
        // `Inside` only. `IsRoot` is the project top itself and `Outside` has
        // left the tree; `scope` documents `Inside` as the sole variant that may
        // relax a caller's verdict, and an approval IS a relaxation.
        let Placement::Inside { path, .. } = &placement else {
            return Determination::undetermined(format!(
                "operand `{token}` does not land strictly inside a safe root, so its \
effect reaches outside this project"
            ));
        };
        let state = match probe(path) {
            Determination::Known(s) => s,
            Determination::Undetermined(why) => {
                return Determination::undetermined(format!(
                    "operand `{token}` could not be inspected: {}",
                    why.as_str()
                ));
            }
        };
        material.push_str("op=");
        material.push_str(path);
        material.push('\0');
        material.push_str(&state);
        material.push('\n');
    }
    Determination::known(sha256_hex(material.as_bytes()))
}

/// The key that survives the PreToolUse → PostToolUse boundary.
///
/// The full [`fingerprint`] cannot: by the time `PostToolUse` runs, the command
/// has already changed the very targets the fingerprint hashes (`rm -rf x`
/// leaves `x` absent), so a fingerprint recomputed there would describe a state
/// no future `PreToolUse` ever sees. So the recorder finds the pending entry by
/// command identity alone and promotes the fingerprint that `PreToolUse`
/// computed — which is the state of the world the human actually looked at.
#[must_use]
pub fn command_key(tool: &str, command: &str) -> String {
    let normalized = normalize_command(command);
    sha256_hex(format!("v1\ntool={tool}\ncmd={normalized}\n").as_bytes())
}

/// The on-disk approval memory: `<dir>/pending/<command-key>` and
/// `<dir>/approved/<fingerprint>`.
///
/// One file per entry rather than one index file, deliberately: concurrent
/// sessions share this store (CLAUDE.md §8 assumes another session is always
/// running), and a single index would need a lock whose failure would have to
/// resolve to `Undetermined` on every read. Per-entry files make the common read
/// lock-free, and make a corrupt entry cost exactly one approval.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Point the store at `dir`. Creates nothing: a store that does not exist
    /// yet is an empty store, which is "first use", which is an ask.
    pub fn open(dir: impl Into<PathBuf>) -> Store {
        Store { dir: dir.into() }
    }

    fn approved_path(&self, fingerprint: &str) -> PathBuf {
        self.dir.join("approved").join(fingerprint)
    }

    fn pending_path(&self, key: &str) -> PathBuf {
        self.dir.join("pending").join(key)
    }

    /// Is this exact fingerprint on record as approved?
    pub fn lookup(&self, fingerprint: &str) -> Lookup {
        let path = self.approved_path(fingerprint);
        // `boundary::read_to_string` rather than `std::fs`: it already draws the
        // one distinction this function turns on — `Known(None)` is "absent"
        // (exactly `NotFound`), and every other error kind, `PermissionDenied`
        // included, is `Undetermined`. Present-but-unreadable is NOT absent:
        // reporting it as `NotRecorded` would merely ask, whereas
        // `Undetermined` also asks but says why, and keeps a permission-broken
        // store from reading as a working one.
        let body = match harness_core::boundary::read_to_string(&path) {
            Determination::Known(Some(b)) => b,
            Determination::Known(None) => return Lookup::NotRecorded,
            Determination::Undetermined(why) => {
                return Lookup::Undetermined(format!(
                    "approval entry could not be read: {}",
                    why.as_str()
                ));
            }
        };
        // The entry must name the fingerprint it is filed under. A truncated or
        // hand-edited file therefore fails to approve rather than approving
        // whatever it happens to be named.
        match serde_json::from_str::<Entry>(&body) {
            Ok(entry) if entry.fingerprint == fingerprint => Lookup::Approved,
            Ok(_) => Lookup::Undetermined(
                "approval entry does not name the fingerprint it is filed under".to_string(),
            ),
            Err(e) => Lookup::Undetermined(format!("approval entry did not parse: {e}")),
        }
    }

    /// Stash the fingerprint `PreToolUse` computed, so `PostToolUse` can promote
    /// it if the tool actually runs.
    ///
    /// Returns the IO error rather than swallowing it, so the caller decides;
    /// the caller in `main` discards it, because failing to stash means the next
    /// run asks again — the restrictive direction.
    pub fn put_pending(&self, key: &str, fingerprint: &str, command: &str) -> std::io::Result<()> {
        let path = self.pending_path(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let entry = Entry {
            fingerprint: fingerprint.to_string(),
            command: normalize_command(command),
        };
        let body = serde_json::to_string(&entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_atomic(&path, &body)
    }

    /// Promote the pending entry for `key` into an approval.
    ///
    /// Consumes the pending entry: an approval is granted once per ask, so a
    /// stale pending cannot keep granting approvals for later runs that were
    /// never asked about.
    pub fn promote(&self, key: &str) -> Determination<String> {
        let pending = self.pending_path(key);
        let body = match harness_core::boundary::read_to_string(&pending) {
            Determination::Known(Some(b)) => b,
            Determination::Known(None) => {
                // Ordinary: the command was allowed outright, so nothing ever
                // asked and there is nothing to approve.
                return Determination::undetermined("no pending approval for this command");
            }
            Determination::Undetermined(why) => {
                return Determination::undetermined(format!(
                    "pending unreadable: {}",
                    why.as_str()
                ));
            }
        };
        let entry: Entry = match serde_json::from_str(&body) {
            Ok(e) => e,
            Err(e) => {
                let _ = std::fs::remove_file(&pending);
                return Determination::undetermined(format!("pending did not parse: {e}"));
            }
        };
        let approved = self.approved_path(&entry.fingerprint);
        if let Some(parent) = approved.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Determination::undetermined(format!("approval dir not creatable: {e}"));
            }
        }
        let serialized = match serde_json::to_string(&entry) {
            Ok(s) => s,
            Err(e) => {
                return Determination::undetermined(format!("approval not serializable: {e}"))
            }
        };
        if let Err(e) = write_atomic(&approved, &serialized) {
            return Determination::undetermined(format!("approval not writable: {e}"));
        }
        let _ = std::fs::remove_file(&pending);
        Determination::known(entry.fingerprint)
    }
}

/// A store entry. `fingerprint` is duplicated inside the file it names so
/// [`Store::lookup`] can verify the two agree.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Entry {
    fingerprint: String,
    /// The normalised command text, for human inspection of the store. Never
    /// read back as part of the decision — the fingerprint is the key.
    command: String,
}

/// Write via a temp file + rename so a concurrent reader never sees a
/// half-written entry (which `lookup` would report as `Undetermined`, asking
/// about a command that was in fact approved).
fn write_atomic(path: &Path, body: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A probe that answers for everything, so tests exercise the LOCATION
    /// rules without a filesystem.
    fn probe_known(_path: &str) -> Determination<String> {
        Determination::known("file:deadbeef".to_string())
    }

    fn probe_undetermined(_path: &str) -> Determination<String> {
        Determination::undetermined("cannot read")
    }

    fn roots() -> SafeRoots {
        SafeRoots::new(
            Some("/home/yuki/proj"),
            Some("/home/yuki/proj"),
            Some("/home/yuki"),
            None,
            Some(|p: &str| Some(p.to_string())),
        )
    }

    fn fp(cmd: &str) -> Determination<String> {
        fingerprint("Bash", cmd, &roots(), probe_known)
    }

    fn is_undetermined(d: &Determination<String>) -> bool {
        matches!(d, Determination::Undetermined(_))
    }

    #[test]
    fn normalize_collapses_whitespace_only() {
        assert_eq!(
            normalize_command("  chmod   -R  755\tsub \n"),
            "chmod -R 755 sub"
        );
        // A changed parameter is a changed string, which is the whole basis of
        // control (ii).
        assert_ne!(
            normalize_command("chmod -R 755 sub"),
            normalize_command("chmod -R 777 sub")
        );
    }

    #[test]
    fn changed_parameters_change_the_fingerprint() {
        let a = fp("chmod -R 755 sub");
        let b = fp("chmod -R 777 sub");
        let c = fp("chmod -R 755 sub extra");
        for d in [&a, &b, &c] {
            assert!(!is_undetermined(d), "expected a fingerprint, got {d:?}");
        }
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn changed_target_state_changes_the_fingerprint() {
        fn probe_other(_p: &str) -> Determination<String> {
            Determination::known("file:cafebabe".to_string())
        }
        let before = fingerprint("Bash", "chmod -R 755 sub", &roots(), probe_known);
        let after = fingerprint("Bash", "chmod -R 755 sub", &roots(), probe_other);
        assert!(!is_undetermined(&before));
        assert_ne!(before, after);
    }

    #[test]
    fn same_command_same_state_is_stable() {
        assert_eq!(fp("chmod -R 755 sub"), fp("chmod  -R  755  sub"));
    }

    #[test]
    fn expansions_and_quoting_are_undetermined() {
        for cmd in [
            "chmod -R 755 $TARGET",
            "chmod -R 755 `echo sub`",
            "chmod -R 755 'sub dir'",
            "chmod -R 755 \"sub\"",
            "chmod -R 755 sub\\ dir",
            "",
            "   ",
        ] {
            assert!(
                is_undetermined(&fp(cmd)),
                "`{cmd}` must not be fingerprintable"
            );
        }
    }

    #[test]
    fn outside_and_root_operands_are_undetermined() {
        // Absolute, outside every safe root.
        assert!(is_undetermined(&fp("chmod -R 755 /etc")));
        // Escapes the tree through a parent reference.
        assert!(is_undetermined(&fp("chmod -R 755 ../../etc/passwd")));
        // The safe root ITSELF is not `Inside`.
        assert!(is_undetermined(&fp("chmod -R 755 /home/yuki/proj")));
    }

    #[test]
    fn an_unprobeable_target_is_undetermined() {
        assert!(is_undetermined(&fingerprint(
            "Bash",
            "chmod -R 755 sub",
            &roots(),
            probe_undetermined
        )));
    }

    #[test]
    fn no_location_model_is_undetermined() {
        // `SafeRoots::none()` classifies everything as `Undetermined`, so a
        // consumer with no location model can never approve anything.
        assert!(is_undetermined(&fingerprint(
            "Bash",
            "chmod -R 755 sub",
            &SafeRoots::none(),
            probe_known
        )));
    }

    #[test]
    fn command_key_ignores_target_state_but_not_parameters() {
        assert_eq!(
            command_key("Bash", "chmod -R 755 sub"),
            command_key("Bash", "chmod  -R 755   sub")
        );
        assert_ne!(
            command_key("Bash", "chmod -R 755 sub"),
            command_key("Bash", "chmod -R 777 sub")
        );
        assert_ne!(
            command_key("Bash", "chmod -R 755 sub"),
            command_key("Write", "chmod -R 755 sub")
        );
    }

    /// A scratch directory for the store tests.
    ///
    /// `std::env::temp_dir()` rather than `CARGO_TARGET_TMPDIR`, which Cargo
    /// only defines for INTEGRATION tests — these are lib unit tests. The
    /// inside/outside distinction that forced `tests/approval_memory.rs` off
    /// `/tmp` does not arise here: these tests exercise [`Store`] alone and
    /// never build a [`SafeRoots`].
    fn store_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("blastguard-approve-unit")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn empty_store_is_not_recorded_not_approved() {
        let s = Store::open(store_dir("empty"));
        assert_eq!(s.lookup("abc"), Lookup::NotRecorded);
        assert!(!s.lookup("abc").is_approved());
    }

    #[test]
    fn pending_is_not_an_approval_until_promoted() {
        let s = Store::open(store_dir("promote"));
        s.put_pending("k", "fp1", "chmod -R 755 sub").unwrap();
        assert_eq!(s.lookup("fp1"), Lookup::NotRecorded);
        assert_eq!(s.promote("k"), Determination::known("fp1".to_string()));
        assert_eq!(s.lookup("fp1"), Lookup::Approved);
    }

    #[test]
    fn promote_consumes_the_pending_entry() {
        let s = Store::open(store_dir("consume"));
        s.put_pending("k", "fp1", "cmd").unwrap();
        assert!(matches!(s.promote("k"), Determination::Known(_)));
        // A second promotion has nothing to promote: one ask, one approval.
        assert!(matches!(s.promote("k"), Determination::Undetermined(_)));
    }

    #[test]
    fn promote_without_a_pending_is_undetermined() {
        let s = Store::open(store_dir("no-pending"));
        assert!(matches!(s.promote("k"), Determination::Undetermined(_)));
    }

    #[test]
    fn an_unparseable_entry_is_undetermined_not_approved() {
        let dir = store_dir("corrupt");
        let s = Store::open(&dir);
        std::fs::create_dir_all(dir.join("approved")).unwrap();
        std::fs::write(dir.join("approved").join("fp1"), "{not json").unwrap();
        assert!(matches!(s.lookup("fp1"), Lookup::Undetermined(_)));
        assert!(!s.lookup("fp1").is_approved());
    }

    #[test]
    fn an_entry_naming_another_fingerprint_does_not_approve() {
        let dir = store_dir("mismatch");
        let s = Store::open(&dir);
        std::fs::create_dir_all(dir.join("approved")).unwrap();
        std::fs::write(
            dir.join("approved").join("fp1"),
            r#"{"fingerprint":"fp2","command":"cmd"}"#,
        )
        .unwrap();
        assert!(matches!(s.lookup("fp1"), Lookup::Undetermined(_)));
    }

    #[test]
    fn an_unparseable_pending_does_not_become_an_approval() {
        let dir = store_dir("corrupt-pending");
        let s = Store::open(&dir);
        std::fs::create_dir_all(dir.join("pending")).unwrap();
        std::fs::write(dir.join("pending").join("k"), "{not json").unwrap();
        assert!(matches!(s.promote("k"), Determination::Undetermined(_)));
        assert!(!dir.join("approved").exists());
    }
}
