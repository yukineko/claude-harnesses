//! The credential must not appear in anything this crate emits.
//!
//! The dangerous paths are the *error* paths, which is where secrets usually
//! escape: a server that echoes the request back, a transport failure message
//! that quotes what it was given, a `Debug` added later by someone chasing a
//! bug. Each case here feeds the real key into one of those paths and asserts
//! it does not come out the other side.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use jev::availability::KEY_ENV;
use jev::{Availability, Client, Config, JevRequest, Question, RecordingTransport};
use std::sync::{Mutex, OnceLock};

/// A key shaped like a real one, distinctive enough that a substring search
/// for it cannot match by accident.
const KEY: &str = "apikey_LEAKCANARY0123456789abcdef";
/// The part that must never be printed anywhere.
const CANARY: &str = "LEAKCANARY0123456789abcdef";

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn config(dir: &std::path::Path) -> Config {
    Config {
        model: "jev-test".to_string(),
        max_time_secs: 1,
        ledger_path: Some(dir.join("usage.jsonl")),
    }
}

fn a_request() -> JevRequest {
    JevRequest::new(serde_json::json!("state text"), "jev-test").with_question(
        "q",
        Question::Noul {
            instructions: "Is this true?".to_string(),
            criteria: None,
        },
    )
}

fn with_key<R>(f: impl FnOnce() -> R) -> R {
    let prev = std::env::var_os(KEY_ENV);
    std::env::set_var(KEY_ENV, KEY);
    let r = f();
    match prev {
        Some(p) => std::env::set_var(KEY_ENV, p),
        None => std::env::remove_var(KEY_ENV),
    }
    r
}

#[test]
fn an_http_error_body_that_echoes_the_key_is_redacted() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    // The hostile case: a 401 whose body quotes the credential it rejected.
    let body = format!("{{\"error\":\"invalid key: Bearer {KEY}\",\"sent\":\"{KEY}\"}}");
    let client = Client::new(
        RecordingTransport::with_response(401, body),
        config(dir.path()),
    );

    let out = with_key(|| client.ask(&a_request()));
    assert_eq!(client.transport().calls(), 1);

    let advice = out.advice_noul("q", 0.0);
    let rendered = advice.render();
    assert!(!rendered.contains(CANARY), "render leaked: {rendered}");
    let debug = format!("{advice:?}");
    assert!(!debug.contains(CANARY), "Debug leaked: {debug}");
}

#[test]
fn the_ledger_never_records_the_key_or_the_state() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let body = format!("{{\"error\":\"rejected Bearer {KEY}\"}}");
    let client = Client::new(
        RecordingTransport::with_response(401, body),
        config(dir.path()),
    );

    let out = with_key(|| client.ask(&a_request()));
    assert!(out.ledger_error.is_none(), "ledger: {:?}", out.ledger_error);

    let written = std::fs::read_to_string(dir.path().join("usage.jsonl")).unwrap();
    assert!(!written.is_empty(), "nothing was recorded");
    assert!(
        !written.contains(CANARY),
        "ledger leaked the key: {written}"
    );
    // The state is the caller's payload; it must never reach the ledger.
    assert!(
        !written.contains("state text"),
        "ledger leaked the state: {written}"
    );
    // ...but the question key, which is the caller's own label, is recorded.
    assert!(
        written.contains("\"q\""),
        "ledger lost the question key: {written}"
    );
}

#[test]
fn availability_debug_and_json_show_only_the_tail() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let availability = with_key(Availability::detect);
    let debug = format!("{availability:?}");
    assert!(!debug.contains(CANARY), "Debug leaked: {debug}");
    let json = availability.to_json().to_string();
    assert!(!json.contains(CANARY), "check JSON leaked: {json}");
    // The tail is the last four characters and nothing more.
    assert!(json.contains("cdef"), "tail missing from {json}");
}

#[test]
fn a_serialization_or_transport_giveup_message_is_redacted() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    // A transport that declines to answer; its reason text goes through the
    // same redaction as a server body.
    let client = Client::new(RecordingTransport::new(), config(dir.path()));
    let out = with_key(|| client.ask(&a_request()));
    let advice = out.advice_noul("q", 0.0);
    let shown = format!("{advice:?}{}", advice.render());
    assert!(!shown.contains(CANARY), "leaked: {shown}");
}

#[test]
fn the_canary_would_actually_be_detected_if_redaction_were_removed() {
    // A guard against the guards: show that the assertions above are capable
    // of failing, by checking the raw (un-redacted) text does contain the
    // canary. If this ever stops holding, the tests above are vacuous.
    let raw = format!("invalid key: Bearer {KEY}");
    assert!(raw.contains(CANARY));
    let redacted = jev::redact::redact(&raw, KEY);
    assert!(!redacted.contains(CANARY), "redaction failed: {redacted}");
}
