//! A refusal's reason must not say the gate does not refuse.
//!
//! Observed 2026-10-20 (verbatim PreToolUse block):
//! `'> /Users/yuki/src/.harness-worktrees/bg-p1-c5b9/crates/blastguard/src/detect.rs'
//! destroys the file's current contents and it is inside a git work tree,
//! which this gate does not refuse or ask about` — the gate refused while its
//! own reason says it does not refuse. The second half is
//! `Recovery::RecoverableFromGit.describe()` (src/reversible.rs), appended to
//! the redirect Deny in src/detect.rs when the target is recoverable from git
//! but neither inside a confined safe root nor a recognised worktree checkout.
//!
//! Reproduced with the hook's scoped entry: the session tree is a fresh temp
//! dir, and the redirect truncates this crate's own tracked `src/lib.rs`,
//! which lies in a git work tree OUTSIDE that session tree.

#![allow(clippy::expect_used)]

use blastguard::detect::detect_scoped;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;

fn identity(p: &str) -> Option<String> {
    Some(p.to_string())
}

#[test]
#[ignore = "open defect: refusal reason claims the gate does not refuse (backlog ticket filed from this commit)"]
fn refusal_reason_does_not_claim_the_gate_does_not_refuse() {
    let session = std::env::temp_dir().join(format!("bg-reason-{}", std::process::id()));
    std::fs::create_dir_all(&session).expect("create session dir");
    let session = session.to_str().expect("utf-8 temp path").to_string();
    let roots = SafeRoots::new(
        Some(&session),
        Some(&session),
        Some("/nonexistent-home-for-test"),
        None,
        Some(identity),
    );
    // A `.rs` source file: `Cargo.toml` would be exempt as a config file.
    let target = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs");
    let cmd = format!("echo x > {target}");
    let d = detect_scoped("Bash", Some(&serde_json::json!({ "command": cmd })), &roots);
    let _ = std::fs::remove_dir(&session);
    let refused = matches!(d, Decision::Deny(_) | Decision::Ask(_));
    let reason = match &d {
        Decision::Deny(r) | Decision::Ask(r) => r.as_str(),
        Decision::Allow => "",
    };
    assert!(
        refused,
        "precondition: the redirect onto a tree outside the session must be refused, got {d:?}"
    );
    assert!(
        !reason.contains("does not refuse or ask"),
        "the gate refused with a reason claiming it does not refuse or ask: {d:?}"
    );
}
