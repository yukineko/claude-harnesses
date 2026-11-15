//! File-tool handling (Write / Edit / MultiEdit / NotebookEdit): the target
//! path extraction and the per-tool verdicts `detect_scoped` dispatches to.
//! Moved out of `detect.rs` unchanged (file-growth cap).

use serde_json::Value;

use super::protected::{protected_path_block, real_system_path_block, system_path_block};
use super::{exclude, unreadable_operand, Ctx, Decision};

fn extract_path(ti: Option<&Value>) -> Option<String> {
    let v = ti?;
    for key in ["file_path", "notebook_path", "path"] {
        if let Some(p) = v.get(key).and_then(|p| p.as_str()) {
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

pub(super) fn detect_write(ti: Option<&Value>, ctx: &Ctx<'_>) -> Decision {
    let path = match extract_path(ti) {
        Some(p) => p,
        None => return unreadable_operand("Write", "file_path"),
    };
    // Git internals first: a whole-file Write over `.git/<x>` has always been a
    // flat Deny. `.git/config` (and `.git/hooks/**`) are ALSO protected paths,
    // whose verdict is the weaker Ask — checking the protected list first would
    // downgrade that Deny (backlog 03885f11). The stricter verdict wins.
    if exclude::is_git_internal(&path) {
        return Decision::deny(format!(
            "Write would overwrite git internals ({path}) — refusing"
        ));
    }
    // BEFORE the config-file allowlist, not after: `.claude/**`, `*.toml` and
    // `settings.local.json` are all matched by `is_config_file`, so the
    // exemption used to swallow exactly the paths that decide whether the gates
    // run at all. "It is a config file" is the reason to look harder here, not
    // the reason to stop looking.
    if let Some(deny) = protected_path_block(ctx, "Write", &path) {
        return deny;
    }
    // Also before the config-file allowlist: `/etc/<x>.toml` is a config file
    // by name and a system-directory drop-in by location.
    if let Some(deny) = file_tool_system_block(ctx, "Write", &path) {
        return deny;
    }
    if exclude::is_config_file(&path) {
        return Decision::Allow;
    }
    let content = ti
        .and_then(|v| v.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if content.trim().is_empty() {
        return Decision::deny(format!(
            "Write would replace {path} with empty content, wiping the file"
        ));
    }
    Decision::Allow
}

/// Edit / MultiEdit / NotebookEdit: a partial edit of an ordinary file is
/// allowed, but a partial edit of a protected gate/hook/policy path is exactly
/// how a guard gets switched off, and an edit inside a system directory is the
/// same payload as a redirect there ([`file_tool_system_block`]), so the target
/// is classified rather than assumed harmless. `NotebookEdit` addresses its
/// target via `notebook_path`, which `extract_path` already covers.
pub(super) fn detect_edit(ti: Option<&Value>, ctx: &Ctx<'_>) -> Decision {
    let path = match extract_path(ti) {
        Some(p) => p,
        None => return unreadable_operand("Edit", "file_path/notebook_path"),
    };
    if let Some(deny) = protected_path_block(ctx, "Edit", &path) {
        return deny;
    }
    if let Some(deny) = file_tool_system_block(ctx, "Edit", &path) {
        return deny;
    }
    Decision::Allow
}

/// The system-directory axis for the file tools (Write / Edit / MultiEdit /
/// NotebookEdit), user ruling 2026-11-01: the same rule, reason and directory
/// set as a Bash redirect — [`system_path_block`] on the path as written, then
/// [`real_system_path_block`] on the path the kernel really writes, so a
/// dangling file symlink, a chain of them, or a directory symlink into `/etc`
/// is the same Deny as the direct spelling. The `confined_root` exemption in
/// [`system_path_block`] applies unchanged (a project checked out under `/opt`).
///
/// When the landing cannot be resolved (a symlink loop, an `lstat`/`read_link`
/// error other than "does not exist") the answer is Ask, never Allow: "could
/// not resolve" is not "not a system directory" (CLAUDE.md §3).
fn file_tool_system_block(ctx: &Ctx<'_>, action: &str, path: &str) -> Option<Decision> {
    if let Some(deny) = system_path_block(action, path, ctx) {
        return Some(deny);
    }
    match real_system_path_block(action, path, ctx) {
        Ok(verdict) => verdict,
        Err(why) => Some(Decision::ask(format!(
            "{action} target ({path}) could not be resolved through its symlinks ({why}) — \
blastguard cannot tell whether it lands in a system directory, so it refuses to guess"
        ))),
    }
}
