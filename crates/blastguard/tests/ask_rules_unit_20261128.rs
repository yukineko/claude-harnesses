#![cfg(feature = "ask-rules-red")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(ask-rules): the pure judging module `blastguard::askrules`
//! (spec docs/specs/blastguard-ask-learning.md, "Rule files and the judging
//! module"). Facts in, `Judgement` out; no I/O. `Judgement` must derive
//! `Debug + PartialEq`.
//!
//! Written by an agent that does not implement it (CLAUDE.md 2(a)). The module
//! does not exist yet, so this file fails to COMPILE: that is the observed RED.
//! It is gated behind the cargo feature `ask-rules-red` so the rest of the
//! crate's tests keep building meanwhile. Observe with
//! `cargo test -p blastguard --features ask-rules-red --test
//! ask_rules_unit_20261128 -- --include-ignored`. The implementer removes the
//! feature gate (and the feature) and the `#[ignore]`s together.
//!
//! What these do NOT prove: that the binary wires the module in (see
//! ask_rules_toml_20261128.rs), nor shape extraction.

use blastguard::askrules::{judge, Facts, Judgement, RuleSet};
use harness_core::asklearn::{Shape, TargetClass};

fn shape(class: TargetClass, verbs: &[&str]) -> Shape {
    Shape {
        rule_id: "git-whole-tree-stage".into(),
        target_class: class,
        verbs: verbs.iter().map(|s| s.to_string()).collect(),
    }
}

fn facts(class: TargetClass, verbs: &[&str]) -> Facts {
    Facts::from_shape(&shape(class, verbs), false)
}

fn allow_git_add() -> String {
    r#"
[[rule]]
decision = "allow"
reason = "stage in a worktree"
[rule.match]
rule = "git-whole-tree-stage"
class = ["worktree", "scratchpad", "main-tree", "system", "protected-config", "home", "undetermined", "none"]
tool = "git"
sub = "add"
flags_all = ["-A"]
"#
    .to_string()
}

fn is_allow(j: &Judgement) -> bool {
    matches!(j, Judgement::Allow(_))
}

#[test]
#[ignore = "RED(ask-rules): blastguard::askrules does not exist yet"]
fn allow_rule_matches_structured_facts() {
    let rs = RuleSet::from_toml(&allow_git_add());
    assert!(rs.allow_enabled(), "warnings: {:?}", rs.warnings());
    let j = judge(&facts(TargetClass::Worktree, &["git add -A"]), &[&rs]);
    assert!(is_allow(&j), "{j:?}");
    // canary: a different tool / missing flag / other sub does not match
    for v in ["git add", "git commit -A", "rm -A", "git add -A -f"] {
        let j = judge(&facts(TargetClass::Worktree, &[v]), &[&rs]);
        if v == "git add -A -f" {
            assert!(is_allow(&j), "extra flags are fine for flags_all: {j:?}");
        } else {
            assert_eq!(j, Judgement::NoMatch, "{v}");
        }
    }
}

#[test]
#[ignore = "RED(ask-rules): blastguard::askrules does not exist yet"]
fn deny_rule_matches() {
    let rs = RuleSet::from_toml(
        r#"
[[rule]]
decision = "deny"
reason = "never"
[rule.match]
tool = "rm"
class = ["home"]
"#,
    );
    let j = judge(&facts(TargetClass::Home, &["rm -f -r"]), &[&rs]);
    assert!(matches!(j, Judgement::Deny(_)), "{j:?}");
    let j = judge(&facts(TargetClass::Worktree, &["rm -f -r"]), &[&rs]);
    assert_eq!(j, Judgement::NoMatch);
}

#[test]
#[ignore = "RED(ask-rules): blastguard::askrules does not exist yet"]
fn deny_beats_allow_in_one_file_and_across_files() {
    let both = format!(
        "{}\n[[rule]]\ndecision = \"deny\"\nreason = \"no\"\n[rule.match]\ntool = \"git\"\nsub = \"add\"\n",
        allow_git_add()
    );
    let rs = RuleSet::from_toml(&both);
    let f = facts(TargetClass::Worktree, &["git add -A"]);
    assert!(matches!(judge(&f, &[&rs]), Judgement::Deny(_)));

    let a = RuleSet::from_toml(&allow_git_add());
    let d = RuleSet::from_toml(
        "[[rule]]\ndecision = \"deny\"\nreason = \"no\"\n[rule.match]\ntool = \"git\"\n",
    );
    assert!(matches!(judge(&f, &[&a, &d]), Judgement::Deny(_)));
    assert!(matches!(judge(&f, &[&d, &a]), Judgement::Deny(_)));
}

#[test]
#[ignore = "RED(ask-rules): blastguard::askrules does not exist yet"]
fn never_allowed_classes_ignore_allow_rules() {
    let rs = RuleSet::from_toml(&allow_git_add());
    assert!(is_allow(&judge(
        &facts(TargetClass::Worktree, &["git add -A"]),
        &[&rs]
    )));
    for c in [
        TargetClass::System,
        TargetClass::ProtectedConfig,
        TargetClass::MainTree,
    ] {
        let j = judge(&facts(c, &["git add -A"]), &[&rs]);
        assert!(!is_allow(&j), "{c:?} must never match an allow rule: {j:?}");
    }
    // ...but a deny rule still applies there
    let d = RuleSet::from_toml(
        "[[rule]]\ndecision = \"deny\"\nreason = \"no\"\n[rule.match]\ntool = \"git\"\n",
    );
    assert!(matches!(
        judge(&facts(TargetClass::MainTree, &["git add -A"]), &[&d]),
        Judgement::Deny(_)
    ));
}

#[test]
#[ignore = "RED(ask-rules): blastguard::askrules does not exist yet"]
fn system_harm_never_allowed() {
    let rs = RuleSet::from_toml(&allow_git_add());
    let f = Facts::from_shape(&shape(TargetClass::Worktree, &["git add -A"]), true);
    assert!(!is_allow(&judge(&f, &[&rs])));
    // canary: same shape without system_harm is allowed
    assert!(is_allow(&judge(
        &facts(TargetClass::Worktree, &["git add -A"]),
        &[&rs]
    )));
}

#[test]
#[ignore = "RED(ask-rules): blastguard::askrules does not exist yet"]
fn unresolved_verb_never_matches_allow() {
    let rs = RuleSet::from_toml(&allow_git_add());
    assert!(is_allow(&judge(
        &facts(TargetClass::Worktree, &["git add -A"]),
        &[&rs]
    )));
    for v in ["git add -A ?", "?"] {
        let j = judge(&facts(TargetClass::Worktree, &[v]), &[&rs]);
        assert!(!is_allow(&j), "{v}: {j:?}");
    }
    // an unresolved verb can still learn a Deny
    let d = RuleSet::from_toml(
        "[[rule]]\ndecision = \"deny\"\nreason = \"no\"\n[rule.match]\ntool = \"git\"\n",
    );
    assert!(matches!(
        judge(&facts(TargetClass::Worktree, &["git add -A ?"]), &[&d]),
        Judgement::Deny(_)
    ));
}

#[test]
#[ignore = "RED(ask-rules): blastguard::askrules does not exist yet"]
fn flags_any_none_and_program() {
    let rs = RuleSet::from_toml(
        r##"
[[rule]]
decision = "allow"
reason = "x"
[rule.match]
tool = "python3"
program = "#26610971178c87de"
class = ["worktree"]

[[rule]]
decision = "allow"
reason = "y"
[rule.match]
tool = "git"
sub = "branch"
flags_any = ["-d", "-m"]
flags_none = ["-D"]
class = ["worktree"]
"##,
    );
    assert!(rs.allow_enabled(), "{:?}", rs.warnings());
    let w = TargetClass::Worktree;
    assert!(is_allow(&judge(
        &facts(w, &["python3 -c #26610971178c87de"]),
        &[&rs]
    )));
    assert!(!is_allow(&judge(
        &facts(w, &["python3 -c #0000000000000000"]),
        &[&rs]
    )));
    assert!(is_allow(&judge(&facts(w, &["git branch -d"]), &[&rs])));
    assert!(!is_allow(&judge(&facts(w, &["git branch -D"]), &[&rs])));
    assert!(!is_allow(&judge(&facts(w, &["git branch"]), &[&rs])));
    assert!(!is_allow(&judge(&facts(w, &["git branch -d -D"]), &[&rs])));
}

#[test]
#[ignore = "RED(ask-rules): blastguard::askrules does not exist yet"]
fn multi_verb_allow_needs_every_verb_deny_needs_any() {
    let rs = RuleSet::from_toml(&allow_git_add());
    let w = TargetClass::Worktree;
    assert!(is_allow(&judge(&facts(w, &["git add -A"]), &[&rs])));
    assert!(
        !is_allow(&judge(&facts(w, &["git add -A", "rm"]), &[&rs])),
        "an unmatched second verb must not ride along on an allow"
    );
    let d = RuleSet::from_toml(
        "[[rule]]\ndecision = \"deny\"\nreason = \"no\"\n[rule.match]\ntool = \"rm\"\n",
    );
    assert!(matches!(
        judge(&facts(w, &["git add -A", "rm"]), &[&d]),
        Judgement::Deny(_)
    ));
}

#[test]
#[ignore = "RED(ask-rules): blastguard::askrules does not exist yet"]
fn no_regex_no_raw_text_matching() {
    // `tool` is exact equality, never a pattern.
    let rs = RuleSet::from_toml(
        r#"
[[rule]]
decision = "allow"
reason = "x"
[rule.match]
tool = "r.*"
class = ["worktree"]
"#,
    );
    assert!(!is_allow(&judge(
        &facts(TargetClass::Worktree, &["rm"]),
        &[&rs]
    )));
    // canary so this cannot pass on an inert implementation
    let ok = RuleSet::from_toml(&allow_git_add());
    assert!(is_allow(&judge(
        &facts(TargetClass::Worktree, &["git add -A"]),
        &[&ok]
    )));
}

/// Files whose Allow rules must all be disabled. Each carries a valid allow
/// rule PLUS one defect, and a valid deny rule that must stay active (except
/// the unparsable file, where nothing is usable).
fn defective_files() -> Vec<(&'static str, String)> {
    let good_allow = allow_git_add();
    let deny = "\n[[rule]]\ndecision = \"deny\"\nreason = \"no\"\n[rule.match]\ntool = \"rm\"\n";
    vec![
        (
            "unknown key in match",
            format!("{good_allow}{deny}\n[[rule]]\ndecision=\"allow\"\nreason=\"r\"\n[rule.match]\ntool=\"git\"\nregex=\"x\"\n"),
        ),
        (
            "unknown key in rule",
            format!("{good_allow}{deny}\n[[rule]]\ndecision=\"allow\"\nreason=\"r\"\nprio=1\n[rule.match]\ntool=\"git\"\n"),
        ),
        (
            "unknown top-level key",
            format!("version = 1\n{good_allow}{deny}"),
        ),
        (
            "wrong type",
            format!("{good_allow}{deny}\n[[rule]]\ndecision=\"allow\"\nreason=\"r\"\n[rule.match]\ntool=[\"git\"]\n"),
        ),
        (
            "unknown decision",
            format!("{good_allow}{deny}\n[[rule]]\ndecision=\"maybe\"\nreason=\"r\"\n[rule.match]\ntool=\"git\"\n"),
        ),
        (
            "missing reason",
            format!("{good_allow}{deny}\n[[rule]]\ndecision=\"allow\"\n[rule.match]\ntool=\"git\"\n"),
        ),
        (
            "empty match",
            format!("{good_allow}{deny}\n[[rule]]\ndecision=\"allow\"\nreason=\"r\"\n[rule.match]\n"),
        ),
    ]
}

#[test]
#[ignore = "RED(ask-rules): blastguard::askrules does not exist yet"]
fn defective_file_disables_allow_keeps_deny() {
    let f_allow = facts(TargetClass::Worktree, &["git add -A"]);
    let f_deny = facts(TargetClass::Worktree, &["rm"]);
    // canary: the same allow rule in a clean file is allowed
    let clean = RuleSet::from_toml(&allow_git_add());
    assert!(is_allow(&judge(&f_allow, &[&clean])));
    for (name, text) in defective_files() {
        let rs = RuleSet::from_toml(&text);
        assert!(!rs.allow_enabled(), "{name}: allow must be disabled");
        assert!(!rs.warnings().is_empty(), "{name}: reason must be reported");
        assert!(
            !is_allow(&judge(&f_allow, &[&rs])),
            "{name}: no allow from a defective file"
        );
        assert!(
            matches!(judge(&f_deny, &[&rs]), Judgement::Deny(_)),
            "{name}: valid deny rules stay active"
        );
    }
}

#[test]
#[ignore = "RED(ask-rules): blastguard::askrules does not exist yet"]
fn unparsable_file_allows_nothing_and_says_why() {
    let clean = RuleSet::from_toml(&allow_git_add());
    assert!(is_allow(&judge(
        &facts(TargetClass::Worktree, &["git add -A"]),
        &[&clean]
    )));
    let rs = RuleSet::from_toml("[[rule\nthis is = = not toml");
    assert!(!rs.allow_enabled());
    assert!(!rs.warnings().is_empty());
    let j = judge(&facts(TargetClass::Worktree, &["git add -A"]), &[&rs]);
    assert_eq!(j, Judgement::NoMatch);
}

#[test]
#[ignore = "RED(ask-rules): blastguard::askrules does not exist yet"]
fn empty_rule_set_is_no_match_and_enabled() {
    let rs = RuleSet::from_toml("");
    assert!(rs.warnings().is_empty(), "{:?}", rs.warnings());
    assert_eq!(
        judge(&facts(TargetClass::Worktree, &["git add -A"]), &[&rs]),
        Judgement::NoMatch
    );
    // canary
    let ok = RuleSet::from_toml(&allow_git_add());
    assert!(is_allow(&judge(
        &facts(TargetClass::Worktree, &["git add -A"]),
        &[&ok]
    )));
}

fn tool_rule(t: &str) -> RuleSet {
    RuleSet::from_toml(&format!(
        "[[rule]]\ndecision=\"allow\"\nreason=\"b\"\n[rule.match]\ntool={t:?}\nclass=[\"worktree\",\"scratchpad\",\"none\",\"home\",\"undetermined\"]\n"
    ))
}

const ALLOW_WITH_EXCEPT: &str = r#"
[[rule]]
decision = "allow"
reason = "stage in a worktree"
[rule.match]
tool = "git"
sub = "add"
class = ["worktree"]
except = [ { command_regex = '\s-f\b' }, { verbs_regex = 'push' } ]
"#;

fn with_cmd(verbs: &[&str], cmd: &str) -> Facts {
    facts(TargetClass::Worktree, verbs).with_command(cmd)
}

#[test]
#[ignore = "RED(ask-rules): except / regex not implemented"]
fn except_entry_turns_an_allow_into_no_match() {
    let rs = RuleSet::from_toml(ALLOW_WITH_EXCEPT);
    assert!(rs.allow_enabled(), "{:?}", rs.warnings());
    assert!(is_allow(&judge(
        &with_cmd(&["git add -A"], "git add -A"),
        &[&rs]
    )));
    // except (regex on the raw command) hits: NoMatch, NOT Deny
    assert_eq!(
        judge(&with_cmd(&["git add -A"], "git add -A -f x"), &[&rs]),
        Judgement::NoMatch
    );
    // regex is an unanchored search, not a full match
    assert_eq!(
        judge(&with_cmd(&["git add -A"], "cd x && git add -A -f"), &[&rs]),
        Judgement::NoMatch
    );
}

#[test]
#[ignore = "RED(ask-rules): except / regex not implemented"]
fn deny_table_and_not_learnable_with_regex() {
    let rs = RuleSet::from_toml(
        r#"
[[rule]]
decision = "allow"
reason = "a"
[rule.match]
tool = "git"
class = ["worktree"]

[[deny]]
reason = "env that cds"
[deny.match]
command_regex = '\benv\s+-[^ ]*[CS]'

[[not_learnable]]
reason = "inline shell"
[not_learnable.match]
command_regex = '\b(ba|z|fi)?sh\s+-c\b'
"#,
    );
    assert!(rs.allow_enabled(), "{:?}", rs.warnings());
    let c = "git add -A";
    assert!(is_allow(&judge(&with_cmd(&["git add -A"], c), &[&rs])));
    assert!(matches!(
        judge(&with_cmd(&["git add -A"], "env -C /m git add -A"), &[&rs]),
        Judgement::Deny(_)
    ));
    // not_learnable blocks the allow but is NOT a deny
    assert_eq!(
        judge(&with_cmd(&["git add -A"], "bash -c 'git add -A'"), &[&rs]),
        Judgement::NoMatch
    );
    // regex on structured fields
    let r2 = RuleSet::from_toml(
        "[[deny]]\nreason=\"d\"\n[deny.match]\nverbs_regex='^rm\\b'\nclass_regex='^home$'\n",
    );
    assert!(matches!(
        judge(&facts(TargetClass::Home, &["rm -f"]), &[&r2]),
        Judgement::Deny(_)
    ));
    assert_eq!(
        judge(&facts(TargetClass::Worktree, &["rm -f"]), &[&r2]),
        Judgement::NoMatch
    );
}

#[test]
#[ignore = "RED(ask-rules): except / regex not implemented"]
fn regex_in_an_allow_match_disables_allow_for_the_file() {
    for key in ["command_regex", "verbs_regex", "rule_regex", "class_regex"] {
        let text = format!(
            "{}\n[[rule]]\ndecision=\"allow\"\nreason=\"r\"\n[rule.match]\ntool=\"git\"\n{key}=\"x\"\n\n[[deny]]\nreason=\"d\"\n[deny.match]\ntool=\"rm\"\n",
            allow_git_add()
        );
        let rs = RuleSet::from_toml(&text);
        assert!(!rs.allow_enabled(), "{key}");
        assert!(!rs.warnings().is_empty(), "{key}");
        assert!(!is_allow(&judge(
            &with_cmd(&["git add -A"], "git add -A"),
            &[&rs]
        )));
        assert!(matches!(
            judge(&with_cmd(&["rm"], "rm"), &[&rs]),
            Judgement::Deny(_)
        ));
    }
}

#[test]
#[ignore = "RED(ask-rules): except / regex not implemented"]
fn broken_or_unevaluable_restrictive_regex_counts_as_matched() {
    // canary: clean file allows
    let ok = RuleSet::from_toml(&allow_git_add());
    assert!(is_allow(&judge(
        &with_cmd(&["git add -A"], "git add -A"),
        &[&ok]
    )));
    // broken regex in `except` blocks its allow (matched), and is reported
    let rs = RuleSet::from_toml(&allow_git_add().replace(
        "flags_all = [\"-A\"]",
        "flags_all = [\"-A\"]\nexcept = [ { command_regex = '(' } ]",
    ));
    assert!(!rs.warnings().is_empty(), "the bad regex must be reported");
    assert!(!is_allow(&judge(
        &with_cmd(&["git add -A"], "git add -A"),
        &[&rs]
    )));
    // broken regex in [[deny]] counts as matched -> Deny
    let d = RuleSet::from_toml("[[deny]]\nreason=\"d\"\n[deny.match]\ncommand_regex='('\n");
    assert!(!d.warnings().is_empty());
    assert!(matches!(
        judge(&with_cmd(&["git add -A"], "git add -A"), &[&d]),
        Judgement::Deny(_)
    ));
    // command_regex but Facts carry no command: counts as matched
    let rs = RuleSet::from_toml(ALLOW_WITH_EXCEPT);
    assert!(!is_allow(&judge(
        &facts(TargetClass::Worktree, &["git add -A"]),
        &[&rs]
    )));
}

/// Positive allow-list (spec "Positive allow-list"): only fully modelled
/// tools in a confirmed class can match an Allow rule, however broad.
#[test]
#[ignore = "RED(ask-rules): positive allow-list not implemented"]
fn only_modelled_tools_and_confirmed_classes_are_allow_eligible() {
    let g = tool_rule("git");
    assert!(is_allow(&judge(
        &facts(TargetClass::Worktree, &["git add -A"]),
        &[&g]
    )));
    for v in ["rm -f -r", "mv", "cp", "mkdir"] {
        let t = v.split(' ').next().unwrap();
        let rs = tool_rule(t);
        assert!(
            is_allow(&judge(&facts(TargetClass::Worktree, &[v]), &[&rs])),
            "{v} is modelled"
        );
    }
    for v in [
        "env git add -A",
        "env -C git add",
        "sudo rm",
        "timeout git add -A",
        "nice git add -A",
        "nohup git add -A",
        "exec git add -A",
        "command git add -A",
        "xargs rm",
        "find -exec",
        "bash -c #26610971178c87de",
        "sh -c #26610971178c87de",
        "fish #26610971178c87de",
        "pwsh #26610971178c87de",
        "frobnicate",
    ] {
        let t = v.split(' ').next().unwrap();
        let rs = tool_rule(t);
        let j = judge(&facts(TargetClass::Worktree, &[v]), &[&rs]);
        assert!(!is_allow(&j), "`{v}` must not be allow-eligible: {j:?}");
        // a Deny rule on the same tool still matches
        let d = RuleSet::from_toml(&format!(
            "[[rule]]\ndecision=\"deny\"\nreason=\"d\"\n[rule.match]\ntool={t:?}\n"
        ));
        assert!(matches!(
            judge(&facts(TargetClass::Worktree, &[v]), &[&d]),
            Judgement::Deny(_)
        ));
    }
    // the class must be confirmed none / worktree / scratchpad
    for c in [TargetClass::Home, TargetClass::Undetermined] {
        let j = judge(&facts(c, &["git add -A"]), &[&g]);
        assert!(!is_allow(&j), "{c:?} is not a confirmed class: {j:?}");
    }
    for c in [TargetClass::None, TargetClass::Scratchpad] {
        assert!(is_allow(&judge(&facts(c, &["git add -A"]), &[&g])), "{c:?}");
    }
    // interpreters need a pinned program hash
    let py = tool_rule("python3");
    assert!(is_allow(&judge(
        &facts(TargetClass::Worktree, &["python3 #26610971178c87de"]),
        &[&py]
    )));
    assert!(!is_allow(&judge(
        &facts(TargetClass::Worktree, &["python3"]),
        &[&py]
    )));
}
