//! Deterministic canon-coverage resolution for `specguard brief`
//! (specs/spec-loop.toml R3-brief-emits-tri-state-verdict).
//!
//! `brief` used to emit only prose written by an agent. Prose cannot be branched
//! on, so `/flow` had no way to ask "does canon already cover what this task
//! touches?" — the third spec-gap trigger of R4 had no signal to read. This
//! module supplies that signal, and it supplies it as **three** answers rather
//! than two.
//!
//! Why three. `not-covered` is not inert: R4 routes it back to `specforge` to
//! draft a spec. So folding "the spec map could not be read" into `not-covered`
//! would turn "I could not look" into "there is nothing there", and the loop
//! would draft a spec for an area that may already be governed — the exact
//! substitution CLAUDE.md §3 forbids and that `crates/blastguard/src/model.rs`
//! names as the defect of binary verdicts ("Three answers, not two."). The
//! carrier is therefore [`Determination<Coverage>`] from `harness-core`, whose
//! `Undetermined` variant cannot be forged outside that crate.
//!
//! Which observations are undetermined (each of these is a state where the
//! store cannot answer, NOT a state where the store says "no canon"):
//!
//!   * the map file does not exist — `specguard map` was never run here;
//!   * the map file cannot be read or parsed (permission, corruption);
//!   * the map exists but holds zero entries — indistinguishable from a store
//!     that was created and never synced. An empty set read as `not-covered`
//!     would divert *every* task, which is CLAUDE.md §3's "empty set is read as
//!     a verdict" in its other direction;
//!   * the task text yields no usable query token, so "nothing matched" was
//!     never actually tested.
//!
//! Entry targeting reuses [`specmap::entry_matches`], the same predicate behind
//! `specguard audit --filter` and `specguard map list --filter`, so a task is
//! matched to entries by one shared rule rather than a third private one.

use std::path::Path;

use harness_core::verdict::Determination;

use crate::specmap::{self, MapEntry, SpecMap};

/// Stable verdict tokens. These are a machine contract — `/flow` branches on
/// them — so they live here as constants rather than as inline string literals
/// at each emit site.
pub const COVERED: &str = "covered";
pub const NOT_COVERED: &str = "not-covered";
pub const UNDETERMINED: &str = "undetermined";

/// Shortest task token used as a spec-map query. One- and two-character
/// fragments ("of", "を", "a") match almost any entry substring, so admitting
/// them would make `covered` trivially true.
const MIN_TOKEN_LEN: usize = 3;

/// What a coverage probe observed when it *was* able to observe something.
///
/// This type deliberately has no third variant for "unknown": that state is
/// carried by [`Determination::Undetermined`] instead, so a caller cannot
/// pattern-match its way past it or default it into one of these two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    /// At least one matching entry names a spec document.
    Covered {
        /// Entry keys that matched AND carry a `spec_doc`, sorted.
        entries: Vec<String>,
    },
    /// The store was read and nothing in it governs this task: either no entry
    /// matched, or the ones that matched have no spec document yet.
    NotCovered {
        /// Entry keys that matched but carry no `spec_doc`, sorted. Empty when
        /// nothing matched at all.
        matched_without_spec: Vec<String>,
    },
}

/// The machine token for a resolved coverage determination.
pub fn verdict_token(d: &Determination<Coverage>) -> &'static str {
    match d {
        Determination::Known(Coverage::Covered { .. }) => COVERED,
        Determination::Known(Coverage::NotCovered { .. }) => NOT_COVERED,
        Determination::Undetermined(_) => UNDETERMINED,
    }
}

/// Split free-form task text into spec-map query tokens.
///
/// Path-ish punctuation (`/ . _ -`) is kept inside a token so `crates/flow` and
/// `spec-map.toml` survive as single queries; everything else separates. Tokens
/// shorter than [`MIN_TOKEN_LEN`] are dropped (see that constant).
pub fn tokens(task: &str) -> Vec<String> {
    let mut out: Vec<String> = task
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-')))
        .map(|t| t.trim_matches(|c| matches!(c, '.' | '-' | '_' | '/')))
        .filter(|t| t.chars().count() >= MIN_TOKEN_LEN)
        .map(|t| t.to_lowercase())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Resolve whether the spec map already governs the area `task` touches.
///
/// Every failure to observe returns `Undetermined` with the reason attached;
/// nothing here falls back to `NotCovered`, because that verdict makes `/flow`
/// draft a new spec (R4).
pub fn resolve(map_path: &Path, task: &str) -> Determination<Coverage> {
    if !map_path.exists() {
        return Determination::undetermined(format!(
            "spec map {} does not exist — `specguard map build` has not run here, \
             so whether canon covers this task was never observed",
            map_path.display()
        ));
    }
    let map: SpecMap = match SpecMap::load(map_path) {
        Ok(m) => m,
        Err(e) => {
            return Determination::undetermined(format!(
                "spec map {} could not be read: {e:#}",
                map_path.display()
            ))
        }
    };
    if map.entries.is_empty() {
        return Determination::undetermined(format!(
            "spec map {} holds zero entries — an unsynced store and a repo with \
             nothing mapped are indistinguishable here",
            map_path.display()
        ));
    }
    let queries = tokens(task);
    if queries.is_empty() {
        return Determination::undetermined(format!(
            "the task text yields no query token of at least {MIN_TOKEN_LEN} \
             characters, so no entry was actually tested against it"
        ));
    }

    let matched: Vec<&MapEntry> = map
        .entries
        .values()
        .filter(|e| queries.iter().any(|q| specmap::entry_matches(e, q)))
        .collect();

    let mut with_spec: Vec<String> = matched
        .iter()
        .filter(|e| e.spec_doc.as_deref().is_some_and(|d| !d.trim().is_empty()))
        .map(|e| e.key.clone())
        .collect();
    if !with_spec.is_empty() {
        with_spec.sort();
        with_spec.dedup();
        return Determination::Known(Coverage::Covered { entries: with_spec });
    }
    let mut without: Vec<String> = matched.iter().map(|e| e.key.clone()).collect();
    without.sort();
    without.dedup();
    Determination::Known(Coverage::NotCovered {
        matched_without_spec: without,
    })
}

/// Render the determination as the markdown block the brief prompt embeds.
///
/// The rendered prompt and the `--json` verdict are produced from the SAME
/// `Determination` value by the same `brief` call (R3 acceptance 3): this
/// function only formats what [`resolve`] already decided, so the prose cannot
/// disagree with the machine field.
pub fn block(d: &Determination<Coverage>) -> String {
    match d {
        Determination::Known(Coverage::Covered { entries }) => format!(
            "判定: **covered** — spec-map に、このタスクに一致し spec ドキュメントを持つ\n\
             エントリがある。以下を正典として実際に Read すること。\n{}",
            bullets(entries)
        ),
        Determination::Known(Coverage::NotCovered {
            matched_without_spec,
        }) => {
            if matched_without_spec.is_empty() {
                "判定: **not-covered** — spec-map にこのタスクへ一致するエントリが無い。\n\
                 この領域を支配する正典は未整備の可能性が高い。着手前の論点として明示すること。\n"
                    .to_string()
            } else {
                format!(
                    "判定: **not-covered** — 一致するエントリはあるが、いずれも spec ドキュメントを\n\
                     持たない。正典が未執筆である。\n{}",
                    bullets(matched_without_spec)
                )
            }
        }
        Determination::Undetermined(why) => format!(
            "判定: **undetermined** — 正典の有無を観測できなかった: {}\n\
             これは「正典が無い」ではない。無いことにして進めず、この事実自体を\n\
             着手前の論点として人間に上げること。\n",
            why.reason().as_str()
        ),
    }
}

fn bullets(keys: &[String]) -> String {
    let mut s = String::new();
    for k in keys {
        s.push_str(&format!("- `{k}`\n"));
    }
    s
}

// ---------------------------------------------------------------------------
// Verification tests for specs/spec-loop.toml R3-brief-emits-tri-state-verdict.
// Written by an agent with no stake in this implementation passing
// (CLAUDE.md §2(a)): every one of these was proved non-vacuous by re-injecting
// the defect it exists to catch and observing it fail.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::verdict::Determination;
    use std::fs;
    use std::path::PathBuf;

    /// One `[entries.<key>]` table as TOML.
    fn entry_toml(key: &str, spec_doc: Option<&str>, impl_files: &[&str]) -> String {
        let mut s = format!(
            "[entries.\"{key}\"]\nkey = \"{key}\"\nkind = \"feature\"\nstatus = \"tracked\"\n"
        );
        if let Some(d) = spec_doc {
            s.push_str(&format!("spec_doc = \"{d}\"\n"));
        }
        s.push_str(&format!("impl_files = {impl_files:?}\n"));
        s
    }

    /// Write a spec map containing `entries` and return its path.
    fn map_with(dir: &std::path::Path, entries: &[String]) -> PathBuf {
        let path = dir.join(".specguard/spec-map.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut body = String::from("last_synced = \"deadbeef\"\n");
        for e in entries {
            body.push_str(e);
        }
        fs::write(&path, body).unwrap();
        path
    }

    /// The reason carried by an `Undetermined`. A `Known` here is itself the
    /// failure, so it is rendered into the returned text: the caller's
    /// `contains(..)` assertion then fails loudly with the offending value
    /// (this crate denies `clippy::panic`, so no bare panic is used).
    fn reason_of(d: &Determination<Coverage>) -> String {
        match d {
            Determination::Undetermined(why) => why.reason().as_str().to_string(),
            other => format!("NOT-UNDETERMINED (test failure): {other:?}"),
        }
    }

    /// `id -u`, only used to make a precondition failure message actionable.
    fn current_uid() -> String {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "?".to_string())
    }

    /// R3 acceptance 2, path 1: the store was never built here. Nothing was
    /// observed, so this may not arrive at /flow as `not-covered` (which makes
    /// it draft a spec for an area canon may already govern).
    #[test]
    fn absent_map_resolves_undetermined_never_not_covered() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".specguard/spec-map.toml");
        assert!(!path.exists());
        let d = resolve(&path, "rework the coverage resolver in specguard");
        assert_eq!(
            verdict_token(&d),
            UNDETERMINED,
            "absent map must be undetermined, got {d:?}"
        );
        assert_ne!(
            verdict_token(&d),
            NOT_COVERED,
            "an unread store must never be reported as 'no canon exists': {d:?}"
        );
        assert!(
            reason_of(&d).contains("does not exist"),
            "reason must name the observation that failed: {}",
            reason_of(&d)
        );
    }

    /// R3 acceptance 2, path 2: the file is there but is not parseable.
    #[test]
    fn corrupt_map_resolves_undetermined_never_not_covered() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".specguard/spec-map.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "this is not TOML [[[ entries = ??\n").unwrap();
        let d = resolve(&path, "rework the coverage resolver in specguard");
        assert_eq!(
            verdict_token(&d),
            UNDETERMINED,
            "unparseable map must be undetermined, got {d:?}"
        );
        assert_ne!(verdict_token(&d), NOT_COVERED, "corruption is not absence");
    }

    /// R3 acceptance 2, path 3: the file exists and parses for `root`, but this
    /// process cannot read it.
    ///
    /// If the environment cannot produce an unreadable file (running as root,
    /// or a filesystem that ignores mode bits) this test PANICS rather than
    /// passing: an unobserved permission path must not be reported as verified.
    #[test]
    fn unreadable_map_resolves_undetermined_never_not_covered() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = map_with(
            tmp.path(),
            &[entry_toml(
                "specguard-brief",
                Some("docs/DESIGN-spec-loop.md"),
                &["crates/specguard/src/coverage.rs"],
            )],
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        // Prove the precondition instead of assuming it.
        assert!(
            fs::read_to_string(&path).is_err(),
            "this environment cannot make a file unreadable (uid {}), so the \
             permission branch of resolve() is UNOBSERVED by this test — it must \
             not be reported as passing",
            current_uid()
        );
        let d = resolve(&path, "rework the coverage resolver in specguard");
        let token = verdict_token(&d);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            token, UNDETERMINED,
            "an unreadable store must be undetermined, got {d:?}"
        );
        assert_ne!(
            token, NOT_COVERED,
            "'permission denied' must not be reported as 'no canon exists'"
        );
    }

    /// R3 acceptance 2, path 4: a store with zero entries is indistinguishable
    /// from a store that was created and never synced.
    #[test]
    fn empty_map_resolves_undetermined_never_not_covered() {
        let tmp = tempfile::tempdir().unwrap();
        let path = map_with(tmp.path(), &[]);
        let d = resolve(&path, "rework the coverage resolver in specguard");
        assert_eq!(
            verdict_token(&d),
            UNDETERMINED,
            "empty store must be undetermined, got {d:?}"
        );
        assert_ne!(
            verdict_token(&d),
            NOT_COVERED,
            "an empty set must not be read as a verdict (CLAUDE.md §3)"
        );
        assert!(
            reason_of(&d).contains("zero entries"),
            "reason: {}",
            reason_of(&d)
        );
    }

    /// R3 acceptance 2, path 5: no entry was ever tested against the task, so
    /// "nothing matched" was never actually observed.
    #[test]
    fn task_without_usable_token_resolves_undetermined_never_not_covered() {
        let tmp = tempfile::tempdir().unwrap();
        let path = map_with(
            tmp.path(),
            &[entry_toml("some-feature", Some("docs/x.md"), &["src/x.rs"])],
        );
        assert!(tokens("a of を !! -").is_empty(), "precondition: no token");
        let d = resolve(&path, "a of を !! -");
        assert_eq!(
            verdict_token(&d),
            UNDETERMINED,
            "untested task text must be undetermined, got {d:?}"
        );
        assert_ne!(verdict_token(&d), NOT_COVERED);
        assert!(
            reason_of(&d).contains("no query token"),
            "reason: {}",
            reason_of(&d)
        );
    }

    /// The `covered` arm is reachable, and it reports which entries carried the
    /// canon (so a caller can check the claim).
    #[test]
    fn matching_entry_with_spec_doc_resolves_covered() {
        let tmp = tempfile::tempdir().unwrap();
        let path = map_with(
            tmp.path(),
            &[entry_toml(
                "specguard-brief",
                Some("docs/DESIGN-spec-loop.md"),
                &["crates/specguard/src/coverage.rs"],
            )],
        );
        let d = resolve(&path, "make coverage.rs emit a tri-state verdict");
        assert_eq!(verdict_token(&d), COVERED, "got {d:?}");
        assert_eq!(
            d,
            Determination::Known(Coverage::Covered {
                entries: vec!["specguard-brief".to_string()]
            }),
            "covered must name the entry that carried the canon"
        );
    }

    /// The `not-covered` arm is reachable in BOTH its shapes, and it is only
    /// reached after the store was actually read and queried.
    #[test]
    fn read_store_resolves_not_covered_in_both_shapes() {
        let tmp = tempfile::tempdir().unwrap();
        // (a) an entry matches but has no spec doc yet.
        let path = map_with(
            tmp.path(),
            &[entry_toml(
                "specguard-brief",
                None,
                &["crates/specguard/src/coverage.rs"],
            )],
        );
        let d = resolve(&path, "make coverage.rs emit a tri-state verdict");
        assert_eq!(verdict_token(&d), NOT_COVERED, "got {d:?}");
        assert_eq!(
            d,
            Determination::Known(Coverage::NotCovered {
                matched_without_spec: vec!["specguard-brief".to_string()]
            }),
            "a matched-but-unspecced entry must be named"
        );

        // (b) nothing in a populated store matches the task at all.
        let d = resolve(&path, "rewrite the kubernetes ingress controller");
        assert_eq!(verdict_token(&d), NOT_COVERED, "got {d:?}");
        assert_eq!(
            d,
            Determination::Known(Coverage::NotCovered {
                matched_without_spec: vec![]
            }),
            "nothing matched, and that was OBSERVED (the store was read)"
        );
    }

    /// A `spec_doc` that is present but blank is not canon. It must fall to
    /// `not-covered` (an observed absence), not to `covered`.
    #[test]
    fn blank_spec_doc_is_not_coverage() {
        let tmp = tempfile::tempdir().unwrap();
        let path = map_with(
            tmp.path(),
            &[entry_toml(
                "specguard-brief",
                Some("   "),
                &["crates/specguard/src/coverage.rs"],
            )],
        );
        let d = resolve(&path, "make coverage.rs emit a tri-state verdict");
        assert_eq!(
            verdict_token(&d),
            NOT_COVERED,
            "a whitespace spec_doc is not a spec: {d:?}"
        );
    }

    /// Tokenization: path-ish fragments survive whole, sub-`MIN_TOKEN_LEN`
    /// noise is dropped, output is lowercased/sorted/deduped.
    #[test]
    fn tokens_keeps_paths_and_drops_short_noise() {
        let t = tokens("Fix crates/flow/SKILL.md — a of を, FLOW; flow.");
        assert!(t.contains(&"crates/flow/skill.md".to_string()), "{t:?}");
        assert!(t.contains(&"flow".to_string()), "{t:?}");
        assert_eq!(
            t.iter().filter(|x| *x == "flow").count(),
            1,
            "deduped: {t:?}"
        );
        assert!(!t.iter().any(|x| x.chars().count() < 3), "{t:?}");
        let mut sorted = t.clone();
        sorted.sort();
        assert_eq!(t, sorted, "tokens must be sorted for determinism");
    }

    /// The three tokens are distinct and each is reachable through
    /// `verdict_token`. This is the machine contract /flow branches on.
    #[test]
    fn verdict_token_is_three_valued_and_distinct() {
        let covered = Determination::Known(Coverage::Covered {
            entries: vec!["e".into()],
        });
        let not_covered = Determination::Known(Coverage::NotCovered {
            matched_without_spec: vec![],
        });
        let undet: Determination<Coverage> = Determination::undetermined("could not look");
        assert_eq!(verdict_token(&covered), COVERED);
        assert_eq!(verdict_token(&not_covered), NOT_COVERED);
        assert_eq!(verdict_token(&undet), UNDETERMINED);
        let all = [COVERED, NOT_COVERED, UNDETERMINED];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "verdict tokens must be distinct");
            }
        }
    }

    /// The prose block is a rendering of the SAME determination the machine
    /// token comes from: for every state, the block states that state's token
    /// and states no other state's token (R3 acceptance 3, unit half).
    #[test]
    fn block_states_exactly_the_token_of_its_determination() {
        let cases: Vec<(Determination<Coverage>, &str)> = vec![
            (
                Determination::Known(Coverage::Covered {
                    entries: vec!["ent-A".into()],
                }),
                COVERED,
            ),
            (
                Determination::Known(Coverage::NotCovered {
                    matched_without_spec: vec!["ent-B".into()],
                }),
                NOT_COVERED,
            ),
            (
                Determination::Known(Coverage::NotCovered {
                    matched_without_spec: vec![],
                }),
                NOT_COVERED,
            ),
            (
                Determination::undetermined("spec map could not be read: EACCES"),
                UNDETERMINED,
            ),
        ];
        for (d, token) in &cases {
            let b = block(d);
            assert!(
                b.contains(&format!("**{token}**")),
                "block for {token} must state it: {b}"
            );
            for other in [COVERED, NOT_COVERED, UNDETERMINED] {
                if other == *token {
                    continue;
                }
                assert!(
                    !b.contains(&format!("**{other}**")),
                    "block for {token} must not also state {other}: {b}"
                );
            }
        }
        // The undetermined block carries the reason and explicitly refuses the
        // "no canon" reading.
        let b = block(&Determination::undetermined("EACCES on the store"));
        assert!(b.contains("EACCES on the store"), "reason must travel: {b}");
        // Entry keys travel into the prose, so a reader can check the claim.
        assert!(block(&Determination::Known(Coverage::Covered {
            entries: vec!["ent-A".into()]
        }))
        .contains("ent-A"));
        assert!(block(&Determination::Known(Coverage::NotCovered {
            matched_without_spec: vec!["ent-B".into()]
        }))
        .contains("ent-B"));
    }
}
