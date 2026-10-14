- **SessionStart** で、進捗ファイルをトップレベルの `additionalContext` として注入する [test: crates/taskprog/tests/prose_claims.rs::claim_session_start_injects]。
- **Stop** で、進捗ファイルの更新をエージェントに促す [test: crates/taskprog/tests/prose_claims.rs::claim_stop_prompts_update]。

2 つのフック（SessionStart + Stop）を登録し、それぞれ `taskprog session-start` と `taskprog stop` を呼ぶ [test: crates/taskprog/tests/prose_claims.rs::claim_hooks_json_two_hooks]。

管理対象の `.claude/progress.md` は**プロジェクトルート**（セッション cwd の祖先のうち `.git` を持つもの、`.git` がファイルである git worktree のルートも該当する）に置かれる [test: crates/taskprog/src/config.rs::a_subdirectory_cwd_anchors_at_the_project_root] [test: crates/taskprog/src/config.rs::a_worktree_root_is_anchored_even_though_dot_git_is_a_file]。ルートが見つからない場合、Stop フックは cwd にフォールバックせず、何も書かずにその旨を伝える [test: crates/taskprog/src/update.rs::on_stop_writes_nothing_when_the_root_is_undeterminable] [test: crates/taskprog/tests/prose_claims.rs::claim_no_root_says_so]。Stop が書く雛形には `## Done` セクションがある [test: crates/taskprog/src/update.rs::on_stop_writes_skeleton_when_missing]。

## インストールすること

プラグインとして導入すること。

```
/plugin install taskprog@yukineko
```

手動で入れる場合は次を実行すること。

```sh
cargo install --path .
taskprog install
```

```sh
taskprog session-start   # SessionStart フック: 進捗ファイルを注入する [test: crates/taskprog/tests/prose_claims.rs::claim_session_start_injects]
taskprog stop            # Stop フック: 進捗ファイルの更新をエージェントに促す [test: crates/taskprog/tests/prose_claims.rs::claim_stop_prompts_update]
taskprog show            # 現在の進捗ファイルを表示する [test: crates/taskprog/tests/prose_claims.rs::claim_show_prints_file]
taskprog write --cwd .   # stdin から progress.md を書き込む（--cwd の既定は .） [test: crates/taskprog/tests/prose_claims.rs::claim_write_from_stdin_default_cwd]
taskprog init [TARGET]   # 雛形の設定を書き出す（TARGET の既定は taskprog.toml） [test: crates/taskprog/tests/prose_claims.rs::claim_init_default_target]
taskprog install         # フックを ~/.claude/settings.json にマージする（--dry-run 可） [test: crates/taskprog/tests/prose_claims.rs::claim_install_merges_hooks]
taskprog uninstall       # フックを取り除く（--dry-run 可） [test: crates/taskprog/tests/prose_claims.rs::claim_uninstall_removes_hooks]
taskprog status          # 解決済みの設定を表示する [test: crates/taskprog/tests/prose_claims.rs::claim_config_lookup_order]
```

`/taskprog` スキルは `--reset` 引数を取る [test: crates/taskprog/tests/prose_claims.rs::claim_skill_reset_arg]。

設定はセッション cwd の `taskprog.toml`、無ければ `~/.taskprog/config.toml` から読む [test: crates/taskprog/tests/prose_claims.rs::claim_config_lookup_order]。

```toml
enabled = true
# progress_file = "~/.claude/progress.md"   # 既定: <プロジェクトルート>/.claude/progress.md [test: crates/taskprog/src/config.rs::a_subdirectory_cwd_anchors_at_the_project_root]
inject_limit = 4096                          # SessionStart で注入するファイルのバイト数の上限（0 = 全部） [test: crates/taskprog/tests/prose_claims.rs::claim_inject_limit_bytes]
```

`progress_file` を明示した場合はそのまま使い、プロジェクトルートの探索はしない [test: crates/taskprog/src/config.rs::an_explicit_progress_file_is_unchanged_and_needs_no_project_root]。
`progress_file` の先頭の `~` は展開される [test: crates/taskprog/tests/prose_claims.rs::claim_tilde_expanded]。
`TASKPROG_DISABLED=1`（または `true`）でそのセッションの両フックが無効になる [test: crates/taskprog/tests/prose_claims.rs::claim_disabled_env]。

License: MIT [test: crates/taskprog/tests/prose_claims.rs::claim_license_mit]。
