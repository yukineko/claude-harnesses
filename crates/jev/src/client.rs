//! [`Client`] — resolve availability, then (only then) talk to jev.
//!
//! The ordering in [`Client::ask`] is the whole safety property of this crate:
//! [`Availability`] is resolved **before** the transport is reachable, and
//! every non-`Configured` answer returns without touching it. Because the
//! transport is injected, that is checkable by counting calls rather than by
//! reading the function — see `tests/no_key_no_network.rs`, which pins the
//! four environments (unset, empty, non-UTF-8, present) against an exact
//! expected invocation count.

use crate::advice::Advice;
use crate::availability::Availability;
use crate::config::Config;
use crate::ledger::{self, Record};
use crate::redact;
use crate::transport::{self, RawHttp, Transport};
use crate::wire::{JevRequest, Question, RawResponse};
use harness_core::verdict::{Determination, Required};

/// The result of one attempt, plus whatever went wrong while recording it.
///
/// The ledger error is a separate field rather than folded into the result
/// because the two are independent facts: jev can answer while the ledger
/// write fails, and a swallowed ledger error would mean spend silently stops
/// being observable. The caller is forced to see both.
#[must_use = "an Outcome carries both the answer and any ledger failure; handle both"]
pub struct Outcome {
    pub result: Determination<RawResponse>,
    pub ledger_error: Option<String>,
}

impl Outcome {
    /// Interpret one Noul answer as advice. See [`Advice`] for why there is no
    /// way to get an approval out of this.
    pub fn advice_noul(self, question: &str, threshold: f64) -> Advice {
        match self.result.require() {
            Required::Determined(resp) => match resp.answers.get(question) {
                Some(a) => Advice::from_noul(question, a, threshold),
                None => Advice::call_failed(format!("no answer for question {question:?}")),
            },
            Required::Blocked(why) => {
                let reason = why.as_str();
                // Availability failures and call failures are both NoSignal;
                // they differ only in the recorded reason.
                if reason.starts_with(UNAVAILABLE_PREFIX) {
                    Advice::unavailable(reason.to_string())
                } else {
                    Advice::call_failed(reason.to_string())
                }
            }
        }
    }
}

/// Prefix used so an availability give-up is distinguishable in telemetry
/// (not in control flow — both land in `Advice::NoSignal`).
pub(crate) const UNAVAILABLE_PREFIX: &str = "jev unavailable: ";

/// A jev client over an injected [`Transport`].
pub struct Client<T: Transport> {
    transport: T,
    config: Config,
}

impl<T: Transport> Client<T> {
    pub fn new(transport: T, config: Config) -> Self {
        Client { transport, config }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Resolve availability from the environment.
    pub fn availability(&self) -> Availability {
        Availability::detect()
    }

    /// Ask jev.
    ///
    /// Returns an `Undetermined` result — and performs **no** transport call —
    /// whenever the key is not [`Availability::Configured`]. A non-2xx status,
    /// a timeout, an unparseable body and a transport failure are likewise
    /// `Undetermined`: an attempt that did not produce an answer is never
    /// reported as one.
    pub fn ask(&self, req: &JevRequest) -> Outcome {
        let started = std::time::Instant::now();

        // ---- THE GUARD -------------------------------------------------
        // Availability is resolved first, and the two non-Configured arms
        // return here. Nothing below this block is reachable without a key,
        // which is what `tests/no_key_no_network.rs` counts.
        let credential = match self.availability() {
            Availability::Configured(c) => c,
            Availability::NotConfigured { reason } => {
                return self.give_up(
                    req,
                    started,
                    None,
                    "unavailable",
                    format!("{UNAVAILABLE_PREFIX}{}", reason.as_str()),
                )
            }
            Availability::Undetermined { reason } => {
                return self.give_up(
                    req,
                    started,
                    None,
                    "unavailable",
                    format!("{UNAVAILABLE_PREFIX}{reason}"),
                )
            }
        };
        // ---- end of guard ----------------------------------------------

        let body = match transport::body_of(req).require() {
            Required::Determined(b) => b,
            Required::Blocked(why) => {
                return self.give_up(req, started, None, "undetermined", why.as_str().to_string())
            }
        };

        let http = self.transport.post_json(
            &transport::endpoint(),
            &credential.authorization_header(),
            &body,
            self.config.max_time_secs,
        );

        let secret = credential.secret();
        let RawHttp { status, body } = match http.require() {
            Required::Determined(h) => h,
            Required::Blocked(why) => {
                return self.give_up(
                    req,
                    started,
                    None,
                    "undetermined",
                    redact::redact(why.as_str(), secret),
                )
            }
        };

        if !(200..300).contains(&status) {
            // 401 / 422 / 429 / 529 all land here. The server's body can echo
            // request material, so it is redacted before it is kept.
            let detail = redact::redact(body.trim(), secret);
            let detail = truncate(&detail, 400);
            return self.give_up(
                req,
                started,
                Some(status),
                "http-error",
                format!("HTTP {status}: {detail}"),
            );
        }

        let parsed: RawResponse = match serde_json::from_str(&body) {
            Ok(p) => p,
            Err(e) => {
                let detail = redact::redact(&e.to_string(), secret);
                return self.give_up(
                    req,
                    started,
                    Some(status),
                    "undetermined",
                    format!("unparseable jev response: {detail}"),
                );
            }
        };

        let duration_ms = elapsed_ms(started);
        let rec = Record::answered(&parsed, req.question_keys(), status, duration_ms);
        let ledger_error = ledger::append(&self.config, &rec).err();
        Outcome {
            result: Determination::known(parsed),
            ledger_error,
        }
    }

    /// Convenience: one Noul question, straight to [`Advice`].
    pub fn advise_noul(
        &self,
        state: serde_json::Value,
        question: &str,
        instructions: &str,
        threshold: f64,
    ) -> (Advice, Option<String>) {
        let req = JevRequest::new(state, self.config.model.clone()).with_question(
            question,
            Question::Noul {
                instructions: instructions.to_string(),
                criteria: None,
            },
        );
        let out = self.ask(&req);
        let ledger_error = out.ledger_error.clone();
        (out.advice_noul(question, threshold), ledger_error)
    }

    fn give_up(
        &self,
        req: &JevRequest,
        started: std::time::Instant,
        status: Option<u16>,
        outcome: &str,
        detail: String,
    ) -> Outcome {
        let duration_ms = elapsed_ms(started);
        let rec = Record::failed(
            &self.config.model,
            req.question_keys(),
            status,
            duration_ms,
            outcome,
            detail.clone(),
        );
        // An availability give-up is recorded too: "jev was asked for and was
        // not configured" is a fact worth having in the ledger, and it costs
        // nothing (no tokens).
        let ledger_error = ledger::append(&self.config, &rec).err();
        Outcome {
            result: Determination::undetermined(detail),
            ledger_error,
        }
    }
}

fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max).collect();
    format!("{kept}… (truncated)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::RecordingTransport;

    fn cfg(dir: &std::path::Path) -> Config {
        Config {
            model: "jev-test".to_string(),
            max_time_secs: 1,
            ledger_path: Some(dir.join("usage.jsonl")),
        }
    }

    #[test]
    fn truncate_keeps_short_strings_intact() {
        assert_eq!(truncate("abc", 10), "abc");
        assert!(truncate(&"x".repeat(50), 10).contains("truncated"));
    }

    #[test]
    fn outcome_without_the_question_key_is_no_signal() {
        let dir = tempfile::tempdir().unwrap();
        let out = Outcome {
            result: Determination::known(RawResponse {
                model: "m".into(),
                answers: Default::default(),
                usage: Default::default(),
            }),
            ledger_error: None,
        };
        let a = out.advice_noul("missing", 0.5);
        assert!(!a.is_escalation());
        let _ = cfg(dir.path());
    }

    #[test]
    fn a_transport_give_up_is_no_signal_not_an_escalation() {
        let dir = tempfile::tempdir().unwrap();
        let c = Client::new(RecordingTransport::new(), cfg(dir.path()));
        // With a key present the transport is reached; it declines to answer.
        temp_env_key(Some("apikey_testvalue0001"), || {
            let (advice, _) = c.advise_noul(serde_json::json!("s"), "q", "Is it?", 0.5);
            assert!(!advice.is_escalation());
        });
        assert_eq!(c.transport().calls(), 1);
    }

    /// Set/restore `TYPESAFE_API_KEY` around a closure. Tests that touch the
    /// process environment are serialized by the caller's module lock.
    fn temp_env_key<R>(value: Option<&str>, f: impl FnOnce() -> R) -> R {
        let prev = std::env::var_os(crate::availability::KEY_ENV);
        match value {
            Some(v) => std::env::set_var(crate::availability::KEY_ENV, v),
            None => std::env::remove_var(crate::availability::KEY_ENV),
        }
        let r = f();
        match prev {
            Some(p) => std::env::set_var(crate::availability::KEY_ENV, p),
            None => std::env::remove_var(crate::availability::KEY_ENV),
        }
        r
    }
}
