# harness プロジェクト knowledge

## リポジトリ構造

- Rust workspace: `crates/*` がすべてメンバー（`cargo test --workspace` で全件実行）
- `rust-version = "1.82"`, `edition = "2021"`
- release profile はワークスペースルートに一元化（strip/lto/opt-level="z"）

## 各クレートの役割

| クレート | 種別 | 説明 |
|---|---|---|
| harness-core | ライブラリ | pricing/usage/transcript/config/hook の共有基盤。他クレートは静的リンク |
| session-insights | バイナリ | PostToolUse/Stop/SessionEnd フック + record ノート生成 |
| gauge | バイナリ | Stop フック: トークン・コスト集計 |
| condukt | バイナリ+スキル | タスク分解・worktree 管理・状態追跡 |
| compass | バイナリ+スキル | charter re-grounding → condukt へ routing |
| backlog | バイナリ+スキル | cross-session タスクキュー |
| autoflow | バイナリ | Stop/SessionStart: open backlog ブロックゲート |
| fugu-router | バイナリ | 類似過去タスクからモデル選択、playbook 蓄積 |
| tdd | バイナリ+スキル | Stop: テストなし commit ブロック |
| stuckguard | バイナリ | PostToolUse: 繰り返し検出・ループ遮断 |
| reviewgate | バイナリ | Stop: diff レビューゲート |
| ctxrot | バイナリ | context-rot 検出・蒸留・復元 |
| beacon | バイナリ | デスクトップ/Slack 通知 |
| budgetguard | バイナリ | Stop: USD コスト上限ゲート |
| donegate | バイナリ | Stop: 受け入れコマンド実行ゲート |
| specguard | バイナリ+スキル | 仕様↔実装 整合監査 |
| playbook | バイナリ | UserPromptSubmit: 手順ノート注入 |
| difflog | バイナリ | SessionStart/End: git diff サマリ |
| deepwiki | バイナリ+スキル | リポジトリ wiki 生成 |
| harness-status | バイナリ | budget/sessions/progress 統合ダッシュボード |
| taskprog | バイナリ | SessionStart/Stop: progress.md 注入 |
| runbook | バイナリ | UserPromptSubmit: !macro 展開 |

## harness-core の利用規則

- `pricing`, `usage`, `transcript`, `config`, `hook` は harness-core から re-export する
- 各プラグインクレートで同一ロジックを再実装しない
- gauge の `pricing.rs`/`transcript.rs` が re-export のモデルケース

## プラグイン構造（必須ファイル）

各 `crates/<name>/` に以下が必要:
- `.claude-plugin/plugin.json` — スキル自動検出に必須（欠落するとシステムに現れない）
- `bin/<name>-linux-x86_64` 等 — platform バイナリ（Makefile でビルド）
- `skills/<name>/SKILL.md` — スキル定義（あれば）
- `hooks/hooks.json` — フック定義（あれば）

## テスト規則

- `cargo test --workspace` が常に全件 pass であること（DoD 条件）
- 新機能追加時はユニットテストを同一 PR に含める
- harness-core の変更は全クレートのビルド・テストで影響を確認

## condukt 運用での注意

- `session-insights/src/main.rs` の `stop()` が record 書き出しを行う（`cfg.record=true` 時）
- `autoflow stop` が open backlog あると Stop をブロックする（condukt run 中は競合に注意）
- インストールキャッシュ (`~/.claude/plugins/cache/`) を直接編集しない。ソース変更 → cargo build → キャッシュ更新の順で行う
- plugin.json のバージョンと Cargo.toml のバージョンを揃えておく（現在 session-insights は plugin.json=0.2.0 / Cargo.toml=0.1.0 の乖離あり）
