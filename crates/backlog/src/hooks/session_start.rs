use std::time::{SystemTime, UNIX_EPOCH};

use harness_core::hook::HookInput;

use crate::config::Config;
use crate::divergence;
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
    //
    // A cwd with no repo root above it has no project store at all. Returning
    // `None` here would render that as "nothing queued" — and this hook has no
    // exit code and no stderr the agent ever sees, so `additionalContext` is
    // the ONLY channel that can say otherwise (the same reason the divergence
    // notice below is injected even when there is nothing else to say). So the
    // refusal is injected as text instead of swallowed.
    let location = cfg.locate(Some(&cwd_str));
    let tasks_path = match location.tasks_path() {
        Ok(p) => p,
        Err(why) => {
            return Some(format!(
                "## Backlog \u{2014} no project store for this session\n\n{why}\n\n"
            ));
        }
    };
    // A requeue failure (store unreadable, save failed, lock not acquired) is
    // NOT "0 tasks needed requeuing": expired deferrals and stale claims stay
    // out of the queue until something rescues them. stderr never reaches the
    // agent, so the failure is carried into `additionalContext` (CA-backlog-02).
    let mut warnings = String::new();
    match store::requeue_expired(&tasks_path, now) {
        Ok(count) => {
            if count >= 1 {
                eprintln!("{} 件の保留タスクが再キューされました", count);
            }
        }
        Err(e) => {
            warnings.push_str(&format!(
                "## Backlog \u{2014} requeue of expired/stale tasks FAILED\n\n\
                 backlog: requeue_expired failed for {}: {e:#} \u{2014} expired deferrals and \
                 stale claims were NOT returned to the queue; the pending list below may be \
                 incomplete.\n\n",
                tasks_path.display()
            ));
        }
    }

    // A repo store IS the scope: it is a tracked, repo-local file, so every
    // task in it belongs to this repo whichever checkout wrote it. Filtering
    // by the label of the checkout this session happens to be running in is
    // what hid one machine's queue from the other (see `main`'s
    // `read_project_scope`). A pinned store may hold several projects, so
    // there the label is still the only separator.
    let scope = match location.project_root() {
        Some(_) => None,
        None => Some(root.as_str()),
    };
    let listed = store::list(&tasks_path, None, scope, None);

    // Store divergence (backlog 5ba13c3e). This hook's own failure mode is the
    // silent one: injecting NOTHING is how a session was told "no queue" while
    // 490 items sat in the legacy store, and unlike the CLI there is no exit
    // code and no stderr the agent ever sees — `additionalContext` is the only
    // channel that reaches it. So the notice is injected here, and injected
    // EVEN WHEN there is nothing else to say (the empty case is precisely the
    // one that needs it).
    let notice = divergence::check(&tasks_path, Some(&root), scope)
        .message()
        .map(|m| format!("## Backlog \u{2014} store divergence\n\n{m}\n\n"));

    // A read/parse failure is NOT an empty queue (CA-backlog-01). Returning
    // `None` here made "tasks.toml is unreadable" byte-identical to "nothing
    // queued" on the only channel this hook has, so the failure is injected
    // explicitly — alongside the divergence notice, which does not depend on
    // the resolved store being readable.
    let tasks = match listed {
        Ok(tasks) => tasks,
        Err(e) => {
            let mut out = notice.unwrap_or_default();
            out.push_str(&warnings);
            out.push_str(&format!(
                "## Backlog \u{2014} tasks.toml UNREADABLE\n\n\
                 backlog: tasks.toml unreadable at {}: {e:#} \u{2014} queue state UNKNOWN, not \
                 empty. Do not treat this as \"no pending tasks\"; fix or restore the file \
                 (e.g. `backlog list` to reproduce the error).\n",
                tasks_path.display()
            ));
            return Some(out);
        }
    };

    // pending または failed のタスクのみ対象 (is_pending() で判定)
    let mut pending: Vec<_> = tasks.into_iter().filter(|t| t.is_pending()).collect();

    // 優先度順 (priority() 昇順)、同優先度は created_at 昇順
    pending.sort_by_key(|t| (t.priority(), t.created_at));

    if pending.is_empty() {
        let out = notice.unwrap_or_default() + &warnings;
        return if out.is_empty() { None } else { Some(out) };
    }

    let mut out = notice.unwrap_or_default();
    out.push_str(&warnings);
    out.push_str("## Backlog \u{2014} pending tasks for this project\n\n");

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
    use harness_core::hook::HookInput;

    /// Builds a fresh temp "repo" — a dir with a `.git` entry so
    /// `Config::locate` resolves `StoreLocation::Repo` — with `<repo>/.backlog/`
    /// pre-created and `tasks.toml` written verbatim as `contents`. Independent
    /// of the real `$HOME`: `Config::load()` only consults
    /// `~/.backlog/config.toml`, which this repo's environment does not have
    /// (`store_dir_pinned` stays false), so resolution falls straight through
    /// to this temp repo root.
    fn repo_with_tasks_toml(contents: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".git")).expect(".git");
        let backlog_dir = dir.path().join(".backlog");
        std::fs::create_dir_all(&backlog_dir).expect(".backlog");
        std::fs::write(backlog_dir.join("tasks.toml"), contents).expect("write tasks.toml");
        dir
    }

    fn hook_input_for(cwd: &std::path::Path) -> HookInput {
        HookInput {
            cwd: cwd.to_string_lossy().to_string(),
            ..Default::default()
        }
    }

    /// CA-backlog-01: `run`'s line `store::list(&tasks_path, None, scope,
    /// None).ok()?` uses `?` on the `.ok()`-converted `Result`, so a
    /// read/parse failure and "queue is empty" produce the exact same
    /// observable outcome — `None`, i.e. zero `additionalContext` — even
    /// though this hook has no exit code and no stderr the agent ever sees,
    /// so `additionalContext` is the only channel that could say otherwise.
    #[test]
    fn ca_backlog_01_corrupt_tasks_toml_is_silently_treated_as_empty_queue() {
        let dir = repo_with_tasks_toml("this is not valid toml [[[ } not even close\n");
        let input = hook_input_for(dir.path());

        let result = run(&input);

        assert!(
            result.is_some(),
            "session_start::run returned None for a repo whose tasks.toml fails to parse \
             (CA-backlog-01, session_start.rs:67); that is byte-identical to the empty-queue \
             case (also None, see :86-87) and gives the agent no signal — via the only channel \
             this hook has, additionalContext — that the store is unreadable rather than empty"
        );
    }

    /// CA-backlog-02: `run`'s line `if let Ok(count) = store::requeue_expired(...)`
    /// has no `else` arm. When `requeue_expired` errors (its own `save` can
    /// fail independently of `load`, e.g. the store directory is not
    /// writable), the hook proceeds exactly as if 0 tasks were requeued — no
    /// log, no diagnostic. A stale `claimed` task (`status="claimed"`, which
    /// `Task::is_pending()` does NOT count as pending) that `requeue_expired`
    /// should rescue back to `pending` therefore stays `claimed` forever,
    /// invisible to every future SessionStart, with nothing ever reaching the
    /// agent to say so.
    #[test]
    fn ca_backlog_02_requeue_expired_failure_is_swallowed_without_diagnostic() {
        let dir = repo_with_tasks_toml(
            r#"[[task]]
id = "stale-claim-01"
title = "stale claim needing rescue"
project = "whatever"
status = "claimed"
tags = []
notes = ""
created_at = 1000
updated_at = 1000
"#,
        );

        // Make `.backlog` read-only-and-unwritable AFTER seeding tasks.toml, so
        // `load()` (read-only) still succeeds but `requeue_expired`'s `save()`
        // (which must create a new temp file in this directory) fails — this
        // isolates the "save fails, load still works" fault the finding names,
        // independently of CA-backlog-01's "load itself fails" fault.
        let backlog_dir = dir.path().join(".backlog");
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&backlog_dir).unwrap().permissions();
            perms.set_mode(0o555);
            std::fs::set_permissions(&backlog_dir, perms).unwrap();
        }

        let input = hook_input_for(dir.path());
        let result = run(&input);

        // Restore write permission so the TempDir's own Drop cleanup can
        // remove the directory's contents.
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&backlog_dir).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&backlog_dir, perms).unwrap();
        }

        // Ground truth: the fixture really is a genuine stale claim that WOULD
        // requeue given a writable directory (proving the earlier failure
        // inside `run` was the injected fault, not a malformed fixture that
        // would never requeue regardless of permissions).
        let requeues_once_writable =
            store::requeue_expired(&backlog_dir.join("tasks.toml"), 9_999_999_999).is_ok();
        assert!(
            requeues_once_writable,
            "fixture is broken: the stale-claim task never requeues even once permissions are \
             restored, so this test's fault injection is not exercising CA-backlog-02 at all"
        );

        let text = result.unwrap_or_default();
        assert!(
            text.to_lowercase().contains("requeue")
                || text.contains("再キュー")
                || text.contains("stale-claim-01"),
            "session_start::run gave no diagnostic at all about the failed requeue_expired \
             call (CA-backlog-02, session_start.rs:51); additionalContext was: {text:?}"
        );
    }

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
