# claude-harnesses — リポジトリ指針

`yukineko` の Claude Code ハーネス一家を単一ソースで管理する Cargo ワークスペース・モノレポ。
各 `crates/<name>/` は Rust クレートかつ Claude Code プラグイン（`.claude-plugin/plugin.json` +
`hooks/` + 同梱 `bin/` + `skills/`）。共通基盤は `crates/harness-core`（ビルド時ライブラリ。各
プラグインバイナリに静的に焼き込まれる）。

## context 読み込み戦略（重要 — 盲目的に crate を探索しない）

このリポジトリは 39 クレートある。全体を毎回読むと context を浪費するので、**必要な層だけを
オンデマンドで**読む:

1. **まず [`docs/GLOSSARY.md`](docs/GLOSSARY.md) を読む** — 全クレートの一言早見表＋頻出ドメイン用語
   （harness / hook / SKILL / worktree / gate / source↔executor / PDO / HOTL / autonomy gate /
   fail-soft など）。「どの crate が何をするか」はここで解決する。1 枚（約 80 行）で全体像がつかめる。
2. **特定 crate を触るときだけ** その `crates/<name>/README.ja.md`（詳細）と `src/` を読む。
   GLOSSARY で当たりをつけてから深掘りする。
3. 横断テーマは `docs/` の該当ファイルへ（下記「さらに読む」）。
4. **重い探索・横断検索は sub-agent 経由**にして main context を汚さない。

## ビルド / テスト / ゲート

- ツールチェーンは **rustup 経由**。cargo コマンドの前に `. "$HOME/.cargo/env"` を通す。
- テストはクレート単位: `cargo test -p <crate>`。
- CI ゲートは **fmt + clippy を強制**する。コミット前に `cargo fmt` と
  `cargo clippy -p <crate> --all-targets` を green にする。
- **prompt-injection 防御ゲート（2本）** — prompt に load される資産と同梱バイナリの改竄を機械検出する:
  - `python3 scripts/check-prompt-injection.py`（injectguard）— skills/agents/hooks/CLAUDE.md/.compass/docs
    に隠蔽・検証バイパス・egress 文言が植わっていないか走査（防御 framing は除外）。ローカルは
    `git config core.hooksPath .githooks` で pre-commit を有効化（速い advisory 層。CI の `injectguard` job が
    非バイパスの本ゲート）。
  - `python3 scripts/check-bin-reproducibility.py`（CI `bin-reproducibility` job）— 全 bin をソースから再ビルドし、
    committed-only な悪性パターン文字列（source が生成しない焼き込み）を検出。生の committed-only 件数・size 差は
    ビルド非決定性なので**判定に使わない**（悪性デルタのみ）。host triple のみ対象。

## プラグインを改修したときの反映（忘れやすい）

`crates/<name>/` が唯一の正典。`/plugin install` はここを
`~/.claude/plugins/cache/<owner>/<name>/<version>/` に**プレーンコピー**する（git 外）。稼働中の
ハーネスはキャッシュ側を読むので、**repo をビルドしただけでは何も反映されない**:

### 「plugin 更新（`/plugin update` 相当）」を求められたら — `scripts/rollout-plugins.sh` を使う（正典・一発）

**version を上げた／テキスト資産を変えたプラグインを稼働ハーネスへ反映するときは、手動 `cp` ではなく
必ず `scripts/rollout-plugins.sh` を実行する。** このスクリプトは `/plugin update`（UI 専用操作）が
directory marketplace `yukineko` に対してやる2操作 —(1) `crates/<name>/` を新しい
`cache/yukineko/<name>/<version>/` **dir へコピー**、(2) `installed_plugins.json` の
`<name>@yukineko` を新 dir へ **repoint** — を再現し、続けて `rebuild-plugins.sh`（バイナリ swap）と
各 plugin の `sync-plugin-assets.sh`（skills/agents/hooks 同期）まで一括で走らせる。順序も保証する
（version dir + registry pointer を rebuild より先に作る）。

```sh
python3 scripts/check-plugin-versions.py && python3 scripts/check-version-bumped.py   # 先に version 整合を確認
scripts/rollout-plugins.sh --plugin <name> --dry-run   # 動作を確認（何も書かない）
scripts/rollout-plugins.sh --plugin <name>             # 実反映（全 plugin なら無引数）
```

- 冪等（version 不変なら no-op）。`--force` で無変更でも再コピー、`--no-rebuild`/`--no-sync` で段階分け。
- **手動 `cp` は禁止**: cache の binary/asset を手で上書きするだけだと **version dir が旧名のまま残り**
  registry も更新されず、`sync-plugin-assets.sh` が version から dir を誤解決して**古い版が配布される**。
- `/plugin update`（ユーザー UI 操作）は本スクリプトで完全代替できるので、手動 UI 操作は不要。

低レベルの構成要素（通常は上の rollout 経由で十分。個別に叩くのは段階分けのときだけ）:

- バイナリを反映: `scripts/rebuild-plugins.sh`（`--no-clean` で増分）— target のバイナリを live
  キャッシュへ swap する（**既存 dir に swap するだけ。version dir は作らない**）。
- テキスト資産（skills/agents/hooks）を反映: `crates/<name>/scripts/sync-plugin-assets.sh`
  （`--check` で drift 検出）。
- **キャッシュを手編集しない**（git 外で黙って乖離する）。必ず repo を編集 → 上記で同期。

## バージョン整合（**絶対厳守** — 「今動けばいい」は後で壊れる）

各プラグインの version は **3つの正典で常に lockstep** でなければならない。片方だけ上げた状態は
**バグ**として扱う（「今は動く」で放置しない）。ズレると sync-plugin-assets.sh が version から
キャッシュ dir を誤解決し、**古い版がユーザーに配布される**。

### 変更したら必ず version を上げる（**禁忌ルール**）

- **あるプラグインの中身（コード・hooks・skills・agents 等いずれか）を1行でも触ったら、その
  プラグインの version を最低でも micro（patch, `x.y.Z` の Z）上げる。** 触ったのに据え置きは
  **禁忌**。
- version を上げるときは **必ず3ファイル同時**: `crates/<name>/Cargo.toml` の `[package].version`
  ／ `crates/<name>/.claude-plugin/plugin.json` の `version` ／ `.claude-plugin/marketplace.json`
  の当該エントリ `version`（skill-only プラグインは Cargo.toml が無いので後者2つ）。
- 言い換え: **「何か変更したのに plugin version と marketplace version が変わっていない」状態を
  commit してはならない。** 変更の大小を問わず、最低 micro を必ず上げる（意味的に大きければ
  minor/major で判断）。
- これは「今動けばいい」を禁じ、後の drift・古い版配布を根絶するための**徹底順守ルール**。

**強制ゲート（2つを両方、commit 前・push 前・CI で回す）:**

```sh
python3 scripts/check-plugin-versions.py            # lockstep: 3ファイルの version が一致するか（exit 1 で drift）
python3 scripts/check-version-bumped.py             # bump-on-change: base(既定 HEAD)から変更のある plugin が bump 済みか
python3 scripts/check-version-bumped.py --base origin/main   # CI/push前は pushed ref と比較
```

`check-version-bumped.py` は `crates/<name>/` に差分がある plugin の plugin.json version が
base より**厳密に上がっている**ことを要求し、上がっていなければ exit 1 で該当 plugin と変更ファイルを
表示する（新規 plugin は base に無いので OK）。**「変更したのに未 bump」を機械的に止めるゲート**。

- 正典3ファイル（バイナリ付きプラグイン）: `crates/<name>/Cargo.toml` の `[package].version` /
  `crates/<name>/.claude-plugin/plugin.json` の `version` / `.claude-plugin/marketplace.json` の
  当該エントリ `version`。**skill-only プラグイン**（Cargo.toml 無し。例: `scout`）は
  plugin.json + marketplace.json の2つ。
- 正典の向き: **`Cargo.toml == plugin.json` が真**。`marketplace.json` は取り残されやすい（laggard）。
  version を上げるときは **必ず3ファイル同時**に上げる。marketplace.json だけ手で上げ忘れない。
- **すべてのタイミングで徹底チェック**（version を触る commit、rebuild 前、push 前）。
  自動チェッカで機械的に確認する:

  ```sh
  python3 scripts/check-plugin-versions.py   # exit 0 = 全整合 / exit 1 = drift（該当プラグインを表示）
  ```

- **rebuild は version を上げない**（別工程）。`rebuild-plugins.sh` は正典の version をそのまま
  コンパイルして live cache へ swap するだけ。version bump は 3ファイル編集という別の意図的操作。
- **cache の version dir が古い**（例: source は 0.7.0 だが `cache/.../<plugin>/0.6.0/` のまま）のは
  `/plugin update`（ユーザー UI 操作）未実行が原因。rebuild-plugins.sh は既存 dir にバイナリを
  swap するのでコードは動くが、正式ロールアウトは `/plugin update` → rebuild → sync-plugin-assets.sh
  の順。

## さらに読む（docs/）

- `docs/GLOSSARY.md` — クレート・用語早見表（**最初に読む**）
- `docs/OVERVIEW.md` / `docs/USAGE.md` — 全体像と使い方
- `docs/context-optimization.md` / `docs/context-optimization-flow.md` — context 節約の設計
- `docs/plugin-dependency-graph.md` / `docs/plugin-activation-scopes.md` — プラグイン間依存・起動スコープ
- `docs/AGENTIC-CODING-GUIDE.md` — エージェント実装ガイド
