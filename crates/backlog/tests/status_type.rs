//! User ruling 2026-10-15: "needs-ruling" and "unconfirmed" are rejected at
//! the TYPE level. `backlog::status::Status` has exactly five variants and no
//! catch-all, so those strings cannot deserialize. Written by an independent
//! test writer (CLAUDE.md §2(a)).
mod common;
use backlog::status::Status;
use common::*;
use std::str::FromStr;

const REMOVED: [&str; 2] = ["needs-ruling", "unconfirmed"];
const KEPT: [&str; 5] = ["pending", "done", "failed", "cancelled", "claimed"];

#[derive(serde::Deserialize, Debug)]
struct Row {
    #[allow(dead_code)]
    status: Status,
}

#[test]
fn json_rejects_removed_statuses_with_unknown_variant() {
    for s in REMOVED {
        let e = serde_json::from_str::<Status>(&format!("\"{s}\""))
            .expect_err(&format!("{s} must not deserialize"));
        assert!(e.to_string().contains("unknown variant"), "{s}: {e}");
    }
}

#[test]
fn toml_rejects_removed_statuses_with_unknown_variant() {
    for s in REMOVED {
        let e = toml::from_str::<Row>(&format!("status = \"{s}\""))
            .expect_err(&format!("{s} must not deserialize"));
        assert!(e.to_string().contains("unknown variant"), "{s}: {e}");
    }
}

#[test]
fn from_str_rejects_removed_statuses() {
    for s in REMOVED {
        assert!(Status::from_str(s).is_err(), "{s} must not parse");
        assert!(s.parse::<Status>().is_err(), "{s} must not parse");
    }
}

#[test]
fn kept_statuses_round_trip_everywhere() {
    for s in KEPT {
        let from_json: Status = serde_json::from_str(&format!("\"{s}\"")).unwrap();
        let from_toml: Row = toml::from_str(&format!("status = \"{s}\"")).unwrap();
        let from_str = Status::from_str(s).unwrap();
        assert_eq!(from_json, from_str, "{s}");
        assert_eq!(from_toml.status, from_str, "{s}");
        assert_eq!(from_str.as_str(), s);
        assert_eq!(from_str.to_string(), s);
        assert_eq!(
            serde_json::to_string(&from_str).unwrap(),
            format!("\"{s}\""),
            "{s}"
        );
    }
}

#[test]
fn exactly_five_distinct_statuses() {
    let all: Vec<Status> = KEPT.iter().map(|s| s.parse().unwrap()).collect();
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

fn assert_store_refuses(tag: &str, id: &str, status: &str) {
    let f = Fixture::new(tag);
    let p = f.project();
    f.plant_open(id, "planted", status, &p);
    for args in [vec!["list"], vec!["list", "--all", "--json"]] {
        let o = f.run(&args);
        assert_ne!(o.code, 0, "{args:?} must fail: {}", o.both());
        assert!(
            !o.stdout.contains(id),
            "{args:?} must print no rows: {}",
            o.both()
        );
        assert!(
            o.stderr.contains(id),
            "{args:?} stderr names {id}: {}",
            o.both()
        );
        assert!(
            o.stderr.contains(status),
            "{args:?} stderr names {status}: {}",
            o.both()
        );
    }
}

#[test]
fn store_with_needs_ruling_row_fails_naming_id_and_status() {
    assert_store_refuses("st-nr", "zz000001", "needs-ruling");
}

#[test]
fn store_with_unconfirmed_row_fails_naming_id_and_status() {
    assert_store_refuses("st-uc", "zz000001", "unconfirmed");
}
