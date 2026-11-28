//! File-tool handling (Write / Edit / MultiEdit / NotebookEdit): the target
//! path extraction and the per-tool verdicts `detect_scoped` dispatches to.
//! Moved out of `detect.rs` unchanged (file-growth cap).

use serde_json::Value;

use super::protected::{protected_path_block, system_landing_block};
use super::{exclude, Ctx, Decision, UNREADABLE_OPERAND};

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
    if let Some(deny) = system_landing_block(ctx, "Write", &path) {
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
/// same payload as a redirect there ([`system_landing_block`]), so the target
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
    if let Some(deny) = system_landing_block(ctx, "Edit", &path) {
        return deny;
    }
    Decision::Allow
}

/// Verdict for a call blastguard has jurisdiction over whose operand it could
/// not extract.
///
/// # Why `Ask` and not `Allow`
///
/// This is the crate's own front door failing the test the crate exists to
/// enforce. `model.rs:5` names the defect: a two-valued form "forced every
/// construct blastguard cannot analyse into `Allow`, i.e. 'I don't understand
/// this' was recorded as 'this is fine'". The 25-odd sub-analysers in this file
/// were all converted to say `Ask` for constructs they cannot parse — but the
/// entry points, where the input itself is unreadable, were left binary. An
/// analyser that refuses to guess about a nested `$(...)` while its front door
/// waves through a payload it could not parse at all is not fail-closed; it is
/// fail-closed in the part that was looked at.
///
/// # Why `Ask` and not `Deny`
///
/// Same reason as [`protected_path_block`]: the unreadability is certain, the
/// HARM is not. A payload shape blastguard does not recognise is usually a
/// version skew between the hook and Claude Code, not an attack. `Ask` puts it
/// to the human when one is present, and `Decision::hardened` collapses it to
/// `Deny` in every headless/agent context where nobody can answer — so the
/// autonomous threat model gets the strict answer either way.
pub(super) fn unreadable_operand(tool: &str, field: &str) -> Decision {
    Decision::ask(format!(
        "{UNREADABLE_OPERAND} the operand of this {tool} call ({field} is \
         missing, empty, or not a string), so it was never analysed — refusing \
         to guess. If this recurs, blastguard's payload schema has drifted from \
         what Claude Code sends and needs updating."
    ))
}
