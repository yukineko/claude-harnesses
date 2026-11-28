//! What no rule file can change: the gate in front of every Allow.
//!
//! A shape can match an Allow rule (human or learned) only if ALL hold
//! (spec "Positive allow-list", coordinator ruling 2026-11-28):
//!
//! 1. its class is one of `none`, `worktree`, `scratchpad` (so `home`,
//!    `undetermined`, `main-tree`, `system`, `protected-config` are not);
//! 2. every verb's tool is in [`MODELLED_TOOLS`] with a modelled shape of its
//!    own (a known git subcommand; a pinned program hash for an interpreter);
//! 3. no verb contains `?`;
//! 4. the call is not system harm.
//!
//! Everything else (unknown tools, every wrapper that runs or `cd`s another
//! command, `sh -c` bodies, unknown interpreters) is automatically not
//! Allow-eligible. A Deny rule may still match it. Adding a tool to
//! [`MODELLED_TOOLS`] is the ONLY way a new kind of shape becomes
//! Allow-eligible.

use super::{Facts, Verb};
use harness_core::asklearn::TargetClass;

/// How much of a tool's invocation the shape pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// The verb says everything that matters (`rm`, `mv`, a file tool).
    Plain,
    /// `git`: only a listed local subcommand; no global option that changes
    /// the repository, the configuration or the helper executables.
    Git,
    /// An interpreter: the program must be pinned by a `#<hash>` token.
    Interpreter,
    /// `sh` / `bash`: a script FILE or literal stdin body (hash required);
    /// never `-c` / `-s` / rc-file options.
    ShellFile,
}

/// The single table of tools a shape may be Allowed for.
pub const MODELLED_TOOLS: &[(&str, Model)] = &[
    ("git", Model::Git),
    ("rm", Model::Plain),
    ("mv", Model::Plain),
    ("cp", Model::Plain),
    ("mkdir", Model::Plain),
    ("touch", Model::Plain),
    // File tools: their shape is the single target path, fully classed.
    ("Write", Model::Plain),
    ("Edit", Model::Plain),
    ("MultiEdit", Model::Plain),
    ("NotebookEdit", Model::Plain),
    ("python", Model::Interpreter),
    ("python3", Model::Interpreter),
    ("node", Model::Interpreter),
    ("perl", Model::Interpreter),
    ("ruby", Model::Interpreter),
    ("bash", Model::ShellFile),
    ("sh", Model::ShellFile),
];

/// The git subcommands the shape models completely (local; no network, no
/// configuration, no remote management).
const GIT_SUBCOMMANDS: &[&str] = &[
    "add",
    "am",
    "apply",
    "bisect",
    "blame",
    "branch",
    "checkout",
    "cherry-pick",
    "clean",
    "commit",
    "describe",
    "diff",
    "grep",
    "log",
    "ls-files",
    "merge",
    "mv",
    "notes",
    "rebase",
    "reflog",
    "restore",
    "rev-parse",
    "revert",
    "reset",
    "rm",
    "show",
    "stash",
    "status",
    "switch",
    "tag",
    "worktree",
];

/// Options that change what a git command runs or acts on, whatever the
/// subcommand. `-c` is here although `switch -c` / `commit -c` exist: the
/// normalised verb cannot tell a global `-c k=v` from a subcommand `-c`, and
/// the refusal only withholds an Allow.
const GIT_REFUSED_FLAGS: &[&str] = &[
    "-c",
    "--exec-path",
    "--config-env",
    "--git-dir",
    "--work-tree",
];

/// Options a script-file shell run may carry.
const SHELL_FILE_FLAGS: &[&str] = &["-e", "-u", "-x", "-v", "-f"];

fn verb_eligible(v: &Verb) -> bool {
    if v.unresolved {
        return false;
    }
    let Some((_, model)) = MODELLED_TOOLS.iter().find(|(t, _)| *t == v.tool) else {
        return false;
    };
    match model {
        Model::Plain => true,
        Model::Git => {
            v.sub
                .as_deref()
                .is_some_and(|s| GIT_SUBCOMMANDS.contains(&s))
                && !v
                    .flags
                    .iter()
                    .any(|f| GIT_REFUSED_FLAGS.contains(&f.as_str()))
        }
        Model::Interpreter => v.program.is_some(),
        Model::ShellFile => {
            v.program.is_some()
                && v.sub.is_none()
                && v.flags
                    .iter()
                    .all(|f| SHELL_FILE_FLAGS.contains(&f.as_str()))
        }
    }
}

/// Whether `facts` may match an Allow rule at all, whatever the rules say.
pub fn allow_eligible(facts: &Facts) -> bool {
    matches!(
        facts.class(),
        TargetClass::None | TargetClass::Worktree | TargetClass::Scratchpad
    ) && !facts.system_harm()
        && !facts.verbs().is_empty()
        && facts.verbs().iter().all(verb_eligible)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::asklearn::Shape;

    fn facts(class: TargetClass, verbs: &[&str]) -> Facts {
        Facts::from_shape(
            &Shape {
                rule_id: "r".into(),
                target_class: class,
                verbs: verbs.iter().map(|s| s.to_string()).collect(),
            },
            false,
        )
    }

    #[test]
    fn empty_verbs_are_not_eligible() {
        assert!(!allow_eligible(&facts(TargetClass::Worktree, &[])));
    }

    #[test]
    fn git_global_options_and_unknown_subcommands_are_refused() {
        let w = TargetClass::Worktree;
        assert!(allow_eligible(&facts(w, &["git add -A"])));
        assert!(!allow_eligible(&facts(w, &["git add -c"])));
        assert!(!allow_eligible(&facts(w, &["git push"])));
        assert!(!allow_eligible(&facts(w, &["git"])));
    }

    #[test]
    fn shell_files_only() {
        let w = TargetClass::Worktree;
        assert!(allow_eligible(&facts(w, &["bash #26610971178c87de"])));
        assert!(allow_eligible(&facts(w, &["sh -e #26610971178c87de"])));
        assert!(!allow_eligible(&facts(w, &["bash -c #26610971178c87de"])));
        assert!(!allow_eligible(&facts(w, &["bash"])));
        assert!(!allow_eligible(&facts(w, &["sh -e -s #26610971178c87de"])));
    }
}
