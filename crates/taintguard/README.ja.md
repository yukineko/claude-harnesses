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

## 3 つの hook と 1 つの CLI readout

| サブコマンド | event | matcher | 役割 |
|---|---|---|---|
| `taintguard mark`  | PostToolUse | `WebFetch\|WebSearch\|Read` | 出所を判定し taint marker を記録 |
| `taintguard gate`  | PreToolUse  | `Bash\|Write\|Edit\|MultiEdit\|NotebookEdit` | tainted なら ask/deny |
| `taintguard clear` | Stop        | (全体)                        | クリーンなターン終了で marker を解除 |
| `taintguard tally` | **hook ではない**（CLI readout） | — | observe-only 台帳の件数を人間向けに出力（`--json` で機械可読）。stdin を読まず、読み取り失敗時は **exit 非0**。詳細は後述の「`tally` で読む」 |

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
  **observe-only** posture では `permissionDecision` をどの値でも出さず、警告テキストと
  台帳への追記だけになる（＝強制されない）。**この警告が画面に出ることは保証されていない**
  — 人間が確実に読める経路は `taintguard tally` だけである。詳細と注意点は後述の
  「observe-only モード」を参照。

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
  「このターンは tainted」「どの source が汚染したか」「observe-only なので強制を
  抑止した」ことを述べる警告テキストを **2 つのチャンネル**に出し
  （`hookSpecificOutput.additionalContext` と、その兄弟にあたる **top-level の
  `systemMessage`**。`observe_json`）、同時に append-only の JSONL 台帳に1行追記する。
  ただし**この 3 つは信頼度が違う**ので、混ぜて読んではならない:

  - **`additionalContext` は model 向けチャンネルであり、ユーザーに提示される経路ではない。**
    Claude Code はこの文字列を system reminder で包んで hook 発火位置の会話に挿入し、
    Claude は次のモデルリクエストでそれを読むが、**インターフェース上のチャット
    メッセージとしては現れない**（出典: <https://code.claude.com/docs/en/hooks.md>）。
    docs の文言はここまでなので、主張もここまでに留める — 「人間が絶対に到達できない」
    とまでは言わない（transcript を掘れば見えるかもしれない）。言えるのは
    **人間に提示されるチャンネルではないので、これで人間に知らせたことにはならない**
    ということである。
  - **`systemMessage` はユーザーに出ることを期待した best-effort** にすぎない。
    docs でこのフィールドは "Warning message shown to the user" と説明されているが、
    **`permissionDecision` を省いた non-blocking な応答でこれが描画されるかは
    ドキュメントに記載がない**（docs の PreToolUse の例は `permissionDecision: "deny"`
    を伴うものだけで、決定を持たない応答と `systemMessage` を組み合わせた例が存在しない）。
    未検証であって「出ない」と確認されたわけでもない。**どちらとも言えないので、
    これに依存してはならない。**
  - **人間が確実に読める唯一の経路は `taintguard tally`** と、その裏にある append-only
    台帳である。抑止件数を数える／観測結果を報告する際は、必ずこちらを根拠にする。

  **以前この節は「沈黙はしない」と主張していたが、それは誤りだった。** 当時 gate が
  出していたのは `additionalContext` だけで、上記のとおりそれは model にしか届かない。
  つまり**人間の読み手にとって、observe-only モードは「何も見つけなかったゲート」と
  区別できていなかった** — 沈黙していないのはモデルに対してだけで、まさにこの節が
  fail-open だと呼んでいた状態そのものだった。`systemMessage` の追加と `tally` は
  その誤りへの是正であり、うち**保証があるのは `tally` の側だけ**である。
- **`permissionDecision` はどの値でも出さない。とくに `"allow"` は出さない**（意図的）。
  明示的な `allow` は他のゲートやユーザー自身の権限規則を**上書きする**正の判定なので、
  強制をやめるためのモードが結果としてより広い範囲を強制してしまう。この理由は今も
  そのまま有効である。
  ただし省略の位置づけは正直に述べる: これは**構成上（by construction）安全側**である
  — 何も主張していないので正の判定にはなり得ない — というだけであり、
  **ドキュメントがこの形状の挙動を保証しているわけではない**。docs の
  「exit code 0 with no output means the hook has no decision to report」という記述は
  **出力が無い場合**に限定されており、observe-only は出力を出す。JSON body があるのに
  `permissionDecision` を欠く場合どう扱われるかは docs に記載がない。`defer` も
  許容値として列挙されているだけで説明の散文が一切無い（既知のドキュメント欠落。
  GitHub issue #41791）。したがって「省略＝`defer` と同じ」と**ドキュメントに書いてある**
  という主張はしない。

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

#### `tally` で読む（人間が確実に読める唯一の経路）

`taintguard tally` は **hook ではなく operator 向けの CLI readout** である。stdin を
読まず、**プロセスの現在ディレクトリ**から project を決めて、その project の台帳を数える。
hook 用の `harness_core::hook::run_hook` ラッパは**意図的に使っていない** — あれは
最後に `exit(0)` するので、それで包むと「読めなかった」と「0 件だった」が同じ終了
ステータスになってしまう（`run_tally` の doc comment 参照）。

プラグインとして導入している場合、PATH に `taintguard` は入らないので同梱ランチャを
直接叩く（以下の例では `taintguard` と略記する）:

```
$ "${CLAUDE_PLUGIN_ROOT}/bin/taintguard" tally
```

```
$ taintguard tally
[taintguard] observe-only ledger: /home/…/.taintguard/state/<project_key>/observe-only.jsonl
  suppressed: 3
  corrupt: 0

$ taintguard tally --json
{"corrupt":0,"ledger":"/home/…/observe-only.jsonl","suppressed":3}
```

各フィールドの意味:

| フィールド | 意味 |
|---|---|
| `ledger` | 数えた台帳ファイルの実パス（どの project を見たかを取り違えないため） |
| `suppressed` | **パースできた**レコード数 ＝ 抑止した強制の件数 |
| `corrupt` | **パースできなかった**行数。`suppressed` には**足されない**（壊れた行は摩擦ではなく store の健全性問題なので、合算すると両方を誤読する） |

**読み取り失敗は exit 非0 であり、0 件とは区別される**（CLAUDE.md §3）:

- **本物の 0 件**: 台帳が存在しない／空 → exit **0**。テキスト出力では
  `suppressed: 0` / `corrupt: 0` に加えて **`nothing observed yet`** の行が出る。
  この行が出るのが「読めて、かつ 0 件だった」という本物の 0 の印である。
- **読み取り失敗**: 台帳が存在するのに読めない、あるいはプロセスの cwd が解決できない
  （どの project を数えるべきか不明）→ **exit 1**。メッセージは **stderr** に出て
  `NOT a tally of zero`（件数は UNKNOWN）と明言し、**件数は一切出力しない**
  （stdout には 0 すら出さない）。`--json` の失敗時は stderr に `{"error": …}` のみで、
  `suppressed` キーは**存在しない** — 成功時の JSON は `ledger` / `suppressed` /
  `corrupt` の 3 キーちょうどで、件数は数値として出る（文字列化すると
  `jq '.suppressed > 0'` が黙って誤判定する）。

したがって `tally` の結果を計測値として記録する前に、**必ず終了ステータスを見る**。
非0 のときは「0 件だった」ではなく「数えられなかった」であり、その run は観測として
使えない。

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
