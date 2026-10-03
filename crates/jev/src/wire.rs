//! The jev request/response wire types.
//!
//! Shapes measured 2026-10-02 from docs.typesafe.ai (`/api.md`,
//! `/primitives/*.md`). Where the docs and a third-party tutorial disagreed,
//! the docs won; the one asymmetry worth remembering is that a **Noul answer
//! carries no `confidence` field** — the probability *is* the answer — while
//! Choice and Score both carry one derived from the distribution's shape.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A question. `criteria` is optional for a Noul, a required map for a Choice
/// (option name → description, max 255), and a required array for a Score
/// (2–10 ordered levels).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    Noul {
        instructions: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        criteria: Option<BTreeMap<String, String>>,
    },
    Choice {
        instructions: String,
        criteria: BTreeMap<String, Option<String>>,
    },
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
}

impl Question {
    pub fn kind(&self) -> &'static str {
        match self {
            Question::Noul { .. } => "noul",
            Question::Choice { .. } => "choice",
            Question::Score { .. } => "score",
        }
    }
}

/// A request body. `state` is free-form JSON (string, object, or array).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JevRequest {
    pub state: serde_json::Value,
    pub model: String,
    pub questions: BTreeMap<String, Question>,
}

impl JevRequest {
    pub fn new(state: serde_json::Value, model: impl Into<String>) -> Self {
        JevRequest {
            state,
            model: model.into(),
            questions: BTreeMap::new(),
        }
    }

    pub fn with_question(mut self, key: impl Into<String>, q: Question) -> Self {
        self.questions.insert(key.into(), q);
        self
    }

    /// The question keys, for the ledger. Keys only — never the state, which
    /// may hold whatever the caller was evaluating.
    pub fn question_keys(&self) -> Vec<String> {
        self.questions.keys().cloned().collect()
    }
}

/// One answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    /// A probability in `0.0..=1.0`. **No `confidence` field** — see the module
    /// docs.
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        #[serde(default)]
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        confidence: f64,
    },
}

impl Answer {
    pub fn kind(&self) -> &'static str {
        match self {
            Answer::Noul { .. } => "noul",
            Answer::Choice { .. } => "choice",
            Answer::Score { .. } => "score",
        }
    }
}

/// Token accounting from a response. Output tokens are free as of 2026-10-02
/// but are recorded anyway, because "free" is a pricing fact that can change
/// and the ledger should not have to be back-filled when it does.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

/// A decoded response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawResponse {
    /// What actually answered. Recorded in the ledger rather than assumed from
    /// the request, because `jev-latest` resolves to a concrete version that
    /// drifts.
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    #[serde(default)]
    pub usage: Usage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_to_the_documented_shape() {
        let req = JevRequest::new(serde_json::json!("a ticket"), "jev-latest")
            .with_question(
                "billing",
                Question::Noul {
                    instructions: "Is this about billing?".into(),
                    criteria: None,
                },
            )
            .with_question(
                "urgency",
                Question::Score {
                    instructions: "How urgent?".into(),
                    criteria: vec!["can wait".into(), "this week".into(), "today".into()],
                },
            );
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["model"], "jev-latest");
        assert_eq!(v["questions"]["billing"]["type"], "noul");
        // Optional criteria must be omitted, not sent as null.
        assert!(v["questions"]["billing"].get("criteria").is_none());
        assert_eq!(v["questions"]["urgency"]["type"], "score");
        assert_eq!(v["questions"]["urgency"]["criteria"][2], "today");
    }

    #[test]
    fn response_decodes_and_noul_has_no_confidence() {
        let body = r#"{
          "model": "jev-1.13.0",
          "answers": {
            "billing": {"type": "noul", "noul": 0.93},
            "tone": {"type": "choice", "choice": "angry",
                     "probabilities": {"calm": 0.01, "angry": 0.99}, "confidence": 0.78}
          },
          "usage": {"input_tokens": 120, "output_tokens": 0}
        }"#;
        let r: RawResponse = serde_json::from_str(body).unwrap();
        assert_eq!(r.model, "jev-1.13.0");
        assert_eq!(r.usage.input_tokens, 120);
        match &r.answers["billing"] {
            Answer::Noul { noul } => assert!((*noul - 0.93).abs() < 1e-9),
            other => panic!("expected noul, got {other:?}"),
        }
        match &r.answers["tone"] {
            Answer::Choice { choice, .. } => assert_eq!(choice, "angry"),
            other => panic!("expected choice, got {other:?}"),
        }
    }

    #[test]
    fn question_keys_are_recorded_but_state_is_not_part_of_them() {
        let req = JevRequest::new(serde_json::json!({"secret": "hunter2"}), "m").with_question(
            "q1",
            Question::Noul {
                instructions: "x".into(),
                criteria: None,
            },
        );
        assert_eq!(req.question_keys(), vec!["q1".to_string()]);
        assert!(!req.question_keys().join(",").contains("hunter2"));
    }
}
