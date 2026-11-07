// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-07): `stalled::scan` treats every entry of the project transcript
//! dir as a session dir, including the `<session>.jsonl` FILES Claude Code writes
//! beside the `<session>/` dirs, and reports an Unknown row
//! "cannot list .../<id>.jsonl/subagents: Not a directory" for each
//! (281 such rows seen in `overwatch status --json` for this repo).

use overwatch::stalled::{scan, StallState};
use std::time::SystemTime;

#[test]
#[ignore = "RED: stalled::scan reports Unknown for <session>.jsonl files 2026-11-07"]
fn session_jsonl_file_beside_session_dir_yields_no_unknown_row() {
    let home = std::env::temp_dir().join(format!("ow-stalled-jsonl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let project = std::path::PathBuf::from("/tmp/ow-stalled-jsonl-project");
    let proj_dir = home
        .join(".claude/projects")
        .join(overwatch::stalled::slug(&project));
    let sid = "0d3d5bb8-0000-4000-8000-000000000001";
    std::fs::create_dir_all(proj_dir.join(sid).join("subagents")).unwrap();
    std::fs::write(proj_dir.join(format!("{sid}.jsonl")), "{}\n").unwrap();

    let rows = scan(&home, &project, SystemTime::now());
    let unknown: Vec<_> = rows
        .iter()
        .filter(|r| matches!(r.state, StallState::Unknown { .. }))
        .collect();
    assert!(
        unknown.is_empty(),
        "a <sid>.jsonl file produced Unknown rows: {unknown:?}"
    );
}
