# taskprog

Claude Code 向けの、セッションをまたぐ単一の進捗ファイル管理ハーネス。Rust 製。

## 目的

taskprog は、プロジェクトに `.claude/progress.md` を 1 つ持たせ、それをセッション間で常に最新に保つ責務を負う。

役割は 2 つのフックに集約される。

- **SessionStart** で、進捗ファイルを `additionalContext` として注入する。新しいセッションは、開始した瞬間に「何が完了したか・何が残っているか・何がブロックされているか」を把握できる。
- **Stop** で、いま起きたことを反映するよう進捗ファイルの更新をエージェントに促す。

単一の Rust バイナリと 2 つのフック（SessionStart + Stop）だけで動き、サブスクリプションネイティブである。`ANTHROPIC_API_KEY` も追加のインストールも要らない。

管理対象の `.claude/progress.md` は**プロジェクトルート**（セッション cwd から遡って最初に `.git` を持つ祖先。git worktree のルートも該当するので、worktree ごとに別ファイルになる）に置かれる。ルートが見つからない場合、taskprog は cwd にフォールバックせず、何も書かずにその旨を伝える。ファイルは次のような構造を持つ（コードと一緒にコミットしてもよい）。

```markdown
# Progress

Updated: 2026-06-23

## Completed
- budgetguard cost gate wired into Stop

## Pending
- specforge ⑤ parallel-impl worktree merge

## Blockers
- (none)

## Notes
- harness-status reads gauge from ~/.gauge/store, not /state
```

## どうして必要か

長いタスクが 1 セッションに収まることは稀だ。セッションが切り替わるたびに、人間が「前回どこまでやったか」を手作業で再ブリーフィングしていては、引き継ぎのコストが毎回かかり、文脈が抜け落ちる。

taskprog はこの HOTL（human-on-the-loop）の引き継ぎループを閉じる。人間は境界に座って進捗ファイルをレビューし、必要なら方向を修正するだけでよく、各セッションは前回が終わった地点から正確に再開する。手作業の再説明が要らなくなる。

進捗ファイルという 1 つの正典をセッション間で共有することで、「完了・残・ブロッカー」の状態が散逸せず、再開時の文脈喪失を防ぐ。

## どう使うか

### インストール

プラグインとして導入する。

```
/plugin install taskprog@yukineko
```

手動で入れる場合は次のとおり。`taskprog install` が `~/.claude/settings.json` にフックをマージする。

```sh
cargo install --path .
taskprog install
```

### フック配線とサブコマンド

フックは内部で対応するサブコマンドを呼ぶ。主なサブコマンドは以下。

```sh
taskprog session-start   # SessionStart フック: 進捗ファイルを注入する（stdin の JSON を読む）
taskprog stop            # Stop フック: 進捗ファイルの更新をエージェントに促す
taskprog show            # 現在の進捗ファイルを表示する
taskprog write --cwd .   # stdin から progress.md を書き込む（--cwd の既定は .）
taskprog init [TARGET]   # 雛形の設定を書き出す（TARGET の既定は taskprog.toml）
taskprog install         # フックを ~/.claude/settings.json にマージする（--dry-run 可）
taskprog uninstall       # フックを取り除く（--dry-run 可）
taskprog status          # 解決済みの設定を表示する
```

### `/taskprog` で更新する

いつでも `/taskprog` を実行すると、エージェントが現在の Completed / Pending / Blockers の状態で進捗ファイルを書き直す。`--reset` を付けると、進捗ファイルを空にしてから書き直す。Completed・Pending・Blockers の 3 セクションは必須で、空なら `(none)` と書く。

### 設定（`taskprog.toml`）

セッション cwd の `taskprog.toml`、無ければ `~/.taskprog/config.toml` から読む（[`taskprog.example.toml`](taskprog.example.toml) 参照）。

```toml
enabled = true
# progress_file = "~/.claude/progress.md"   # 既定: <プロジェクトルート>/.claude/progress.md
inject_limit = 4096                          # SessionStart で注入するバイト数（0 = 全部）
```

`progress_file` を明示した場合は（`~` を展開して）そのまま使い、プロジェクトルートの探索はしない。セッション単位で無効化したいときは `TASKPROG_DISABLED=1`（または `true`）を指定する。

## License

MIT
