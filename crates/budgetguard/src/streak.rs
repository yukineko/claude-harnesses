//! Retry bound for the unparseable-stdin block (user ruling 2026-10-19:
//! 「retry3回でぬけだす」).
//!
//! A Stop hook whose stdin does not parse has no `stop_hook_active` to bound a
//! block with, so a persistently malformed payload would block every stop. The
//! streak of consecutive unparseable stops is kept in a marker file
//! `<state_dir>/unparseable-streak/<key>`, keyed by `CLAUDE_CODE_SESSION_ID`
//! (one global key when it is unset). The first [`MAX_BLOCKS`] consecutive
//! unparseable stops block; the next one is allowed loudly and the streak
//! starts over. A parsed stop clears it. A marker that cannot be read or
//! written allows loudly (the same rule as `run_guarded`'s marker-IO case):
//! without the count the block could not be bounded.

use std::path::{Path, PathBuf};

/// Consecutive unparseable stops that block before the gate gives up once.
pub const MAX_BLOCKS: u32 = 3;

/// What to do with this unparseable stop.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Block; this is consecutive unparseable stop number `n` (1-based).
    Block(u32),
    /// The streak reached [`MAX_BLOCKS`]: allow this one loudly.
    GiveUp,
    /// The marker could not be read or written: allow loudly with the reason.
    MarkerUnusable(String),
}

fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join("unparseable-streak")
}

/// The marker key: the session id reduced to filename-safe characters, or
/// `global` when there is none.
fn key(session: Option<&str>) -> String {
    let cleaned: String = session
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if cleaned.is_empty() {
        "global".to_string()
    } else {
        cleaned
    }
}

fn marker(state_dir: &Path, session: Option<&str>) -> PathBuf {
    dir(state_dir).join(key(session))
}

/// Record one more unparseable stop and decide what to do with it.
pub fn on_unparseable(state_dir: &Path, session: Option<&str>) -> Action {
    let path = marker(state_dir, session);
    let prev = match std::fs::read_to_string(&path) {
        Ok(s) => match s.trim().parse::<u32>() {
            Ok(n) => n,
            Err(e) => {
                return Action::MarkerUnusable(format!(
                    "streak marker {} holds {s:?}, not a count: {e}",
                    path.display()
                ))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => {
            return Action::MarkerUnusable(format!(
                "streak marker {} could not be read: {e}",
                path.display()
            ))
        }
    };
    if prev >= MAX_BLOCKS {
        // Start over: the next unparseable stop blocks again.
        return match std::fs::remove_file(&path) {
            Ok(()) => Action::GiveUp,
            Err(e) => Action::MarkerUnusable(format!(
                "streak marker {} could not be reset: {e}",
                path.display()
            )),
        };
    }
    let n = prev + 1;
    let written =
        std::fs::create_dir_all(dir(state_dir)).and_then(|()| std::fs::write(&path, n.to_string()));
    match written {
        Ok(()) => Action::Block(n),
        Err(e) => Action::MarkerUnusable(format!(
            "streak marker {} could not be written: {e}",
            path.display()
        )),
    }
}

/// A stop parsed: clear this key's streak. A marker that is already absent is
/// the goal state. A failure to remove it only leaves a stale count, which can
/// make the next unparseable stop give up sooner; it is reported on stderr.
pub fn clear(state_dir: &Path, session: Option<&str>) {
    let path = marker(state_dir, session);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!(
            "budgetguard: could not clear the unparseable-stdin streak marker {}: {e}",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_blocks_then_give_up_then_start_over() {
        let d = tempfile::tempdir().unwrap();
        for n in 1..=MAX_BLOCKS {
            assert_eq!(on_unparseable(d.path(), Some("s")), Action::Block(n));
        }
        assert_eq!(on_unparseable(d.path(), Some("s")), Action::GiveUp);
        assert_eq!(on_unparseable(d.path(), Some("s")), Action::Block(1));
    }

    #[test]
    fn clear_resets_and_tolerates_absence() {
        let d = tempfile::tempdir().unwrap();
        clear(d.path(), None);
        assert_eq!(on_unparseable(d.path(), None), Action::Block(1));
        clear(d.path(), None);
        assert_eq!(on_unparseable(d.path(), None), Action::Block(1));
    }

    #[test]
    fn a_garbled_marker_is_unusable_not_zero() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir(d.path())).unwrap();
        std::fs::write(marker(d.path(), Some("s")), "garbage").unwrap();
        assert!(matches!(
            on_unparseable(d.path(), Some("s")),
            Action::MarkerUnusable(_)
        ));
    }

    #[test]
    fn key_sanitizes_and_defaults_to_global() {
        assert_eq!(key(None), "global");
        assert_eq!(key(Some("../..")), "global");
        assert_eq!(key(Some("ab-12_x/y")), "ab-12_xy");
    }
}
