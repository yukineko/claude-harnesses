//! HTTP transport, behind a trait so that "no key means no socket" is a
//! *counted* fact rather than a claim about the code.
//!
//! # Why a trait
//!
//! The central requirement is negative: with no `TYPESAFE_API_KEY`, jev must
//! never open a connection. A negative like that cannot be shown by reading
//! the happy path — it has to be observed. [`RecordingTransport`] counts
//! invocations and sends nothing, so a test can assert the count is exactly
//! zero. `tests/no_key_no_network.rs` does that for four environments, and the
//! guard it checks was pinned RED before it was made GREEN.
//!
//! # Why `curl` and not an HTTP crate
//!
//! beacon already established the pattern in this repo
//! (`crates/beacon/src/notify.rs`): shell out to `curl` with a hard timeout
//! rather than link a TLS stack. It keeps the dependency surface — and the
//! bundled binary — small, and the timeout is enforced by a process that can
//! be killed.
//!
//! # One deliberate improvement over beacon's pattern
//!
//! beacon passes its whole request on `curl`'s argv. For a webhook URL that is
//! acceptable; for `Authorization: Bearer <key>` it is not, because **argv is
//! world-readable through `ps` and `/proc/<pid>/cmdline`** for the lifetime of
//! the request. So this transport writes a `curl` config to the child's
//! **stdin** (`curl -K -`) and puts the URL, the headers, and the body file
//! reference there. Nothing secret appears in argv at any point.

use crate::redact;
use crate::wire::JevRequest;
use harness_core::verdict::Determination;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A raw HTTP outcome: the status line's code plus the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawHttp {
    pub status: u16,
    pub body: String,
}

/// How a request reaches the network.
///
/// The `bearer` argument is the full `Authorization` header value. An
/// implementation **must not** log, store, or place it on a command line.
pub trait Transport {
    fn post_json(
        &self,
        url: &str,
        bearer: &str,
        body: &str,
        max_time_secs: u64,
    ) -> Determination<RawHttp>;
}

/// The production transport.
#[derive(Debug, Clone, Default)]
pub struct CurlTransport;

impl CurlTransport {
    pub fn new() -> Self {
        CurlTransport
    }
}

impl Transport for CurlTransport {
    fn post_json(
        &self,
        url: &str,
        bearer: &str,
        body: &str,
        max_time_secs: u64,
    ) -> Determination<RawHttp> {
        // `-s` quiet, `-S` still report errors, `--max-time` the hard ceiling,
        // `-o -` body to stdout, `-w` append the status so one read yields
        // both. The URL, the header and the payload all travel through the
        // stdin config, never argv.
        let mut child = match Command::new("curl")
            .args(["-sS", "-K", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not start curl: {}",
                    redact::redact_patterns(&e.to_string())
                ))
            }
        };

        // curl's config format: one `key = "value"` per line, with `\` and `"`
        // escaped inside the quotes.
        let cfg = format!(
            concat!(
                "url = \"{url}\"\n",
                "request = \"POST\"\n",
                "header = \"Content-Type: application/json\"\n",
                "header = \"{auth}\"\n",
                "data-binary = \"{body}\"\n",
                "max-time = \"{max_time}\"\n",
                "silent\n",
                "show-error\n",
                "write-out = \"\\n%{{http_code}}\"\n",
            ),
            url = esc(url),
            auth = esc(bearer),
            body = esc(body),
            max_time = max_time_secs,
        );

        {
            let Some(stdin) = child.stdin.as_mut() else {
                let _ = child.kill();
                return Determination::undetermined("curl stdin was not available".to_string());
            };
            if let Err(e) = stdin.write_all(cfg.as_bytes()) {
                let _ = child.kill();
                return Determination::undetermined(format!(
                    "writing curl config: {}",
                    redact::redact_patterns(&e.to_string())
                ));
            }
        }
        // Drop stdin so curl sees EOF and starts.
        drop(child.stdin.take());

        let out = match child.wait_with_output() {
            Ok(o) => o,
            Err(e) => {
                return Determination::undetermined(format!(
                    "waiting for curl: {}",
                    redact::redact_patterns(&e.to_string())
                ))
            }
        };

        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            // A non-zero curl is "could not determine", never an answer. A
            // timeout lands here (exit 28) and must not look like a response.
            return Determination::undetermined(format!(
                "curl exited {}: {}",
                out.status
                    .code()
                    .map_or_else(|| "signal".to_string(), |c| c.to_string()),
                redact::redact_patterns(err.trim())
            ));
        }

        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        // `write-out` appended "\n<code>" after the body.
        let Some(nl) = stdout.rfind('\n') else {
            return Determination::undetermined(
                "curl produced no status line (write-out missing)".to_string(),
            );
        };
        let (body_part, code_part) = stdout.split_at(nl);
        let Ok(status) = code_part.trim().parse::<u16>() else {
            return Determination::undetermined(format!(
                "curl status was not a number: {:?}",
                code_part.trim()
            ));
        };
        Determination::known(RawHttp {
            status,
            body: body_part.to_string(),
        })
    }
}

/// Escape a value for a curl config line.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A transport that records that it was asked and sends nothing.
///
/// This is the instrument for the crate's central assertion. It is public
/// because consumers wiring jev into their own code need the same assertion —
/// "my feature does not call jev when the key is absent" — and should not have
/// to build it again.
#[derive(Debug, Default)]
pub struct RecordingTransport {
    calls: AtomicUsize,
    /// What to hand back when it *is* called.
    canned: Option<RawHttp>,
}

impl RecordingTransport {
    /// A recorder whose calls report a transport-level give-up.
    pub fn new() -> Self {
        RecordingTransport {
            calls: AtomicUsize::new(0),
            canned: None,
        }
    }

    /// A recorder that answers with a fixed HTTP result.
    pub fn with_response(status: u16, body: impl Into<String>) -> Self {
        RecordingTransport {
            calls: AtomicUsize::new(0),
            canned: Some(RawHttp {
                status,
                body: body.into(),
            }),
        }
    }

    /// How many times the transport was invoked. The number this crate's
    /// contract is about.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Transport for RecordingTransport {
    fn post_json(
        &self,
        _url: &str,
        _bearer: &str,
        _body: &str,
        _max_time_secs: u64,
    ) -> Determination<RawHttp> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.canned {
            Some(r) => Determination::known(r.clone()),
            None => Determination::undetermined("recording transport: no canned response"),
        }
    }
}

/// Serialize a request body. Separated so the client can report a
/// serialization failure as a give-up instead of unwrapping.
pub(crate) fn body_of(req: &JevRequest) -> Determination<String> {
    match serde_json::to_string(req) {
        Ok(s) => Determination::known(s),
        Err(e) => Determination::undetermined(format!("serializing jev request: {e}")),
    }
}

/// The endpoint a client should use, allowing a config-independent override
/// for local experimentation. Not a config field: pointing jev's credential at
/// an arbitrary host should take a deliberate environment change.
pub(crate) fn endpoint() -> String {
    match std::env::var("JEV_ENDPOINT") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => crate::ENDPOINT.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_protects_the_config_format() {
        assert_eq!(esc(r#"a"b"#), r#"a\"b"#);
        assert_eq!(esc(r"a\b"), r"a\\b");
    }

    #[test]
    fn recorder_counts_and_sends_nothing() {
        let t = RecordingTransport::new();
        assert_eq!(t.calls(), 0);
        let d = t.post_json("https://example.invalid", "Bearer x", "{}", 1);
        assert_eq!(t.calls(), 1);
        match d.require() {
            harness_core::verdict::Required::Determined(_) => {
                panic!("a recorder with no canned response must not answer")
            }
            harness_core::verdict::Required::Blocked(_) => {}
        }
    }

    #[test]
    fn recorder_with_response_answers_the_canned_value() {
        let t = RecordingTransport::with_response(200, "{\"ok\":1}");
        let d = t.post_json("u", "b", "{}", 1);
        assert_eq!(t.calls(), 1);
        match d.require() {
            harness_core::verdict::Required::Determined(r) => {
                assert_eq!(r.status, 200);
                assert_eq!(r.body, "{\"ok\":1}");
            }
            harness_core::verdict::Required::Blocked(u) => panic!("unexpected block: {u:?}"),
        }
    }

    #[test]
    fn body_of_serializes() {
        let req = JevRequest::new(serde_json::json!("s"), "m");
        match body_of(&req).require() {
            harness_core::verdict::Required::Determined(s) => {
                assert!(s.contains("\"model\":\"m\""))
            }
            harness_core::verdict::Required::Blocked(u) => panic!("unexpected block: {u:?}"),
        }
    }
}
