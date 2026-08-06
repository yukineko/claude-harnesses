use std::time::{SystemTime, UNIX_EPOCH};

use harness_core::hook::HookInput;

use crate::config::Config;
use crate::store;

/// SessionStart hook のメイン処理。
/// cwd に紐づくリポジトリの pending タスクを additionalContext として返す。
pub fn run(input: &HookInput) -> Option<String> {
    if Config::disabled_env() {
        return None;
    }

    let cfg = Config::load();
    if !cfg.enabled {
        return None;
    }

    let cwd = input.cwd_or_current();
    let cwd_str = cwd.to_string_lossy().to_string();
    let root = repo_root(&cwd_str);

    // 期限切れ deferred タスクをキューに戻す。
    // クロックが epoch 以前だと duration_since が失敗するが、SessionStart hook を
    // panic させない（never break a turn）。失敗時は t=0 とみなす＝requeue 0 件
    // （どの defer_until も <= 0 にはならない）。
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // The session's own repo root selects the store, so a SessionStart in
    // project A never reads (or requeues) project B's queue.
    let tasks_path = cfg.tasks_path_for(Some(&root));
    if let Ok(count) = store::requeue_expired(&tasks_path, now) {
        if count >= 1 {
            eprintln!("{} 件の保留タスクが再キューされました", count);
        }
    }

    let tasks = store::list(&tasks_path, None, Some(&root), None).ok()?;

    // pending または failed のタスクのみ対象 (is_pending() で判定)
    let mut pending: Vec<_> = tasks.into_iter().filter(|t| t.is_pending()).collect();

    // 優先度順 (priority() 昇順)、同優先度は created_at 昇順
    pending.sort_by_key(|t| (t.priority(), t.created_at));

    if pending.is_empty() {
        return None;
    }

    let mut out = String::from("## Backlog \u{2014} pending tasks for this project\n\n");

    for task in &pending {
        let priority_str = match task.priority() {
            0 => "p0",
            1 => "p1",
            2 => "p2",
            _ => "-",
        };

        out.push_str(&format!(
            "### [{priority}] {title} (id: {id})\n",
            priority = priority_str,
            title = task.title,
            id = task.id,
        ));

        let tags_str = task.tags.join(", ");
        out.push_str(&format!("tags: {}\n", tags_str));

        if !task.notes.is_empty() {
            out.push_str(&format!("notes: {}\n", task.notes));
        }

        let cycle_instruction = cycle_tag_instruction(task.cycle_tag(), &task.id, &task.title);
        out.push_str(&format!("cycle: {}\n", cycle_instruction));

        out.push('\n');
    }

    out.push_str("---\n\nTo mark a task done: `backlog done {id}`\nTo mark failed: `backlog fail {id} [--reason \"...\"]`\n");

    // inject_limit 超なら切り詰め
    if out.len() > cfg.inject_limit {
        let truncated = truncate_to_byte_boundary(&out, cfg.inject_limit);
        out = format!("{}\n*(truncated)*", truncated);
    }

    Some(out)
}

/// cycle タグ別の指示文を返す。
fn cycle_tag_instruction(cycle_tag: Option<&str>, id: &str, title: &str) -> String {
    match cycle_tag {
        Some("cycle:test-fix") => format!(
            "テスト実行 → 失敗解析 → 修正 → 繰り返し。全テストが green になったら `backlog done {}` を呼ぶ",
            id
        ),
        Some("cycle:tdd") => format!(
            "RED → GREEN → VERIFY の TDD フロー (/tdd スキル)。VERIFY 完了後に `backlog done {}`",
            id
        ),
        Some("cycle:implement") => format!(
            "`/condukt {}` で実装。検証完了後に `backlog done {}`",
            title, id
        ),
        Some("cycle:review-fix") => format!(
            "`/code-review` で差分レビュー → 指摘修正 → 再レビュー。LGTM 後に `backlog done {}`",
            id
        ),
        Some("cycle:once") => format!(
            "一度実行して完了したら `backlog done {}`",
            id
        ),
        _ => format!("`backlog done {}` で完了を記録してください", id),
    }
}

/// UTF-8 境界を壊さずに `max_bytes` バイト以下に切り詰める。
fn truncate_to_byte_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // char boundary を逆から探す
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// .git を上に辿ってリポジトリルートを返す。見つからなければ cwd をそのまま返す。
fn repo_root(cwd: &str) -> String {
    let mut cur = std::path::Path::new(cwd).to_path_buf();
    loop {
        if cur.join(".git").exists() {
            return cur.to_string_lossy().to_string();
        }
        if !cur.pop() {
            break;
        }
    }
    cwd.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_root_returns_cwd_when_no_git() {
        // 存在しないパスは .git が見つからないので cwd を返す
        let result = repo_root("/nonexistent/path/that/has/no/git");
        assert_eq!(result, "/nonexistent/path/that/has/no/git");
    }

    #[test]
    fn repo_root_finds_git_dir() {
        // /tmp は .git がないが、このリポジトリの worktree には .git がある
        let root = repo_root(env!("CARGO_MANIFEST_DIR"));
        // .git が見つかれば cwd と異なる可能性がある。少なくとも文字列が返ること。
        assert!(!root.is_empty());
    }

    #[test]
    fn truncate_to_byte_boundary_short() {
        assert_eq!(truncate_to_byte_boundary("hello", 100), "hello");
    }

    #[test]
    fn truncate_to_byte_boundary_exact() {
        assert_eq!(truncate_to_byte_boundary("hello", 5), "hello");
    }

    #[test]
    fn truncate_to_byte_boundary_ascii() {
        assert_eq!(truncate_to_byte_boundary("hello world", 5), "hello");
    }

    #[test]
    fn cycle_tag_instruction_test_fix() {
        let instr = cycle_tag_instruction(Some("cycle:test-fix"), "abc123", "My Task");
        assert!(instr.contains("backlog done abc123"));
        assert!(instr.contains("green"));
    }

    #[test]
    fn cycle_tag_instruction_tdd() {
        let instr = cycle_tag_instruction(Some("cycle:tdd"), "abc123", "My Task");
        assert!(instr.contains("TDD"));
        assert!(instr.contains("backlog done abc123"));
    }

    #[test]
    fn cycle_tag_instruction_implement() {
        let instr = cycle_tag_instruction(Some("cycle:implement"), "abc123", "My Task");
        assert!(instr.contains("/condukt My Task"));
        assert!(instr.contains("backlog done abc123"));
    }

    #[test]
    fn cycle_tag_instruction_review_fix() {
        let instr = cycle_tag_instruction(Some("cycle:review-fix"), "abc123", "My Task");
        assert!(instr.contains("/code-review"));
        assert!(instr.contains("backlog done abc123"));
    }

    #[test]
    fn cycle_tag_instruction_once() {
        let instr = cycle_tag_instruction(Some("cycle:once"), "abc123", "My Task");
        assert!(instr.contains("backlog done abc123"));
    }

    #[test]
    fn cycle_tag_instruction_unknown() {
        let instr = cycle_tag_instruction(Some("cycle:custom"), "abc123", "My Task");
        assert!(instr.contains("backlog done abc123"));
        assert!(instr.contains("記録"));
    }

    #[test]
    fn cycle_tag_instruction_none() {
        let instr = cycle_tag_instruction(None, "abc123", "My Task");
        assert!(instr.contains("backlog done abc123"));
    }
}
