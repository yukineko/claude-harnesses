# taintguard

**provenance（出所）に基づく最小権限ゲート** — このターンが信頼できない出所の
content（WebFetch/WebSearch の結果、プロジェクト外の `Read`）を取り込んだら、
書き込み系ツールをそのターンの間だけ read-only 相当に格下げする Claude Code
hook トリオ。

## 目的

プロンプトインジェクション対策の一つの層として、「このターンで外部由来の
content を読んだ直後は、書き込み系操作を無条件には許可しない」という
least-privilege を機械的に強制する。エージェントが騙されて悪意ある指示を
外部 content から取り込んでいても、それが即座に `Bash`/`Write`/`Edit` 等の
実行に直結しないようにする。

## 3 つの hook

| hook | event | matcher | 役割 |
|---|---|---|---|
| `taintguard mark`  | PostToolUse | `WebFetch\|WebSearch\|Read` | 出所を判定し taint marker を記録 |
| `taintguard gate`  | PreToolUse  | `Bash\|Write\|Edit\|MultiEdit\|NotebookEdit` | tainted なら ask/deny |
| `taintguard clear` | Stop        | (全体)                        | クリーンなターン終了で marker を解除 |

### mark（PostToolUse）

- `WebFetch` / `WebSearch` の結果は常に信頼できない出所として扱う（source: `web`）。
- `Read` はターゲットパスをプロジェクトルート（`cwd`）基準で分類する
  （`src/classify.rs`）:
  - ルート配下に解決される → 信頼できる（no-op）。
  - ルート外（`/tmp`、ホームディレクトリ、他プロジェクト、`..` での脱出）→
    信頼できない（source: `external-read`）。
  - パスが判定不能（file_path が空/欠落、シンボリックリンクの解決不能等）→
    **fail-closed**（信頼できないものとして mark）。
- 解析自体が panic した場合も fail-closed（source: `internal-error` で
  強制的に mark）。「解析に失敗したので何もしない」は「解析して問題なしと判断した」
  と区別がつかず、それは fail-open になるため。

### gate（PreToolUse）

- このセッション（`session_id` + `cwd`）が tainted なら、blastguard と同じ
  `hookSpecificOutput.permissionDecision` 形状で `ask`（対話セッション）または
  `deny`（非対話）を返す。
- 対話判定は blastguard の `interactive::ask_available` と同一ロジック
  （`CLAUDECODE=1` かつ `CLAUDE_CODE_ENTRYPOINT=cli` のときだけ `ask`、それ以外は
  `deny` — 人間が実際に応答できない `ask` は、一時停止ではなくエージェントが
  解除できない block になるため）。
- taint marker が読めない/壊れている場合も **fail-closed**（`ask`/`deny` 側）。
  「判定できなかった」を「問題なし（silent allow）」に潰さない。
- tainted でなければ何も出力しない（silent allow）。
- 以上は既定の **enforce** posture の挙動。`TAINTGUARD_OBSERVE_ONLY=1` の
  **observe-only** posture では `permissionDecision` を出さず警告のみになる
  （＝強制されない）。詳細と注意点は後述の「observe-only モード」を参照。

### clear（Stop）

- ターンがクリーンに終わったら marker を削除し、次のターンを信頼できる状態に
  戻す。
- 削除に失敗した場合は stderr に出すだけで exit 0（**tainted のまま残る方が
  安全側**なので、消せなかったことを許可の失敗として扱わない）。

## observe-only モード（計測専用・既定では無効）

`TAINTGUARD_OBSERVE_ONLY=1` を設定すると、`gate` は **強制を抑止する**。
**これはゲートを無効化する設定である** — 有効化する前に「実際どれくらい摩擦を
生むか」を予測ではなく観測で答えるための計測モードであり、防御の代わりには
ならない。

- **抑止するもの**: `ask`/`deny` の `permissionDecision` を**一切出さない**。
  したがって tainted なターンでも write-class ツールは通常どおり実行される。
  **保護は効いていない。**
- **抑止しないもの**: 検査そのもの、および報告。tainted を検出したら
  `hookSpecificOutput.additionalContext` で「このターンは tainted」「どの source が
  汚染したか」「observe-only なので強制を抑止した」ことを明示し、同時に
  append-only の JSONL 台帳に1行追記する。**沈黙はしない** — 黙って通るゲートは
  「何も見つけなかったゲート」と区別できず、それ自体が fail-open になるため。
- **`permissionDecision: "allow"` は出さない**（意図的）。明示的な `allow` は
  他のゲートやユーザー自身の権限規則を**上書きする**正の判定なので、強制を
  やめるためのモードが結果としてより広い範囲を強制してしまう。
  `additionalContext` のみならその副作用が無い。

### opt-in は fail-closed

`TAINTGUARD_OBSERVE_ONLY` が **厳密に `1`** のときだけ observe-only になる。
未設定・空文字・`0`・`false`・`true`・`yes`・` 1`（前後の空白つき）・`01`・
その他あらゆる値は**すべて従来どおり強制する**。トリム・大小無視・truthy 文字列
解釈は一切しない（`observe::resolve`）。`Posture` に `Default` 実装は無い
（許容側が `Default::default()` や `.into()` から生まれないようにするため）。

### panic は observe-only を尊重しない

`gate` の panic barrier が発火した場合は、observe-only が設定されていても
**`ask`/`deny` に倒れる**。panic は「解析が完走しなかった」＝判定不能であり、
posture の読み取り自体も barrier の内側にあるため、尊重すべき posture を
知っているとは言えない。observe-only は**動いているゲートを計測する**ための
affordance であって、内部エラーを飲み込む許可ではない。

### 計測値の読み方

台帳は `$TAINTGUARD_STATE_DIR/<project_key>/observe-only.jsonl`
（**session 単位ではなく project 単位** — Stop hook の `clear` は session marker を
消すので、session に閉じた台帳では発火率が測れない）。1行 = 抑止した強制1件で、
`{ts, tool, sources, check, session}` を持つ。`check` は `"tainted"` と
`"undetermined"` を**区別して**記録する（marker が読めなかった件を摩擦の統計に
混ぜると store の健全性問題が隠れるため）。`observe::tally` は
パースできた件数と**できなかった件数を別々に**返す（壊れた行を黙って捨てて
件数を下げると「思ったより摩擦が少ない」と誤読される）。

台帳への追記に失敗した場合は `Err` を返し stderr に出す（「記録したつもり」を
作らない）。この失敗は**権限の fail-open にはならない** — 台帳は「強制するか」の
判断に一切読まれず、判断は `state::check`（独自の書き込み可能性プローブつき）
だけが行うため。失敗すると計測が過少計上されるだけで、その事実は stderr に出る。

## 状態の保存先

`$TAINTGUARD_STATE_DIR/<project_key>/<session>/taint.json`
（`TAINTGUARD_STATE_DIR` 未設定時は `~/.taintguard/state`）。
`project_key`/セッションディレクトリの命名規則は
`harness_core::store::context_state_dir` と同じ慣習（cwd を canonicalize
してから project key を作る）。

observe-only の台帳のみ **project 単位**（`<project_key>/observe-only.jsonl`）で、
session ディレクトリの下ではない（理由は上記「計測値の読み方」）。

## fail-closed の設計

- `state::check` は 3 値（`Clean` / `Tainted(sources)` / `Undetermined(why)`）
  で、marker が読めない・パースできないケースを `Clean` に潰さない。
  `is_tainted` はこの 3 値のうち `Clean` 以外を全部 `true` にする bool 便利関数。
- marker が**存在しない**場合も無条件に `Clean` を返さない。「このセッションは
  一度も untrusted content を消費していない（安全）」と「直前の `mark` が
  state dir 書き込み不能（読み取り専用マウント／`chmod 555`／disk full）で
  静かに失われた（`Clean` にしてはならない）」は marker の不在という同じ
  観測結果を作るため、`check` は marker 不在のケースに限り、その session state
  dir が今まさに書き込み可能かをプローブ（作成→書き込み→削除の使い捨てファイル）
  する。プローブ成功（健全な store）→ `Clean`（通常の空セッションはこれまで通り
  素通り＝over-block しない）。プローブ失敗（store が書き込み不能）→
  `Undetermined`（corrupt marker と同じ fail-closed 側）。
- marker が**存在する**が想定スキーマと食い違う場合（例: 必須の `tainted`
  フィールドを欠いた `{"foo":123}`）も、serde のデフォルト値で黒魔術的に
  `tainted: false` → `Clean` に解決したりしない。`tainted` は
  `#[serde(default)]` を外してあるので、欠落は deserialize の `Err` になり、
  corrupt marker と同じ `Undetermined` に落ちる。
- `mark`/`gate` はどちらも `catch_unwind` によるパニックバリアで包まれており、
  内部で panic しても「何もしなかった（＝silent allow）」にはならず、
  fail-closed な結果（強制 mark / 強制 ask-deny）に解決する。ただし `mark` の
  書き込み失敗自体（panic ではない通常の IO エラー）はこのパニックバリアの
  対象外 — その場合の fail-closed の実体は上記の `check` 側の書き込み可能性
  プローブである（`mark` は失敗を stderr に出して exit 0 するだけ）。
