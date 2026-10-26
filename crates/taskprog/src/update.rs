//! Stop hook: seed `.claude/progress.md` deterministically when it is missing
//! or empty. The hook prints NOTHING (no `systemMessage`, no context) — by user
//! ruling ("progressはいらない" / "何も表示しないようにして"), which reverses the
//! Stop `systemMessage` notice of 914ed765. The deterministic skeleton write is
//! the only effect, and it is what guarantees the file exists.
//!
//! This hook carries no verdict: nothing downstream reads its output (there is
//! none), and its return value is consumed only by tests.

use std::path::{Path, PathBuf};

use anyhow::Result;
use harness_core::hook::HookInput;
use harness_core::verdict::Determination;

use crate::config::Config;

/// What [`on_stop`] actually did.
///
/// This exists so that "could not tell where the progress file belongs" is
/// **distinguishable by the caller** from "wrote one" / "kept one", even though
/// the hook itself prints none of them (user ruling: hooks print nothing). The
/// third variant is the one that used to not exist — the old code answered an
/// unanchorable cwd by writing into it, which is the defect it guards against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    /// No real content existed, so the deterministic skeleton was seeded here.
    Seeded(PathBuf),
    /// A non-empty progress file already existed here and was left untouched.
    Existing(PathBuf),
    /// The project root could not be determined. **Nothing was written**, by
    /// design — carries why, so a caller can tell it apart from success.
    NotAnchored(String),
}

/// Run at Stop. Prints nothing. If no non-empty progress file exists yet, seed
/// the deterministic skeleton; an existing file is left untouched.
///
/// A failed skeleton write is returned as `Err` rather than reported as
/// [`StopOutcome::Seeded`].
///
/// When the progress file cannot be anchored to a project root, this writes
/// nothing at all and returns [`StopOutcome::NotAnchored`]. It deliberately does
/// **not** fall back to the cwd: doing so is what scattered `.claude/` skeletons
/// through the source tree (see [`Config::resolve_progress_path`]).
pub fn on_stop(input: &HookInput, cfg: &Config) -> Result<StopOutcome> {
    let cwd = if input.cwd.is_empty() {
        "."
    } else {
        &input.cwd
    };

    let path = match cfg.resolve_progress_path(cwd) {
        Determination::Known(p) => p,
        // Write nothing anywhere, and (by user ruling) print nothing either.
        Determination::Undetermined(why) => {
            return Ok(StopOutcome::NotAnchored(why.as_str().to_string()));
        }
    };

    // Does a *non-empty* progress file already exist? We only treat a file with
    // real content as "present" — an empty/whitespace-only file is as good as
    // missing and should be (re)seeded with the deterministic skeleton below.
    if crate::progress::read_file(&path).is_some() {
        return Ok(StopOutcome::Existing(path));
    }

    // Deterministic, LLM-independent write: seed a minimal skeleton so the file
    // is never left missing. An IO error is returned, not reported as `Seeded`.
    let skeleton = build_skeleton(input, &path);
    write_progress(&path, &skeleton)?;
    Ok(StopOutcome::Seeded(path))
}

/// The project's display name for the skeleton header, taken from the directory
/// the progress file is anchored in (`<root>/.claude/progress.md` → `<root>`).
///
/// It is deliberately NOT `HookInput::project_name()`, which is the basename of
/// the cwd: with a subagent running in `crates/blastguard/src` that produced the
/// header `# Progress — src`, naming the incidental cwd instead of the project.
/// The observed stray file carried exactly that header.
fn project_label(input: &HookInput, path: &Path) -> String {
    path.parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| input.project_name())
}

/// Build a minimal, deterministic progress-file skeleton from whatever session
/// context the HookInput carries. Kept intentionally small and robust: no
/// external deps, no timestamps that would make output nondeterministic — just
/// the standard sections a handoff needs, prefilled with a session breadcrumb.
fn build_skeleton(input: &HookInput, path: &Path) -> String {
    let project = project_label(input, path);
    let session = if input.session_id.is_empty() {
        "(unknown session)".to_string()
    } else {
        input.session_id.clone()
    };
    format!(
        "# Progress — {project}\n\
         \n\
         _Auto-seeded skeleton (taskprog Stop hook). Session: {session}._\n\
         _Replace the placeholders below with concrete detail._\n\
         \n\
         ## Done\n\
         - (nothing recorded yet)\n\
         \n\
         ## In progress / remaining\n\
         - (nothing recorded yet)\n\
         \n\
         ## Blockers\n\
         - (none recorded)\n"
    )
}

/// Actually write the progress file (used by `taskprog write` command).
pub fn write_progress(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A temp directory that **is** a project root.
    ///
    /// The `.git` marker is new. Before progress files were anchored, any
    /// directory was implicitly its own root, so a bare temp dir was a valid
    /// fixture. Now a root has to be identifiable, and a directory with no
    /// marker is legitimately `NotAnchored` — which is the behaviour
    /// `on_stop_writes_nothing_when_the_root_is_undeterminable` pins. The two
    /// tests below are about *seeding* and *preservation*, so they need a real
    /// root; giving them one keeps them testing what they always tested rather
    /// than silently becoming tests of the unanchored path.
    fn temp_project_root() -> std::path::PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let id = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("taskprog-ut-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    fn progress_path(cfg: &Config, cwd: &str) -> std::path::PathBuf {
        match cfg.resolve_progress_path(cwd) {
            Determination::Known(p) => p,
            Determination::Undetermined(why) => panic!("fixture must be anchorable: {why}"),
        }
    }

    /// on_stop must deterministically seed a non-empty `.claude/progress.md`
    /// even when no file exists yet and the LLM does nothing.
    #[test]
    fn on_stop_writes_skeleton_when_missing() {
        let dir = temp_project_root();
        let input = HookInput {
            cwd: dir.to_string_lossy().into_owned(),
            session_id: "sess-abc".to_string(),
            ..Default::default()
        };
        let cfg = Config::default();
        let path = progress_path(&cfg, &input.cwd);
        assert!(!path.exists(), "precondition: file absent");

        let outcome = on_stop(&input, &cfg).unwrap();
        assert_eq!(outcome, StopOutcome::Seeded(path.clone()));

        let written = std::fs::read_to_string(&path).expect("progress.md must exist after Stop");
        assert!(!written.trim().is_empty(), "skeleton must be non-empty");
        assert!(
            written.contains("## Done"),
            "skeleton has expected sections"
        );
        assert!(
            written.contains("sess-abc"),
            "skeleton carries the session breadcrumb"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// on_stop must NOT clobber a pre-existing, model-authored progress file.
    #[test]
    fn on_stop_preserves_existing_content() {
        let dir = temp_project_root();
        let input = HookInput {
            cwd: dir.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let cfg = Config::default();
        let path = progress_path(&cfg, &input.cwd);
        write_progress(&path, "# Real notes\n\n- shipped feature X\n").unwrap();

        let outcome = on_stop(&input, &cfg).unwrap();
        assert_eq!(outcome, StopOutcome::Existing(path.clone()));

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("shipped feature X"),
            "existing content preserved, got: {after}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The regression. A subagent whose cwd is a source subdirectory must seed
    /// the file at the ROOT and must not create a `.claude/` beside its cwd —
    /// the observed artefact was `crates/blastguard/src/.claude/progress.md`.
    #[test]
    fn on_stop_from_a_subdirectory_writes_only_at_the_project_root() {
        let root = temp_project_root();
        let sub = root.join("crates").join("blastguard").join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let input = HookInput {
            cwd: sub.to_string_lossy().into_owned(),
            session_id: "809492a4".to_string(),
            ..Default::default()
        };
        let cfg = Config::default();

        let outcome = on_stop(&input, &cfg).unwrap();

        let at_root = root
            .canonicalize()
            .unwrap()
            .join(".claude")
            .join("progress.md");
        assert_eq!(outcome, StopOutcome::Seeded(at_root.clone()));
        assert!(at_root.exists(), "the root's progress file was seeded");
        assert!(
            !sub.join(".claude").exists(),
            "must NOT create .claude beside the subdirectory cwd"
        );

        // And the header names the project, not the cwd's basename ("src").
        let written = std::fs::read_to_string(&at_root).unwrap();
        let label = root.canonicalize().unwrap();
        let label = label.file_name().unwrap().to_string_lossy();
        assert!(
            written.contains(&format!("# Progress — {label}")),
            "header must name the project root, got: {written}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// CLAUDE.md §3. With no determinable root, on_stop writes nothing anywhere
    /// and reports why — it does not quietly fall back to the cwd.
    #[test]
    fn on_stop_writes_nothing_when_the_root_is_undeterminable() {
        static N: AtomicU32 = AtomicU32::new(0);
        let id = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("taskprog-unanchored-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            !Path::new("/tmp/.git").exists() && !Path::new("/.git").exists(),
            "precondition: no stray .git above the temp dir"
        );
        let input = HookInput {
            cwd: dir.to_string_lossy().into_owned(),
            ..Default::default()
        };

        let outcome = on_stop(&input, &Config::default()).unwrap();

        match &outcome {
            StopOutcome::NotAnchored(why) => assert!(
                why.contains("no ancestor"),
                "outcome must carry the reason, got: {why}"
            ),
            other => panic!("expected NotAnchored, got {other:?}"),
        }
        assert!(
            !dir.join(".claude").exists(),
            "nothing may be written under an unanchorable cwd"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
