//! "Is this path a repo config file we should never block?" — the allowlist.
//!
//! Editing or even deleting a project's config files (`Cargo.toml`, lockfiles,
//! `package.json`, `.claude/**`, …) is routine and must never be treated as a
//! destructive blast. This module centralizes that judgement plus the helpers
//! that pull candidate paths out of tool inputs and shell operands.

use std::sync::OnceLock;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

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

/// Directory glob patterns derived MECHANICALLY from [`PROTECTED_GLOBS`]: every
/// proper path prefix of a protected pattern that still carries at least one
/// literal component.
///
/// The gap this closes: `.claude` itself matches nothing in `PROTECTED_GLOBS`
/// — only `.claude/settings.json`, `.claude/hooks/**` and friends do. So an
/// operation aimed at the DIRECTORY (`rm -rf .claude`, `cp -r evil/ .claude/`)
/// reached every protected file underneath it while matching no protected
/// pattern at all. "The container of a protected file" is a distinct question
/// from "is a protected file", and it needed its own answer.
///
/// Why DERIVED rather than a second hand-written list: a hand list is a mirror,
/// and the recurring failure mode in this crate is a mirror that stops being
/// updated (one spelling of an operation fixed, its twin left open). Deriving
/// means a new entry in `PROTECTED_GLOBS` extends this set for free and cannot
/// be forgotten.
///
/// Prefixes made ONLY of `**` are dropped: `**` matches every directory, so
/// keeping it would classify every path anywhere as "holds protected paths" and
/// turn the rules built on it into a universal block.
fn protected_dir_patterns() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for pat in PROTECTED_GLOBS {
        let comps: Vec<&str> = pat.split('/').collect();
        for i in 1..comps.len() {
            let prefix = &comps[..i];
            if prefix.iter().all(|c| *c == "**") {
                continue;
            }
            let joined = prefix.join("/");
            if !out.contains(&joined) {
                out.push(joined);
            }
        }
    }
    out
}

fn build_set(patterns: &[&str], case_insensitive: bool) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for pat in patterns {
        // Patterns are compile-time constants; a build error would be a bug.
        if let Ok(g) = GlobBuilder::new(pat)
            .case_insensitive(case_insensitive)
            .build()
        {
            b.add(g);
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

/// The allowlist is matched CASE-SENSITIVELY, deliberately.
///
/// Case-folding it would EXEMPT more paths (`CARGO.TOML`, `.CLAUDE/agents/x`),
/// i.e. it would move the permissive side of the gate, and a spelling this side
/// does not recognise must fall through to the rules rather than skip them. The
/// protected set is folded instead (see [`protected_set`]) because folding
/// *there* denies more. Same filesystem fact, opposite direction — the
/// asymmetry is the point, not an oversight.
fn glob_set() -> &'static GlobSet {
    static SET: OnceLock<GlobSet> = OnceLock::new();
    SET.get_or_init(|| build_set(ALLOW_GLOBS, false))
}

/// The protected set is matched CASE-INSENSITIVELY.
///
/// macOS (and Windows) filesystems are case-insensitive by default, so
/// `.CLAUDE/Settings.json`, `.claude/settings.json` and `DENY.TOML` are the SAME
/// file on disk — but `globset` matches case-sensitively, so a single shifted
/// letter walked straight past the protected check and back into the
/// `is_config_file` allowlist. Folding here is the restrictive direction (it can
/// only ever deny MORE), which is the side "cannot determine" must resolve to.
fn protected_set() -> &'static GlobSet {
    static SET: OnceLock<GlobSet> = OnceLock::new();
    SET.get_or_init(|| build_set(PROTECTED_GLOBS, true))
}

/// Collapse the syntactic no-ops POSIX collapses: strip surrounding quotes,
/// convert backslashes to forward slashes, collapse redundant separators
/// (`//`) and no-op `/./` components, and drop a leading `./`.
///
/// The collapsing steps exist because the kernel does them too: POSIX treats
/// `a//b` and `a/./b` as naming exactly the same file as `a/b`, while glob
/// matching splits on every literal `/` and therefore saw a different path.
/// `.claude//settings.json` slipped past the protected patterns for that reason
/// alone. A normalizer that is *less* aggressive than the filesystem it models
/// is a bypass generator: every un-collapsed spelling is a free variant.
///
/// `..` is NOT resolved here — [`resolve_parents`] does that — because the two
/// steps are consumed differently: the protected side matches BOTH the
/// collapsed and the resolved spelling (a union can only ever deny more), while
/// the allowlist only ever sees the resolved one. See [`is_protected_path`].
fn collapse(raw: &str) -> String {
    let mut s = raw.trim();
    // Strip a single layer of matching surrounding quotes.
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        s = &s[1..s.len() - 1];
    }
    let mut s = s.replace('\\', "/");
    // Each pass strictly shortens `s`, so both loops terminate. `replace` is
    // non-overlapping, hence the loop: `a///b` needs two passes.
    while s.contains("//") {
        s = s.replace("//", "/");
    }
    while s.contains("/./") {
        s = s.replace("/./", "/");
    }
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    s
}

/// True when `path` still carries a `..` component after [`resolve_parents`],
/// i.e. it names something OUTSIDE the tree the string itself describes and
/// blastguard — which does no I/O and has no cwd model — cannot say what.
fn has_unresolved_parent(path: &str) -> bool {
    path.split('/').any(|c| c == "..")
}

/// Resolve `..` segments LEXICALLY — purely syntactic, no disk access.
///
/// Deliberately not `canonicalize`: the path may not exist yet (a `cp` creating
/// its destination), the hook must stay fast, and touching the filesystem from a
/// PreToolUse hook is a side effect this crate does not take. The price of a
/// lexical resolution is that it is wrong across a symlinked component
/// (`link/..` really means the link's target's parent); that error moves paths
/// INTO the protected set at least as often as out of it, and the union in
/// [`is_protected_path`] means the unresolved spelling is checked too, so the
/// protected side never loses a match it previously had.
///
/// A leading `..` that would escape the start of a RELATIVE path is KEPT, not
/// dropped: dropping it would silently rewrite `../.claude` into `.claude` and
/// invent a claim about a directory the string never named. For an ABSOLUTE
/// path `/..` really is `/` (POSIX), so it is dropped there.
fn resolve_parents(s: &str) -> String {
    if !has_unresolved_parent(s) {
        // Overwhelmingly the common case; also guarantees this function is a
        // no-op (byte for byte) on every path that has no `..` at all.
        return s.to_string();
    }
    let absolute = s.starts_with('/');
    let trailing_slash = s.len() > 1 && s.ends_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for comp in s.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            match stack.last() {
                // Cancel a real component.
                Some(last) if *last != ".." => {
                    stack.pop();
                }
                // At the root of an absolute path `..` is a no-op (POSIX).
                None if absolute => {}
                // Escaping a relative path: keep the `..` visible.
                _ => stack.push(".."),
            }
            continue;
        }
        stack.push(comp);
    }
    let joined = stack.join("/");
    let mut out = if absolute {
        format!("/{joined}")
    } else {
        joined
    };
    if trailing_slash && !out.is_empty() && !out.ends_with('/') {
        out.push('/');
    }
    out
}

/// Normalize a raw path/operand for matching: [`collapse`] the POSIX no-ops,
/// then resolve `..` lexically with [`resolve_parents`].
///
/// Why `..` is resolved (it deliberately was not, until 0.2.20): leaving it
/// unresolved was assumed to be the restrictive choice, and the previous version
/// of this comment said so verbatim — "a path that still contains them simply
/// fails to match the allowlist and falls through to the rules, which is the
/// restrictive side". THAT WAS FALSE, and measurably so. A `..` breaks the
/// multi-component protected patterns (`.claude/settings.json`,
/// `.claude/hooks.json`) because glob matching splits on every literal `/`, but
/// it does NOT break the allowlist's `.claude/**`, whose single `**` swallows
/// the `..` whole. So the un-resolved path failed the PROTECTED list while
/// still matching the ALLOWLIST — it moved to the permissive side, not the
/// restrictive one. `Write .claude/x/../settings.json` was ALLOW on 0.2.18 and
/// 0.2.19 while `Write .claude/settings.json` was DENY.
///
/// The asymmetry, not the `..`, was the defect: two lists were being asked the
/// same question about the same string and only one of them could see the
/// answer. The fix is symmetry, in both directions —
///
///   * [`is_protected_path`] matches the collapsed AND the resolved spelling
///     (a union: resolution can never REMOVE a protected match);
///   * [`is_config_file`] matches only the resolved spelling and refuses
///     outright when a `..` survives resolution (an unresolvable path fails the
///     allowlist too, per CLAUDE.md §3).
///
/// Still deliberately NOT resolved here: `~` expansion, symlinks, and
/// environment/glob expansion. Those need runtime state. Unlike `..` they have
/// no lexical answer at all, and the D1 note in `detect.rs` records what
/// happened the one time a convenience carve-out pretended otherwise.
pub fn normalize(raw: &str) -> String {
    resolve_parents(&collapse(raw))
}

/// True when `path` is a repo config file that must never be blocked.
///
/// A path whose `..` survives resolution returns FALSE — it fails the allowlist.
/// That is the whole point: the allowlist is the permissive side, so "I cannot
/// tell what this names" must not be answered with "it is fine". The cost is a
/// false negative on the exemption (`rm -rf ../other-repo/Cargo.toml` no longer
/// qualifies as a config-file delete and falls through to the rules, which deny
/// it); the cost of the other default was the finding above.
pub fn is_config_file(path: &str) -> bool {
    let norm = normalize(path);
    if norm.is_empty() || has_unresolved_parent(&norm) {
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
///
/// Matched against BOTH spellings of the path — the [`collapse`]d one and the
/// `..`-[`resolve_parents`]ed one — and true if EITHER matches. The union is
/// deliberate and is the restrictive direction: resolution turns
/// `.claude/x/../settings.json` into a match it did not have (the finding this
/// closes), while the unresolved spelling keeps matches that resolution would
/// have thrown away (`.claude/hooks/sub/..` matches `.claude/hooks/**` only
/// before resolution). Checking one alone would trade one hole for another.
pub fn is_protected_path(path: &str) -> bool {
    let collapsed = collapse(path);
    if !collapsed.is_empty() && protected_set().is_match(&collapsed) {
        return true;
    }
    let resolved = resolve_parents(&collapsed);
    !resolved.is_empty() && resolved != collapsed && protected_set().is_match(&resolved)
}

/// True when `path` names, or lexically contains, anything protected — the union
/// of [`is_protected_path`] and [`holds_protected_paths`], with a trailing slash
/// tolerated on the file side.
///
/// This is the predicate for "does this operand touch the gates at all", used by
/// the rules that classify a TARGET without knowing what the verb will do to it
/// (an unrecognised command, a `git rm`, a wildcard's literal prefix). Callers
/// that know the verb use the narrower two instead.
pub fn touches_protected(path: &str) -> bool {
    is_protected_path(path)
        || is_protected_path(path.trim_end_matches('/'))
        || holds_protected_paths(path)
}

/// The derived directory set, matched CASE-INSENSITIVELY for the same reason as
/// [`protected_set`]: folding here can only ever classify MORE paths as
/// containers of protected files, which is the restrictive direction.
fn protected_dir_set() -> &'static GlobSet {
    static SET: OnceLock<GlobSet> = OnceLock::new();
    SET.get_or_init(|| {
        let pats = protected_dir_patterns();
        let refs: Vec<&str> = pats.iter().map(String::as_str).collect();
        build_set(&refs, true)
    })
}

/// True when `path` names a DIRECTORY that lexically holds protected paths, even
/// though the directory itself matches no pattern in [`PROTECTED_GLOBS`].
///
/// This is deliberately a statement about the NAME, not about the filesystem:
/// blastguard does no I/O, so it cannot know whether `.claude/settings.json`
/// exists right now. What it can know is that `.claude` is the place that file
/// lives, so an operation that consumes the whole directory reaches it. Callers
/// decide what that means for their verb — a recursive `rm` definitely destroys
/// everything under it (Deny), while a recursive `cp` INTO it lands a set of
/// files that is not knowable from the command line (Ask; see
/// [`crate::model::Decision::Ask`]).
///
/// Matched against both the collapsed and the `..`-resolved spelling for the
/// same reason as [`is_protected_path`].
pub fn holds_protected_paths(path: &str) -> bool {
    let collapsed = collapse(path);
    let resolved = resolve_parents(&collapsed);
    for cand in [collapsed.as_str(), resolved.as_str()] {
        let trimmed = cand.trim_end_matches('/');
        if !trimmed.is_empty() && protected_dir_set().is_match(trimmed) {
            return true;
        }
    }
    false
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

    // ---- 0.2.20: `..` moved a path to the PERMISSIVE side ----

    #[test]
    fn parent_segments_resolve_lexically() {
        assert_eq!(
            normalize(".claude/x/../settings.json"),
            ".claude/settings.json"
        );
        assert_eq!(normalize("a/b/../../c"), "c");
        assert_eq!(normalize("/a/b/../c"), "/a/c");
        // Absolute paths cannot escape the root (POSIX: `/..` is `/`).
        assert_eq!(normalize("/../etc/hosts"), "/etc/hosts");
        // Relative escapes stay VISIBLE — they must not silently vanish.
        assert_eq!(normalize("../a"), "../a");
        assert_eq!(normalize("a/../../b"), "../b");
        // Trailing-slash shape survives resolution.
        assert_eq!(normalize(".claude/x/../hooks/"), ".claude/hooks/");
        // No-`..` paths are untouched, byte for byte.
        assert_eq!(normalize(".claude//settings.json"), ".claude/settings.json");
        assert_eq!(normalize("./src/main.rs"), "src/main.rs");
    }

    #[test]
    fn protected_match_survives_a_parent_segment() {
        assert!(is_protected_path(".claude/x/../settings.json"));
        assert!(is_protected_path(
            "/Users/y/.claude/plugins/../settings.json"
        ));
        assert!(is_protected_path(".claude/a/../hooks.json"));
        assert!(is_protected_path("nested/.claude/x/../settings.local.json"));
    }

    #[test]
    fn resolution_never_removes_a_protected_match() {
        // Resolves to `.claude/hooks`, which matches no PROTECTED_GLOB — but
        // the UNRESOLVED spelling matches `.claude/hooks/**`, and the union
        // keeps it. This is the regression the union exists to prevent.
        assert!(is_protected_path(".claude/hooks/sub/.."));
        assert!(holds_protected_paths(".claude/sub/.."));
    }

    #[test]
    fn an_unresolvable_parent_fails_the_allowlist_too() {
        // The defect was ASYMMETRY: `..` broke the protected match while the
        // allowlist's `**` swallowed it. Both sides must lose the match.
        assert!(!is_config_file("../foo/Cargo.toml"));
        assert!(!is_config_file("../.claude/agents/x.md"));
        assert!(!is_config_file("a/../../b/Cargo.toml"));
        // Controls: resolvable and plain spellings still qualify.
        assert!(is_config_file("foo/bar/../Cargo.toml"));
        assert!(is_config_file("foo/Cargo.toml"));
        assert!(is_config_file(".claude/agents/x.md"));
    }

    #[test]
    fn touches_protected_covers_file_dir_and_trailing_slash() {
        assert!(touches_protected(".claude/settings.json"));
        assert!(touches_protected(".claude/hooks/"));
        assert!(touches_protected(".githooks"));
        assert!(touches_protected(".claude"));
        assert!(touches_protected(".claude/x/../settings.json"));
        assert!(!touches_protected("src/main.rs"));
        assert!(!touches_protected("Cargo.toml"));
        assert!(!touches_protected(""));
    }

    #[test]
    fn git_internals_detected() {
        assert!(is_git_internal(".git/config"));
        assert!(is_git_internal("worktree/.git/HEAD"));
        assert!(!is_git_internal("src/git.rs"));
        assert!(!is_git_internal("gitignore"));
    }
}
