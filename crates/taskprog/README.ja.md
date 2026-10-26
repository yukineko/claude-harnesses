- **SessionStart** で、進捗ファイルを`hookSpecificOutput.additionalContext`（hookEventName `SessionStart`） として注入する。
- **Stop** で、ファイルが無ければ雛形を書き、`{"systemMessage":"…"}` を出す。これはユーザーにのみ表示され、モデルには届かない（Stop に `additionalContext` は未定義で、トップレベルのものは誰にも届かないことを実測済み）。

2 つのフック（SessionStart + Stop）を登録し、それぞれ `taskprog session-start` と `taskprog stop` を呼ぶ。

管理対象の `.claude/progress.md` は**プロジェクトルート**（セッション cwd の祖先のうち `.git` を持つもの、`.git` がファイルである git worktree のルートも該当する）に置かれる。ルートが見つからない場合、Stop フックは cwd にフォールバックせず、何も書かずにその旨を伝える。Stop が書く雛形には `## Done` セクションがある。

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
taskprog session-start   # SessionStart フック: 進捗ファイルを注入する
taskprog stop            # Stop フック: 雛形を書き、進捗ファイルの更新をユーザーに通知する
taskprog show            # 現在の進捗ファイルを表示する
taskprog write --cwd .   # stdin から progress.md を書き込む（--cwd の既定は .）
taskprog init [TARGET]   # 雛形の設定を書き出す（TARGET の既定は taskprog.toml）
taskprog install         # フックを ~/.claude/settings.json にマージする（--dry-run 可）
taskprog uninstall       # フックを取り除く（--dry-run 可）
taskprog status          # 解決済みの設定を表示する
```

`/taskprog` スキルは `--reset` 引数を取る。

設定はセッション cwd の `taskprog.toml`、無ければ `~/.taskprog/config.toml` から読む。

```toml
enabled = true
# progress_file = "~/.claude/progress.md"   # 既定: <プロジェクトルート>/.claude/progress.md
inject_limit = 4096                          # SessionStart で注入するファイルのバイト数の上限（0 = 全部）
```

`progress_file` を明示した場合はそのまま使い、プロジェクトルートの探索はしない。
`progress_file` の先頭の `~` は展開される。
`TASKPROG_DISABLED=1`（または `true`）でそのセッションの両フックが無効になる。

License: MIT。
