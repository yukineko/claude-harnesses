#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `has_unattributable_write` after heredoc parsing and `sed -i ''` support
//! (commit d277787b4). Contract: Known(true) for any Bash write that does not
//! name its targets; Known(false) only for closed-table named writes. A
//! heredoc body is data, except an unquoted-delimiter body containing `$(` or
//! backticks; a malformed heredoc is unattributable.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)); only the public
//! contract and the transcript format were read.

use harness_core::attribution::has_unattributable_write;
use harness_core::verdict::Determination;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

fn verdict(cmd: &str) -> Determination<bool> {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = std::env::temp_dir().join(format!("attr-heredoc-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let cwd = base.join("repo");
    std::fs::create_dir_all(cwd.join("x")).unwrap();
    for f in ["a.rs", "existing.rs", "gen.py", "x/b.rs"] {
        std::fs::write(cwd.join(f), "x\n").unwrap();
    }
    let cwd = cwd.canonicalize().unwrap();
    let tdir: PathBuf = base.join("transcripts");
    std::fs::create_dir_all(&tdir).unwrap();
    let transcript = tdir.join("sess.jsonl");
    let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
    body.push('\n');
    body.push_str(
        &serde_json::json!({
            "type": "assistant",
            "cwd": cwd.to_string_lossy(),
            "isSidechain": false,
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": cmd}}
            ]}
        })
        .to_string(),
    );
    body.push('\n');
    std::fs::write(&transcript, body).unwrap();
    has_unattributable_write(transcript.to_str().unwrap())
}

fn expect(cmd: &str, want: bool) {
    match verdict(cmd) {
        Determination::Known(got) => assert_eq!(
            got, want,
            "`{cmd}`: expected Known({want}) (unattributable={want}), got Known({got})"
        ),
        other => panic!("`{cmd}`: expected Known({want}), got a non-Known result: {other:?}"),
    }
}

// ---- Known(true): unattributable ----

#[test]
fn unquoted_body_with_cmdsubst() {
    expect("cat > a.rs <<EOF\n$(touch x)\nEOF", true);
}

#[test]
fn unquoted_body_with_backticks() {
    expect("cat > a.rs <<EOF\n`touch x`\nEOF", true);
}

#[test]
fn stage_after_body_cargo_fmt() {
    expect("cat > a.rs <<'EOF'\nx\nEOF\ncargo fmt", true);
}

#[test]
fn stage_after_terminator_line_bash() {
    expect("cat > a.rs <<EOF\nx\nEOF\nbash gen.py", true);
}

#[test]
fn stage_after_terminator_semicolon_bash() {
    expect("cat > a.rs <<EOF\nx\nEOF; bash gen.py", true);
}

#[test]
fn heredoc_piped_to_bash() {
    expect("cat <<EOF | bash\ntouch x\nEOF", true);
}

#[test]
fn heredoc_into_bash() {
    expect("bash <<'EOF'\ntouch x\nEOF", true);
}

#[test]
fn heredoc_into_python() {
    expect("python3 - <<'EOF'\nopen('x','w')\nEOF", true);
}

#[test]
fn no_terminator() {
    expect("cat > a.rs <<EOF\nx", true);
}

#[test]
fn indented_terminator_without_dash() {
    expect("cat > a.rs <<EOF\nx\n EOF", true);
}

#[test]
fn dash_heredoc_then_cargo_fmt() {
    expect("cat > a.rs <<-EOF\n\tx\n\tEOF\ncargo fmt", true);
}

#[test]
fn two_heredocs_one_line() {
    expect("cat <<A > a.rs; bash <<B\nx\nA\ntouch y\nB", true);
}

#[test]
fn double_quoted_delim_then_and_cargo_fmt() {
    expect("cat > a.rs <<\"EOF\"\nx\nEOF && cargo fmt", true);
}

#[test]
fn sed_i_bak_suffix() {
    expect("sed -i .bak s/a/b/ a.rs", true);
}

#[test]
fn sed_i_empty_multi_e() {
    expect("sed -i '' -e s/a/b/ -e s/c/d/ a.rs", true);
}

#[test]
fn sed_i_empty_w_flag() {
    expect("sed -i '' s/a/b/w\\ out a.rs", true);
}

// ---- Known(false): controls ----

#[test]
fn control_quoted_heredoc_redirect_after() {
    expect("cat > a.rs <<'EOF'\nx\nEOF", false);
}

#[test]
fn control_quoted_heredoc_redirect_before_delim() {
    expect("cat <<'EOF' > a.rs\nx\nEOF", false);
}

#[test]
fn control_unquoted_plain_body() {
    expect("cat > a.rs <<EOF\nplain text\nEOF", false);
}

#[test]
fn control_dash_quoted_tab_indented() {
    expect("cat > a.rs <<-'EOF'\n\tx\n\tEOF", false);
}

#[test]
fn control_sed_i_empty_simple() {
    expect("sed -i '' 's/a/b/' a.rs", false);
}

#[test]
fn control_quoted_delim_body_cmdsubst_is_data() {
    expect("cat > a.rs <<'EOF'\n$(touch x)\nEOF", false);
}
