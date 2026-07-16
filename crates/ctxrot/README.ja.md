# ctxrot

Claude Code 向けの **context-rot（コンテキスト腐敗）ガード**。Rust 製の単一バイナリ。

## 目的

長いセッションではモデルの注意力が劣化する。序盤の指示が埋もれ、決定事項や未消化の todo が沈み、生のダンプが全体を薄める——これが *context rot* である。`ctxrot` は Claude Code の各種フックに配線され、context に入るものを **検知・退避・復元・蒸留・制御**する責務を負う。

中核は「フック＝速くて決定論的・LLM を呼ばない安全網」と「`/distill` スキル＝必要時に走る LLM 品質の要約」という役割分担である。フックは PreCompact の厳しいタイムアウト内でも安全に動き、判断を要する蒸留はセッション内のスキルに任せる。

各フックは `ctxrot` バイナリのサブコマンドであり、フックの JSON ペイロードを **stdin** から読む。鉄則は「フックは決してターンを壊さない」——どんなエラーでも exit 0 で黙って終わる。

| サブコマンド | フック | 役割 |
|---|---|---|
| `ctxrot guard` | `UserPromptSubmit` | 大きな参照（巨大ローカルファイル / URL /「全文」キーワード）と **context バジェットの帯（窓の 50/75/90%）**を検知し、*最小限・条件付き*の助言だけを注入する。バジェット助言は帯をまたいだ時に一度だけ（助言自体が rot 源にならないよう）。**band ≥ 2（約 75%+）**では rescue ノートを先回りで書くので、手動 `/compact`（や `/clear`）が PreCompact を待たずに安全になる。**re-anchor は2トラック**: 決定事項/todo の re-anchor（band ≥ 2・8 プロンプトごと）に加え、別トラックの **PDO session-anchor** —— このセッションが overwatch の live lease を保持していれば（`overwatch lease --session $CLAUDE_CODE_SESSION_ID --json` で読む）、「あなたは今 *&lt;title&gt;* を担当中。done_criteria: …」の 1〜2 行を再浮上させる（scope の生 glob は注入しない）。band ≥ 1 から `anchor_reinject_every`（既定 12・決定事項トラックより疎で専用クールダウン）ごとに。lease 無し・overwatch 未導入・lease JSON parse 不能なら無音で no-op（fail-soft）。 |
| `ctxrot rescue` | `PreCompact` | `/compact` の直前に直近 transcript をたどり、永続的な **rescue ノート**（決定事項・未消化 todo・触れたファイル・リンク・直近の生ターン）を書き出し、不可逆な圧縮で何も失わせない。決定論的・LLM 不使用。ファイル名にセッションタグ（`rescue-<session>-<ts>.md`）を持つ。既定では切り離した `claude -p` を fire-and-forget して非同期で LLM 品質に格上げする（`distill_on_compact`、圧縮はブロックしない）。 |
| `ctxrot restore` | `SessionStart` | セッション開始時に **簡潔な carryover**（決定事項＋未消化 todo＋リンク）を注入する。*このセッション自身*のノート（セッションタグ照合）を優先し、並列セッションが同じプロジェクトディレクトリを共有する場合は兄弟セッションの carryover を掴まないよう制限する。ノート全文は決して注入しない。 |
| `ctxrot preguard` | `PreToolUse` | **ロード前の予防ゲート。**（1）ルールベース——`load_deny` グロブに一致する `Read` はサイズに関係なく拒否、`load_allow` はサイズゲートを迂回。（2）サイズベース——`limit` 無しの *無制限* `Read` が `gate_file_bytes`（既定 **1MB**）以上だと実行可能な理由付きで拒否。優先順位は **deny → limit → allow → size**。通常のソース読みは触らないよう狭く設計。 |
| `ctxrot toolguard` | `PostToolUse` | `Read`/`Bash`/`Grep`/… が巨大なペイロードを返した時、*次の*重い読みを sub-agent 経由にして結論だけ残すよう促す（preguard ゲートを通り抜ける 50KB〜1MB の中間帯を扱う）。1 セッションあたりの回数は `toolguard_nudge_cap`（既定 3）で上限を設ける（助言自体が rot 源にならないよう）。 |
| `ctxrot stop` | `Stop` | **オプトインの auto-compact 促し。** バジェットメーターの使用率が `auto_compact_at_percentage`（既定 **0.90**）を超えると `{"decision":"block"}` を返して Claude に `/compact` を促す。しきい値は ctxrot **自身のバジェットメーター**（`est_tokens / context_window`。`guard`/`usage` と同じ推定で 100% 超も出せる）で測り、生の model-window `used_percentage` では **ない**——だから真の ~1M 窓に対しても正しく発火する。既定は無効（`auto_compact_enabled = false`）。block は帯をまたぐごとに一度だけで、ターンを恒久的に塞ぐことはない。 |
| `ctxrot statusline` | `statusLine` | 常時表示の context 使用率メーター（`ctxrot 52% ▮▮▯▯ band1 ~104k/200k`）を帯ごとに色付け（緑→黄→赤）で出す。Claude の `context_window.used_percentage` を読み（無ければ transcript から推定）。 |

加えて 2 つのスキルがある:

- **`/distill`** — オンデマンドの高品質 LLM 蒸留。フックが安価な決定論的安全網なのに対し、こちらが「賢い方」。
- **`/ctx`** — context に何を載せる/載せないかを明示制御する。`/ctx load <path>` で意図的にロード（巨大なら sub-agent 経由で要約だけ）、`/ctx pin <path>` で次セッション以降もポインタを再浮上、`/ctx unload <path>`（別名 `drop`）で除外、`/ctx list` でプロジェクトの loadset を確認。状態は `ctxrot ctx`（プロジェクト単位の `pinned`/`dropped` セット）に持ち、`restore` がセッション開始時に浮上させる。

蒸留ノートは **契約**で守られる。`ctxrot note write --require-sections` は `restore` が読む見出し（`決定事項/Decisions`、`残課題/Open todos`）を欠くノートを拒否（exit 1・未書き込み）するので、スキーマのずれは空の carryover を黙って生むのではなく書き込み時に大声で落ちる。

## どうして必要か

Claude Code の長いセッションでは、context window が埋まるほど序盤の指示・決定・未消化 todo が「lost in the middle」で埋もれ、注意力が劣化する。さらに巨大なログや全文ダンプを一度 `Read` で読み込むと、降ろせないトークンとして窓を占有し続け、`/compact` での不可逆な圧縮で重要な決定が失われることもある。

ctxrot が無いと、こうした劣化を人間が気付いて手で対処するしかない——いつ `/compact` すべきか、どのファイルを丸ごと読むと危険か、圧縮の前に何を退避すべきか、新セッションで何を引き継ぐか。ctxrot はこれらを決定論的なフックで先回りする:

- 巨大ファイルの無制限 `Read` を **ロード前に止める**（preguard）ので、降ろせないトークンが窓を汚す前に防げる。
- 使用率が帯をまたぐたびに **一度だけ**助言を出し、助言自体が rot を増やさない。
- 圧縮や clear の前に rescue ノートを **自動で書く**ので、何も失わない。
- 新セッション開始時に決定事項と todo を **carryover として復元**する。並列セッションでも兄弟のノートを掴まない。

重要な前提として、フックは「すでに窓に載っているトークンを後から evict する」ことはできない。だから制御は **入口で止める/絞る**（preguard・`/ctx load`）と **再構成で実効化する**（`drop` は `/compact`・`/distill`・新セッションの carryover で効く）の 2 点に集約される。

## どう使うか

推奨は **Claude Code プラグインとしての導入**。このリポジトリは Rust crate であると同時にプラグイン／マーケットプレースでもあり、6 つのフック・`/distill` と `/ctx` スキル・`ctxrot-distiller` subagent・ビルド済みバイナリ（`bin/ctxrot`）を同梱する。すべて Claude **サブスクリプション**で完結し、API キーも別途の `cargo install` も不要。

```text
# Claude Code 内で:
/plugin marketplace add <このリポジトリの git URL>
/plugin install ctxrot@yukineko
```

フックは `${CLAUDE_PLUGIN_ROOT}/bin/ctxrot <sub>` を呼ぶ。`bin/ctxrot` はホストに合わせて per-platform バイナリ（`bin/ctxrot-<os>-<arch>`）を選ぶ POSIX ランチャ。設定とストアのディレクトリが要るなら `ctxrot init` を一度走らせる（任意・既定でも動く）。

> **ユーザごとの一手**: 各ユーザが一度 `/plugin marketplace add <git URL>` する必要がある（チェックインされたリポジトリからマーケットプレースは自動登録されない）。

> **ステータスラインはプラグインで自動登録されない。** フック・`/distill`・subagent は自動ロードされるが、プラグインマニフェストは汎用 `statusLine` を宣言できない。使用率メーターを使うなら `~/.claude/settings.json` に一度追記する:
>
> ```json
> "statusLine": {
>   "type": "command",
>   "command": "<CLAUDE_PLUGIN_ROOT>/bin/ctxrot statusline",
>   "padding": 0
> }
> ```
>
> `<CLAUDE_PLUGIN_ROOT>` は導入済みプラグインの絶対パスに置き換える（settings.json では `${CLAUDE_PLUGIN_ROOT}` 展開が保証されない）。

### スキルの使い方

- **`/distill [フォーカス]`** — 今の会話を蒸留して ctxrot ストアへ退避し、main context を「要約＋リンク」に置換する。まず `ctxrot usage` で帯を見て挙動を変える: band 0 は急ぎ不要（確認）、band 1 は通常蒸留、band 2 以上は蒸留した上で `/compact` を必須で促す。重い読みは `ctxrot-distiller` subagent に Task で委譲し、main context を汚さない。なお、ノート保存だけではトークンは解放されない——実際に軽くするのは `/compact`・`/clear`・auto-compact のみ（ユーザ操作）。
- **`/ctx <action> [path]`** — context への載せ/降ろしを手で制御。`load`（意図的ロード）/ `pin`・`unpin`（セッション間ポインタ）/ `drop`・`unload`・`undrop`（除外）/ `use-note`・`clear-note`（restore が使うノートの固定/解除）/ `list`（既定・現状確認）。

### 手動インストール（プラグインを使わない場合）

```sh
cargo build --release
ctxrot init                 # 設定 + ストアのディレクトリ
ctxrot install --dry-run    # ~/.claude/settings.json への変更をプレビュー
ctxrot install              # 適用（先に settings.json をバックアップ）
cp -r skills/distill ~/.claude/skills/
cp -r skills/ctx ~/.claude/skills/
```

`ctxrot install` は冪等で、過去の ctxrot エントリと旧 `context-rot-guard.py` フックを **置換**しつつ、他のフックや設定は保つ。`ctxrot uninstall` で除去する。

### 設定とストア

設定は `~/.ctxrot/config.toml`（`ctxrot init` が作る）。主要項目は `store_dir`（Obsidian vault も指定可）/ `context_window`（**実窓ではなく「これ以下に抑えたい目標値」**——既定 200000 のまま使うこと。実 1M 窓に直すと帯が ~950K まで発火せず無効化する）/ `large_file_bytes`・`huge_tool_output_bytes`・`gate_file_bytes`（各種しきい値）/ `bands` / `load_deny`・`load_allow`（入口ゲートの恒久ルール）/ `restore_enabled`・`inject_*`（carryover 制御）/ `anchor_reinject_every`（既定 12。PDO session-anchor 再注入の頻度。live な overwatch lease を持つセッションで「今の担当（title + done_criteria）」を band ≥ 1 から N プロンプトごとに再浮上。決定事項 re-anchor とは別トラック・別クールダウン。lease 無し／overwatch 未導入なら無音）/ `distill_on_compact`・`auto_distill_on_band`（圧縮時／トップ帯到達時の非同期 LLM 蒸留）/ `auto_compact_enabled`（既定 false）・`auto_compact_at_percentage`（既定 0.90。Stop フックの auto-compact 促し。ctxrot 自身のバジェットメーターに対して測る）など。

ノートは Obsidian 互換 markdown でプロジェクト単位に格納される（`<store_dir>/<project-basename>-<hash>/`）。`ctxrot note list` / `ctxrot note latest` / `ctxrot note dir` で確認する。

### メモリがセッションをまたぐ流れ

```
… 長いセッション …
   │  preguard: 1.8MB ログの Read ──► ロード前に DENY（sub-agent / limit を促す）
   │  guard:    「推定 ~76% — /distill で退避を」（帯ごとに一度）
   │            └─ band ≥ 2: rescue-<session>-<ts>.md を先回りで書く ← 今すぐ /clear しても安全
   │  toolguard:「Read が ~59KB 投入 — 次回は sub-agent 経由」
   ▼
/compact ──► rescue (PreCompact): rescue-<session>-<ts>.md を書く ← 何も失わない
   ▼
新セッション ──► restore (SessionStart): 決定事項 + todo + リンクを注入
                （並列安全: タグでこのセッション自身のノートに戻る）
```

導入後はフック・subagent・スキルが通常のセッションモデル内で動くため、`ANTHROPIC_API_KEY` も別途の `cargo install` も不要。サブスクリプションで完結する。

## プラットフォーム対応 / バイナリのビルド

プラグインはランタイムに `bin/ctxrot` ランチャが選ぶプリビルド per-platform バイナリを同梱する:

| Host | ファイル | 状態 |
|---|---|---|
| Linux x86_64 | `bin/ctxrot-linux-x86_64` | 同梱 |
| macOS Apple Silicon | `bin/ctxrot-darwin-arm64` | Mac 上でビルド（下記） |
| macOS Intel | `bin/ctxrot-darwin-x86_64` | Mac 上でビルド（下記） |

一致するバイナリが無ければランチャは無言で exit 0 する（フックはターンを壊さない）。ビルドヒントを stderr に一行出す。

```sh
# host platform（Linux ならここ、Apple Silicon なら Mac 上で）
scripts/build-plugin-bin.sh

# Mac 上で Intel 版もクロスビルドする場合:
rustup target add x86_64-apple-darwin
scripts/build-plugin-bin.sh x86_64-apple-darwin

git add bin/ && git update-index --chmod=+x bin/ctxrot bin/ctxrot-*
git commit -m "Add <platform> binary"
```

このリポジトリは `core.filemode=false` マウント上にあるため、実行ビットは `git update-index --chmod=+x` で git index に強制する（そうしないとランチャ/バイナリが非実行可能でチェックアウトされフックが失敗する）。

## プラグイン構成

```
.claude-plugin/plugin.json        # プラグインマニフェスト
.claude-plugin/marketplace.json   # 単一プラグインのマーケットプレース (name: yukineko)
hooks/hooks.json                  # 6 つの hook → ${CLAUDE_PLUGIN_ROOT}/bin/ctxrot
agents/ctxrot-distiller.md        # distiller subagent (重い読み込みを main context から追い出す)
skills/distill/SKILL.md           # /distill skill (subagent へ委譲)
skills/ctx/SKILL.md               # /ctx skill (`ctxrot ctx` 経由の pin/drop/load)
bin/ctxrot                        # POSIX ランチャ → ctxrot-<os>-<arch>
bin/ctxrot-<os>-<arch>            # プリビルドバイナリ
src/ … Cargo.toml                 # Rust クレート本体（無変更で再利用）
```

## メトリクス

各フックは `<state_dir>/metrics.jsonl` に1行 JSONL を追記する — トークンの**軌跡**（プロンプトごとの `budget`）、すべての**帯またぎ**、rescue **ノートサイズ**、**gate** の拒否（context から締め出したバイト数）、通過した tool **ダンプ**。ローカルのみ、`metrics = false` または `GUARD_METRICS=0` で無効化できる。

```sh
ctxrot metrics             # セッション単位の集計 (プロンプト数/帯またぎ/ピークトークン/rescue/gate/dump)
ctxrot metrics path        # metrics.jsonl のパス（jq に渡して即席分析）
ctxrot metrics compare A B # A/B 2つの session-id prefix で両群 + Δ(A−B) を表示
ctxrot metrics peak ID     # session-id prefix のピーク% + 最大帯 (/record がノートに記す用)
```

`ctxrot usage` は `$CLAUDE_CODE_SESSION_ID` から実 transcript を解決し、**現在の**セッションの使用率（`ctxrot 52% … band1` + アクション `hint:`）を出力する。`/distill` skill はまずこれを呼んで挙動を決める: band 0 なら省略、band 1 なら通常蒸留、band 2 以上なら蒸留に加えて `/compact` を必須で促す。

これは guard が実際に N を抑えているかを測る土台である。

### A/B 比較: guard は occupancy を下げるか

代表的な重いタスクを guard-ON と `GUARD_DISABLE=1`（guard-OFF）の2回走らせ、それぞれを session-id prefix でグループ化して比較する:

```sh
# グループA (guard ON): 例えば on-… で始まる session id で実行
# グループB (guard OFF): GUARD_DISABLE=1, off-… で始まる session id で実行
ctxrot metrics compare on- off-
```

`compare` は両グループ・符号付き Δ(A−B)・**dwell** 行（各帯で費やしたプロンプト数 `b0 b1 b2 b3`）を出力する。occupancy の質はピークだけでなく形で見る — 効いている guard は高い帯で過ごすプロンプトが少ない。A の `peak_tok`/`band` の Δ が負で高帯 dwell が軽ければ、guard が context の高水位を下げたことを意味する。

### Recall eval: re-anchor はトークンに見合うか

re-anchor は窓の末尾に既知の決定事項を再注入する — *追加トークン*であり、まさに ctxrot が戦っている対象。recall の利得がコストを上回る場合のみ正当化されるので直接測定する。フック自体は LLM を呼ばず、eval はプロセス外で走る:

```sh
cargo build --release
eval/run-recall.sh           # `claude` CLI + `jq` が必要。case ごとに claude -p を駆動
# または手動で:
ctxrot eval gen --out cases --cases 9      # filler に埋めた推測不能な決定事項を仕込む
#   …各 cases/*.on.txt (決定事項が再浮上) と *.off.txt (再浮上なし) をモデルに渡す…
ctxrot eval score --manifest cases/manifest.json --results cases/results.jsonl
```

`score` は OFF/ON 両バリアントの精度と re-anchor の追加トークンコスト（Σ anchor bytes /4）を出力し、正味の利得を1つの表にまとめる:

```
variant     cases  correct  accuracy
off             9        4       44%
on              9        8       89%
re-anchor 追加注入: ~1.3k bytes (~330 tok) over 9 ON case(s)
Δ accuracy (on − off): +45 pts
```

小さいトークンコストで大きな正の Δ なら re-anchor は正当化される。Δ がゼロ近辺（または負）なら `reanchor_min_band` を上げるか `reanchor_enabled = false` にするシグナル。既定（`reanchor_min_band = 2 ≈ 75%`、`reanchor_every_prompts = 8`）は意図的に保守的 — lost-in-the-middle が効いてくる窓の深いところでしか発火せず、最大でも8プロンプトに1回なのでトークンコストは抑えられる。自分の eval Δ から調整すること。

## 開発

```sh
cargo test          # unit + fixture テスト
cargo build
```

手動 hook チェック:

```sh
echo '{"prompt":"read /big.log","cwd":"'"$PWD"'","transcript_path":"tests/fixtures/transcript.jsonl","session_id":"s1"}' | ctxrot guard
```

## context-governor との共存

`ctxrot` と [`context-governor`](../context-governor/README.ja.md) はどちらも `PostToolUse` / `UserPromptSubmit` / `SessionStart` / `PreCompact` にフックする。両者は異なるレバーを引く — ctxrot は*助言・退避・制御*（助言テキスト・rescue ノート・load gate）、context-governor は*窓を変異させる*（tool output の刈り込み・参照本文の注入・compact 時のスナップショット）— ので書き込み先は重ならず、どちらもブロックしない（ctxrot の `preguard` deny と context-governor の実質発火しない `PreCompact` block を除く）。両プラグインのハンドラコードで検証済みのイベントごとの決定マトリクスは [`context-governor/docs/coexistence-with-ctxrot.md`](../context-governor/docs/coexistence-with-ctxrot.md) にある。

## ライセンス

MIT
