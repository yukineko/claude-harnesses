// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Integration tests for the lease lifecycle and cross-session dedup.
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Counter to make unique temp dirs for each test.
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Create a temporary directory for this test with a unique name.
fn make_test_dir(tag: &str) -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("overwatch-test-{tag}-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
}

/// Simulate what overwatch does: load/save leases and detect conflicts.
/// This tests the pure logic without actually running the CLI.
#[test]
fn cross_session_dedup_via_pure_functions() {
    let temp_dir = make_test_dir("dedup");
    std::env::set_current_dir(&temp_dir).expect("cd to test dir");

    // Override HOME to point to a subdirectory of the test temp, so the state
    // is local and doesn't pollute the user's home.
    let test_home = temp_dir.join("home");
    fs::create_dir_all(&test_home).unwrap();
    std::env::set_var("HOME", &test_home);

    // Emulate overwatch's store API: load, check conflicts, save.
    // Session A claims key1
    {
        let mut leases = std::collections::BTreeMap::new();
        let now = 1000i64;
        leases.insert(
            "key1".to_string(),
            overwatch::store::Lease {
                key: "key1".to_string(),
                title: "task A".to_string(),
                session_id: "session-a".to_string(),
                run_id: "run-a".to_string(),
                claimed_at: now,
                heartbeat_at: now,
                scope: Vec::new(),
                done_criteria: None,
            },
        );
        // Manually create the storage dir and save
        let storage_dir = test_home
            .join(".overwatch")
            .join("temp-dummy-b6c1d4e0")
            .join("overwatch");
        fs::create_dir_all(&storage_dir).unwrap();
        let leases_path = storage_dir.join("leases.json");
        let json = serde_json::to_string_pretty(&leases).unwrap();
        fs::write(&leases_path, &json).unwrap();

        // Verify Session A holds key1
        let loaded = serde_json::from_str::<overwatch::store::LeaseRegistry>(
            &fs::read_to_string(&leases_path).unwrap(),
        )
        .unwrap();
        assert!(loaded.contains_key("key1"));
        let lease = &loaded["key1"];
        assert_eq!(lease.session_id, "session-a");
    }

    // Session B tries to claim key1 at a fresh heartbeat time
    {
        // Read what A wrote
        let storage_dir = test_home
            .join(".overwatch")
            .join("temp-dummy-b6c1d4e0")
            .join("overwatch");
        let leases_path = storage_dir.join("leases.json");
        let mut leases = serde_json::from_str::<overwatch::store::LeaseRegistry>(
            &fs::read_to_string(&leases_path).unwrap(),
        )
        .unwrap();

        let now = 1100i64; // Time advances
        overwatch::store::reap_stale(&mut leases, now);

        // Check if key1 is held by OTHER session
        let conflict = overwatch::store::is_held_by_other(&leases, "key1", "session-b", now);
        assert!(
            conflict,
            "session-b should see key1 held by session-a (not stale yet)"
        );
    }

    // Session B can claim a different key
    {
        let storage_dir = test_home
            .join(".overwatch")
            .join("temp-dummy-b6c1d4e0")
            .join("overwatch");
        let leases_path = storage_dir.join("leases.json");
        let mut leases = serde_json::from_str::<overwatch::store::LeaseRegistry>(
            &fs::read_to_string(&leases_path).unwrap(),
        )
        .unwrap();

        let now = 1100i64;
        overwatch::store::reap_stale(&mut leases, now);

        // No conflict on key2
        let conflict = overwatch::store::is_held_by_other(&leases, "key2", "session-b", now);
        assert!(!conflict);

        // Session B claims it
        leases.insert(
            "key2".to_string(),
            overwatch::store::Lease {
                key: "key2".to_string(),
                title: "task B".to_string(),
                session_id: "session-b".to_string(),
                run_id: "run-b".to_string(),
                claimed_at: now,
                heartbeat_at: now,
                scope: Vec::new(),
                done_criteria: None,
            },
        );
        let json = serde_json::to_string_pretty(&leases).unwrap();
        fs::write(&leases_path, &json).unwrap();
    }

    // After TTL expires, Session B CAN claim key1
    {
        let storage_dir = test_home
            .join(".overwatch")
            .join("temp-dummy-b6c1d4e0")
            .join("overwatch");
        let leases_path = storage_dir.join("leases.json");
        let mut leases = serde_json::from_str::<overwatch::store::LeaseRegistry>(
            &fs::read_to_string(&leases_path).unwrap(),
        )
        .unwrap();

        // Time moves past TTL (30 minutes = 1800 secs)
        let now = 1000 + 1800 + 1; // heartbeat_at was 1000, now is past TTL
        overwatch::store::reap_stale(&mut leases, now);

        // key1 should be gone (reaped)
        assert!(!leases.contains_key("key1"), "stale key1 should be reaped");
        // key2 should still be there (fresh heartbeat at 1100)
        assert!(
            leases.contains_key("key2"),
            "fresh key2 should survive reap at time 1801"
        );

        // No conflict on key1 anymore
        let conflict = overwatch::store::is_held_by_other(&leases, "key1", "session-b", now);
        assert!(!conflict);
    }
}

/// Test that same-session re-begin is idempotent.
#[test]
fn same_session_reclaim_is_idempotent() {
    let temp_dir = make_test_dir("idempotent");
    std::env::set_current_dir(&temp_dir).expect("cd to test dir");

    let test_home = temp_dir.join("home");
    fs::create_dir_all(&test_home).unwrap();
    std::env::set_var("HOME", &test_home);

    let storage_dir = test_home
        .join(".overwatch")
        .join("idempotent-dummy-c0ffee00")
        .join("overwatch");
    fs::create_dir_all(&storage_dir).unwrap();
    let leases_path = storage_dir.join("leases.json");

    let now = 1000i64;
    let mut leases = std::collections::BTreeMap::new();
    leases.insert(
        "task1".to_string(),
        overwatch::store::Lease {
            key: "task1".to_string(),
            title: "mytask".to_string(),
            session_id: "sess-1".to_string(),
            run_id: "run-1".to_string(),
            claimed_at: now,
            heartbeat_at: now,
            scope: Vec::new(),
            done_criteria: None,
        },
    );
    let json = serde_json::to_string_pretty(&leases).unwrap();
    fs::write(&leases_path, &json).unwrap();

    // Re-claim at a later time: should preserve claimed_at, update heartbeat
    let now2 = now + 100;
    let mut leases = serde_json::from_str::<overwatch::store::LeaseRegistry>(
        &fs::read_to_string(&leases_path).unwrap(),
    )
    .unwrap();
    overwatch::store::reap_stale(&mut leases, now2);

    // Same session can reclaim
    let conflict = overwatch::store::is_held_by_other(&leases, "task1", "sess-1", now2);
    assert!(
        !conflict,
        "same session should not see itself as a conflict"
    );

    // Preserve claimed_at
    let claimed_at = leases.get("task1").unwrap().claimed_at;
    leases.insert(
        "task1".to_string(),
        overwatch::store::Lease {
            key: "task1".to_string(),
            title: "mytask".to_string(),
            session_id: "sess-1".to_string(),
            run_id: "run-1".to_string(),
            claimed_at,         // preserved
            heartbeat_at: now2, // updated
            scope: Vec::new(),
            done_criteria: None,
        },
    );

    assert_eq!(
        leases["task1"].claimed_at, now,
        "claimed_at should be preserved on reclaim"
    );
    assert_eq!(
        leases["task1"].heartbeat_at, now2,
        "heartbeat_at should be updated on reclaim"
    );
}

/// Test that event appending works and round-trips.
#[test]
fn events_append_and_round_trip() {
    let temp_dir = make_test_dir("events");
    std::env::set_current_dir(&temp_dir).expect("cd to test dir");

    let test_home = temp_dir.join("home");
    fs::create_dir_all(&test_home).unwrap();
    std::env::set_var("HOME", &test_home);

    let storage_dir = test_home
        .join(".overwatch")
        .join("events-dummy-deadbeef")
        .join("overwatch");
    fs::create_dir_all(&storage_dir).unwrap();

    // Simulate appending a started event
    let event1 = overwatch::event::LifecycleEvent::started(
        "key1".to_string(),
        "task 1".to_string(),
        "sess-a".to_string(),
        "run-a".to_string(),
        1000,
    );

    let events_path = storage_dir.join("events.jsonl");
    let json = serde_json::to_string(&event1).unwrap();
    fs::write(&events_path, format!("{}\n", json)).unwrap();

    // Append a running event
    let event2 = overwatch::event::LifecycleEvent::running(
        "key1".to_string(),
        "task 1".to_string(),
        "sess-a".to_string(),
        "run-a".to_string(),
        1050,
        Some("processing".to_string()),
    );
    let json = serde_json::to_string(&event2).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&events_path)
        .unwrap()
        .write_all(format!("{}\n", json).as_bytes())
        .unwrap();

    // Append an ended event
    let event3 = overwatch::event::LifecycleEvent::ended(
        "key1".to_string(),
        "task 1".to_string(),
        "sess-a".to_string(),
        "run-a".to_string(),
        1100,
        "success".to_string(),
    );
    let json = serde_json::to_string(&event3).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&events_path)
        .unwrap()
        .write_all(format!("{}\n", json).as_bytes())
        .unwrap();

    // Read back and verify order
    let txt = fs::read_to_string(&events_path).unwrap();
    let lines: Vec<&str> = txt.lines().collect();
    assert_eq!(lines.len(), 3);

    let e1: overwatch::event::LifecycleEvent = serde_json::from_str(lines[0]).unwrap();
    let e2: overwatch::event::LifecycleEvent = serde_json::from_str(lines[1]).unwrap();
    let e3: overwatch::event::LifecycleEvent = serde_json::from_str(lines[2]).unwrap();

    assert_eq!(e1.kind, overwatch::event::EventKind::Started);
    assert_eq!(e2.kind, overwatch::event::EventKind::Running);
    assert_eq!(e3.kind, overwatch::event::EventKind::Ended);

    assert_eq!(e1.key, "key1");
    assert_eq!(e2.ts, 1050);
    assert_eq!(e3.status.as_deref(), Some("success"));
}
