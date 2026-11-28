//! What no rule file can change: the gate in front of every Allow.
//!
//! A shape can match an Allow rule (human or learned) only if ALL hold
//! (spec "Positive allow-list", coordinator ruling 2026-11-28):
//!
//! 1. its class is one of `none`, `worktree`, `scratchpad` (so `home`,
//!    `undetermined`, `main-tree`, `system`, `protected-config` are not);
//! 2. every verb's tool is in [`MODELLED_TOOLS`] with a modelled shape of its
//!    own (a known git subcommand; an interpreter run and a shell script run
//!    are never eligible);
//! 3. no verb contains `?`;
//! 4. the call is not system harm;
//! 5. the Bash line is ONE simple command of static literal words (on the
//!    AST, [`crate::shape::is_single_static_command`]); a call with no command
//!    text is eligible only as a file-tool call. (The path half of the gate —
//!    no `..`, no symlink or case difference in the spelling — is in the
//!    class: such a path is classed `undetermined`.)
//!
//! Everything else (unknown tools, every wrapper that runs or `cd`s another
//! command, `sh -c` bodies, unknown interpreters) is automatically not
//! Allow-eligible. A Deny rule may still match it. Adding a tool to
//! [`MODELLED_TOOLS`] is the ONLY way a new kind of shape becomes
//! Allow-eligible.

use super::{Facts, Structure, Verb};
use harness_core::asklearn::TargetClass;

/// How much of a tool's invocation the shape pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// The verb says everything that matters (`rm`, `mv`, a file tool).
    Plain,
    /// `git`: only a listed local subcommand; no global option that changes
    /// the repository, the configuration or the helper executables.
    Git,
    /// `sh` / `bash`: NEVER Allow-eligible (ruling 2026-11-28, round 6: the
    /// script file's content may change after it was learned). Listed so the
    /// shape key keeps its program hash without the unmodelled-tool `?`.
    ShellFile,
    /// An interpreter (`python3`, `node`, `perl`, `ruby`): its run is NEVER
    /// Allow-eligible, whatever program hash the verb carries. The tool is
    /// listed only so the shape's key is not marked `?` (frozen keys).
    Interpreter,
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
    // Interpreters stay listed so their shape keeps the program hash WITHOUT
    // the unmodelled-tool `?` (shape keys are frozen), but running a program
    // is never Allow-eligible (ruling 2026-11-28: no attempt at completeness,
    // fail closed): see `Model::Interpreter`.
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

fn verb_eligible(v: &Verb) -> bool {
    if v.unresolved {
        return false;
    }
    let Some((_, model)) = MODELLED_TOOLS.iter().find(|(t, _)| *t == v.tool) else {
        return false;
    };
    match model {
        Model::Plain => true,
        Model::Interpreter => false,
        Model::Git => {
            v.sub
                .as_deref()
                .is_some_and(|s| GIT_SUBCOMMANDS.contains(&s))
                && !v
                    .flags
                    .iter()
                    .any(|f| GIT_REFUSED_FLAGS.contains(&f.as_str()))
        }
        // Ruling 2026-11-28 (round 6): a script FILE may change after it was
        // learned, so a shell run is never eligible.
        Model::ShellFile => false,
    }
}

/// The file tools: no shell line, one target path.
pub(crate) fn is_file_tool(tool: &str) -> bool {
    matches!(tool, "Write" | "Edit" | "MultiEdit" | "NotebookEdit")
}

/// The structural gate: a Bash line must be ONE simple command of static
/// literal words (checked on the AST, [`crate::shape::is_single_static_command`]);
/// a shell call whose text was not readable is not eligible. Facts with no
/// command attached at all (the judge-level unit facts) assert nothing.
fn structure_eligible(facts: &Facts) -> bool {
    matches!(
        facts.structure(),
        Structure::SimpleStatic | Structure::Unchecked
    )
}

/// Whether `facts` may match an Allow rule at all, whatever the rules say.
pub fn allow_eligible(facts: &Facts) -> bool {
    matches!(
        facts.class(),
        TargetClass::None | TargetClass::Worktree | TargetClass::Scratchpad
    ) && !facts.system_harm()
        && !facts.verbs().is_empty()
        && facts.verbs().iter().all(verb_eligible)
        && structure_eligible(facts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::asklearn::Shape;

    fn bare(class: TargetClass, verbs: &[&str]) -> Facts {
        Facts::from_shape(
            &Shape {
                rule_id: "r".into(),
                target_class: class,
                verbs: verbs.iter().map(|s| s.to_string()).collect(),
            },
            false,
        )
    }

    /// Facts whose command text is the verbs themselves (one simple command).
    fn facts(class: TargetClass, verbs: &[&str]) -> Facts {
        bare(class, verbs).with_command(&verbs.join(" "))
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

    /// Rewritten to the round-6 ruling (it asserted the opposite: that a
    /// pinned script file run was eligible): a bash / sh run is never
    /// eligible, whatever the flags and the program hash.
    #[test]
    fn shell_runs_are_never_eligible() {
        let w = TargetClass::Worktree;
        // The refusal must come from the ShellFile model, not from the
        // structural gate (verifier round 7): the command text of each fact
        // set is one simple static command (`#..` is a comment), and the same
        // fact set with a Plain tool is eligible.
        for v in ["bash #26610971178c87de", "sh -e #26610971178c87de"] {
            assert_eq!(facts(w, &[v]).structure(), Structure::SimpleStatic, "{v}");
        }
        assert!(allow_eligible(&facts(w, &["rm -e #26610971178c87de"])));
        assert!(!allow_eligible(&facts(w, &["bash #26610971178c87de"])));
        assert!(!allow_eligible(&facts(w, &["sh -e #26610971178c87de"])));
        assert!(!allow_eligible(&facts(w, &["bash -c #26610971178c87de"])));
        assert!(!allow_eligible(&facts(w, &["bash"])));
        assert!(!allow_eligible(&facts(w, &["sh -e -s #26610971178c87de"])));
    }

    #[test]
    fn a_non_simple_or_non_static_line_is_not_eligible() {
        let w = TargetClass::Worktree;
        let f = |cmd: &str| facts(w, &["git add -A"]).with_command(cmd);
        assert!(allow_eligible(&f("git add -A")));
        for c in [
            "git add -A && git status",
            "for P in a; do git add -A; done",
            "git add $'\\x2dA'",
            "PATH=/x git add -A",
            "git add -A | cat",
        ] {
            assert!(!allow_eligible(&f(c)), "{c}");
        }
        // unreadable command text: only a file tool stays eligible
        assert!(!allow_eligible(
            &bare(w, &["git add -A"]).without_command_text()
        ));
        assert!(allow_eligible(&bare(w, &["Write"]).without_command_text()));
    }
}
