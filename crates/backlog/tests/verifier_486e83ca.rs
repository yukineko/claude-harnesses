//! Independent verifier test for backlog 486e83ca (CLAUDE.md §2(a): written by
//! a verifier, not the implementer).
//!
//! Properties probed against the REAL binary, with `HOME` pinned to a temp dir
//! (the real `~/.backlog` is never read or written):
//!   1. a lease whose holder session heartbeats (`backlog driver heartbeat`)
//!      keeps excluding its task after `claimed_at` is older than
//!      CLAIM_STALE_SECS;
//!   2. a lease whose holder stopped heartbeating more than CLAIM_STALE_SECS
//!      ago is reclaimable (a crashed claimer cannot strand its task);
//!   3. adversarial: a claim made with an EMPTY `CLAUDE_CODE_SESSION_ID` must
//!      not become refreshable by `driver heartbeat --session-id ""` (an empty
//!      id would otherwise match every other session-less claimer and keep a
//!      dead holder's lease alive);
//!   4. adversarial: a malformed `heartbeat_at` is never read as "holder dead"
//!      by ANY reader — plain `next`, `list` and `next --claim` all refuse
//!      (non-zero) rather than re-offering the leased task.

mod common;

use common::{Fixture, Out};

const CLAIM_STALE_SECS: i64 = 3600;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

struct V {
    f: Fixture,
    id: String,
    title: String,
}

fn setup(tag: &str) -> V {
    let f = Fixture::new(&format!("v486-{tag}"));
    let title = format!("verifier 486e83ca {tag} task");
    let id = f.add(&title);
    V { f, id, title }
}

impl V {
    fn as_session(&self, args: &[&str], session: &str) -> Out {
        self.f.run_env(args, &[("CLAUDE_CODE_SESSION_ID", session)])
    }

    fn claim(&self, session: &str) -> Out {
        let p = self.f.project();
        self.as_session(&["next", "--claim", "--project", &p], session)
    }

    fn heartbeat(&self, session: &str) -> Out {
        let p = self.f.project();
        self.as_session(
            &[
                "driver",
                "heartbeat",
                "--session-id",
                session,
                "--project",
                &p,
            ],
            session,
        )
    }

    fn ledger(&self) -> std::path::PathBuf {
        let dir = self.f.home.join(".backlog").join("claims");
        let mut v: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("no claims dir {}: {e}", dir.display()))
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        assert_eq!(v.len(), 1, "exactly one ledger expected: {v:?}");
        v.pop().unwrap()
    }

    /// Set fields on every ledger entry (raw JSON edit, as a clock would).
    fn set(&self, fields: &[(&str, serde_json::Value)]) {
        let p = self.ledger();
        let mut doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let entries = doc["entries"].as_array_mut().expect("entries array");
        assert!(!entries.is_empty(), "ledger holds no lease");
        for e in entries {
            for (k, v) in fields {
                e[*k] = v.clone();
            }
        }
        std::fs::write(&p, serde_json::to_string(&doc).unwrap()).unwrap();
    }

    fn first_claim(&self, session: &str) {
        let o = self.claim(session);
        assert_eq!(o.code, 0, "first claim failed: {}", o.both());
        assert!(
            o.stdout.contains(&self.id),
            "first claim did not take the task: {}",
            o.both()
        );
    }
}

/// Property 1 (RED before the fix: the lease lapsed purely by claim age).
#[test]
fn verifier_live_holder_keeps_lease_past_window() {
    let v = setup("live");
    v.first_claim("holder-A");
    let hb = v.heartbeat("holder-A");
    assert_eq!(hb.code, 0, "heartbeat failed: {}", hb.both());
    // The claim itself is now 2h old; the holder's heartbeat just happened.
    v.set(&[(
        "claimed_at",
        serde_json::json!(now() - 2 * CLAIM_STALE_SECS),
    )]);

    let other = v.claim("intruder-B");
    assert!(
        !other.stdout.contains(&v.id) && !other.stdout.contains(&v.title),
        "a live (heartbeating) holder's task was handed to another session: {}",
        other.both()
    );
}

/// Property 2: the holder heartbeated once, then died. Both its claim and its
/// last heartbeat are older than the window, so the task must be reclaimable.
#[test]
fn verifier_dead_holder_is_reclaimable() {
    let v = setup("dead");
    v.first_claim("holder-A");
    let hb = v.heartbeat("holder-A");
    assert_eq!(hb.code, 0, "heartbeat failed: {}", hb.both());
    let old = now() - CLAIM_STALE_SECS - 120;
    v.set(&[
        ("claimed_at", serde_json::json!(old)),
        ("heartbeat_at", serde_json::json!(old)),
    ]);

    let other = v.claim("rescuer-B");
    assert_eq!(other.code, 0, "reclaim refused: {}", other.both());
    assert!(
        other.stdout.contains(&v.id),
        "a dead holder stranded its task: {}",
        other.both()
    );
}

/// Property 3 (adversarial): empty session id must not make a lease
/// refreshable by an empty-id heartbeat.
#[test]
fn verifier_empty_session_id_cannot_keep_dead_lease_alive() {
    let v = setup("empty");
    v.first_claim("");
    // Someone heartbeats with the empty id while the lease is still live.
    let _ = v.heartbeat("");
    v.set(&[(
        "claimed_at",
        serde_json::json!(now() - CLAIM_STALE_SECS - 120),
    )]);

    let other = v.claim("rescuer-B");
    assert_eq!(other.code, 0, "reclaim refused: {}", other.both());
    assert!(
        other.stdout.contains(&v.id),
        "an empty-session lease was kept alive by an empty-id heartbeat: {}",
        other.both()
    );
}

/// Property 4 (adversarial, RED before the fix: the unknown field was ignored
/// and the claim-aged lease re-offered): a malformed liveness field refuses on
/// every reader.
#[test]
fn verifier_malformed_heartbeat_refused_by_every_reader() {
    let v = setup("garbage");
    v.first_claim("holder-A");
    v.set(&[
        (
            "claimed_at",
            serde_json::json!(now() - 2 * CLAIM_STALE_SECS),
        ),
        ("heartbeat_at", serde_json::json!("yesterday-ish")),
    ]);
    let p = v.f.project();
    for (what, args) in [
        ("plain next", vec!["next", "--project", p.as_str()]),
        ("list", vec!["list", "--project", p.as_str()]),
        (
            "next --claim",
            vec!["next", "--claim", "--project", p.as_str()],
        ),
    ] {
        let o = v.as_session(&args, "reader-C");
        assert_ne!(
            o.code,
            0,
            "{what} treated an unparseable liveness field as readable/dead: {}",
            o.both()
        );
        if what != "list" {
            assert!(
                !o.stdout.contains(&v.id),
                "{what} re-offered the task under an unparseable ledger: {}",
                o.both()
            );
        }
    }
}
