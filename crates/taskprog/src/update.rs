//! Stop hook: prompt the LLM to update the progress file by returning
//! additionalContext asking it to write an updated `.claude/progress.md`.

use std::path::Path;

use anyhow::Result;
use harness_core::hook::HookInput;
use serde_json::json;

use crate::config::Config;

/// Run at Stop. If no progress file exists yet, suggest creating one.
/// If it does exist, ask the LLM to keep it current.
pub fn on_stop(input: &HookInput, cfg: &Config) -> Result<()> {
    let cwd = if input.cwd.is_empty() {
        "."
    } else {
        &input.cwd
    };
    let path = cfg.resolve_progress_path(cwd);

    // Does a *non-empty* progress file already exist? We only treat a file with
    // real content as "present" — an empty/whitespace-only file is as good as
    // missing and should be (re)seeded with the deterministic skeleton below.
    let has_content = crate::progress::read_file(&path, 0).is_some();

    // Deterministic, LLM-independent write: if there is no real progress content
    // yet, seed a minimal skeleton ourselves so the file is never left stale/
    // missing even when the model ignores the additionalContext nudge below.
    // Fail-soft: an IO error here must never break the turn (Stop runs under
    // run_hook, which already guarantees exit 0, but we also swallow the error).
    if !has_content {
        let skeleton = build_skeleton(input);
        let _ = write_progress(&path, &skeleton);
    }

    let (verb, current_block) = if has_content {
        let content = crate::progress::read_file(&path, 0).unwrap_or_else(|| "(empty)".to_string());
        (
            "update",
            format!(
                "\n\nCurrent progress file (`{}`):\n\n```markdown\n{}\n```",
                path.display(),
                content
            ),
        )
    } else {
        // We just seeded a skeleton — ask the model to flesh it out in place.
        (
            "update",
            format!(
                "\n\nA minimal skeleton was just written to `{}` — please flesh it out.",
                path.display()
            ),
        )
    };

    let msg = format!(
        "Before ending this session, please {verb} the progress file with what was accomplished, \
         what is pending, and any blocking issues. Keep it concise (bullet points). \
         Write it with the Write tool.{current_block}"
    );

    let out = json!({ "additionalContext": msg });
    println!("{out}");
    Ok(())
}

/// Build a minimal, deterministic progress-file skeleton from whatever session
/// context the HookInput carries. Kept intentionally small and robust: no
/// external deps, no timestamps that would make output nondeterministic — just
/// the standard sections a handoff needs, prefilled with a session breadcrumb.
fn build_skeleton(input: &HookInput) -> String {
    let project = input.project_name();
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

    fn temp_cwd() -> std::path::PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let id = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("taskprog-ut-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// on_stop must deterministically seed a non-empty `.claude/progress.md`
    /// even when no file exists yet and the LLM does nothing.
    #[test]
    fn on_stop_writes_skeleton_when_missing() {
        let dir = temp_cwd();
        let input = HookInput {
            cwd: dir.to_string_lossy().into_owned(),
            session_id: "sess-abc".to_string(),
            ..Default::default()
        };
        let cfg = Config::default();
        let path = cfg.resolve_progress_path(&input.cwd);
        assert!(!path.exists(), "precondition: file absent");

        on_stop(&input, &cfg).unwrap();

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
        let dir = temp_cwd();
        let input = HookInput {
            cwd: dir.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let cfg = Config::default();
        let path = cfg.resolve_progress_path(&input.cwd);
        write_progress(&path, "# Real notes\n\n- shipped feature X\n").unwrap();

        on_stop(&input, &cfg).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("shipped feature X"),
            "existing content preserved, got: {after}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
