/// Lease lifecycle management.
use crate::event::LifecycleEvent;
use crate::store;
use anyhow::Result;
use serde_json::json;

/// Determine the session ID: from `--session` arg, or env var, or fallback to `pid-<pid>`.
fn resolve_session_id(session: Option<&str>) -> String {
    if let Some(s) = session {
        return s.to_string();
    }
    if let Ok(s) = std::env::var("CLAUDE_CODE_SESSION_ID") {
        return s;
    }
    format!("pid-{}", std::process::id())
}

/// Determine the run ID: from env var, or fallback to a timestamp-based id.
fn resolve_run_id() -> String {
    if let Ok(r) = std::env::var("OVERWATCH_RUN_ID") {
        return r;
    }
    format!("run-{}", store::now())
}

/// Begin a new lease for a task. If a live lease from another session already holds the key,
/// print a skip JSON to stdout and exit with code 1.
///
/// `scope` (files/globs) and `done_criteria` are the PDO session-anchor fields
/// (DESIGN §4.1/§4.2); both are optional so pre-existing callers that omit them
/// keep working unchanged. An empty `scope` means "not yet fixed".
pub fn begin(
    key: &str,
    title: &str,
    session: Option<&str>,
    scope: Vec<String>,
    done_criteria: Option<String>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let session_id = resolve_session_id(session);
    let run_id = resolve_run_id();
    let now = store::now();

    // Load and reap stale leases
    let mut leases = store::load_leases(&cwd)?;
    store::reap_stale(&mut leases, now);

    // Check if key is held by a live OTHER session
    if store::is_held_by_other(&leases, key, &session_id, now) {
        let holder = &leases[key];
        let skip_json = json!({
            "skipped": true,
            "holder": {
                "session_id": holder.session_id,
                "run_id": holder.run_id,
                "heartbeat_at": holder.heartbeat_at,
            }
        });
        println!("{skip_json}");
        std::process::exit(1);
    }

    // If same session already holds it, this is idempotent: just refresh
    // Otherwise insert new lease
    let claimed_at = leases.get(key).map(|l| l.claimed_at).unwrap_or(now);

    leases.insert(
        key.to_string(),
        store::Lease {
            key: key.to_string(),
            title: title.to_string(),
            session_id: session_id.clone(),
            run_id: run_id.clone(),
            claimed_at,
            heartbeat_at: now,
            scope,
            done_criteria,
        },
    );

    // Append Started event
    let event =
        LifecycleEvent::started(key.to_string(), title.to_string(), session_id, run_id, now);
    store::append_event(&cwd, &event)?;

    // Save updated leases
    store::save_leases(&cwd, &leases)?;

    Ok(())
}

/// Record that a task is running (heartbeat + event).
pub fn run(key: &str, note: Option<&str>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let now = store::now();

    // Load leases
    let mut leases = store::load_leases(&cwd)?;
    store::reap_stale(&mut leases, now);

    // If the lease exists, update heartbeat. Otherwise just record the event (fail-soft).
    let (session_id, run_id, title) = if let Some(lease) = leases.get_mut(key) {
        lease.heartbeat_at = now;
        (
            lease.session_id.clone(),
            lease.run_id.clone(),
            lease.title.clone(),
        )
    } else {
        // Fail-soft: if no lease, still record the event with derived ids
        (resolve_session_id(None), resolve_run_id(), key.to_string())
    };

    // Append Running event
    let event = LifecycleEvent::running(
        key.to_string(),
        title,
        session_id,
        run_id,
        now,
        note.map(str::to_string),
    );
    store::append_event(&cwd, &event)?;

    // Save updated leases
    store::save_leases(&cwd, &leases)?;

    Ok(())
}

/// End a lease and release it.
pub fn end(key: &str, status: &str) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let now = store::now();

    // Load leases
    let mut leases = store::load_leases(&cwd)?;
    store::reap_stale(&mut leases, now);

    // Get lease info before removing it (for the event)
    let (session_id, run_id, title) = if let Some(lease) = leases.get(key) {
        (
            lease.session_id.clone(),
            lease.run_id.clone(),
            lease.title.clone(),
        )
    } else {
        (resolve_session_id(None), resolve_run_id(), key.to_string())
    };

    // Append Ended event
    let event = LifecycleEvent::ended(
        key.to_string(),
        title,
        session_id,
        run_id,
        now,
        status.to_string(),
    );
    store::append_event(&cwd, &event)?;

    // Remove the lease
    leases.remove(key);

    // Save updated leases
    store::save_leases(&cwd, &leases)?;

    Ok(())
}

/// Refresh the heartbeat for a held lease.
pub fn heartbeat(key: &str) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let now = store::now();

    // Load leases
    let mut leases = store::load_leases(&cwd)?;
    store::reap_stale(&mut leases, now);

    // Update heartbeat if the lease exists
    if let Some(lease) = leases.get_mut(key) {
        lease.heartbeat_at = now;
        let count = 1;
        println!("{{\"refreshed\": {}}}", count);
    } else {
        println!("{{\"refreshed\": 0}}");
    }

    // Save updated leases
    store::save_leases(&cwd, &leases)?;

    Ok(())
}

/// Look up the live lease held by `session_id` (PDO anchor read path, §4.3/§5.1).
/// Stale leases are reaped first so only a live anchor is returned. If the
/// session holds more than one lease, the most recently claimed one is returned
/// (the session's current focus). Prints the lease as JSON when `json` is true,
/// else a short human line; prints nothing and exits 1 when there is no live
/// lease (fail-soft: callers treat a non-zero exit / empty output as "no anchor").
pub fn lease_for_session(session_id: &str, json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let now = store::now();
    let mut leases = store::load_leases(&cwd)?;
    store::reap_stale(&mut leases, now);

    match pick_session_lease(&leases, session_id) {
        Some(lease) => {
            if json {
                println!("{}", serde_json::to_string(lease)?);
            } else {
                println!("{} — {}", lease.key, lease.title);
            }
            Ok(())
        }
        None => {
            // No live anchor for this session: silent, non-zero exit (fail-soft).
            std::process::exit(1);
        }
    }
}

/// Pure selection of a session's current anchor from a registry: the
/// most-recently-claimed lease held by `session_id` (its current focus), or
/// `None` if the session holds no lease. Separated from I/O so it is unit
/// testable.
fn pick_session_lease<'a>(
    leases: &'a store::LeaseRegistry,
    session_id: &str,
) -> Option<&'a store::Lease> {
    leases
        .values()
        .filter(|l| l.session_id == session_id)
        .max_by_key(|l| l.claimed_at)
}

/// Reap expired leases.
pub fn reap() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let now = store::now();

    // Load and reap
    let mut leases = store::load_leases(&cwd)?;
    let before = leases.len();
    store::reap_stale(&mut leases, now);
    let after = leases.len();
    let reaped = before - after;

    // Save
    store::save_leases(&cwd, &leases)?;

    println!("{{\"reaped\": {}}}", reaped);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_session_id_uses_arg() {
        let id = resolve_session_id(Some("my-session"));
        assert_eq!(id, "my-session");
    }

    #[test]
    fn resolve_session_id_falls_back_to_pid() {
        // Clear the env var to test fallback
        std::env::remove_var("CLAUDE_CODE_SESSION_ID");
        let id = resolve_session_id(None);
        assert!(id.starts_with("pid-"));
    }

    #[test]
    fn resolve_run_id_generates_default() {
        let id = resolve_run_id();
        assert!(id.starts_with("run-"));
    }

    fn lease_at(key: &str, session: &str, claimed_at: i64) -> store::Lease {
        store::Lease {
            key: key.to_string(),
            title: format!("title-{key}"),
            session_id: session.to_string(),
            run_id: "r".to_string(),
            claimed_at,
            heartbeat_at: claimed_at,
            scope: Vec::new(),
            done_criteria: None,
        }
    }

    #[test]
    fn pick_session_lease_returns_most_recent_for_session() {
        let mut leases = store::LeaseRegistry::new();
        leases.insert("k1".into(), lease_at("k1", "sess-a", 100));
        leases.insert("k2".into(), lease_at("k2", "sess-a", 300)); // newer
        leases.insert("k3".into(), lease_at("k3", "sess-b", 999));

        let got = pick_session_lease(&leases, "sess-a").expect("sess-a has a lease");
        assert_eq!(got.key, "k2"); // most-recently-claimed of sess-a
    }

    #[test]
    fn pick_session_lease_none_when_session_absent() {
        let mut leases = store::LeaseRegistry::new();
        leases.insert("k1".into(), lease_at("k1", "sess-a", 100));
        assert!(pick_session_lease(&leases, "sess-x").is_none());
    }
}
