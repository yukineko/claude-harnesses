//! A static read of the git configuration a command line does not show
//! (backlog 5c848034): the aliases and remote URLs git would resolve from the
//! repository's own config and the user's `~/.gitconfig`.
//!
//! `git mf b o` with `alias.mf = merge-file ~/.claude/settings.json` in the repo
//! config writes a protected file while the command line names nothing
//! protected, and `git push bk main` pushes to whatever `remote.bk.url` says.
//! Both are resolved here from the files themselves (no git subprocess, no
//! environment). Anything unreadable simply contributes nothing: the callers
//! keep judging the words they CAN see (an unknown verb with a protected-looking
//! operand is still an Ask).

use std::path::{Path, PathBuf};

/// What the config files say about aliases and remotes.
#[derive(Default)]
pub(super) struct GitConfig {
    /// `(alias name lowercased, value)`, in file order (later wins).
    pub aliases: Vec<(String, String)>,
    /// `(remote name, url or pushurl)`.
    pub remotes: Vec<(String, String)>,
}

impl GitConfig {
    /// The URLs configured for remote `name`.
    pub(super) fn urls_of<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.remotes
            .iter()
            .filter(move |(n, _)| n == name)
            .map(|(_, u)| u.as_str())
    }
}

/// The config of the repository git runs in (`git_dir` when `--git-dir` /
/// `GIT_DIR` names it, else discovered upward from `cwd`) and `~/.gitconfig`.
pub(super) fn read(cwd: Option<&str>, git_dir: Option<&str>) -> GitConfig {
    let mut out = GitConfig::default();
    if let Ok(home) = std::env::var("HOME") {
        if home.starts_with('/') {
            parse_file(&Path::new(&home).join(".gitconfig"), &mut out);
        }
    }
    let dir = match (git_dir, cwd) {
        (Some(g), _) => resolve_git_dir(&absolute(g, cwd)),
        (None, Some(c)) => discover(Path::new(c)),
        (None, None) => None,
    };
    if let Some(d) = dir {
        parse_file(&d.join("config"), &mut out);
        // A linked worktree's git dir keeps only its own files; the shared
        // config is in the common dir.
        if let Some(common) = read_text(&d.join("commondir")) {
            parse_file(&d.join(common.trim()).join("config"), &mut out);
        }
    }
    out
}

fn absolute(p: &str, cwd: Option<&str>) -> PathBuf {
    let p = Path::new(p);
    match cwd {
        Some(c) if p.is_relative() => Path::new(c).join(p),
        _ => p.to_path_buf(),
    }
}

/// `p` itself when it is a git dir, or the target of a `.git` file.
fn resolve_git_dir(p: &Path) -> Option<PathBuf> {
    if p.join("config").is_file() {
        return Some(p.to_path_buf());
    }
    let text = read_text(p)?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    Some(absolute(target, p.parent().and_then(|d| d.to_str())))
}

/// The git dir of the repository containing `start`, searching upward.
fn discover(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|d| {
        let dot = d.join(".git");
        if dot.is_dir() {
            Some(dot)
        } else if dot.is_file() {
            resolve_git_dir(&dot)
        } else {
            None
        }
    })
}

fn parse_file(path: &Path, out: &mut GitConfig) {
    if let Some(text) = read_text(path) {
        parse(&text, out);
    }
}

/// The file's text through the shared boundary reader. Absent and unreadable
/// both contribute nothing here: the callers keep judging the words the command
/// line shows (an unknown verb with a protected operand is still an Ask), so an
/// unresolved alias never turns a visible hit into an Allow.
fn read_text(path: &Path) -> Option<String> {
    match harness_core::boundary::read_to_string(path) {
        harness_core::verdict::Determination::Known(text) => text,
        _ => None,
    }
}

/// Read `[alias]` and `[remote "NAME"]` entries from config text.
pub(super) fn parse(text: &str, out: &mut GitConfig) {
    enum Section {
        Alias,
        Remote(String),
        Other,
    }
    let mut section = Section::Other;
    for line in text.lines() {
        let line = line.trim();
        if let Some(head) = line.strip_prefix('[') {
            let head = head.split(']').next().unwrap_or("");
            let (name, sub) = match head.split_once(char::is_whitespace) {
                Some((n, s)) => (n, Some(s.trim().trim_matches('"').to_string())),
                None => (head, None),
            };
            section = match (name.to_ascii_lowercase().as_str(), sub) {
                ("alias", None) => Section::Alias,
                ("remote", Some(s)) => Section::Remote(s),
                _ => Section::Other,
            };
            continue;
        }
        if line.starts_with(['#', ';']) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim().to_ascii_lowercase(), unquote_value(value.trim()));
        match &section {
            Section::Alias => out.aliases.push((key, value)),
            Section::Remote(name) if key == "url" || key == "pushurl" => {
                out.remotes.push((name.clone(), value))
            }
            _ => {}
        }
    }
}

/// A config value: a surrounding pair of `"` is removed and `\"` / `\\`
/// unescaped.
fn unquote_value(v: &str) -> String {
    let inner = v
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(v);
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(e @ ('"' | '\\')) => out.push(e),
                Some(e) => {
                    out.push('\\');
                    out.push(e);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
