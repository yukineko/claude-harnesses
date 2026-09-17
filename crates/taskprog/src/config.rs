use anyhow::Result;
use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// Path to the progress file. Defaults to `<project root>/.claude/progress.md`
    /// — the root, NOT the cwd; see [`Config::resolve_progress_path`].
    pub progress_file: Option<String>,
    /// Max bytes of progress file to inject at SessionStart (0 = no limit).
    pub inject_limit: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            progress_file: None,
            inject_limit: 4096,
        }
    }
}

impl Config {
    pub fn load(cwd: &str) -> Self {
        // Project-local config wins over home config.
        let project = PathBuf::from(cwd).join("taskprog.toml");
        if project.exists() {
            if let Ok(s) = std::fs::read_to_string(&project) {
                if let Ok(c) = toml::from_str::<Config>(&s) {
                    return c;
                }
            }
        }
        let home_cfg = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".taskprog")
            .join("config.toml");
        if home_cfg.exists() {
            if let Ok(s) = std::fs::read_to_string(&home_cfg) {
                if let Ok(c) = toml::from_str::<Config>(&s) {
                    return c;
                }
            }
        }
        Config::default()
    }

    /// Where this project's progress file belongs — **anchored at the project
    /// root**, never at the incidental cwd of whoever happened to run the hook.
    ///
    /// # Why this is three-valued
    ///
    /// This used to be `-> PathBuf` and simply joined `.claude/progress.md` onto
    /// the hook-reported `cwd`. Any session or subagent whose cwd was a
    /// subdirectory therefore seeded a `.claude/` directory *there*: the observed
    /// artefact was `crates/blastguard/src/.claude/progress.md`, an empty
    /// skeleton stamped with an unrelated session id. Because `.claude/**` is one
    /// of blastguard's `PROTECTED_GLOBS`, that scattered protected directories
    /// through the source tree.
    ///
    /// The fix is to anchor at the root, which means the answer can now be "I
    /// cannot tell where the root is". Per CLAUDE.md §3 that answer is **not**
    /// a path — falling back to the cwd is precisely the defect above. So the
    /// return type is [`Determination`], whose `Undetermined` arm cannot be
    /// collapsed into a permissive default by any method call, and callers must
    /// say in their own code what "unknown" means. Every caller here resolves it
    /// to "write nothing, and say so".
    ///
    /// An explicitly configured `progress_file` is returned unchanged: the
    /// operator named an absolute location themselves, so there is nothing to
    /// anchor and nothing to determine.
    pub fn resolve_progress_path(&self, cwd: &str) -> Determination<PathBuf> {
        if let Some(p) = &self.progress_file {
            return Determination::known(harness_core::config::expand_tilde(p));
        }
        match resolve_project_root(cwd) {
            Determination::Known(root) => {
                Determination::known(root.join(".claude").join("progress.md"))
            }
            Determination::Undetermined(why) => Determination::undetermined(format!(
                "progress file location is unknown because the project root is: {why}"
            )),
        }
    }
}

/// The project root containing `cwd`: the nearest ancestor (starting at `cwd`
/// itself) that holds a `.git` entry.
///
/// `.git` is tested with `exists()` rather than `is_dir()` on purpose — in a
/// `git worktree` (how every session in this repo works, per CLAUDE.md §8) the
/// worktree root carries a `.git` **file**, not a directory. Anchoring per
/// worktree is the intended behaviour: sibling worktrees are separate working
/// copies and each keeps its own progress file.
///
/// This mirrors the marker walk in `harness_core::projkey::repo_root`, but is
/// deliberately **not** that function: `repo_root` ends with
/// `cwd.to_path_buf()`, i.e. it answers "the cwd" when it finds no marker at
/// all. That fallback is exactly the fail-open this module exists to remove, and
/// `repo_root` has other callers that depend on it, so the three-valued walk
/// lives here instead. It also avoids spawning `git rev-parse` — this runs in a
/// hook on every Stop, where a subprocess is a latency cost paid for nothing.
pub fn resolve_project_root(cwd: &str) -> Determination<PathBuf> {
    if cwd.is_empty() {
        return Determination::undetermined("the hook reported an empty cwd".to_string());
    }
    // Canonicalize first so `.`/`..`/symlinked spellings of one directory all
    // walk the same ancestor chain (and so a cwd that does not exist on this
    // machine is reported as undetermined rather than walked lexically).
    let start = match Path::new(cwd).canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return Determination::undetermined(format!("cwd {cwd:?} cannot be resolved: {e}"));
        }
    };
    let mut cur: &Path = &start;
    loop {
        if cur.join(".git").exists() {
            return Determination::known(cur.to_path_buf());
        }
        match cur.parent() {
            Some(parent) => cur = parent,
            None => break,
        }
    }
    Determination::undetermined(format!(
        "unknown: no ancestor of {} contains a .git entry",
        start.display()
    ))
}

pub fn disabled_env() -> bool {
    std::env::var("TASKPROG_DISABLED")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false)
}

pub fn init_config(target: &str) -> Result<()> {
    let path = PathBuf::from(target);
    if path.exists() {
        anyhow::bail!("{} already exists", path.display());
    }
    std::fs::write(&path, include_str!("../taskprog.example.toml"))?;
    println!("wrote {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh temp directory. Callers that want it to look like a project root
    /// must create the `.git` marker themselves — that is the whole point of the
    /// tests below.
    fn temp_dir() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let id = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("taskprog-cfg-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn known(d: Determination<PathBuf>) -> PathBuf {
        match d {
            Determination::Known(p) => p,
            Determination::Undetermined(why) => panic!("expected a resolved path, got: {why}"),
        }
    }

    fn why(d: Determination<PathBuf>) -> String {
        match d {
            Determination::Known(p) => {
                panic!("expected undetermined, got a path: {}", p.display())
            }
            Determination::Undetermined(u) => u.as_str().to_string(),
        }
    }

    /// The regression this module exists for. Observed RED before the fix:
    /// `.../crates/blastguard/src/.claude/progress.md` — a subagent's cwd became
    /// the anchor and littered the source tree.
    #[test]
    fn a_subdirectory_cwd_anchors_at_the_project_root() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let sub = root.join("crates").join("blastguard").join("src");
        std::fs::create_dir_all(&sub).unwrap();

        let got = known(Config::default().resolve_progress_path(&sub.to_string_lossy()));

        let want = root
            .canonicalize()
            .unwrap()
            .join(".claude")
            .join("progress.md");
        assert_eq!(got, want, "must anchor at the root, not the cwd");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The root itself resolves to the same place a subdirectory does — the two
    /// entry points must not disagree, or one session would inject a file the
    /// other never sees.
    #[test]
    fn the_root_itself_resolves_to_the_same_file_as_its_subdirectory() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let sub = root.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();

        let from_root = known(Config::default().resolve_progress_path(&root.to_string_lossy()));
        let from_sub = known(Config::default().resolve_progress_path(&sub.to_string_lossy()));

        assert_eq!(from_root, from_sub);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A `git worktree` root carries a `.git` FILE, not a directory. Anchoring
    /// must work there — every session in this repo runs in one (CLAUDE.md §8).
    #[test]
    fn a_worktree_root_is_anchored_even_though_dot_git_is_a_file() {
        let root = temp_dir();
        std::fs::write(root.join(".git"), "gitdir: /elsewhere/.git/worktrees/w\n").unwrap();
        let sub = root.join("crates").join("taskprog");
        std::fs::create_dir_all(&sub).unwrap();

        let got = known(Config::default().resolve_progress_path(&sub.to_string_lossy()));

        assert_eq!(
            got,
            root.canonicalize()
                .unwrap()
                .join(".claude")
                .join("progress.md")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// CLAUDE.md §3: "cannot determine" resolves to the restricted side. With no
    /// project root anywhere above the cwd there is no answer, and the answer is
    /// NOT "use the cwd" — that was the defect.
    #[test]
    fn a_cwd_with_no_project_root_is_undetermined_not_the_cwd() {
        let dir = temp_dir();
        // Fail loudly rather than pass vacuously if the machine ever grows a
        // marker above the temp dir (which would make the walk succeed).
        assert!(
            !Path::new("/tmp/.git").exists() && !Path::new("/.git").exists(),
            "precondition: no stray .git above the temp dir"
        );

        let reason = why(Config::default().resolve_progress_path(&dir.to_string_lossy()));

        assert!(
            reason.contains("no ancestor"),
            "reason must name the cause, got: {reason}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A cwd that does not exist cannot be walked. That is an observation
    /// failure, not a licence to invent a path.
    #[test]
    fn a_nonexistent_cwd_is_undetermined() {
        let reason = why(Config::default().resolve_progress_path("/no/such/dir/anywhere-12345"));
        assert!(
            reason.contains("cannot be resolved"),
            "reason must name the cause, got: {reason}"
        );
    }

    /// An empty cwd is what `HookInput::default()` carries when stdin was
    /// garbage. It is not "the current directory" — it is no information.
    #[test]
    fn an_empty_cwd_is_undetermined() {
        let reason = why(Config::default().resolve_progress_path(""));
        assert!(
            reason.contains("empty cwd"),
            "reason must name the cause, got: {reason}"
        );
    }

    /// Back-compat: an explicitly configured `progress_file` is honoured
    /// verbatim and is NOT subject to root anchoring. The operator named the
    /// location, so there is nothing to determine.
    #[test]
    fn an_explicit_progress_file_is_unchanged_and_needs_no_project_root() {
        let dir = temp_dir();
        let explicit = dir.join("custom-progress.md");
        let cfg = Config {
            progress_file: Some(explicit.to_string_lossy().into_owned()),
            ..Config::default()
        };

        // Note the cwd here has no project root at all — the explicit path must
        // still resolve, proving anchoring did not leak into this branch.
        let got = known(cfg.resolve_progress_path(&dir.to_string_lossy()));

        assert_eq!(got, explicit);
        std::fs::remove_dir_all(&dir).ok();
    }
}
