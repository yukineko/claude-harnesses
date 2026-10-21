//! Spec IR — the normalized, Agent-executable spec produced by ②normalize.
//!
//! The agent emits only the `[[requirement]]` tables (the part that needs
//! judgment); the deterministic harness owns the `[spec]` header (id, status,
//! provenance) and assembles the full [`Spec`]. This keeps content judgment in
//! the LLM and identity/provenance in the harness (DESIGN.md §3, §7).

use serde::{Deserialize, Serialize};

/// A full Spec IR document, serialized to `<spec_dir>/<id>.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Spec {
    pub spec: SpecMeta,
    #[serde(default, rename = "requirement")]
    pub requirements: Vec<Requirement>,
}

/// The `[spec]` header — harness-owned identity and provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpecMeta {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// `draft` until a human ratifies it, then `ratified` (DESIGN.md §5).
    #[serde(default)]
    pub status: String,
    /// canon commit (HEAD) at draft time — pins provenance.
    #[serde(default)]
    pub provenance_commit: String,
    #[serde(default)]
    pub date: String,
    /// canon pointers this spec is derived from (file or `file#section`).
    #[serde(default)]
    pub canon: Vec<String>,
    /// Present only after ratification (the human consent ceremony).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratification: Option<Ratification>,
}

/// The pinned record of human consent that promotes draft → ratified.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ratification {
    pub canon_commit: String,
    pub date: String,
    pub reason: String,
    /// Fingerprint of the requirement content at ratification — a later edit
    /// changes this and forces re-ratification (DESIGN.md §5).
    pub fingerprint: String,
}

/// One atomic requirement: a verifiable unit ≒ one implementation task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Requirement {
    pub id: String,
    pub statement: String,
    /// Falsifiable acceptance criteria — the verbatim cross-check points for the
    /// downstream ⑥ audit (DESIGN.md §3, §5.3 G4).
    #[serde(default)]
    pub acceptance: Vec<String>,
    /// canon pointers grounding this requirement (G1).
    #[serde(default)]
    pub canon: Vec<String>,
    /// The agent's assertion that every acceptance criterion is observable.
    #[serde(default)]
    pub falsifiable: bool,
    /// A committed test command that REPRODUCES this requirement as unmet (it
    /// must FAIL at HEAD), e.g. `cargo test -p x --test y z`. `ratify`/`queue`
    /// file the requirement only through `backlog add --repro-test <this>`; a
    /// requirement without one is NOT filed and is reported by id, with a
    /// non-zero exit (backlog eb93be49, user ruling 2026-10-22: no test, no
    /// ticket). Deliberately NOT part of [`Spec::fingerprint`]: the test is the
    /// evidence the queue demands, not requirement content, so adding one after
    /// ratification and re-running `specforge queue` does not void consent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repro_test: Option<String>,
}

/// What the agent emits in the report body: just the requirement tables.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentDraft {
    #[serde(default, rename = "requirement")]
    pub requirements: Vec<Requirement>,
}

impl AgentDraft {
    /// Parse the agent's body (TOML `[[requirement]]` tables). Real models often
    /// prepend reasoning prose despite the "TOML only" instruction, so we first
    /// EXTRACT the requirement TOML rather than parse the whole body verbatim
    /// (see [`extract_requirement_toml`]).
    pub fn parse(body: &str) -> Result<AgentDraft, toml::de::Error> {
        toml::from_str(&extract_requirement_toml(body))
    }
}

/// Pull the requirement TOML out of the agent's report body, tolerating a prose
/// preamble. Priority: (1) a fenced ```` ```toml ```` block that declares a
/// `[[requirement]]`; (2) a slice from the first `[[requirement]]` line to the
/// end (bare TOML after prose); (3) the whole body unchanged (back-compat with
/// bare-TOML output, and so a genuinely malformed body still yields a TOML error).
pub fn extract_requirement_toml(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();

    // (1) Fenced code blocks — prefer one containing a requirement table.
    let mut i = 0;
    let mut fenced: Vec<String> = Vec::new();
    while i < lines.len() {
        if lines[i].trim_start().starts_with("```") {
            i += 1;
            let mut block = String::new();
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                block.push_str(lines[i]);
                block.push('\n');
                i += 1;
            }
            fenced.push(block);
        }
        i += 1;
    }
    if let Some(b) = fenced.iter().find(|b| b.contains("[[requirement]]")) {
        return b.clone();
    }

    // (2) No fence: take everything from the first requirement table onward.
    if let Some(pos) = lines
        .iter()
        .position(|l| l.trim_start().starts_with("[[requirement]]"))
    {
        return lines[pos..].join("\n");
    }

    // (3) Give up gracefully — let the caller's TOML parse report the error.
    body.to_string()
}

impl Spec {
    /// Load a spec from disk.
    pub fn load(path: &std::path::Path) -> anyhow::Result<Spec> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading spec {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| anyhow::anyhow!("parsing spec {}: {e}", path.display()))
    }

    /// Serialize to TOML for persistence.
    pub fn to_toml(&self) -> anyhow::Result<String> {
        toml::to_string_pretty(self).map_err(|e| anyhow::anyhow!("serializing spec: {e}"))
    }

    /// Content fingerprint over the requirements (FNV-1a). Re-ratification is
    /// forced when this changes vs the pinned value. Mirrors `ratify::hash`.
    pub fn fingerprint(&self) -> String {
        let mut h = harness_core::hash::Fnv1a64::new();
        let mut feed = |s: &str| h.update(s.as_bytes());
        for r in &self.requirements {
            feed(&r.id);
            feed(&r.statement);
            for a in &r.acceptance {
                feed(a);
            }
            for c in &r.canon {
                feed(c);
            }
            feed(if r.falsifiable { "T" } else { "F" });
        }
        format!("{:016x}", h.finish())
    }

    /// Machine contract check (the rigor floor the harness can verify without an
    /// LLM): every requirement must be grounded (canon), falsifiable (acceptance
    /// present + flagged), and identified. The *policy* judgment (are these the
    /// right criteria?) is the human's, recorded as the ratification reason.
    /// Returns one message per violation; empty = contract satisfied.
    pub fn contract_violations(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.requirements.is_empty() {
            v.push("requirement が1つも無い (rigor: pass なら最低1つ必要)".to_string());
        }
        for (i, r) in self.requirements.iter().enumerate() {
            let who = if r.id.trim().is_empty() {
                format!("requirement[{i}]")
            } else {
                format!("requirement '{}'", r.id)
            };
            if r.id.trim().is_empty() {
                v.push(format!("{who}: id が空"));
            }
            if r.statement.trim().is_empty() {
                v.push(format!("{who}: statement が空"));
            }
            if r.acceptance.iter().all(|a| a.trim().is_empty()) {
                v.push(format!("{who}: acceptance が空 (G4 反証可能性が無い)"));
            }
            if r.canon.iter().all(|c| c.trim().is_empty()) {
                v.push(format!("{who}: canon が空 (G1 接地が無い)"));
            }
            if !r.falsifiable {
                v.push(format!("{who}: falsifiable=false (G4 未充足)"));
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(id: &str) -> Requirement {
        Requirement {
            id: id.into(),
            statement: "do X".into(),
            acceptance: vec!["X observable".into()],
            canon: vec!["docs/spec.md#x".into()],
            falsifiable: true,
            repro_test: None,
        }
    }

    #[test]
    fn parses_requirement_tables() {
        let body = r#"
[[requirement]]
id = "R1"
statement = "rate-limit"
acceptance = ["429 after 5", "Retry-After header"]
canon = ["docs/auth.md#rate-limit"]
falsifiable = true
"#;
        let d = AgentDraft::parse(body).unwrap();
        assert_eq!(d.requirements.len(), 1);
        assert_eq!(d.requirements[0].acceptance.len(), 2);
    }

    #[test]
    fn parses_despite_prose_preamble_and_fence() {
        // What real models actually emit: reasoning prose, then a ```toml fence.
        let body = "canon を読みました。全ゲートを判定します。\n\n```toml\n\
                    [[requirement]]\n\
                    id = \"R1\"\n\
                    statement = \"clamp\"\n\
                    acceptance = [\"n<0 -> 0\"]\n\
                    canon = [\"canon/clamp.md\"]\n\
                    falsifiable = true\n```\nおわり。";
        let d = AgentDraft::parse(body).unwrap();
        assert_eq!(d.requirements.len(), 1);
        assert_eq!(d.requirements[0].id, "R1");
    }

    #[test]
    fn parses_prose_then_bare_toml_without_fence() {
        let body = "判定: 全て pass。\n[[requirement]]\nid = \"R1\"\n\
                    statement = \"x\"\nacceptance = [\"a\"]\ncanon = [\"c\"]\nfalsifiable = true";
        let d = AgentDraft::parse(body).unwrap();
        assert_eq!(d.requirements.len(), 1);
    }

    #[test]
    fn extract_falls_back_to_whole_body_when_no_toml() {
        // No requirement table anywhere -> body returned as-is -> TOML error.
        assert!(AgentDraft::parse("just prose, no spec").is_err());
    }

    #[test]
    fn contract_flags_missing_grounding_and_falsifiability() {
        let spec = Spec {
            spec: SpecMeta {
                id: "s".into(),
                title: String::new(),
                status: "draft".into(),
                provenance_commit: String::new(),
                date: String::new(),
                canon: vec![],
                ratification: None,
            },
            requirements: vec![Requirement {
                id: "R1".into(),
                statement: "x".into(),
                acceptance: vec![],
                canon: vec![],
                falsifiable: false,
                repro_test: None,
            }],
        };
        let v = spec.contract_violations();
        assert!(v.iter().any(|m| m.contains("acceptance が空")));
        assert!(v.iter().any(|m| m.contains("canon が空")));
        assert!(v.iter().any(|m| m.contains("falsifiable")));
    }

    #[test]
    fn clean_spec_has_no_violations_and_roundtrips() {
        let spec = Spec {
            spec: SpecMeta {
                id: "s".into(),
                title: "T".into(),
                status: "draft".into(),
                provenance_commit: "abc".into(),
                date: "2026-01-01".into(),
                canon: vec!["docs/spec.md".into()],
                ratification: None,
            },
            requirements: vec![req("R1"), req("R2")],
        };
        assert!(spec.contract_violations().is_empty());
        let toml = spec.to_toml().unwrap();
        let back: Spec = toml::from_str(&toml).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn fingerprint_changes_with_content() {
        let mut a = Spec {
            spec: SpecMeta {
                id: "s".into(),
                title: String::new(),
                status: "draft".into(),
                provenance_commit: String::new(),
                date: String::new(),
                canon: vec![],
                ratification: None,
            },
            requirements: vec![req("R1")],
        };
        let f1 = a.fingerprint();
        a.requirements[0].acceptance.push("another".into());
        assert_ne!(f1, a.fingerprint());
    }
}
