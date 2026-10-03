//! The central assertion of this crate, as a counted fact.
//!
//! > With no usable `TYPESAFE_API_KEY`, jev must never open a connection.
//!
//! That is a negative claim, so it is not shown by exercising the happy path.
//! Here the transport is a [`RecordingTransport`] that sends nothing and
//! counts invocations, and each case asserts an **exact** count. A
//! `RecordingTransport` is also why this suite cannot reach the network: the
//! networked `curl` transport is never named in this file at all, which the
//! last case asserts lexically.
//!
//! # F→P oracle
//!
//! These cases were observed RED before they were made GREEN. With the
//! availability guard removed from `Client::ask` (the naive implementation
//! that reads the env into a bearer and calls anyway), the three
//! absent-key cases failed with `calls() == 1` where they require `0`. The
//! guard was then added and they passed. A test that has never failed proves
//! nothing (CLAUDE.md §2(b)), so the RED is part of the record.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use jev::availability::KEY_ENV;
use jev::{Advice, Availability, Client, Config, JevRequest, Question, RecordingTransport};
use std::sync::{Mutex, OnceLock};

/// The process environment is global, so these cases must not interleave.
///
/// Callers take it with `unwrap_or_else(|e| e.into_inner())`: a panicking case
/// poisons the mutex, and treating that as fatal made one real failure cascade
/// into five `PoisonError`s that hid their own diagnoses (observed while
/// injecting the fault below).
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn config(dir: &std::path::Path) -> Config {
    Config {
        model: "jev-test".to_string(),
        max_time_secs: 1,
        // Never the real ledger: a test must not append to the operator's
        // spend record.
        ledger_path: Some(dir.join("usage.jsonl")),
    }
}

fn a_request() -> JevRequest {
    JevRequest::new(serde_json::json!("some state"), "jev-test").with_question(
        "q",
        Question::Noul {
            instructions: "Is this true?".to_string(),
            criteria: None,
        },
    )
}

/// Run `f` with `TYPESAFE_API_KEY` set to `value` (or removed), restoring the
/// previous value afterwards.
fn with_key<R>(value: Option<std::ffi::OsString>, f: impl FnOnce() -> R) -> R {
    let prev = std::env::var_os(KEY_ENV);
    match &value {
        Some(v) => std::env::set_var(KEY_ENV, v),
        None => std::env::remove_var(KEY_ENV),
    }
    let r = f();
    match prev {
        Some(p) => std::env::set_var(KEY_ENV, p),
        None => std::env::remove_var(KEY_ENV),
    }
    r
}

#[test]
fn case_1_unset_key_sends_nothing() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new(RecordingTransport::new(), config(dir.path()));
    let out = with_key(None, || client.ask(&a_request()));

    assert_eq!(
        client.transport().calls(),
        0,
        "an unset TYPESAFE_API_KEY must not produce a network call"
    );
    let advice = out.advice_noul("q", 0.5);
    assert!(!advice.is_escalation());
    assert_eq!(
        advice.render(),
        "",
        "an absent advisor must add nothing to the output"
    );
}

#[test]
fn case_2_empty_key_sends_nothing() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new(RecordingTransport::new(), config(dir.path()));
    for raw in ["", "   "] {
        let out = with_key(Some(raw.into()), || client.ask(&a_request()));
        assert_eq!(
            client.transport().calls(),
            0,
            "TYPESAFE_API_KEY={raw:?} must not produce a network call"
        );
        assert!(!out.advice_noul("q", 0.5).is_escalation());
    }
}

#[cfg(unix)]
#[test]
fn case_3_non_utf8_key_is_undetermined_and_sends_nothing() {
    use std::os::unix::ffi::OsStringExt;
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new(RecordingTransport::new(), config(dir.path()));

    let bad = std::ffi::OsString::from_vec(vec![0x61, 0x80, 0xff]);
    let (availability, out) = with_key(Some(bad), || {
        (Availability::detect(), client.ask(&a_request()))
    });

    assert_eq!(
        client.transport().calls(),
        0,
        "an unreadable TYPESAFE_API_KEY must not produce a network call"
    );
    // And it must be reported as undetermined, NOT as a deliberate opt-out.
    match availability {
        Availability::Undetermined { .. } => {}
        other => panic!("expected Undetermined, got {other:?}"),
    }
    assert_eq!(availability.exit_code(), 10);
    assert!(!out.advice_noul("q", 0.5).is_escalation());
}

#[test]
fn case_4_present_key_reaches_the_transport_exactly_once() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let body = r#"{"model":"jev-1.13.0","answers":{"q":{"type":"noul","noul":0.97}},
                   "usage":{"input_tokens":42,"output_tokens":0}}"#;
    let client = Client::new(
        RecordingTransport::with_response(200, body),
        config(dir.path()),
    );
    let out = with_key(Some("apikey_livetestvalue9999".into()), || {
        client.ask(&a_request())
    });

    assert_eq!(
        client.transport().calls(),
        1,
        "a configured key must produce exactly one call"
    );
    let advice = out.advice_noul("q", 0.8);
    match advice {
        Advice::Escalate { probability, .. } => assert!((probability - 0.97).abs() < 1e-9),
        other => panic!("expected an escalation, got {other:?}"),
    }
}

#[test]
fn an_absent_key_and_a_reassuring_answer_are_the_same_value() {
    // The property that makes the absence structural rather than promised: a
    // consumer cannot branch on "jev approved" vs "jev never ran".
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();

    let absent = {
        let c = Client::new(RecordingTransport::new(), config(dir.path()));
        let out = with_key(None, || c.ask(&a_request()));
        assert_eq!(c.transport().calls(), 0);
        out.advice_noul("q", 0.8)
    };

    let reassuring = {
        let body = r#"{"model":"m","answers":{"q":{"type":"noul","noul":0.01}},
                       "usage":{"input_tokens":1,"output_tokens":0}}"#;
        let c = Client::new(
            RecordingTransport::with_response(200, body),
            config(dir.path()),
        );
        let out = with_key(Some("apikey_livetestvalue9999".into()), || {
            c.ask(&a_request())
        });
        assert_eq!(c.transport().calls(), 1);
        out.advice_noul("q", 0.8)
    };

    assert!(!absent.is_escalation());
    assert!(!reassuring.is_escalation());
    assert_eq!(
        absent.render(),
        reassuring.render(),
        "a consumer must not be able to tell these apart"
    );
}

#[test]
fn http_errors_are_undetermined_never_an_answer() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    for status in [401u16, 422, 429, 529] {
        let client = Client::new(
            RecordingTransport::with_response(status, "{\"error\":\"nope\"}"),
            config(dir.path()),
        );
        let out = with_key(Some("apikey_livetestvalue9999".into()), || {
            client.ask(&a_request())
        });
        assert_eq!(client.transport().calls(), 1);
        let advice = out.advice_noul("q", 0.0);
        assert!(
            !advice.is_escalation(),
            "HTTP {status} must not become an answer"
        );
    }
}

#[test]
fn this_suite_never_constructs_the_real_transport() {
    // A guard against the suite quietly gaining network reach later: the only
    // transport type named in this file is the recorder.
    let src = include_str!("no_key_no_network.rs");
    // The needle is assembled at runtime so this line is not itself a hit —
    // the first version of this check failed on its own assertion string.
    let needle = format!("{}{}", "Curl", "Transport");
    assert!(
        !src.contains(&needle),
        "this suite must not build the networked transport"
    );
}
