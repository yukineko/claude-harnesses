//! Naming of the files the Stop gate excluded as "not attributed to this
//! session" (backlog 9e565d01). The exclusion set is decided by
//! `gate::scope_to_session`; this module only decides the WORD for each file:
//! a file with a working-tree change is "uncommitted"; a file known only from
//! the commits `gate::include_committed` folded in (a peer's commit in the
//! session window) is "committed". Pure — no git, no IO.

use std::collections::BTreeSet;
use std::ops::Deref;

use crate::gate::Authorship;
use crate::git::{AddedScan, ChangeScan};
use crate::moved;

/// The excluded file names (`Deref`s to them) plus the subset that has no
/// working-tree change, i.e. is known only from committed history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excluded {
    names: Vec<String>,
    committed_only: Vec<String>,
}

impl Deref for Excluded {
    type Target = Vec<String>;
    fn deref(&self) -> &Vec<String> {
        &self.names
    }
}

/// The files with a working-tree change, taken BEFORE committed history is
/// folded into the scan: changed files plus the files of added/removed lines.
pub fn working_files(
    changed: &ChangeScan,
    added: &AddedScan,
    removed: &AddedScan,
) -> BTreeSet<String> {
    let mut working = BTreeSet::new();
    if let ChangeScan::Files(fs) = changed {
        working.extend(fs.iter().cloned());
    }
    if let AddedScan::Lines(ls) = added {
        working.extend(ls.iter().map(|l| l.file.clone()));
    }
    working.extend(moved::removed_lines(removed).iter().map(|l| l.file.clone()));
    working
}

/// An `Attributed` authorship with no committed-only files recorded yet.
pub fn attributed(names: Vec<String>) -> Authorship {
    Authorship::Attributed {
        excluded: Excluded {
            names,
            committed_only: Vec::new(),
        },
    }
}

/// Record, for an `Attributed` authorship, which excluded files have no
/// working-tree change (`working`). Any other authorship passes through.
pub fn mark_committed(authorship: Authorship, working: &BTreeSet<String>) -> Authorship {
    match authorship {
        Authorship::Attributed { excluded } => {
            let committed_only = excluded
                .names
                .iter()
                .filter(|f| !working.contains(*f))
                .cloned()
                .collect();
            Authorship::Attributed {
                excluded: Excluded {
                    committed_only,
                    ..excluded
                },
            }
        }
        other => other,
    }
}

/// "N committed file(s)", "M uncommitted file(s)", or both joined with "and".
/// Committed-only excluded files are never called uncommitted. Empty string
/// for an `Unverified` authorship (nothing was excluded by attribution).
pub fn counts(authorship: &Authorship) -> String {
    let Authorship::Attributed { excluded } = authorship else {
        return String::new();
    };
    let committed = excluded
        .names
        .iter()
        .filter(|f| excluded.committed_only.contains(f))
        .count();
    let uncommitted = excluded.names.len() - committed;
    match (committed, uncommitted) {
        (0, u) => format!("{u} uncommitted file(s)"),
        (c, 0) => format!("{c} committed file(s)"),
        (c, u) => format!("{c} committed file(s) and {u} uncommitted file(s)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn names_committed_and_uncommitted_separately() {
        let working: BTreeSet<String> = s(&["wt.rs"]).into_iter().collect();
        let a = mark_committed(attributed(s(&["peer.rs", "wt.rs", "p2.rs"])), &working);
        assert_eq!(counts(&a), "2 committed file(s) and 1 uncommitted file(s)");
        let only_c = mark_committed(attributed(s(&["peer.rs"])), &working);
        assert_eq!(counts(&only_c), "1 committed file(s)");
        let only_u = mark_committed(attributed(s(&["wt.rs"])), &working);
        assert_eq!(counts(&only_u), "1 uncommitted file(s)");
    }
}
