//! `[capabilities.<name>]` of the decision record (rulings D2): the schema,
//! its parser, the canonical JSON used for history rows and input hashes,
//! and the D2-trust digest of the `command` capabilities.
//!
//! ```toml
//! [capabilities.blender-headless]
//! relevance = "relevant"            # or "irrelevant"; missing = undecided (decide check exit 1)
//! how = "Run scripts/make.py in headless Blender."
//! done_when = "A GLB and a drawn render are produced."
//! [capabilities.blender-headless.verify]
//! kind = "builtin:blender-headless" # closed set, see `Verify`
//! script = "scripts/make.py"        # root-relative
//! glb = "out/model.glb"             # root-relative output
//! render = "out/render.png"         # root-relative output
//! inputs = ["assets/**"]            # optional globs (input hash only)
//! ```
//!
//! The table is kept raw in [`super::record::Decisions`] (so a broken table
//! can be told apart from a missing key) and parsed with [`parse`]:
//!
//! * name not matching `^[a-z0-9][a-z0-9-]*$`, a key other than
//!   `relevance` / `how` / `done_when` / `verify`, a value of the wrong type,
//!   an unknown `verify.kind`, a missing or mistyped required `verify` key, or
//!   a `verify` key the kind does not take → `Err` (the record is unusable:
//!   `decide check` exit 2);
//! * `relevance`, `how`, `done_when` or `verify` simply absent (or `how` /
//!   `done_when` blank) → `Ok` with `None`; [`Capability::pending`] lists
//!   them for a relevant capability (`decide check` exit 1).

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use super::record::Relevance;

/// `verify.kind` of a headless Blender run.
pub const KIND_BLENDER: &str = "builtin:blender-headless";
/// `verify.kind` of a three.js page rendered in headless Chromium.
pub const KIND_THREEJS: &str = "builtin:threejs-render";
/// `verify.kind` of a trusted command.
pub const KIND_COMMAND: &str = "command";
/// The closed set of `verify.kind` values (rulings D2).
pub const KINDS: [&str; 3] = [KIND_BLENDER, KIND_THREEJS, KIND_COMMAND];
/// Default `selector` of a `builtin:threejs-render` capability.
pub const DEFAULT_SELECTOR: &str = "canvas";
/// History `area` of a capability change (rulings D2 / D8).
pub const HISTORY_AREA: &str = "capability";

/// How a capability is verified on the real machine (rulings D2 / D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verify {
    /// `builtin:blender-headless`.
    Blender {
        /// Root-relative Python script run by Blender.
        script: String,
        /// Root-relative GLB output path.
        glb: String,
        /// Root-relative PNG output path.
        render: String,
        /// Optional extra input globs (input hash only).
        inputs: Vec<String>,
    },
    /// `builtin:threejs-render`.
    Threejs {
        /// Dev-server path starting with `/`.
        page: String,
        /// CSS selector of the element screenshotted (default `canvas`).
        selector: String,
        /// Optional extra input globs (input hash only).
        inputs: Vec<String>,
    },
    /// `command` (needs D2-trust).
    Command {
        /// Non-empty argv with a non-blank program.
        argv: Vec<String>,
        /// Non-empty input globs.
        inputs: Vec<String>,
    },
}

impl Verify {
    /// The `verify.kind` string.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Verify::Blender { .. } => KIND_BLENDER,
            Verify::Threejs { .. } => KIND_THREEJS,
            Verify::Command { .. } => KIND_COMMAND,
        }
    }
}

/// One parsed `[capabilities.<name>]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    /// `None` = not yet judged.
    pub relevance: Option<Relevance>,
    /// `None` = missing or blank.
    pub how: Option<String>,
    /// `None` = missing or blank.
    pub done_when: Option<String>,
    /// `None` = missing.
    pub verify: Option<Verify>,
}

impl Capability {
    /// Is the capability judged relevant?
    #[must_use]
    pub fn is_relevant(&self) -> bool {
        self.relevance == Some(Relevance::Relevant)
    }

    /// The keys a capability still needs: `relevance` when undecided; for a
    /// relevant capability also `how`, `done_when` and `verify` when missing.
    #[must_use]
    pub fn pending(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        match self.relevance {
            None => out.push("relevance"),
            Some(Relevance::Irrelevant) => {}
            Some(Relevance::Relevant) => {
                if self.how.is_none() {
                    out.push("how");
                }
                if self.done_when.is_none() {
                    out.push("done_when");
                }
                if self.verify.is_none() {
                    out.push("verify");
                }
            }
        }
        out
    }
}

/// Is `name` a valid capability name (`^[a-z0-9][a-z0-9-]*$`)?
#[must_use]
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn opt_text(t: &toml::Table, key: &str, name: &str) -> Result<Option<String>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(format!(
            "capabilities.{name}.{key} must be a string, got a {}",
            other.type_str()
        )),
    }
}

fn req_str(t: &toml::Table, key: &str, name: &str) -> Result<String, String> {
    match t.get(key) {
        None => Err(format!(
            "capabilities.{name}.verify.{key} is required for kind `{}`",
            t.get("kind").and_then(toml::Value::as_str).unwrap_or("?")
        )),
        Some(toml::Value::String(s)) if s.trim().is_empty() => Err(format!(
            "capabilities.{name}.verify.{key} must be a non-blank string"
        )),
        Some(toml::Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(format!(
            "capabilities.{name}.verify.{key} must be a string, got a {}",
            other.type_str()
        )),
    }
}

/// A root-relative path: not absolute, no `..` component.
fn rel_path(t: &toml::Table, key: &str, name: &str) -> Result<String, String> {
    let p = req_str(t, key, name)?;
    let path = std::path::Path::new(&p);
    if path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!(
            "capabilities.{name}.verify.{key} = {p:?} must be a path relative to the project root without `..`"
        ));
    }
    Ok(p)
}

fn str_array(v: &toml::Value, what: &str) -> Result<Vec<String>, String> {
    let toml::Value::Array(a) = v else {
        return Err(format!(
            "{what} must be an array of strings, got a {}",
            v.type_str()
        ));
    };
    let mut out = Vec::with_capacity(a.len());
    for x in a {
        let toml::Value::String(s) = x else {
            return Err(format!(
                "{what} must be an array of strings, got an element of type {}",
                x.type_str()
            ));
        };
        out.push(s.clone());
    }
    Ok(out)
}

/// `inputs`: absent → `[]` unless `required`; otherwise a (non-empty when
/// required) array of non-blank globs that compile.
fn inputs(t: &toml::Table, name: &str, required: bool) -> Result<Vec<String>, String> {
    let what = format!("capabilities.{name}.verify.inputs");
    let globs = match t.get("inputs") {
        None if required => return Err(format!("{what} is required for kind `command`")),
        None => return Ok(Vec::new()),
        Some(v) => str_array(v, &what)?,
    };
    if required && globs.is_empty() {
        return Err(format!(
            "{what} must not be empty (an empty input set cannot be hashed)"
        ));
    }
    for g in &globs {
        if g.trim().is_empty() {
            return Err(format!("{what} has a blank glob"));
        }
        if let Err(e) = globset::Glob::new(g) {
            return Err(format!("{what}: glob {g:?} does not compile: {e}"));
        }
    }
    Ok(globs)
}

fn only_keys(t: &toml::Table, allowed: &[&str], name: &str, kind: &str) -> Result<(), String> {
    for k in t.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(format!(
                "capabilities.{name}.verify.{k} is not a key of kind `{kind}` (allowed: {})",
                allowed.join(", ")
            ));
        }
    }
    Ok(())
}

fn parse_verify(v: &toml::Value, name: &str) -> Result<Verify, String> {
    let toml::Value::Table(t) = v else {
        return Err(format!(
            "capabilities.{name}.verify must be a table, got a {}",
            v.type_str()
        ));
    };
    let kind = match t.get("kind") {
        None => {
            return Err(format!(
                "capabilities.{name}.verify.kind is missing (one of {})",
                KINDS.join(", ")
            ))
        }
        Some(toml::Value::String(k)) => k.as_str(),
        Some(other) => {
            return Err(format!(
                "capabilities.{name}.verify.kind must be a string, got a {}",
                other.type_str()
            ))
        }
    };
    match kind {
        KIND_BLENDER => {
            only_keys(
                t,
                &["kind", "script", "glb", "render", "inputs"],
                name,
                kind,
            )?;
            Ok(Verify::Blender {
                script: rel_path(t, "script", name)?,
                glb: rel_path(t, "glb", name)?,
                render: rel_path(t, "render", name)?,
                inputs: inputs(t, name, false)?,
            })
        }
        KIND_THREEJS => {
            only_keys(t, &["kind", "page", "selector", "inputs"], name, kind)?;
            let page = req_str(t, "page", name)?;
            if !page.starts_with('/') || page.chars().any(char::is_whitespace) {
                return Err(format!(
                    "capabilities.{name}.verify.page = {page:?} must start with `/` and contain no whitespace"
                ));
            }
            let selector = match t.get("selector") {
                None => DEFAULT_SELECTOR.to_string(),
                Some(_) => req_str(t, "selector", name)?,
            };
            Ok(Verify::Threejs {
                page,
                selector,
                inputs: inputs(t, name, false)?,
            })
        }
        KIND_COMMAND => {
            only_keys(t, &["kind", "argv", "inputs"], name, kind)?;
            let what = format!("capabilities.{name}.verify.argv");
            let argv = match t.get("argv") {
                None => return Err(format!("{what} is required for kind `command`")),
                Some(v) => str_array(v, &what)?,
            };
            match argv.first() {
                None => return Err(format!("{what} must not be empty")),
                Some(p) if p.trim().is_empty() => {
                    return Err(format!("{what}[0] (the program) must not be blank"))
                }
                Some(_) => {}
            }
            Ok(Verify::Command {
                argv,
                inputs: inputs(t, name, true)?,
            })
        }
        other => Err(format!(
            "capabilities.{name}.verify.kind = {other:?} is not one of {}",
            KINDS.join(", ")
        )),
    }
}

/// Parse one raw `[capabilities.<name>]` value (see the module docs).
pub fn parse(name: &str, raw: &toml::Value) -> Result<Capability, String> {
    if !valid_name(name) {
        return Err(format!(
            "capability name {name:?} does not match ^[a-z0-9][a-z0-9-]*$"
        ));
    }
    let toml::Value::Table(t) = raw else {
        return Err(format!(
            "capabilities.{name} must be a table, got a {}",
            raw.type_str()
        ));
    };
    for k in t.keys() {
        if !["relevance", "how", "done_when", "verify"].contains(&k.as_str()) {
            return Err(format!(
                "capabilities.{name}.{k} is not a capability key (relevance, how, done_when, verify)"
            ));
        }
    }
    let relevance = match t.get("relevance") {
        None => None,
        Some(toml::Value::String(s)) if s == "relevant" => Some(Relevance::Relevant),
        Some(toml::Value::String(s)) if s == "irrelevant" => Some(Relevance::Irrelevant),
        Some(other) => {
            return Err(format!(
                "capabilities.{name}.relevance must be \"relevant\" or \"irrelevant\", got {other}"
            ))
        }
    };
    let verify = match t.get("verify") {
        None => None,
        Some(v) => Some(parse_verify(v, name)?),
    };
    Ok(Capability {
        relevance,
        how: opt_text(t, "how", name)?,
        done_when: opt_text(t, "done_when", name)?,
        verify,
    })
}

/// Parse every capability of a record; the first failure is `Err`.
pub fn parse_all(
    raw: &BTreeMap<String, toml::Value>,
) -> Result<BTreeMap<String, Capability>, String> {
    raw.iter()
        .map(|(n, v)| parse(n, v).map(|c| (n.clone(), c)))
        .collect()
}

/// Canonical JSON of a TOML value: objects with keys in byte order, no
/// whitespace. Used for history rows and the D5 input hash.
#[must_use]
pub fn canonical_json(v: &toml::Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

fn write_canonical(v: &toml::Value, out: &mut String) {
    match v {
        toml::Value::String(s) => out.push_str(&json_str(s)),
        toml::Value::Integer(i) => out.push_str(&i.to_string()),
        toml::Value::Float(f) => out.push_str(&serde_json::Value::from(*f).to_string()),
        toml::Value::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
        toml::Value::Datetime(d) => out.push_str(&json_str(&d.to_string())),
        toml::Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        toml::Value::Table(t) => {
            let sorted: BTreeMap<&String, &toml::Value> = t.iter().collect();
            out.push('{');
            for (i, (k, x)) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&json_str(k));
                out.push(':');
                write_canonical(x, out);
            }
            out.push('}');
        }
    }
}

/// The D2-trust digest: sha256 hex of the compact JSON array
/// `[[name, argv], …]` of every `command` capability, in name order. `None`
/// when there is no `command` capability (no digest is required then).
#[must_use]
pub fn command_digest(caps: &BTreeMap<String, Capability>) -> Option<String> {
    let pairs: Vec<serde_json::Value> = caps
        .iter()
        .filter_map(|(name, c)| match &c.verify {
            Some(Verify::Command { argv, .. }) => Some(serde_json::json!([name, argv])),
            _ => None,
        })
        .collect();
    if pairs.is_empty() {
        return None;
    }
    let text = serde_json::Value::Array(pairs).to_string();
    Some(
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

/// Arguments of `sitecraft decide capability`.
#[derive(Debug, Clone, Default)]
pub struct CapabilityArgs {
    /// `--relevance`.
    pub relevance: String,
    /// `--how`.
    pub how: Option<String>,
    /// `--done-when`.
    pub done_when: Option<String>,
    /// `--verify-kind`.
    pub verify_kind: Option<String>,
    /// `--script` (blender-headless).
    pub script: Option<String>,
    /// `--glb` (blender-headless).
    pub glb: Option<String>,
    /// `--render` (blender-headless).
    pub render: Option<String>,
    /// `--page` (threejs-render).
    pub page: Option<String>,
    /// `--selector` (threejs-render).
    pub selector: Option<String>,
    /// `--argv` (command), in order.
    pub argv: Vec<String>,
    /// `--input` (any kind; required for command).
    pub inputs: Vec<String>,
}

/// Build the raw `[capabilities.<name>]` table from CLI arguments and check
/// it with [`parse`]. A relevant capability needs `--how`, `--done-when`
/// and `--verify-kind`; an argument the kind does not take is `Err`.
pub fn table_from_args(name: &str, a: &CapabilityArgs) -> Result<toml::Value, String> {
    if !valid_name(name) {
        return Err(format!(
            "--name {name:?} does not match ^[a-z0-9][a-z0-9-]*$"
        ));
    }
    let mut t = toml::Table::new();
    t.insert("relevance".into(), toml::Value::String(a.relevance.clone()));
    let relevant = a.relevance == "relevant";
    for (key, flag, v) in [
        ("how", "--how", &a.how),
        ("done_when", "--done-when", &a.done_when),
    ] {
        match v {
            Some(s) if !s.trim().is_empty() => {
                t.insert(key.into(), toml::Value::String(s.clone()));
            }
            Some(_) => return Err(format!("{flag} must be non-blank")),
            None if relevant => {
                return Err(format!("{flag} is required for a relevant capability"))
            }
            None => {}
        }
    }
    let kind_given = a.verify_kind.is_some();
    let any_verify_arg = a.script.is_some()
        || a.glb.is_some()
        || a.render.is_some()
        || a.page.is_some()
        || a.selector.is_some()
        || !a.argv.is_empty()
        || !a.inputs.is_empty();
    if !kind_given && relevant {
        return Err("--verify-kind is required for a relevant capability".to_string());
    }
    if !kind_given && any_verify_arg {
        return Err("verify arguments need --verify-kind".to_string());
    }
    if let Some(kind) = &a.verify_kind {
        let mut v = toml::Table::new();
        v.insert("kind".into(), toml::Value::String(kind.clone()));
        let strs = [
            ("script", &a.script),
            ("glb", &a.glb),
            ("render", &a.render),
            ("page", &a.page),
            ("selector", &a.selector),
        ];
        for (key, val) in strs {
            if let Some(s) = val {
                v.insert(key.into(), toml::Value::String(s.clone()));
            }
        }
        let arr = |xs: &[String]| {
            toml::Value::Array(xs.iter().cloned().map(toml::Value::String).collect())
        };
        if !a.argv.is_empty() {
            v.insert("argv".into(), arr(&a.argv));
        }
        if !a.inputs.is_empty() {
            v.insert("inputs".into(), arr(&a.inputs));
        }
        t.insert("verify".into(), toml::Value::Table(v));
    }
    let raw = toml::Value::Table(t);
    parse(name, &raw)?;
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["a", "0", "blender-headless", "a1-", "x--y"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "-a", "A", "a_b", "a b", "a.b", "é"] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn canonical_json_sorts_keys() {
        let v: toml::Value = toml::from_str("b = 1\na = [\"x\", {d = 2, c = true}]\n").unwrap();
        assert_eq!(canonical_json(&v), r#"{"a":["x",{"c":true,"d":2}],"b":1}"#);
    }
}
