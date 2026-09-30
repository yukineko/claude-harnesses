//! `specguard map gate-check --base <rev>` — the push-time spec-doc gate for the
//! blocking gate crates (backlog 0c277117).
//!
//! The spec-map store (`specmap.rs`) records, per feature entry, which source
//! files implement it and which authored spec-doc (`docs/specs/*.md`) describes
//! it. Before this check nothing ever *consumed* `spec_doc`: it was a label no
//! verdict read, so a gate crate could change for hundreds of commits while its
//! entries all said `spec_doc = null` and no hook said a word.
//!
//! The check, over `<base>..<head>` (default head `HEAD`):
//!
//! * a changed path under `crates/<gate>/` for a gate in
//!   [`harness_core::fleet::BLOCKING_GATES`] that matches `[map].exclude` is not
//!   spec-bearing (manifests, docs, …) and is skipped;
//! * every other changed gate path listed in some entry's `impl_files` triggers
//!   that entry. A triggered entry passes only with a non-empty `spec_doc` or a
//!   reasoned ack (`.specguard/spec-doc-acks.toml`, `[[ack]] path/reason`, where
//!   `path` names the entry key or one of its impl files and `reason` is not
//!   blank). Otherwise it is a **violation** (exit 1) naming the impl path;
//! * a changed, non-deleted gate path that NO entry references (in impl, test or
//!   client lists) is **undetermined** (exit 2): `map sync` attributes every
//!   non-excluded path to an entry, so an unreferenced one means the map has not
//!   observed this file and its spec status is unknown — never "no spec needed".
//!   The remedy is `specguard map sync`, after which the file is an entry and
//!   the rules above apply;
//! * a deleted gate path is judged only through the entry that still lists it.
//!   `map sync` detaches deleted paths, so an unreferenced deleted path is
//!   skipped: there is no file left to write a spec for, and treating it as
//!   unreferenced would block with no available remedy.
//!
//! Undetermined — unparseable map or ack file, an unsafe/unresolvable rev, a git
//! call that failed or exited non-zero, unparseable git output, or an absent map
//! while a gate path changed — is exit 2, never 0 (CLAUDE.md §3). An absent ack
//! file is an empty ack set (the restrictive reading: no escapes).

use crate::scope::is_safe_ref;
use crate::specmap::{MapEntry, SpecMap};
use globset::GlobSet;
use harness_core::boundary;
use harness_core::verdict::{Determination, Reason, Required, Verdict};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Repo-root-relative path of the per-entry ack file.
pub const ACK_PATH: &str = ".specguard/spec-doc-acks.toml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AckFile {
    #[serde(default)]
    ack: Vec<Ack>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ack {
    path: String,
    /// Optional in the schema so a missing reason parses — and then does NOT
    /// count as an ack (the entry stays a violation rather than exit 2).
    #[serde(default)]
    reason: Option<String>,
}

/// One changed path from `git diff --name-status`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Changed {
    path: String,
    deleted: bool,
}

/// Inputs for [`check`].
pub struct GateCheck<'a> {
    pub repo_root: &'a Path,
    pub map_path: &'a Path,
    pub ack_path: &'a Path,
    pub base: &'a str,
    pub head: &'a str,
    pub exclude: &'a GlobSet,
    pub gates: &'a [&'a str],
}

/// Run the check. `Clean` → exit 0, `Violation` → 1, `Undetermined` → 2.
pub fn check(gc: &GateCheck<'_>) -> Verdict {
    let map = match read_map(gc.map_path).require() {
        Required::Determined(m) => m,
        Required::Blocked(v) => return v.into_verdict(),
    };
    let acked = match read_acks(gc.ack_path).require() {
        Required::Determined(a) => a,
        Required::Blocked(v) => return v.into_verdict(),
    };
    let changed = match changed_paths(gc.repo_root, gc.base, gc.head).require() {
        Required::Determined(c) => c,
        Required::Blocked(v) => return v.into_verdict(),
    };
    let gate_changed: Vec<Changed> = changed
        .into_iter()
        .filter(|c| is_gate_path(&c.path, gc.gates) && !gc.exclude.is_match(&c.path))
        .collect();
    if gate_changed.is_empty() {
        return Verdict::from_findings(vec![]);
    }
    let Some(map) = map else {
        return Verdict::undetermined(format!(
            "{} gate path(s) changed in {}..{} (first: {}) but the spec map {} is absent, \
             so their spec-doc status cannot be observed — run `specguard map build`",
            gate_changed.len(),
            gc.base,
            gc.head,
            gate_changed[0].path,
            gc.map_path.display()
        ));
    };
    evaluate(&map, &acked, &gate_changed)
}

/// Pure judgement over an already-loaded map/ack set and the changed gate paths.
fn evaluate(map: &SpecMap, acked: &BTreeSet<String>, gate_changed: &[Changed]) -> Verdict {
    let mut unreferenced = Vec::new();
    for c in gate_changed {
        if c.deleted {
            continue;
        }
        let referenced = map.entries.values().any(|e| {
            e.impl_files.contains(&c.path)
                || e.test_files.contains(&c.path)
                || e.client_refs.contains(&c.path)
        });
        if !referenced {
            unreferenced.push(c.path.clone());
        }
    }

    let mut missing = Vec::new();
    for (key, entry) in &map.entries {
        let hit: Vec<&str> = gate_changed
            .iter()
            .filter(|c| entry.impl_files.contains(&c.path))
            .map(|c| c.path.as_str())
            .collect();
        if hit.is_empty() || has_spec_doc(entry) || is_acked(key, entry, acked) {
            continue;
        }
        for p in hit {
            missing.push(format!(
                "{p} (entry '{key}'): no spec_doc and no reasoned ack in {ACK_PATH}"
            ));
        }
    }

    if !unreferenced.is_empty() {
        let mut msg = format!(
            "{} changed gate path(s) are referenced by no spec-map entry, so their spec-doc \
             status cannot be determined — run `specguard map sync`, then `map set-spec` or \
             ack each resulting entry:\n  {}",
            unreferenced.len(),
            unreferenced.join("\n  ")
        );
        if !missing.is_empty() {
            msg.push_str(&format!(
                "\nand {} gate entr{} lack a spec doc:\n  {}",
                missing.len(),
                if missing.len() == 1 { "y" } else { "ies" },
                missing.join("\n  ")
            ));
        }
        return Verdict::undetermined(msg);
    }
    if missing.is_empty() {
        // Every triggered entry ran through the checks above: observed clean.
        return Verdict::from_findings(vec![]);
    }
    // One reason, one line per offending path (Reason::joined would use "; ").
    Verdict::Violation(Reason::new(missing.join("\n  ")))
}

fn has_spec_doc(e: &MapEntry) -> bool {
    e.spec_doc.as_deref().is_some_and(|d| !d.trim().is_empty())
}

fn is_acked(key: &str, e: &MapEntry, acked: &BTreeSet<String>) -> bool {
    acked.contains(key) || e.impl_files.iter().any(|p| acked.contains(p))
}

fn is_gate_path(path: &str, gates: &[&str]) -> bool {
    let mut parts = path.split('/');
    parts.next() == Some("crates")
        && parts.next().is_some_and(|c| gates.contains(&c))
        && parts.next().is_some()
}

/// `Known(None)` = absent map; a present but unparseable map is undetermined.
fn read_map(path: &Path) -> Determination<Option<SpecMap>> {
    match boundary::read_to_string(path).require() {
        Required::Determined(None) => Determination::known(None),
        Required::Determined(Some(text)) => match toml::from_str::<SpecMap>(&text) {
            Ok(m) => Determination::known(Some(m)),
            Err(e) => Determination::undetermined(format!(
                "cannot parse spec map {}: {e}",
                path.display()
            )),
        },
        Required::Blocked(v) => v.into_determination(),
    }
}

/// The set of paths with a reasoned ack. Absent file → empty set.
fn read_acks(path: &Path) -> Determination<BTreeSet<String>> {
    match boundary::read_to_string(path).require() {
        Required::Determined(None) => Determination::known(BTreeSet::new()),
        Required::Determined(Some(text)) => match toml::from_str::<AckFile>(&text) {
            Ok(f) => Determination::known(
                f.ack
                    .into_iter()
                    .filter(|a| a.reason.as_deref().is_some_and(|r| !r.trim().is_empty()))
                    .map(|a| a.path)
                    .collect(),
            ),
            Err(e) => Determination::undetermined(format!(
                "cannot parse ack file {}: {e}",
                path.display()
            )),
        },
        Required::Blocked(v) => v.into_determination(),
    }
}

fn git(repo_root: &Path, args: &[&str]) -> Determination<String> {
    match boundary::run(Command::new("git").arg("-C").arg(repo_root).args(args)).require() {
        Required::Determined(out) => out.stdout_on_success(),
        Required::Blocked(v) => v.into_determination(),
    }
}

fn resolve_rev(repo_root: &Path, rev: &str) -> Determination<String> {
    if !is_safe_ref(rev) {
        return Determination::undetermined(format!("refusing unsafe revision {rev:?}"));
    }
    let spec = format!("{}^{{commit}}", rev.trim());
    match git(repo_root, &["rev-parse", "--verify", "--quiet", &spec]).require() {
        Required::Determined(sha) if !sha.trim().is_empty() => {
            Determination::known(sha.trim().to_string())
        }
        Required::Determined(_) => {
            Determination::undetermined(format!("git rev-parse printed nothing for {rev:?}"))
        }
        Required::Blocked(v) => v.into_determination(),
    }
}

fn changed_paths(repo_root: &Path, base: &str, head: &str) -> Determination<Vec<Changed>> {
    let b = match resolve_rev(repo_root, base).require() {
        Required::Determined(s) => s,
        Required::Blocked(v) => return v.into_determination(),
    };
    let h = match resolve_rev(repo_root, head).require() {
        Required::Determined(s) => s,
        Required::Blocked(v) => return v.into_determination(),
    };
    match git(
        repo_root,
        &["diff", "--name-status", "--no-renames", "-z", &b, &h],
    )
    .require()
    {
        Required::Determined(out) => parse_name_status_z(&out),
        Required::Blocked(v) => v.into_determination(),
    }
}

/// Parse `git diff --name-status --no-renames -z`: `STATUS\0PATH\0` pairs. Any
/// shape it does not recognise is undetermined, not skipped.
fn parse_name_status_z(out: &str) -> Determination<Vec<Changed>> {
    let mut toks: Vec<&str> = out.split('\0').collect();
    if toks.last() == Some(&"") {
        toks.pop();
    }
    if toks.len() % 2 != 0 {
        return Determination::undetermined(format!(
            "unparseable `git diff --name-status -z` output ({} tokens)",
            toks.len()
        ));
    }
    let mut v = Vec::new();
    for pair in toks.chunks(2) {
        let (status, path) = (pair[0], pair[1]);
        let deleted = match status {
            "D" => true,
            "A" | "M" | "T" => false,
            other => {
                return Determination::undetermined(format!(
                    "unexpected git diff status {other:?} for {path:?}"
                ))
            }
        };
        v.push(Changed {
            path: path.to_string(),
            deleted,
        });
    }
    Determination::known(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, spec: Option<&str>, impls: &[&str], tests: &[&str]) -> MapEntry {
        let list = |xs: &[&str]| {
            xs.iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(",")
        };
        let sd = spec
            .map(|d| format!("spec_doc = \"{d}\"\n"))
            .unwrap_or_default();
        toml::from_str(&format!(
            "key = \"{key}\"\n{sd}status = \"changed\"\nimpl_files = [{}]\ntest_files = [{}]\n",
            list(impls),
            list(tests)
        ))
        .unwrap()
    }

    fn map(es: Vec<MapEntry>) -> SpecMap {
        SpecMap {
            entries: es.into_iter().map(|e| (e.key.clone(), e)).collect(),
            ..SpecMap::default()
        }
    }

    fn ch(p: &str, deleted: bool) -> Changed {
        Changed {
            path: p.into(),
            deleted,
        }
    }

    #[test]
    fn parse_name_status_z_reads_pairs_and_rejects_odd_shapes() {
        let d = parse_name_status_z("M\0a.rs\0D\0b.rs\0");
        assert_eq!(
            d,
            Determination::known(vec![ch("a.rs", false), ch("b.rs", true)])
        );
        assert!(matches!(
            parse_name_status_z("M\0a.rs\0D\0"),
            Determination::Undetermined(_)
        ));
        assert!(matches!(
            parse_name_status_z("R100\0a.rs\0"),
            Determination::Undetermined(_)
        ));
        assert_eq!(parse_name_status_z(""), Determination::known(vec![]));
    }

    #[test]
    fn is_gate_path_needs_a_file_under_a_gate_crate() {
        let g = ["blastguard"];
        assert!(is_gate_path("crates/blastguard/src/a.rs", &g));
        assert!(!is_gate_path("crates/blastguard", &g));
        assert!(!is_gate_path("crates/blastguardx/src/a.rs", &g));
        assert!(!is_gate_path("x/crates/blastguard/a.rs", &g));
    }

    #[test]
    fn test_only_reference_passes_but_counts_as_referenced() {
        let m = map(vec![entry("e", None, &[], &["crates/g/tests/t.rs"])]);
        let v = evaluate(&m, &BTreeSet::new(), &[ch("crates/g/tests/t.rs", false)]);
        assert!(!v.blocks());
    }

    #[test]
    fn deleted_path_triggers_the_entry_still_listing_it() {
        let m = map(vec![entry("e", None, &["crates/g/src/a.rs"], &[])]);
        let v = evaluate(&m, &BTreeSet::new(), &[ch("crates/g/src/a.rs", true)]);
        assert!(matches!(v, Verdict::Violation(_)));
    }

    #[test]
    fn blank_spec_doc_is_not_a_spec_doc() {
        let m = map(vec![entry("e", Some("  "), &["crates/g/src/a.rs"], &[])]);
        let v = evaluate(&m, &BTreeSet::new(), &[ch("crates/g/src/a.rs", false)]);
        assert!(matches!(v, Verdict::Violation(_)));
    }
}
