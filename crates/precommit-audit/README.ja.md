# precommit-audit

設定駆動のクロスプラットフォームな pre-commit 静的監査フック。汎用チェックとプロジェクト固有ルールを、保留中の diff に対してコミット前に走らせる。

## 目的

precommit-audit は、コミット前（あるいは Claude Code の停止前）にワーキングセットを静的監査し、問題のある変更をブロックするフックである。`git diff HEAD` と未追跡ファイルを検査し、見つかった指摘を報告する。

監査ロジックは二層に分かれている。

- **汎用チェック（バイナリ組み込み）。** テストを伴わないソース変更、ハードコードされた IP アドレスやシークレット（`password = "…"`）、握り潰された例外や `|| true`、関数の重複定義、`set -e` スクリプト中の `local VAR=$(…)` という silent-failure、壊れた Markdown リンク、CRLF/LF の改行コード、外部リンタ（py_compile・ruff・bash -n・eslint・tsc・radon・semgrep・gitleaks、いずれも任意）など。ファイルが長すぎる場合は警告のみ（ブロックしない）。
- **プロジェクト固有ポリシー（TOML データ）。** `[[rule]]` エントリとして表現する。追加行に対する正規表現に、glob によるスコープ指定と allowlist を組み合わせたもので、コードにハードコードしない。

設計上の要点は、バイナリ自体が汎用・再利用可能であることだ。プロジェクト固有の方針はすべてコードから引き剥がされ `.precommit-audit.toml` に置かれるため、同じバイナリを複数のリポジトリで使い回せる。元は PowerShell（Windows 専用）の pre-commit フックだったものを、Linux/macOS/Windows でまったく同じに動く単一の静的バイナリとして Rust に書き直したものである。

サブスクリプションネイティブ（フック 1 本と同梱の Rust バイナリのみ。`ANTHROPIC_API_KEY` も追加インストールも不要）。

## どうして必要か

汚れたコミットは、それ自体が小さな失敗モードの集積だ。テストの無いソース変更、消し忘れたハードコードシークレット、握り潰された例外——どれも単体では見過ごされやすく、レビューやリンタの設定が揃うまで気づかれない。precommit-audit はこの種の指摘を、コミットが成立する前に機械的に止める。

- **ポリシーがコードに埋まる問題を避ける。** 監査ルールをスクリプトに書き込むと、リポジトリごとに別物のフックを保守する羽目になる。precommit-audit はバイナリを汎用に保ち、各リポジトリの方針を `.precommit-audit.toml` に外出しする。新しいポリシーは `[[rule]]` ブロックを足すだけで、バイナリには触れない。
- **プラットフォーム差で動かない問題を避ける。** 元の PowerShell フックは Windows でしか動かず、CP932 由来の文字化け対策も抱えていた。本実装は UTF-8 一貫で、3 つの OS 上で単一バイナリとして同一に動く。
- **人間のコミットと Claude Code の停止、両方を塞ぐ。** git フック（人間のコミット）と Claude Code の Stop フック（エージェントの停止）では要求される契約が異なる。precommit-audit は dual-mode フックとして両方に対応し、それぞれの規約に従った終了コードで止める。
- **未信頼リポジトリの設定実行を防ぐ。** クローンしてきた未信頼リポジトリの設定をそのまま honor すると、`linters.node_projects` がリポジトリ同梱の `eslint`/`tsc` を解決して実行してしまう余地がある。自動発見した設定は root を信頼するまで無視され（組み込みチェックはデフォルトで走る）、信頼すれば honor される。

## 新しいプロジェクトへの導入

バイナリはプロジェクト非依存であり、特定のリポジトリの事情は何も焼き込まれていない。他のプロジェクトで使うには:

1. **バイナリを一度インストールする**（`cargo install --path .`）。`precommit-audit` が `PATH` 上に来る。
2. **（任意）設定を足す。** リポジトリルートに `.precommit-audit.toml` を置く。設定が無くても汎用チェックは走る。チューニングやプロジェクト固有の `[[rule]]` を宣言したいときだけ設定を足す。注釈付きテンプレート `.precommit-audit.toml` や `examples/web-project.toml`（実例）を出発点にする。
3. **フックに配線する** — pre-commit フレームワーク、または生の git フック（下記「git フック」「Claude Code の Stop フックとして」参照）。
4. **プロジェクトルールを都度足す。** 新しいポリシーはそれぞれ `[[rule]]` ブロック（追加行への正規表現、glob スコープと allowlist 付き）であり、バイナリ自体には触れない。不要な組み込みチェックは `[checks]` で無効化する。

これで完了: 同じバイナリがすべてのリポジトリに使い回され、各リポジトリの `.precommit-audit.toml` が自分の方針を持つ。

## どう使うか

バイナリをインストールし（`cargo install --path .`、または `cargo build --release` で `target/release/precommit-audit`）、フックに配線する。

```sh
precommit-audit [--mode stop|precommit] [--config <file>] [--root <dir>]
precommit-audit trust   # <root> を信頼し、その .precommit-audit.toml を honor させる
```

主なフラグとサブコマンド:

- `--mode precommit` — pre-commit フレームワーク / git フック（人間のコミット）向け。失敗時に **1** で終了し、レビュー契約はスキップする。
- `--mode stop`（既定）— Claude Code の Stop フック向け。subagent のレビュー契約を honor し、指摘をエージェントへ戻すため **2** で終了する。**SessionEnd** 実行時は advisory（助言）モードで走り、ブロッキングな指摘も引き続き表面化する（stderr へ目立つ形で出力し、監査ログに `block` として記録する）が、終了コードは **0** のままなので、監査がセッションを失敗させることはない。
- `--config` — 既定は `<root>/.precommit-audit.toml`。明示指定はオペレータの意図的選択として、信頼の有無にかかわらず常に honor される。
- `--root` — 既定は `$CLAUDE_PROJECT_DIR`、無ければ git のトップレベル。
- `trust` — 解決済みの `--root` を共有のワークスペース信頼リスト（`harness_core::trust`。`donegate`/`reviewgate`/`tdd` と同じリスト）に追加し、自動発見された `.precommit-audit.toml` を honor させる。信頼するまではリポジトリ同梱の設定は無視される（組み込みチェックはデフォルトで動作）。

終了コード: `0` クリーン・`1` ブロック（precommit）・`2` ブロック（stop）。

### git フック（生）として

```sh
# .git/hooks/pre-commit   (chmod +x)
#!/bin/sh
exec precommit-audit --mode precommit
```

### pre-commit フレームワークのフックとして

```yaml
# .pre-commit-config.yaml
- repo: local
  hooks:
    - id: precommit-audit
      name: precommit-audit
      entry: precommit-audit --mode precommit
      language: system
      pass_filenames: false
```

### Claude Code の Stop フックとして

```json
{ "hooks": { "Stop": [ { "hooks": [
  { "command": "precommit-audit --mode stop", "timeout": 30 }
] } ] } }
```

### 設定と抑制

すべての設定は `.precommit-audit.toml` に置く。すべてに組み込みの既定値があるため、ファイル自体は任意だ。設定が無くても汎用チェックは走り、チューニングや `[[rule]]` の宣言が必要なときだけ書く。`[checks]` で不要な組み込みチェックを無効化できる。本リポジトリの注釈付きテンプレート `.precommit-audit.toml` や、`examples/web-project.toml`（node プロジェクトのルート、`console.log` / `print()` ルールを glob でスコープした実例）を出発点にするとよい。

指摘の抑制:

- 行単位: 行末に `# audit-ignore: <理由>` を付ける（JS/TS は `//`）。理由は必須で、マーカーだけでは抑制されない。
- ファイル単位: 先頭 20 行以内に `audit-ignore-file: <理由>` を書く。
- 一回限りのバイパス: `<audit_dir>/.audit-skip` を作る（読み取り時に消費される）。

### 他の Stop ゲート（donegate / reviewgate / tdd）との関係

precommit-audit は意図的に、`harness_core::gate` 上に構築された JSON Stop ゲートの一員ではない。あの 3 つは Claude 専用の Stop フックで、`{"decision":"block","reason":…}` を出力してブロックする。precommit-audit は git フック（`precommit` モード、失敗時 **1**）と Claude Code Stop フック（`stop` モード、**2**）の両方として動き、さらに advisory な **SessionEnd** パス（**0**。ブロッキングな指摘を表面化・記録しつつセッションは失敗させない）も備える dual-mode フックである。git フックは Claude の JSON `decision:block` プロトコルを話せないため、終了コード＋ブロックマーカーという別の契約を保つ。プロジェクトローカル設定を `harness_core::trust` の背後でゲートする点は 3 つと共通だが、JSON Stop ゲートではなく、その兄弟として扱う。

## この repo 自身での位置づけ（self-apply、2026-07-23決定）

precommit-audit はこの repo が生み出し配布する project-agnostic な製品だが、この repo 自身の
git-commit-time ゲート（`.githooks/pre-commit`。injectguard/fail-open-guard/doc-claims/
test-weakening/version-lockstep/bump-on-change/secret-guardの7本、`.githooks/pre-commit:26-43`）
には**統合されていない**（別々に発展した独立実装のまま）。ただし precommit-audit が user scope で
インストール済みの環境では、Stop hook（`crates/precommit-audit/hooks/hooks.json:2-8`）として
この repo 上の Claude Code セッションでも実際に発火する——つまり `.githooks/pre-commit` には
現れないが、この repo でも「使われていない」わけではない。

**決定: 現状維持**。`.githooks/pre-commit` への統合は行わず、precommit-audit は引き続き
project-agnostic な独立製品として、Stop hook 経由でのみこの repo に効く。理由は「意図的な
設計判断が過去にあった」からではなく（そこまでの経緯は確認できていない）、統合の
コスト（`.githooks/pre-commit` の6本は個々に stdlib-only python3 で書かれ独自の許容/除外
規則を持つため、precommit-audit の汎用ルールへ置き換えるには両者のポリシーを一致させる
作業が要る）に見合う具体的な破綻が今のところ観測されていないため。

**記録したギャップ → 解消済み（2026-07-23、backlog aa74be67）**: 以前はここに「`.githooks/
pre-commit` の6ゲートには `check_hardcoded_secret`（`crates/precommit-audit/src/checks/mod.rs:
175-199`）に相当する汎用シークレットスキャンが1つも無い」というギャップを記録していた。
`scripts/check-hardcoded-secret.py`（secret-guard、`.githooks/pre-commit:38`で起動）が
precommit-audit を wholesale 統合せずにこの gap だけを埋めた — 同じキー名・同じ shape 要件
（`(password|passwd|secret|api[_-]?key|token|private[_-]?key) = "<4文字以上>"`）で
`check_hardcoded_secret` と歩調を合わせつつ、対象は ADDED 行のみ（`git diff <base>`、
既存の6ゲートと同じ独立 stdlib-only python3 実装）。したがって precommit-audit を
user scope でインストールしていない contributor にも、この repo では hardcoded secret を
検出する git-commit-time ゲートが存在する。上記「決定: 現状維持」（wholesale 統合はしない）
は変わらない — 埋めたのはこの1つの記録済み gap だけで、他の5ゲートとの統合は今も行っていない。

## なぜ移植したか

元のフックは PowerShell 専用（Windows のみ）だった。この書き直しは:

- Linux/macOS/Windows でネイティブに単一の静的バイナリとして動く、
- UTF-8 で一貫している（CP932 の文字化け対策が不要）、
- 汎用チェック（バイナリ内）とプロジェクトポリシー（TOML）を分離している、
- 統合テストスイート（`cargo test`）を同梱している。

## ライセンス

MIT
