//! System-harm marking of the SHAPE Denies (backlog 3c750df2, user rulings
//! 2026-11-07: 「いかなる理由であれシステムに害になるコマンドはdenyすべし」 /
//! 「1は絶対にdenyでいい」).
//!
//! The shape rules (`find -delete`, `truncate`, `shred`, recursive
//! `chmod`/`chown`, `dd of=`, recursive/wildcard `rm`, `osascript` telling an
//! app to delete) answer a plain [`Decision::deny`] that measures the SHAPE,
//! not the target, so a repeat of `find /etc -delete` or `rm -rf ~` was
//! downgraded to an Ask by the binary's repeat rule (`downgrade_on_repeat`).
//! [`mark_system_harm`] re-issues such a Deny, reason unchanged, through
//! [`Decision::deny_system_harm`] when one of its targets is the SYSTEM or the
//! home directory itself ([`harms_system`]), so the repeat stays a Deny.
//!
//! It only ever turns a plain Deny into a marked Deny: an `Allow` or `Ask`
//! passes through untouched, so it can neither admit nor block anything the
//! rule did not already refuse. A target it cannot place (an expansion, a
//! quoted word, a relative path with no known base) is not marked — that
//! Deny keeps the repeat downgrade it had before, i.e. the pre-existing answer.

use super::fileops::{chmod_mode_and_targets, chown_owner_and_targets, positional_operands};
use super::words::{has_short, normalized_command};
use super::Ctx;
use crate::model::Decision;
use harness_core::verdict::Determination;

/// [`super::analyze_command_at`], its verdict passed through
/// [`mark_system_harm`]. The command-word loops of `detect.rs` call this.
pub(super) fn analyze_command_at(
    tokens: &[&str],
    idx: usize,
    depth: usize,
    seg: Option<&str>,
    ctx: &Ctx<'_>,
) -> Decision {
    let verdict = super::analyze_command_at(tokens, idx, depth, seg, ctx);
    mark_system_harm(tokens, idx, ctx, verdict)
}

/// The verdict of the command at `tokens[idx]`, re-issued as a system-harm
/// Deny when it is a plain Deny whose command targets the system or the home
/// directory. See the module doc.
fn mark_system_harm(tokens: &[&str], idx: usize, ctx: &Ctx<'_>, decision: Decision) -> Decision {
    let Decision::Deny(reason) = &decision else {
        return decision;
    };
    if decision.is_system_harm() {
        return decision;
    }
    let cmd = normalized_command(tokens[idx]);
    let rest = &tokens[idx + 1..];
    let targets: Vec<String> = match cmd.as_str() {
        "rm" => rm_operands(rest),
        "find" if rest.contains(&"-delete") => find_roots(rest),
        "truncate" => owned(positional_operands(
            rest,
            &["-s", "--size", "-r", "--reference", "-o", "--io-blocks"],
        )),
        "shred" => owned(positional_operands(
            rest,
            &["-n", "--iterations", "-s", "--size", "--random-source"],
        )),
        "chmod" if has_short(rest, 'R') || rest.contains(&"--recursive") => {
            owned(chmod_mode_and_targets(rest).1)
        }
        "chown" | "chgrp" if has_short(rest, 'R') || rest.contains(&"--recursive") => {
            owned(chown_owner_and_targets(rest).1)
        }
        "dd" => rest
            .iter()
            .filter_map(|t| t.strip_prefix("of="))
            .map(str::to_string)
            .collect(),
        "osascript" if reason.contains("`osascript` tells ") => quoted_paths(&rest.join(" ")),
        _ => return decision,
    };
    if targets.iter().any(|t| harms_system(ctx, &cmd, t)) {
        return Decision::deny_system_harm(reason.clone());
    }
    decision
}

fn owned(v: Vec<&str>) -> Vec<String> {
    v.into_iter().map(str::to_string).collect()
}

/// Every non-flag word of an `rm`, and every word after `--`.
fn rm_operands(rest: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut after_dashdash = false;
    for t in rest {
        if !after_dashdash && *t == "--" {
            after_dashdash = true;
            continue;
        }
        if after_dashdash || !t.starts_with('-') {
            out.push((*t).to_string());
        }
    }
    out
}

/// `find`'s search roots: the words after its leading global options, up to
/// the first expression word.
fn find_roots(rest: &[&str]) -> Vec<String> {
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "-H" | "-L" | "-P" => i += 1,
            "-D" => i += 2,
            t if t.starts_with("-O") => i += 1,
            _ => break,
        }
    }
    rest[i.min(rest.len())..]
        .iter()
        .take_while(|t| !t.starts_with('-') && !matches!(**t, "(" | ")" | "!" | ","))
        .map(|t| (*t).to_string())
        .collect()
}

/// The double-quoted string literals of an AppleScript / JXA program that
/// spell an absolute or home path (`POSIX file "/etc/hosts"`).
fn quoted_paths(program: &str) -> Vec<String> {
    program
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|s| s.starts_with('/') || s.starts_with('~'))
        .map(str::to_string)
        .collect()
}

/// True when `operand` of `verb` names `/`, a path at or below a system
/// directory ([`crate::scope::is_inside_system_dir`]), a raw disk device, the
/// home directory itself, or an irreplaceable home-level asset (`~/.ssh`,
/// `~/.gnupg`, a home dotfile — [`crate::scope::SafeRoots::irreplaceable_home_asset`]),
/// judged on the lexical absolute spelling and again with symlinks resolved.
/// A system path strictly inside a safe root the session owns is not system
/// harm; the home checks have no such escape. An operand that cannot be placed
/// answers `false` (not marked; the Deny itself stands).
fn harms_system(ctx: &Ctx<'_>, verb: &str, operand: &str) -> bool {
    let home = ctx.scope.home_real().map(|h| h.trim_end_matches('/'));
    let expanded = match home {
        Some(h) => ["${HOME}", "$HOME", "~"]
            .iter()
            .find_map(|p| {
                operand.strip_prefix(p).and_then(|rest| {
                    (rest.is_empty() || rest.starts_with('/')).then(|| format!("{h}{rest}"))
                })
            })
            .unwrap_or_else(|| operand.to_string()),
        None => operand.to_string(),
    };
    let Determination::Known(lexical) = ctx.scope.lexical_absolute(&expanded, ctx.base_for(verb))
    else {
        return false;
    };
    let mut candidates = vec![lexical.clone()];
    if let Some(real) = ctx.scope.resolve(&lexical) {
        candidates.push(real);
    }
    // The home directory and its irreplaceable assets have no escape: a
    // session whose project IS `$HOME` (cwd `~`) owns it as a safe root, and
    // `rm -rf ~` there is the worst case, not a confined one.
    let is_home = |p: &str| home.is_some_and(|h| p.trim_end_matches('/') == h);
    if candidates.iter().any(|p| is_home(p))
        || ctx
            .scope
            .irreplaceable_home_asset(&expanded, ctx.base_for(verb))
            .is_some()
    {
        return true;
    }
    let is_system = |p: &str| {
        let p = p.trim_end_matches('/');
        p.is_empty() || crate::scope::is_inside_system_dir(p) || is_raw_disk(p)
    };
    // A system path the session owns (a project checked out under `/opt` —
    // strictly inside a safe root, as for `rm_system_harm_deny`) escapes.
    candidates.iter().any(|p| is_system(p))
        && ctx.confined_root(verb, &[lexical.as_str()]).is_none()
}

/// A block device that IS a disk or partition (`/dev/disk0`, `/dev/rdisk2s1`,
/// `/dev/sda`, `/dev/nvme0n1`): raw bytes written there destroy a filesystem.
fn is_raw_disk(p: &str) -> bool {
    let Some(name) = p.strip_prefix("/dev/") else {
        return false;
    };
    ["disk", "rdisk", "sd", "hd", "vd", "xvd", "nvme", "mmcblk"]
        .iter()
        .any(|d| name.starts_with(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_disk_names() {
        assert!(is_raw_disk("/dev/disk0"));
        assert!(is_raw_disk("/dev/rdisk2s1"));
        assert!(!is_raw_disk("/dev/null"));
        assert!(!is_raw_disk("/dev/stdout"));
        assert!(!is_raw_disk("/tmp/disk0"));
    }

    #[test]
    fn operand_extraction() {
        assert_eq!(rm_operands(&["-rf", "~", "--", "-x"]), vec!["~", "-x"]);
        assert_eq!(find_roots(&["-L", "/etc", "-name", "x"]), vec!["/etc"]);
        assert_eq!(
            quoted_paths(r#"tell application "Finder" to delete POSIX file "/etc/hosts""#),
            vec!["/etc/hosts"]
        );
    }
}
