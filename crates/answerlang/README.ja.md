# answerlang

回答・結論をユーザーの言語で出させる。お願いではなく機構で。

## 3 つの層

| 層 | 内容 | 場所 | スイッチ |
|---|---|---|---|
| 1 | Claude Code 組み込みの `~/.claude/settings.json` の `"language"` キー（例: `"language": "japanese"`） | Claude Code 本体 — **このクレートの一部ではない**（別途設定する） | settings.json |
| 2 | **inject**: SessionStart / UserPromptSubmit で対象言語を `additionalContext` に注入 | `answerlang session-start` / `answerlang prompt` | `inject` |
| 3 | **enforce**: 最終回答が対象言語の文字体系で書かれていなければ Stop hook がブロック | `answerlang stop` | `enforce` |

すべて切り替え可能。`enabled = false` で全 hook が沈黙する（stdout 空・exit 0）。例外: hook の stdin 自体が読めないときは config を読む前に判定不能として扱う（Stop は block、inject 系は「注入されなかった」旨を表示）。

## 設定 — `~/.answerlang/config.toml`

```toml
enabled   = true      # マスタースイッチ
inject    = true      # 層 2
enforce   = true      # 層 3
language  = "auto"    # または "japanese" / "ja" / "ja-JP" など
min_share = 0.3       # [0.05, 0.95] に clamp
```

- `answerlang set [--language <name|tag|auto>] [--inject on|off] [--enforce on|off] [--enabled on|off]`
  はファイルを作成または merge する（指定しないキーは保持。手書きの `min_share` も残る）。
  書き込みはアトミック（同じディレクトリの一時ファイル + rename）。オプション無し → usage を出して exit 2。
  `on|off` 以外の値 → exit 2 でファイルは触らない。読めない/パースできない既存ファイルは上書きしない（exit 1）。
  言語は小文字化して保存する。
- `answerlang show` は `config_path` / `config_present` / `enabled` / `inject` / `enforce` /
  `language` / `min_share`（clamp 後の実効値）/ `effective_language` / `language_source`
  （`config` | `macos` | `env` | `none`。show には prompt が無いので prompt 段は飛ばす）を出す。
  表に無い明示言語の `effective_language` はその名前そのまま。
- 設定ファイルが**読めない・パースできない**場合: stderr にメッセージを出し、全て既定値（有効）を使い、
  **存在する扱い**（強制が適用され、初回ディレクティブは出ない）。有限でない `min_share` は 0.3 に戻す。

## 言語

正規名（タグは primary subtag で対応付け: `ja-JP` → japanese、`en_US.UTF-8` → english）:
japanese (ja), korean (ko), chinese (zh), russian (ru), ukrainian (uk), greek (el), arabic (ar),
hebrew (he), thai (th), hindi (hi), english (en), french (fr), german (de), spanish (es),
portuguese (pt), italian (it)。

対象の文字体系: japanese = ひらがな + カタカナ（半角含む）+ CJK 漢字、chinese = CJK 漢字、
korean = ハングル（+ CJK 漢字）、russian/ukrainian = キリル、greek、arabic、hebrew、thai、
hindi = デーヴァナーガリー、english/french/german/spanish/portuguese/italian = ラテン。

**制約:** 測れるのは文字体系だけなので、ラテン文字の言語同士は区別できない（フランス語の回答は
`english` として通る）。russian と ukrainian も区別できない。

**表に無い明示言語**（例: `language = "klingon"`）: 層 2 はその名前をそのまま注入する。層 3 は
測る文字体系が無いのでブロックせず、その言語では強制が利用できない旨の `systemMessage` を出す。

## 検出順（`language = "auto"`、最初に決まったものが勝つ）

1. **直近のユーザープロンプト**（UserPromptSubmit）。かなが 1 文字でもあれば japanese、
   無ければハングルがあれば korean、それも無ければ最も多い非ラテン文字体系（漢字 → chinese、
   キリル → russian、greek、arabic、hebrew、thai、デーヴァナーガリー → hindi）。ラテンのみ・文字無し → 決まらない。
   結果（"none" を含む）は `~/.answerlang/state/<session>.prompt` に記録し、Stop はその記録を使う。
   Stop は transcript から再導出しない（Stop hook のフィードバックは transcript 上でユーザーテキストとして現れるため）。
2. **macOS の UI 言語**（`/usr/bin/defaults read -g AppleLanguages` の先頭。macOS のみ）。
   セッションあたり最大 1 回 — 結果（「決まらない」を含む）を `state/<session>.macos` にキャッシュ —
   で、段 1 で決まったときは実行しない。起動失敗・非 0 終了・3 秒のタイムアウト・パース不能 = 決まらない。
   使えるセッション id が無いときはキャッシュせずに実行する。`ANSWERLANG_DEFAULTS_CMD` で
   プログラムを差し替えられる（テスト用）。UserPromptSubmit で起動する subprocess はこれだけ。
3. **`LC_ALL` → `LC_MESSAGES` → `LANG`** — 最初に空でないものが実効ロケール。`C` / `POSIX` /
   `C.UTF-8` や primary subtag が表に無いロケールは決まらない（次の変数へは落ちない）。

何も決まらない場合: 層 2 は言語行を注入しない（stderr に注記）。層 3 はブロックせず、
対象言語が未決定である旨の `systemMessage` を出す。

セッション id はペイロードの `session_id`、無ければ `CLAUDE_CODE_SESSION_ID`。
状態ファイルのキーには `[A-Za-z0-9_-]` だけの id を使う。

## 初回（設定ファイル無し） — 一度だけ聞く

`~/.answerlang/config.toml` が存在しない間:

- SessionStart と UserPromptSubmit は次のディレクティブを注入する:
  `answerlang: no language configured. Ask the user once with AskUserQuestion which language answers should use; offer the auto-detected language as the recommended option, then run `answerlang set --language <choice>`.`
  自動検出で言語が決まればその言語行も併せて注入する。
- Stop は**ブロックしない**。未設定である旨の `systemMessage` を出すだけ。

ファイルができれば（`answerlang set` でも手書きでも）ディレクティブは消え、強制が有効になる。

## 層 2 の出力

```json
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"..."}}
```

言語行は正確に次の文:
`Answers and conclusions: write them in <lang>. Keep code, identifiers, commit messages and verbatim quotes (gate messages, test output) in their original form.`

注入するものが無い（inject off・無効・設定ありで言語未決定）→ stdout 空・exit 0。
stdin 読み取り失敗や内部エラーは、注入が実行されなかった旨の `systemMessage` + `additionalContext` を出す
（沈黙しない）。パースできないペイロードは prompt 検出を飛ばし（stderr に注記）、決まる範囲で注入する。

## 層 3（Stop） — メインセッションのみ

`hooks.json` は `Stop` を登録し、`SubagentStop` は登録しない。順序:

1. `enabled = false` または `enforce = false` → stdout 空。
2. 設定ファイル無し → `systemMessage` のみ。
3. ペイロードがパース不能 → ブロック（「有界な再入」参照）。
4. 表に無い明示言語 / 対象言語未決定 → `systemMessage`、ブロックしない。
5. transcript を読む。検査対象は `"type":"assistant"` の最後のエントリ（`"isSidechain": true` は除く）
   のうち、`message.content` に空でない `text` ブロックを 1 つ以上持つもの。その text ブロックを `\n` で連結する。
   そのエントリより**後ろ**にパースできない行があれば判定不能とする（後の回答かもしれないため）。

### 数える前に除去するもの

- フェンスコードブロック（``` または ~~~、ブロック全体。**閉じていない**フェンスはフェンス扱いしないので、後ろの文章は数える）
- インラインコード（`` `...` ``。対応の無いバッククォートは文字として残る）
- 引用行（先頭 `>`）
- 引用相当の行: trim 後に `$ ` / `test result:` / `error:` / `error[` / `warning:` で始まる行、
  または全体が `"..."` / `「...」` で囲まれた行
- URL（`http://` / `https://` から空白まで）
- ファイルパス: `/` を含む、または `~` で始まる空白区切りトークン
- 識別子トークン（空白と `. : - _` 以外の句読点で区切る）: hex id（`[0-9a-f]` 7〜40 文字で数字を 1 つ以上含む）、
  run id（`run-` + 英数字/ダッシュで数字を含む。`run-time` のような単語は文字として数える）、
  バージョン（`v?\d+(\.\d+)+`）、`name:line`（`:<数字>` で終わるもの。`:<数字>:<数字>` を含む）

Markdown の箇条書き・見出し記号は句読点であり数えない。

### 計測

文字 = `char::is_alphabetic`。**残りが 40 文字未満なら規則により clean。**
share = 対象文字体系の文字数 / 文字数。`share < min_share`（または比較不能）→ 違反。

- clean → stdout 空・exit 0。このセッションのブロックマーカーを消す。
- 違反（初回）→ `{"decision":"block","reason":"..."}`。理由は対象言語を名指しし、回答・結論をその言語で言い直すよう求める。

### 有界な再入

セッションごとのマーカー `~/.answerlang/state/<session>.block` が「このセッションの直前の stop は answerlang がブロックした」を記録する。

- チェーン最初の stop（`stop_hook_active: false`）で違反、または判定不能（`transcript_path` 無し・transcript が読めない・
  assistant テキスト無し・prompt 言語の記録が読めない）→ マーカーを記録してブロック。理由は失敗内容を名指しする
  （transcript の失敗なら `transcript` を含む）。
- 再入（`stop_hook_active: true`）でマーカーあり → マーカーを消して `answerlang` と `failed twice` を含む
  `systemMessage` 付きで**通す**。
- 再入でマーカー無し（別ゲートのブロックで `stop_hook_active` が立った）→ マーカーを記録してブロック。
- 再入でマーカーが読めない・記録できない・使えるセッション id が無い → 明示的な `systemMessage` 付きで通す
  （ブロックが有界にならないため）。
- ペイロードがパース不能: `stop_hook_active` が分からないので常にブロックする（マーカーは記録するので、
  続くパース可能な再入は有界な allow を取れる）。制限側であり、無界になるのはペイロードがずっとパース不能な間だけ。
- panic は `harness_core::gate::run::run_guarded`（fail closed）、stdin 読み取り失敗は
  `read_stop_payload_guarded` を通る。

## 同梱バイナリが無いときのランチャー

`bin/answerlang` は `answerlang-<os>-<arch>` を exec する。無い場合:

- `stop` → 検査が走らなかった＝判定不能なので、最初の stop は**ブロック**（`{"decision":"block"}`）、
  再入（ペイロードに `"stop_hook_active": true`）では検査が実行されなかった旨の `systemMessage` 付きで通す。
  ランチャーにはゲート別マーカーが無いので、ここでは `stop_hook_active` だけで有界にしている。
- `session-start` / `prompt` → 注入が実行されなかった旨の `systemMessage` + `additionalContext` を出して exit 0。
- それ以外 → exit 1。

## 有効化

1. `scripts/rollout-plugins.sh --plugin answerlang`
2. `~/.claude/settings.json` の `enabledPlugins` に `"answerlang@yukineko": true` を追加する（ユーザーが行う）。
3. 任意で層 1: 同じ設定ファイルに `"language": "japanese"`。
