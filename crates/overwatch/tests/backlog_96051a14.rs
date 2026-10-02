#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 96051a14: overwatch README / README.ja / SKILL say the base dir
//! defaults to `~/.local/share/claude-harnesses` and is configurable in
//! `overwatch.toml`. The implementation is `harness_core::config::base_dir`
//! = `$HOME/.overwatch`, and no `overwatch.toml` is read anywhere in
//! overwatch's src. The docs contradicted the code (CLAUDE.md §4).
//!
//! FIXED in 74e55ba2; this is the regression test. RED observed against the
//! 74e55ba2^ README.md / README.ja.md / skills/overwatch/SKILL.md.

const DOCS: &[(&str, &str)] = &[
    ("README.md", include_str!("../README.md")),
    ("README.ja.md", include_str!("../README.ja.md")),
    (
        "skills/overwatch/SKILL.md",
        include_str!("../skills/overwatch/SKILL.md"),
    ),
];

/// The fact the docs must agree with (GREEN): base_dir("overwatch") is the
/// `.overwatch` directory, not `.local/share/claude-harnesses`.
#[test]
fn implementation_base_dir_is_dot_overwatch() {
    let base = harness_core::config::base_dir("overwatch");
    assert_eq!(
        base.file_name().and_then(|s| s.to_str()),
        Some(".overwatch")
    );
    assert!(!base.to_string_lossy().contains(".local/share"));
}

#[test]
fn docs_do_not_claim_local_share_base_dir_or_overwatch_toml() {
    let mut hits = Vec::new();
    for (name, body) in DOCS {
        for (i, l) in body.lines().enumerate() {
            if l.contains(".local/share/claude-harnesses") || l.contains("overwatch.toml") {
                hits.push(format!("{name}:{}: {}", i + 1, l.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "docs claim a base dir / config file the implementation does not use \
         (implementation: ~/.overwatch, no overwatch.toml):\n{}",
        hits.join("\n")
    );
}
