#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod support;
use support::*;

const NOW: i64 = T0 + 5;

fn req(sb: &Sandbox, who: &str, cmd: &str) -> String {
    let o = sb.sm(&["request", "--kind", "push", "--command", cmd], who, NOW);
    assert_eq!(o.code, 0, "stderr {}", o.stderr);
    o.json()["id"].as_str().unwrap().to_string()
}

#[test]
fn queue_is_serial() {
    let sb = Sandbox::new();
    sb.master_and_member();
    let id1 = req(&sb, "A", "git push one");
    let id2 = req(&sb, "A", "git push two");
    assert_ne!(id1, id2);

    let o = sb.sm(&["next"], "A", NOW);
    assert_eq!(o.code, 0, "stderr {}", o.stderr);
    let v = o.json();
    assert_eq!(v["item"]["id"], id1.as_str());
    assert_eq!(v["item"]["status"], "in_progress");
    assert_eq!(v["resumed"], false);

    let v = sb.sm(&["next"], "A", NOW + 1).json();
    assert_eq!(v["item"]["id"], id1.as_str());
    assert_eq!(v["resumed"], true);

    let st = sb.status("A", NOW + 1);
    let q = st["queue"].as_array().unwrap();
    let in_prog = q.iter().filter(|i| i["status"] == "in_progress").count();
    assert_eq!(in_prog, 1);
    assert_eq!(
        q.iter().find(|i| i["id"] == id2.as_str()).unwrap()["status"],
        "pending"
    );

    let o = sb.sm(
        &["complete", &id1, "--outcome", "ok", "--result", "r"],
        "A",
        NOW + 2,
    );
    assert_eq!(o.code, 0, "stderr {}", o.stderr);
    let v = sb.sm(&["next"], "A", NOW + 3).json();
    assert_eq!(v["item"]["id"], id2.as_str());
    assert_eq!(v["resumed"], false);
}

#[test]
fn member_cannot_next_and_state_unchanged() {
    let sb = Sandbox::new();
    sb.master_and_member();
    req(&sb, "B", "git push x");
    let before = sb.status("B", NOW);
    let o = sb.sm(&["next"], "B", NOW);
    assert_eq!(o.code, 1, "stdout {}", o.stdout);
    let after = sb.status("B", NOW);
    assert_eq!(before["queue"], after["queue"]);
}

#[test]
fn requester_gets_result_after_complete() {
    let sb = Sandbox::new();
    sb.master_and_member();
    let id = req(&sb, "B", "git push x");
    let v = sb.sm(&["next"], "A", NOW).json();
    assert_eq!(v["item"]["id"], id.as_str());
    assert_eq!(
        sb.sm(
            &["complete", &id, "--outcome", "ok", "--result", "pushed"],
            "A",
            NOW + 1
        )
        .code,
        0
    );
    let o = sb.sm(&["results"], "B", NOW + 2);
    assert_eq!(o.code, 0, "stderr {}", o.stderr);
    let r = o.json();
    let arr = r["results"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], id.as_str());
    assert_eq!(arr[0]["result"], "pushed");
}

#[test]
fn deny_queued_items_are_serialized_too() {
    let sb = Sandbox::new();
    sb.master_and_member();
    assert!(is_deny(&sb.gate_bash("B", &sb.repo, "git push a", NOW)));
    assert!(is_deny(&sb.gate_bash("B", &sb.repo, "git push b", NOW)));
    let v = sb.sm(&["next"], "A", NOW + 1).json();
    assert_eq!(v["resumed"], false);
    let v2 = sb.sm(&["next"], "A", NOW + 2).json();
    assert_eq!(v2["resumed"], true);
    assert_eq!(v["item"]["id"], v2["item"]["id"]);
}
