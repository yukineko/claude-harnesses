//! "Is this path a repo config file we should never block?" — the allowlist.
//!
//! Editing or even deleting a project's config files (`Cargo.toml`, lockfiles,
//! `package.json`, `.claude/**`, …) is routine and must never be treated as a
//! destructive blast. This module centralizes that judgement plus the helpers
//! that pull candidate paths out of tool inputs and shell operands.

use std::sync::OnceLock;

use globset::{Glob, GlobSet, GlobSetBuilder};

/// Glob patterns whose matches are always allowed (treated as config files).
const ALLOW_GLOBS: &[&str] = &[
    // The Claude Code project config tree, anywhere in the repo.
    ".claude",
    ".claude/**",
    "**/.claude",
    "**/.claude/**",
    // Explicit settings files called out by the spec.
    "settings.local.json",
    "**/settings.local.json",
    "**/.claude/settings.json",
    // Package / manifest files.
    "package.json",
    "**/package.json",
    // Manifests & config formats by extension (basename and nested).
    "*.toml",
    "**/*.toml",
    "*.yaml",
    "*.yml",
    "**/*.yaml",
    "**/*.yml",
    "*.lock",
    "**/*.lock",
    // Dotted config trees.
    ".config",
    ".config/**",
    "**/.config",
    "**/.config/**",
];

/// Glob patterns for paths that are NOT ordinary config files: they decide
/// which gates, hooks and policies run at all. Writing/appending/editing one is
/// how a guard gets disarmed, so a match here OUTRANKS [`ALLOW_GLOBS`] and
/// resolves to `Deny`, never to the config-file exemption.
///
/// Why this list is by NAME rather than by extension: `ALLOW_GLOBS` exempts
/// `*.toml` wholesale, which is right for the routine case (a crate's
/// `Cargo.toml` is a package manifest and gets edited by every `cargo add`) and
/// wrong for the security case (`deny.toml`, a gate crate's own config). The
/// fix names the security-relevant files instead of removing the
/// extension-wide allowance — an ordinary `Cargo.toml` stays allowed, and
/// nothing here matches `Cargo.toml`.
///
/// The `.example.toml` templates are deliberately NOT listed: they are
/// checked-in samples, not the live config a gate reads.
const PROTECTED_GLOBS: &[&str] = &[
    // ---- Claude Code settings & hook wiring (project, $HOME, or nested) ----
    // These decide which hooks fire, i.e. whether blastguard itself runs.
    ".claude/settings.json",
    "**/.claude/settings.json",
    ".claude/settings.local.json",
    "**/.claude/settings.local.json",
    ".claude/hooks.json",
    "**/.claude/hooks.json",
    ".claude/hooks/**",
    "**/.claude/hooks/**",
    // The project-local override, also reachable by its bare basename when the
    // path is given relative to the `.claude` directory itself.
    "settings.local.json",
    "**/settings.local.json",
    // ---- git hook paths ----
    // `.githooks/` is this repo's opt-in `core.hooksPath` tree. It is NOT
    // matched by `is_git_internal` (no literal `.git` directory component), so
    // it needs naming here; `.git/hooks/` is its in-repo twin.
    ".githooks",
    ".githooks/**",
    "**/.githooks",
    "**/.githooks/**",
    ".git/hooks/**",
    "**/.git/hooks/**",
    // ---- shell startup files (startup-persistence vector) ----
    // Matched by basename anywhere, so both the literal unexpanded `~/.zshrc`
    // and an absolute `/Users/x/.zshrc` are covered.
    ".zshrc",
    "**/.zshrc",
    ".zshenv",
    "**/.zshenv",
    ".zprofile",
    "**/.zprofile",
    ".zlogin",
    "**/.zlogin",
    ".bashrc",
    "**/.bashrc",
    ".bash_profile",
    "**/.bash_profile",
    ".profile",
    "**/.profile",
    // ---- security / gate configuration tomls ----
    // Named individually so the `*.toml` allowance keeps covering ordinary
    // manifests. Each of these is read by a gate to decide what it enforces.
    "deny.toml",
    "**/deny.toml",
    "donegate.toml",
    "**/donegate.toml",
    "specguard.toml",
    "**/specguard.toml",
    "tdd.toml",
    "**/tdd.toml",
    "blastguard.toml",
    "**/blastguard.toml",
    "propguard.toml",
    "**/propguard.toml",
    "stuckguard.toml",
    "**/stuckguard.toml",
    "mutategate.toml",
    "**/mutategate.toml",
    "reviewgate.toml",
    "**/reviewgate.toml",
    "budgetguard.toml",
    "**/budgetguard.toml",
    "overwatch.toml",
    "**/overwatch.toml",
    ".precommit-audit.toml",
    "**/.precommit-audit.toml",
];

fn build_set(patterns: &[&str]) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for pat in patterns {
        // Patterns are compile-time constants; a build error would be a bug.
        if let Ok(g) = Glob::new(pat) {
            b.add(g);
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

fn glob_set() -> &'static GlobSet {
    static SET: OnceLock<GlobSet> = OnceLock::new();
    SET.get_or_init(|| build_set(ALLOW_GLOBS))
}

fn protected_set() -> &'static GlobSet {
    static SET: OnceLock<GlobSet> = OnceLock::new();
    SET.get_or_init(|| build_set(PROTECTED_GLOBS))
}

/// Normalize a raw path/operand for matching: strip surrounding quotes,
/// convert backslashes to forward slashes, and drop a leading `./`.
pub fn normalize(raw: &str) -> String {
    let mut s = raw.trim();
    // Strip a single layer of matching surrounding quotes.
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        s = &s[1..s.len() - 1];
    }
    let mut s = s.replace('\\', "/");
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    s
}

/// True when `path` is a repo config file that must never be blocked.
pub fn is_config_file(path: &str) -> bool {
    let norm = normalize(path);
    if norm.is_empty() {
        return false;
    }
    glob_set().is_match(&norm)
}

/// True when `path` is a gate/hook/policy file whose modification disarms a
/// guard. This is checked BEFORE [`is_config_file`] everywhere a path is
/// classified: "it is a config file" was previously enough to wave through a
/// write to `.claude/settings.json` — i.e. the allowlist covered exactly the
/// files an attacker (or an over-eager agent) would edit to turn the gates off.
///
/// A protected path is not an "unknown"; it is a positively recognised
/// high-blast-radius target, so it resolves to `Deny`, not `Ask`.
pub fn is_protected_path(path: &str) -> bool {
    let norm = normalize(path);
    if norm.is_empty() {
        return false;
    }
    protected_set().is_match(&norm)
}

/// True when `path` points inside a `.git/` directory (git internals). Wholesale
/// overwriting these is destructive even though they are not "config files".
pub fn is_git_internal(path: &str) -> bool {
    let norm = normalize(path);
    norm == ".git"
        || norm.starts_with(".git/")
        || norm.contains("/.git/")
        || norm.ends_with("/.git")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_extensions_are_excluded() {
        assert!(is_config_file("Cargo.toml"));
        assert!(is_config_file("crates/blastguard/Cargo.toml"));
        assert!(is_config_file("/abs/path/Cargo.toml"));
        assert!(is_config_file("pnpm-lock.yaml"));
        assert!(is_config_file("Cargo.lock"));
        assert!(is_config_file("package.json"));
        assert!(is_config_file("config.yml"));
        assert!(is_config_file("ci.yaml"));
    }

    #[test]
    fn claude_and_dot_config_trees_are_excluded() {
        assert!(is_config_file(".claude"));
        assert!(is_config_file(".claude/settings.json"));
        assert!(is_config_file(".claude/agents/foo.md"));
        assert!(is_config_file("nested/.claude/settings.json"));
        assert!(is_config_file("settings.local.json"));
        assert!(is_config_file(".config/foo/bar.conf"));
        assert!(is_config_file("home/.config/x.ini"));
    }

    #[test]
    fn quotes_and_dot_slash_are_normalized() {
        assert!(is_config_file("\"Cargo.toml\""));
        assert!(is_config_file("'Cargo.toml'"));
        assert!(is_config_file("./Cargo.toml"));
    }

    #[test]
    fn ordinary_source_is_not_a_config_file() {
        assert!(!is_config_file("src/main.rs"));
        assert!(!is_config_file("README.md"));
        assert!(!is_config_file("notes.txt"));
        assert!(!is_config_file("build"));
        assert!(!is_config_file("*"));
        assert!(!is_config_file(""));
    }

    #[test]
    fn git_internals_detected() {
        assert!(is_git_internal(".git/config"));
        assert!(is_git_internal("worktree/.git/HEAD"));
        assert!(!is_git_internal("src/git.rs"));
        assert!(!is_git_internal("gitignore"));
    }
}
