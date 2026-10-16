---
name: taskprog
description: 進捗ファイルを更新するときに使うこと。「何が完了したか」「何が残っているか」「ブロッカーは何か」を箇条書きで簡潔に整理して書き込むこと。
argument-hint: [--reset]
allowed-tools: Bash(taskprog:*), Read, Write
---

- ナラティブは書かないこと。
- 箇条書きのみで書くこと。
- 完了・残・ブロッカーの3セクションを必ず書き、空なら "(none)" と書くこと。

1) 現在の進捗ファイルを `taskprog show` で確認すること [test: crates/taskprog/tests/prose_claims.rs::claim_show_prints_file]。
2) このセッションで完了したタスク・残タスク・ブロッカーを把握すること。
3) 以下の構造で `.claude/progress.md`（プロジェクトルート直下）を Write ツールで書き込むこと [test: crates/taskprog/src/config.rs::a_subdirectory_cwd_anchors_at_the_project_root]。

```markdown
# Progress

Updated: <date>

## Completed
- <task>

## Pending
- <task>

## Blockers
- <issue> (if any)

## Notes
- <context for next session>
```

4) 書き込み後、`taskprog show` で確認すること [test: crates/taskprog/tests/prose_claims.rs::claim_show_prints_file]。

`--reset` が渡されたら、進捗ファイルを空にしてから書き直すこと [test: crates/taskprog/tests/prose_claims.rs::claim_skill_reset_arg]。
