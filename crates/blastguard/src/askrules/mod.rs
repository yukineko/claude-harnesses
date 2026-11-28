//! The one place an `Ask` is judged from the facts of its shape (spec
//! `docs/specs/blastguard-ask-learning.md`, "Rule files and the judging
//! module").
//!
//! * `mod.rs` (this file): the public entry. [`Facts`] in, [`Judgement`] out,
//!   through the single pure function [`judge`]. No I/O, no environment.
//! * `rules.rs`: the TOML schema, its strict parser and the matcher. The human
//!   rule file and the learned table both become a [`RuleSet`], so there is one
//!   matcher.
//! * `builtin.rs`: what no file can change (never-allowed classes, the `?`
//!   rule, `system_harm`, the positive allow-list [`MODELLED_TOOLS`]).
//!
//! Shape EXTRACTION (shell parsing, cwd/env tracking, repository class,
//! hashing) is not here: it stays in `shape.rs` and the detectors. Reading the
//! rule files (env, HOME, stderr) is the binary's job; [`judge`] only sees
//! values.
//!
//! # Evaluation order
//!
//! 1. any matching `deny` rule of any set -> [`Judgement::Deny`] (a deny needs
//!    no eligibility: a shape that can never be Allowed can still be Denied);
//! 2. a shape that is not Allow-eligible ([`builtin::allow_eligible`]) ->
//!    [`Judgement::NoMatch`];
//! 3. any matching `not_learnable` entry of any set -> `NoMatch`;
//! 4. any matching `allow` rule (file's allow rules enabled, no `except` entry
//!    matching) -> [`Judgement::Allow`];
//! 5. else `NoMatch`: the Ask stands.

pub mod builtin;
pub mod rules;

pub use builtin::MODELLED_TOOLS;
pub use rules::RuleSet;

use harness_core::asklearn::{Shape, TargetClass};

/// One command of a shape, split into the structured parts a rule can name.
///
/// A verb is `tool [sub] [flag..] [#program-hash] [?]` (see
/// `detect::ast::shape`): `sub` is the first operand that is neither an option,
/// a program hash nor `?`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verb {
    pub tool: String,
    pub sub: Option<String>,
    pub flags: Vec<String>,
    /// The `#<fnv1a64>` token, verbatim.
    pub program: Option<String>,
    /// The verb contains a `?` token (a word or body the evaluator could not
    /// pin), or is empty.
    pub unresolved: bool,
}

impl Verb {
    /// Parse one verb string. Never fails: what it cannot read is `unresolved`.
    pub fn parse(raw: &str) -> Verb {
        let mut toks = raw.split(' ').filter(|t| !t.is_empty());
        let tool = toks.next().unwrap_or("").to_string();
        let mut v = Verb {
            unresolved: tool.is_empty() || tool == harness_core::asklearn::UNRESOLVED_VERB,
            tool,
            sub: None,
            flags: Vec::new(),
            program: None,
        };
        for t in toks {
            if t == harness_core::asklearn::UNRESOLVED_VERB {
                v.unresolved = true;
            } else if t.starts_with('#') {
                v.program = Some(t.to_string());
            } else if t.starts_with('-') {
                v.flags.push(t.to_string());
            } else if v.sub.is_none() {
                v.sub = Some(t.to_string());
            }
        }
        v
    }
}

/// Everything [`judge`] looks at. Built once by the caller from the shape and
/// the call; nothing in it is read lazily.
#[derive(Debug, Clone)]
pub struct Facts {
    key: String,
    rule: String,
    class: TargetClass,
    system_harm: bool,
    verbs: Vec<Verb>,
    verbs_joined: String,
    command: Option<String>,
    /// The quote-stripped words of `command` (what the shell passes). Outer
    /// `None`: no command text. `Undetermined`: the words could not be
    /// determined, which counts as matched for a `command_regex`.
    command_norm: Option<harness_core::verdict::Determination<String>>,
    /// What the Bash command text shows about its structure (see
    /// [`Structure`]).
    structure: Structure,
}

/// Whether the Bash line is one simple command of static literal words
/// ([`crate::shape::is_single_static_command`], on the AST).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Structure {
    /// No command text was attached (a judge-level fact set, or a file tool's
    /// empty text): the structure is not asserted here. The production entry
    /// [`crate::learned::apply_with`] never leaves a shell call in this state
    /// (it marks it [`Structure::Unreadable`]).
    Unchecked,
    /// The call is a shell command whose text could not be read.
    Unreadable,
    /// One simple command of static literal words.
    SimpleStatic,
    /// Anything else (pipeline, `;`, compound, expansion, assignment, ...).
    Other,
}

impl Facts {
    /// The facts of `shape`. `system_harm` is the detector's flag for the call.
    /// Carries no command text until [`Facts::with_command`].
    pub fn from_shape(shape: &Shape, system_harm: bool) -> Facts {
        Facts {
            key: shape.key(),
            rule: shape.rule_id.clone(),
            class: shape.target_class,
            system_harm,
            verbs: shape.verbs.iter().map(|v| Verb::parse(v)).collect(),
            verbs_joined: shape.verbs.join(","),
            command: None,
            command_norm: None,
            structure: Structure::Unchecked,
        }
    }

    /// Attach the raw command text `command_regex` conditions are searched in,
    /// and the quote-stripped words of it (a condition is met when EITHER
    /// spelling matches, so `git ad''d` cannot dodge a `git add` regex; words
    /// that cannot be determined count as matched). Without a command such a
    /// condition counts as matched (restrictive side).
    pub fn with_command(mut self, command: &str) -> Facts {
        self.command = Some(command.to_string());
        self.command_norm = Some(if command.trim().is_empty() {
            harness_core::verdict::Determination::Known(String::new())
        } else {
            crate::shape::static_words(command)
        });
        // Empty text is a file tool's (no shell line at all).
        self.structure = if command.trim().is_empty() {
            Structure::Unchecked
        } else if crate::shape::is_single_static_command(command) {
            Structure::SimpleStatic
        } else {
            Structure::Other
        };
        self
    }

    /// Mark a call that has no readable command text. A file tool (every verb
    /// a file-tool name) has no shell line and stays unchecked; any other call
    /// is a shell command whose structure is unknown, so it is never
    /// Allow-eligible.
    pub fn without_command_text(mut self) -> Facts {
        if !self.verbs.iter().all(|v| builtin::is_file_tool(&v.tool)) {
            self.structure = Structure::Unreadable;
        }
        self
    }

    pub(crate) fn structure(&self) -> Structure {
        self.structure
    }

    pub fn class(&self) -> TargetClass {
        self.class
    }

    pub fn verbs(&self) -> &[Verb] {
        &self.verbs
    }

    pub fn system_harm(&self) -> bool {
        self.system_harm
    }

    /// The class label a rule file spells: the shape-schema tag `s4-` removed.
    pub(crate) fn class_label(&self) -> &'static str {
        class_label(self.class)
    }
}

/// `TargetClass` as a rule file spells it (no schema tag).
pub(crate) fn class_label(c: TargetClass) -> &'static str {
    c.as_str().trim_start_matches("s4-")
}

/// What the rules say about an `Ask`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judgement {
    /// Turn the Ask into an Allow; the string is the reason to record.
    Allow(String),
    /// Turn the Ask into a Deny; the string is the rule's reason.
    Deny(String),
    /// Nothing applies: the Ask stands.
    NoMatch,
}

/// Judge `facts` against `sets` (see the module docs for the order). Pure.
pub fn judge(facts: &Facts, sets: &[&RuleSet]) -> Judgement {
    for set in sets {
        if let Some(reason) = set.matching_deny(facts) {
            return Judgement::Deny(reason);
        }
    }
    // A set whose denials are unknown (unreadable / unparsable / required but
    // absent file, an unreadable restrictive rule) allows nothing at all.
    if sets.iter().any(|s| s.poisons_allow()) {
        return Judgement::NoMatch;
    }
    if !builtin::allow_eligible(facts) {
        return Judgement::NoMatch;
    }
    if sets.iter().any(|s| s.blocks_learning(facts)) {
        return Judgement::NoMatch;
    }
    for set in sets {
        if let Some(reason) = set.matching_allow(facts) {
            return Judgement::Allow(reason);
        }
    }
    Judgement::NoMatch
}

/// The environment variable that replaces the bundled human rules file (tests).
pub const RULES_VAR: &str = "BLASTGUARD_ASK_RULES";

/// The bundled human rules, compiled in.
pub const BUNDLED_RULES: &str = include_str!("../../rules/ask-rules.toml");

/// The human rule set: the file `BLASTGUARD_ASK_RULES` names, else the bundled
/// one. A path the override names that does not exist (or cannot be read or
/// parsed) is undetermined: [`RuleSet::poisons_allow`], so no Allow is
/// honoured (the Ask stays an Ask). I/O lives here, not in [`judge`].
pub fn load_human(env_value: Option<&str>) -> RuleSet {
    match env_value.map(str::trim).filter(|p| !p.is_empty()) {
        Some(path) => {
            let p = std::path::Path::new(path);
            match harness_core::boundary::read_to_string(p) {
                harness_core::verdict::Determination::Known(None) => RuleSet::missing_required(p),
                _ => RuleSet::from_file(p),
            }
        }
        None => RuleSet::from_toml(BUNDLED_RULES),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_rules_parse_clean() {
        let rs = RuleSet::from_toml(BUNDLED_RULES);
        assert!(rs.warnings().is_empty(), "{:?}", rs.warnings());
        assert!(rs.allow_enabled());
    }

    #[test]
    fn verb_parse_splits_the_parts() {
        let v = Verb::parse("python3 -c -u #26610971178c87de");
        assert_eq!(v.tool, "python3");
        assert_eq!(v.sub, None);
        assert_eq!(v.flags, ["-c", "-u"]);
        assert_eq!(v.program.as_deref(), Some("#26610971178c87de"));
        assert!(!v.unresolved);
        let v = Verb::parse("git add -A ?");
        assert_eq!(v.sub.as_deref(), Some("add"));
        assert!(v.unresolved);
        assert!(Verb::parse("?").unresolved);
        assert!(Verb::parse("").unresolved);
    }
}
