//! The rule-file schema, its strict parser and the matcher.
//!
//! One format serves the human file (`ask-rules.toml`) and, converted by
//! [`RuleSet::from_learned`], the learned table.
//!
//! # Strictness (invariant (c))
//!
//! An unknown key (top level, `[[rule]]`, `match`), a wrong type, an unknown
//! `decision` or class label, a missing or empty `reason`, an empty `match`, a
//! regex in an allow `match`, an unparsable file, or an unreadable existing
//! file disables ALL Allow rules of THAT file ([`RuleSet::allow_enabled`] is
//! `false`) and says why in [`RuleSet::warnings`]. A defective rule is skipped;
//! the other rules of the file are still read, and a deny rule that itself
//! parses stays active. A file that cannot be parsed at all yields no rule. An
//! absent file is not an error.
//!
//! # Regex
//!
//! `*_regex` keys exist only on the restrictive side (`deny`, `not_learnable`,
//! and `except` entries): a regex is a guess, so it may never create or widen
//! an Allow. A regex that does not compile counts as MATCHED, as does a
//! `command_regex` evaluated on facts that carry no command.

use super::{Facts, Verb};
use harness_core::asklearn::{LearnedTable, Verdict};
use regex::Regex;
use std::path::Path;

/// The class labels a rule file may spell (the shape tag `s3-` removed).
const CLASS_LABELS: &[&str] = &[
    "worktree",
    "scratchpad",
    "protected-config",
    "system",
    "main-tree",
    "home",
    "undetermined",
    "none",
];

/// A regex condition. `re == None`: the pattern did not compile, which counts
/// as matched.
#[derive(Debug, Clone)]
struct Rx {
    re: Option<Regex>,
}

impl Rx {
    fn compile(src: &str, what: &str, warnings: &mut Vec<String>) -> Rx {
        match Regex::new(src) {
            Ok(re) => Rx { re: Some(re) },
            Err(e) => {
                warnings.push(format!(
                    "`{what} = {src:?}` does not compile ({e}); it counts as matched"
                ));
                Rx { re: None }
            }
        }
    }

    /// Unanchored search. No text to search in, or a pattern that never
    /// compiled, counts as matched.
    fn hits(&self, text: Option<&str>) -> bool {
        match (&self.re, text) {
            (Some(re), Some(t)) => re.is_match(t),
            _ => true,
        }
    }
}

/// One condition: every given key must hold.
#[derive(Debug, Clone, Default)]
struct Cond {
    rule: Option<String>,
    class: Option<Vec<String>>,
    tool: Option<String>,
    sub: Option<String>,
    flags_all: Option<Vec<String>>,
    flags_any: Option<Vec<String>>,
    flags_none: Option<Vec<String>>,
    program: Option<String>,
    rule_regex: Option<Rx>,
    class_regex: Option<Rx>,
    verbs_regex: Option<Rx>,
    command_regex: Option<Rx>,
    /// Exact canonical shape key (the learned table's entries).
    shape_key: Option<String>,
}

impl Cond {
    fn has_regex(&self) -> bool {
        self.rule_regex.is_some()
            || self.class_regex.is_some()
            || self.verbs_regex.is_some()
            || self.command_regex.is_some()
    }

    fn has_verb_keys(&self) -> bool {
        self.tool.is_some()
            || self.sub.is_some()
            || self.flags_all.is_some()
            || self.flags_any.is_some()
            || self.flags_none.is_some()
            || self.program.is_some()
    }

    /// The keys that are about the shape as a whole.
    fn shape_keys_hold(&self, f: &Facts) -> bool {
        if let Some(r) = &self.rule {
            if *r != f.rule {
                return false;
            }
        }
        if let Some(cs) = &self.class {
            if !cs.iter().any(|c| c == f.class_label()) {
                return false;
            }
        }
        if let Some(k) = &self.shape_key {
            if *k != f.key {
                return false;
            }
        }
        for (rx, text) in [
            (&self.rule_regex, Some(f.rule.as_str())),
            (&self.class_regex, Some(f.class_label())),
            (&self.verbs_regex, Some(f.verbs_joined.as_str())),
            (&self.command_regex, f.command.as_deref()),
        ] {
            if let Some(rx) = rx {
                if !rx.hits(text) {
                    return false;
                }
            }
        }
        true
    }

    /// The keys that constrain one verb.
    fn verb_holds(&self, v: &Verb) -> bool {
        if let Some(t) = &self.tool {
            if *t != v.tool {
                return false;
            }
        }
        if let Some(s) = &self.sub {
            if v.sub.as_deref() != Some(s.as_str()) {
                return false;
            }
        }
        let has = |f: &String| v.flags.contains(f);
        if let Some(all) = &self.flags_all {
            if !all.iter().all(has) {
                return false;
            }
        }
        if let Some(any) = &self.flags_any {
            if !any.iter().any(has) {
                return false;
            }
        }
        if let Some(none) = &self.flags_none {
            if none.iter().any(has) {
                return false;
            }
        }
        if let Some(p) = &self.program {
            if v.program.as_deref() != Some(p.as_str()) {
                return false;
            }
        }
        true
    }

    /// Broad matching, for Deny / `not_learnable` / `except`: ANY verb may
    /// satisfy the verb keys.
    fn matches_any_verb(&self, f: &Facts) -> bool {
        self.shape_keys_hold(f)
            && (!self.has_verb_keys() || f.verbs.iter().any(|v| self.verb_holds(v)))
    }

    /// Narrow matching, for Allow: EVERY verb must satisfy the verb keys.
    fn matches_every_verb(&self, f: &Facts) -> bool {
        self.shape_keys_hold(f)
            && (!self.has_verb_keys()
                || (!f.verbs.is_empty() && f.verbs.iter().all(|v| self.verb_holds(v))))
    }
}

#[derive(Debug, Clone)]
struct Allow {
    reason: String,
    cond: Cond,
    except: Vec<Cond>,
}

#[derive(Debug, Clone)]
struct Deny {
    reason: String,
    cond: Cond,
}

/// A parsed rule file. Constructing one never fails.
#[derive(Debug, Clone)]
pub struct RuleSet {
    allow: Vec<Allow>,
    deny: Vec<Deny>,
    not_learnable: Vec<Cond>,
    warnings: Vec<String>,
    allow_enabled: bool,
}

/// What kind of table is being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Rule,
    Deny,
    NotLearnable,
}

impl RuleSet {
    fn empty() -> RuleSet {
        RuleSet {
            allow: Vec::new(),
            deny: Vec::new(),
            not_learnable: Vec::new(),
            warnings: Vec::new(),
            allow_enabled: true,
        }
    }

    /// A defect: say why and disable this file's Allow rules.
    fn defect(&mut self, why: String) {
        self.warnings.push(why);
        self.allow_enabled = false;
    }

    /// Why the file was (partly) refused; empty for a clean file.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// `false` when a defect disabled every Allow rule of this file.
    pub fn allow_enabled(&self) -> bool {
        self.allow_enabled
    }

    /// Read the rule file at `path`. An absent file is no rules; a file that
    /// exists but cannot be read (a directory, a permission error, not UTF-8)
    /// is a defect: its Allow rules are disabled and the reason is reported.
    pub fn from_file(path: &Path) -> RuleSet {
        use harness_core::verdict::Determination;
        match harness_core::boundary::read_to_string(path) {
            Determination::Known(Some(text)) => {
                let mut rs = RuleSet::from_toml(&text);
                for w in &mut rs.warnings {
                    *w = format!("{}: {w}", path.display());
                }
                rs
            }
            Determination::Known(None) => RuleSet::empty(),
            Determination::Undetermined(why) => {
                let mut rs = RuleSet::empty();
                rs.defect(format!(
                    "{}: unreadable rules file ({})",
                    path.display(),
                    why.as_str()
                ));
                rs
            }
        }
    }

    /// Parse rule-file text (see the module docs for the format and the
    /// strictness rules).
    pub fn from_toml(text: &str) -> RuleSet {
        let mut rs = RuleSet::empty();
        let table: toml::Table = match toml::from_str(text) {
            Ok(t) => t,
            Err(e) => {
                rs.defect(format!(
                    "not parsable as TOML, no rule of this file is usable: {}",
                    e.message()
                ));
                return rs;
            }
        };
        for (key, value) in &table {
            let kind = match key.as_str() {
                "rule" => Kind::Rule,
                "deny" => Kind::Deny,
                "not_learnable" => Kind::NotLearnable,
                other => {
                    rs.defect(format!("unknown top-level key `{other}`"));
                    continue;
                }
            };
            let Some(items) = value.as_array() else {
                rs.defect(format!("`{key}` must be an array of tables ([[{key}]])"));
                continue;
            };
            for (i, item) in items.iter().enumerate() {
                let label = format!("[[{key}]] #{}", i + 1);
                match rs.parse_item(kind, item) {
                    Ok(()) => {}
                    Err(why) => rs.defect(format!("{label}: {why}")),
                }
            }
        }
        rs
    }

    fn parse_item(&mut self, kind: Kind, item: &toml::Value) -> Result<(), String> {
        let t = item.as_table().ok_or("not a table")?;
        let mut reason: Option<String> = None;
        let mut decision: Option<String> = None;
        let mut matcher: Option<&toml::Value> = None;
        for (k, v) in t {
            match k.as_str() {
                "reason" => {
                    reason = Some(v.as_str().ok_or("`reason` must be a string")?.to_string())
                }
                "decision" if kind == Kind::Rule => {
                    decision = Some(v.as_str().ok_or("`decision` must be a string")?.to_string())
                }
                "match" => matcher = Some(v),
                other => return Err(format!("unknown key `{other}`")),
            }
        }
        let reason = match reason {
            Some(r) if !r.trim().is_empty() => r,
            _ => return Err("missing or empty `reason`".into()),
        };
        let is_allow = match (kind, decision.as_deref()) {
            (Kind::Rule, Some("allow")) => true,
            (Kind::Rule, Some("deny")) => false,
            (Kind::Rule, Some(other)) => return Err(format!("unknown decision {other:?}")),
            (Kind::Rule, None) => return Err("missing `decision`".into()),
            _ => false,
        };
        let m = matcher
            .ok_or("missing `match`")?
            .as_table()
            .ok_or("`match` must be a table")?;
        let mut warnings = Vec::new();
        let mut defects = Vec::new();
        let (cond, except) = parse_match(m, is_allow, &mut warnings, &mut defects)?;
        self.warnings.append(&mut warnings);
        // A restrictive rule keeps the keys it could read (broader = safer);
        // what it could not read still disables this file's Allow rules.
        for d in defects {
            self.defect(format!("{kind:?} rule: {d}"));
        }
        match kind {
            Kind::Rule if is_allow => self.allow.push(Allow {
                reason: format!("ask-rules: {reason}"),
                cond,
                except,
            }),
            Kind::Rule | Kind::Deny => self.deny.push(Deny {
                reason: format!(
                    "blastguard ask-rules: denied by a rule \u{2014} {reason}. The original \
                     question follows."
                ),
                cond,
            }),
            Kind::NotLearnable => self.not_learnable.push(cond),
        }
        Ok(())
    }

    /// The learned table as a rule set: one exact-shape rule per entry. An
    /// Allow entry that records a decline is inconsistent and not honoured.
    /// Entries of another schema never equal a current key, so they are inert.
    pub fn from_learned(table: &LearnedTable) -> RuleSet {
        let mut rs = RuleSet::empty();
        for e in &table.entries {
            let cond = Cond {
                shape_key: Some(e.shape.clone()),
                ..Cond::default()
            };
            match e.decision {
                Verdict::Allow if e.declines == 0 => rs.allow.push(Allow {
                    reason: format!(
                        "learned: approved {} times across {} sessions, {} declines \u{2014} shape {}",
                        e.approvals, e.sessions, e.declines, e.shape
                    ),
                    cond,
                    except: Vec::new(),
                }),
                Verdict::Allow => {}
                Verdict::Deny => rs.deny.push(Deny {
                    reason: format!(
                        "blastguard: learned deny \u{2014} a human declined this shape {} time(s) \
                         (approved {} times across {} sessions) \u{2014} shape {}. The original \
                         question follows.",
                        e.declines, e.approvals, e.sessions, e.shape
                    ),
                    cond,
                }),
            }
        }
        rs
    }

    pub(super) fn matching_deny(&self, f: &Facts) -> Option<String> {
        self.deny
            .iter()
            .find(|d| d.cond.matches_any_verb(f))
            .map(|d| d.reason.clone())
    }

    pub(super) fn blocks_learning(&self, f: &Facts) -> bool {
        self.not_learnable.iter().any(|c| c.matches_any_verb(f))
    }

    pub(super) fn matching_allow(&self, f: &Facts) -> Option<String> {
        if !self.allow_enabled {
            return None;
        }
        self.allow
            .iter()
            .find(|a| {
                a.cond.matches_every_verb(f) && !a.except.iter().any(|x| x.matches_any_verb(f))
            })
            .map(|a| a.reason.clone())
    }
}

const STRING_KEYS: &[&str] = &["rule", "tool", "sub", "program"];
const LIST_KEYS: &[&str] = &["class", "flags_all", "flags_any", "flags_none"];
const REGEX_KEYS: &[&str] = &["rule_regex", "class_regex", "verbs_regex", "command_regex"];

/// Parse one `match` table. `is_allow`: the table belongs to an allow rule
/// (no regex; `except` permitted).
fn parse_match(
    m: &toml::Table,
    is_allow: bool,
    warnings: &mut Vec<String>,
    defects: &mut Vec<String>,
) -> Result<(Cond, Vec<Cond>), String> {
    let mut except = Vec::new();
    let mut body = toml::Table::new();
    for (k, v) in m {
        if k == "except" {
            if !is_allow {
                return Err("`except` is only valid on an allow rule".into());
            }
            let list = v.as_array().ok_or("`except` must be an array of tables")?;
            for e in list {
                let et = e.as_table().ok_or("an `except` entry must be a table")?;
                if et.contains_key("except") {
                    return Err("`except` entries cannot nest".into());
                }
                except.push(parse_cond(et, false, None, warnings)?);
            }
        } else {
            body.insert(k.clone(), v.clone());
        }
    }
    let lenient = if is_allow { None } else { Some(defects) };
    let cond = parse_cond(&body, is_allow, lenient, warnings)?;
    Ok((cond, except))
}

/// Parse the condition keys of `t`. `forbid_regex`: a permissive-side table.
///
/// `lenient: None` is strict: the first unreadable key fails the table.
/// `Some(defects)` (restrictive tables only) drops an unreadable key, records
/// it in `defects` and keeps the rest: a deny / `not_learnable` that lost a
/// condition only gets broader, which is the restrictive direction. A table
/// with no readable key left is still an error (it would match everything).
fn parse_cond(
    t: &toml::Table,
    forbid_regex: bool,
    mut lenient: Option<&mut Vec<String>>,
    warnings: &mut Vec<String>,
) -> Result<Cond, String> {
    if t.is_empty() {
        return Err("empty `match` (at least one key is required)".into());
    }
    let mut c = Cond::default();
    let mut read = 0usize;
    for (k, v) in t {
        match set_key(&mut c, k, v, forbid_regex, warnings) {
            Ok(()) => read += 1,
            Err(e) => match lenient.as_deref_mut() {
                Some(d) => d.push(e),
                None => return Err(e),
            },
        }
    }
    if read == 0 {
        return Err("no readable key in `match`".into());
    }
    debug_assert!(!forbid_regex || !c.has_regex());
    Ok(c)
}

fn set_key(
    c: &mut Cond,
    k: &str,
    v: &toml::Value,
    forbid_regex: bool,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    {
        let key = k;
        if STRING_KEYS.contains(&key) {
            let s = v
                .as_str()
                .ok_or_else(|| format!("`{key}` must be a string"))?
                .to_string();
            match key {
                "rule" => c.rule = Some(s),
                "tool" => c.tool = Some(s),
                "sub" => c.sub = Some(s),
                _ => c.program = Some(s),
            }
        } else if LIST_KEYS.contains(&key) {
            let arr = v
                .as_array()
                .ok_or_else(|| format!("`{key}` must be an array of strings"))?;
            let list: Vec<String> = arr
                .iter()
                .map(|x| x.as_str().map(str::to_string))
                .collect::<Option<_>>()
                .ok_or_else(|| format!("`{key}` must be an array of strings"))?;
            if key == "class" {
                if let Some(bad) = list.iter().find(|l| !CLASS_LABELS.contains(&l.as_str())) {
                    return Err(format!("unknown class label {bad:?}"));
                }
                c.class = Some(list);
            } else {
                match key {
                    "flags_all" => c.flags_all = Some(list),
                    "flags_any" => c.flags_any = Some(list),
                    _ => c.flags_none = Some(list),
                }
            }
        } else if REGEX_KEYS.contains(&key) {
            if forbid_regex {
                return Err(format!(
                    "`{key}` is not allowed in an allow `match` (a regex may only restrict)"
                ));
            }
            let s = v
                .as_str()
                .ok_or_else(|| format!("`{key}` must be a string"))?;
            let rx = Rx::compile(s, key, warnings);
            match key {
                "rule_regex" => c.rule_regex = Some(rx),
                "class_regex" => c.class_regex = Some(rx),
                "verbs_regex" => c.verbs_regex = Some(rx),
                _ => c.command_regex = Some(rx),
            }
        } else {
            return Err(format!("unknown key `{key}` in `match`"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::askrules::class_label;
    use harness_core::asklearn::TargetClass;

    #[test]
    fn class_labels_match_the_shape_schema() {
        for l in CLASS_LABELS {
            let c = TargetClass::parse(&format!("s3-{l}")).unwrap();
            assert_eq!(class_label(c), *l);
        }
    }
}
