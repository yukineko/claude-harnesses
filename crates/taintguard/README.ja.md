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
| `taintguard gate`  | PreToolUse  | `Bash\|Write\|Edit\|MultiEdit\|NotebookEdit` | tainted なら ask/deny。ただし **静的に write しないと判定できた `Bash`** は taint state を見ずに素通し（0.2.0 以降、後述） |
| `taintguard clear` | Stop        | (全体)                        | クリーンなターン終了で marker を解除 |
| `taintguard tally` | **hook ではない**（CLI readout） | — | observe-only 台帳の件数を人間向けに出力（`--json` で機械可読）。stdin を読まず、読み取り失敗時は **exit 非0**。詳細は後述の「`tally` で読む」 |

### mark（PostToolUse）

- `WebFetch` / `WebSearch` の結果は常に信頼できない出所として扱う（source: `web`）。
- `Read` はターゲットパスを**そのプロジェクト**（`cwd` の配下、または `cwd` と
  **同一 git リポジトリ**）基準で分類する（`src/classify.rs`）:
  - `cwd` 配下に解決される → 信頼できる（no-op）。
  - `cwd` 配下ではないが、**`cwd` と同じリポジトリの worktree** に解決される →
    信頼できる（no-op）。0.1.9 で追加（backlog eb39308e）。
  - それ以外（`/tmp`、ホームディレクトリ、**別の**リポジトリ、git 管理外、
    `..` での脱出）→ 信頼できない（source: `external-read`）。
  - パスが判定不能（file_path が空/欠落、シンボリックリンクの解決不能等）→
    **fail-closed**（信頼できないものとして mark）。
- 解析自体が panic した場合も fail-closed（source: `internal-error` で
  強制的に mark）。「解析に失敗したので何もしない」は「解析して問題なしと判断した」
  と区別がつかず、それは fail-open になるため。

#### 同一リポジトリの worktree を信頼する理由（0.1.9 / eb39308e）

0.1.8 まで `Trusted` への経路は `starts_with(cwd)` だけだった。しかし
condukt/flow の subagent は **linked git worktree**（`/mnt/c/tmp/aegis-worktrees/<topic>`、
`~/harness-wt/<topic>`、`…/.harness-worktrees/session-<id>`）を作業場所として
渡される一方、hook payload の `cwd` はセッションのプロジェクトルート（main
checkout）のままである。そのため subagent が「編集するために作られた木」を
最初に `Read` した時点で `Untrusted` → `external-read` で mark され、次の
`Edit`/`Write` が `ask` に格下げされていた。subagent には `ask` に答える人間が
いないので、実質的に**編集不能**になる。

linked worktree は第三者の content ではなく、同一リポジトリ（同じ objects、
同じ履歴、同じ作者）を 2 回 checkout したものなので、信頼側に分類する。

**これが fail-open でない理由**（詳細は `src/classify.rs` の module docs）:

- **積極的な同一性確認のみ。** git common dir（main checkout の `.git`、linked
  worktree では `.git` ファイル → `gitdir:` → `commondir`）を両側で解決し、
  **両方が解決できて等しいときだけ** 信頼する。解決できない（`.git` が無い/
  壊れている/canonicalize 不能）は「判定不能」→ `Untrusted`（CLAUDE.md §3）。
- **別リポジトリは common dir が違う**ので従来どおり `Untrusted`。`cwd` が
  git 管理外なら、この規則は発火せず `starts_with` だけが残る。
- **手書きの `.git` ファイルでは偽装できない。** git は linked worktree を
  `<gitdir>/gitdir` という**逆ポインタ**で登録するので、それが「今読んだ
  `.git` ファイル」を指していることを要求している。偽装には対象リポジトリの
  `.git` への書き込み権限が必要であり、そこまで持つ攻撃者に対してこの分類器は
  そもそも防御線ではない。
- **`~/.claude/` は許可リストに入れていない**（eb39308e の調査で検討した上で
  却下）。operator が書いた設定（`settings.json`/`agents/`/`skills/`）と、
  `projects/<key>/<id>.jsonl` のセッション transcript（WebFetch/WebSearch の
  出力を逐語で含む＝まさにこの crate が追跡している provenance）と、
  `plugins/cache/`・`plugins/marketplaces/`（第三者由来のコード）が同居して
  おり、単一の信頼領域ではない。なお subagent の編集不能の原因でもなかった:
  skill/agent/`CLAUDE.md` は harness が読み込むもので `Read` ツールを通らない
  ため `mark` の matcher に到達しない。

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
- **hook payload が空でないのにパースできない場合も fail-closed**（`ask`/`deny`）。
  0.1.9 で修正（backlog 9a28b98c）。`gate` は verdict を持つのに
  `harness_core::hook::run_hook` の終端 `exit(0)` の下で動くため、それまでは
  「payload を読めなかった」が **stdout 無し＝silent allow** になっていた。
  「hook はユーザーのターンを壊してはならない」という契約は**プロセス**の話
  （crash しない・非0 で終わらない）であって**判定**の話ではなく、`gate` では
  「exit 0 で判定を print する」以外の意味を持たない。理由文は「payload を
  読めなかった」と述べるだけで、taint を発見したとは主張しない（判定していない
  ことを判定したように書かない）。
  なお **stdin が空**の場合は従来どおり silent（そもそも payload を渡されて
  いない＝hook として起動されていない、という別のケース。blastguard / ctxrot と
  同じ切り分け）。
- tainted でなければ何も出力しない（silent allow）。
- 以上は既定の **enforce** posture の挙動。`TAINTGUARD_OBSERVE_ONLY=1` の
  **observe-only** posture では、**既知の taint（`Tainted`）に限り** `permissionDecision` を
  どの値でも出さず、警告テキストと台帳への追記だけになる（＝強制されない）。
  **この警告が画面に出ることは保証されていない** — 人間が確実に読める経路は
  `taintguard tally` だけである。
- **ただし直前の項（marker が読めない/壊れている＝判定不能 `Undetermined`）は
  observe-only でも抑止されず、常に `ask`/`deny` に倒れる**（0.1.6 で変更）。
  詳細と注意点は後述の「observe-only モード」を参照。

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

- **抑止するもの**: **`Tainted`（＝どの source が汚染したか判っている検出）に対する**
  `ask`/`deny` の `permissionDecision` を**一切出さない**。
  したがって tainted なターンでも write-class ツールは通常どおり実行される。
  **保護は効いていない。**
- **抑止しないもの（判定不能）**: `Undetermined`（marker が壊れている／スキーマ違い／
  state dir が書き込み不能）は observe-only でも **`ask`/`deny` に倒れる**。
  理由は後述の「判定不能（Undetermined）と panic は observe-only を尊重しない」。
- **抑止しないもの（検査と報告）**: 検査そのもの、および報告。tainted を検出したら
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

### 判定不能（Undetermined）と panic は observe-only を尊重しない

**0.1.6 で変更**: `state::check` が `Undetermined`（marker が壊れている／スキーマ違い／
state dir が書き込み不能）を返した場合は、`TAINTGUARD_OBSERVE_ONLY=1` でも
**`ask`/`deny` に倒れる**。observe-only が存在する理由は「**既知の** taint を強制した
ときの摩擦を測る」ことであり、`Undetermined` は source を1つも名指しできない
（`sources` が空）ので、抑止しても得られる計測値が無い。**得るものが無いのに
CLAUDE.md §3（判定不能は必ず制限側に解決する）を支払う**ことになるため、抑止しない。
「判定できなかった」は「panic した」と同じクラスであり、panic は元から強制している。

`gate` の panic barrier が発火した場合も同様に、observe-only が設定されていても
**`ask`/`deny` に倒れる**。panic は「解析が完走しなかった」＝判定不能であり、
posture の読み取り自体も barrier の内側にあるため、尊重すべき posture を
知っているとは言えない。observe-only は**動いているゲートを計測する**ための
affordance であって、内部エラーを飲み込む許可ではない。

observe-only 下で `Undetermined` により強制された場合、`ask`/`deny` の reason 文には
**「observe-only は設定されているがこの経路では尊重されない」**という注記が付く
（`observe::undetermined_not_suppressed_note`）。observe-only を設定した運用者が
`ask` を見て混乱しないため、かつ **この事象は台帳に1行も書かれない**ことを
その場で述べるため（次節参照）。

なお 0.1.5 までは `Undetermined` も observe-only で抑止され、台帳に
`check: "undetermined"` の行が1行残っていた。この挙動は上記の理由で撤去した。

### 計測値の読み方

台帳は `$TAINTGUARD_STATE_DIR/<project_key>/observe-only.jsonl`
（**session 単位ではなく project 単位** — Stop hook の `clear` は session marker を
消すので、session に閉じた台帳では発火率が測れない）。**1行 = 抑止した強制1件**で、
`{ts, tool, sources, check, session}` を持つ。

**台帳が数えるのは「gate が発火した回数」ではなく「強制を抑止した回数」である。**
0.1.6 以降この2つはずれる: observe-only 下の `Undetermined` は**抑止されず強制される**ので、
**台帳には1行も書かれない**。これは意図的な選択である — `tally` の `suppressed` は
「抑止した件数」を主張するカウンタなので、抑止していない事象をそこに足すと
カウンタ自身が嘘になる（`suppressed` を三値に割るには tally の出力契約を変える必要があり、
それはこの変更の範囲外）。その事象は台帳ではなく、**実際に出た `ask`/`deny` と
その reason 文中の注記**として観測する。したがって新規に書かれる行の `check` は
常に `"tainted"` である（フィールドは残す — 0.1.5 以前に書かれた既存の台帳には
`"undetermined"` の行が存在しうるので、パースできる必要がある）。

`observe::tally` はパースできた件数と**できなかった件数を別々に**返す（壊れた行を
黙って捨てて件数を下げると「思ったより摩擦が少ない」と誤読される）。

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
| `suppressed` | **パースできた**レコード数 ＝ 抑止した強制の件数。**gate が発火した回数ではない** — observe-only 下の `Undetermined` は抑止されず強制されるので、ここには現れない（上記「計測値の読み方」参照） |
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

## read-only な `Bash` は gate されない（0.2.0 以降 / backlog a4b59893）

hooks.json の PreToolUse matcher は今も `Bash|Write|Edit|MultiEdit|NotebookEdit` のままだが、
`gate` は **`Bash` の `command` が静的に write しないと判定できた場合、taint state を一切見ずに
silent（＝素通し）を返す**。判定は `crates/taintguard/src/readonly.rs` の `is_readonly_bash`。

**なぜ必要だったか（実測）**: 0.1.10 までは matcher が `Bash` 全体だったため、外部ファイルを
1 つ `Read` した時点で `git status` / `git log` / `git worktree list` まで deny された。
gate 自身の文言は「**write-class** tools are downgraded」と言っているのに実挙動は Bash 全体で、
**tainted な turn は自分を診断することすらできなかった**。非対話の worker（condukt / flow）には
Stop 以外の復帰手段が無く、backlog 96075670 の worker 3 体が再投入でも同じ壁に当たった。

**これは不変条件の緩和ではない**。write できないコマンドは write-class tool ではない。また taint を
消費するのは `mark` の PostToolUse matcher（`WebFetch|WebSearch|Read`）であって `Bash` を見ていないので、
tainted な turn が「読める量」もこの変更では変わらない。

**判定は fail-closed（CLAUDE.md §3）**: 「positively read-only と分かるか？」だけを問い、
分からないものはすべて `false`＝従来どおり gate する。具体的には —

- 未知のプログラム、未知の `git` サブコマンド、パス修飾されたプログラム（`./ls` `/usr/bin/ls`）→ gate。
- `;` `&` `>` `<` `` ` `` `$` `(` `)` `{` `}` 改行 を含む → gate（リダイレクト・コマンド置換・連結）。
- クォート（`'` `"`）を含む → gate。危険だからではなく、この tokenizer が正しく分解できないから
  （**判定不能を「安全」に写さない**）。
- `|` のパイプラインは**全ステージが read-only のときだけ** read-only。`||` は空セグメントとして落ちる。
- インタプリタ（`sh` `bash` `python` `node` `perl`）、`sed`、`find`、`xargs` は**意図的に表に無い**
  （read-only な名前をした汎用実行器）。
- `git worktree` は `list` の 1 形だけ read-only（`add` / `remove` / `prune` / `repair` は mutate）。

### 引数も denylist ではなく allowlist（実装当初からの変更）

**当初の実装はプログラム名を allowlist し、フラグだけを denylist していた**
（`--output` / `-o` / `--exec` / `--config` を全プログラム一律で拒否）。この**ハイブリッドから
13 件の write/exec 到達経路が漏れた** — 実装者の自己レビューで 7 件、独立検証者でさらに 6 件、
毎回「今度こそ網羅した」と宣言した直後に次が出た（測定日 2026-08-05）。最も重かったのは
`uniq in.txt victim.txt` で、**victim.txt が実際に上書きされることを検証者が破壊的に実証**した。
`uniq` の第2 *位置引数*が出力先なので、フラグの表をどれだけ厚くしても捕まらない。
他は `rg --pre <cmd>` / `rg --hostname-bin=<cmd>` / `sort --compress-program=PROG` /
`git grep -O<pager>`（`-o` の前方一致が case-sensitive で `-O` が素通り）/
`git ls-remote <url>`（tainted turn からの外向き通信）。

denylist は必要な性質を**表明できない**。「この列挙に含まれるフラグは無い」は「write しない」ではなく
「私が思いついた writer は無い」であり、**列挙が完了したことを観測する手段が無い**。これは
CLAUDE.md §3 が拒否する形そのもので、§6 の「実装者は permissive 側に倒れる」が 13 対 0 という
スコアで実証された形になっている。そこで引数側もプログラム側と同じ allowlist へ反転した:

- `--long` は**そのプログラムの表に名前がある**ものだけ。`--long=value` 形も**名前で**照合するので、
  `--pre` / `--upload-pack` / `--compress-program` / `--open-files-in-pager` は
  「危険と認識されたから」ではなく「**表に無いから**」拒否される。
- `-abc` バンドルは**1 文字ずつ**そのプログラムの表に照合する。ASCII 数字だけは表に載せずに許可
  （`head -5` / `sort -k2`。数字は直前の文字への引数であって動詞ではない）。
  これにより `-O` は `git` で拒否しつつ `-o` は `grep`（only-matching）で許可し `sort`（出力先）で
  拒否できる — 一律の前方一致では表現できず、両方向に代償を払っていた区別。
- 位置引数は**数える**。N 番目が出力先になるプログラムは N-1 で頭打ちにする（`uniq` は 1、
  `pwd` / `whoami` / `uname` は 0）。
- `git ls-remote` は表から**削除**した。ローカルには何も書かないので当初の admission rule
  （「inspect するだけ」）は通っていたが、呼び出し側が選んだリモートに接続する＝
  **untrusted な内容を消費したせいで tainted になっている turn からの外向きチャネル**である。

表に無いものの答えは常に `false`＝gate。**エントリの追加は理由を書く意図的な行為**であり、
書き忘れのコストは「コマンドが 1 つ gate される」であって「黙って write が通る」ではない。

### 既知の残存（0.2.0 では直さず明示する）

- `git show` / `log` / `diff` / `blame` は repo 設定の `textconv` / `diff.external` を実行しうる。
  対象 repo は turn が既に作業している当のプロジェクトなので *tainted な内容*が開くチャネルでは
  ないが、表の中のコマンドが何かを exec する経路ではある。
- `git status` は `.git/index` の stat cache を更新し `index.lock` を取るので、厳密には write する。
  ユーザデータではなく内部簿記だが、admission rule に carve-out が無いので明示しておく。

## 状態の保存先

`$TAINTGUARD_STATE_DIR/sessions/<session>/taint.json`
（`TAINTGUARD_STATE_DIR` 未設定時は `~/.taintguard/state`）。

**0.2.0 で `<project_key>` の次元を外した**（backlog 90d1ca1d）。0.1.10 までは
`<project_key(cwd)>/<session>/taint.json` で、`cwd` は hook payload 由来だった。
Claude Code の `Bash` ツールは `cd` が呼び出し間で永続するため、mark した `Read` と
gate する `Bash` の間に `cd` が挟まると **gate が別バケットを見て marker を見つけられず
`Clean` を返す**＝ silent allow になっていた。taint は turn の性質であってディレクトリの
性質ではないので、食い違いうる次元そのものを削除した。**アップグレード時、旧レイアウトの
marker は孤児になる（＝一度だけ taint がリセットされる）。**

observe-only の台帳のみ **project 単位**（`<project_key>/observe-only.jsonl`）のままで、
session ディレクトリの下ではない（理由は上記「計測値の読み方」）。こちらは cwd ドリフトで
台帳が分裂しうるが、判定ではなく観測であり fail-open を作らないため 0.2.0 では変えていない。

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
- `Undetermined`（上記のいずれか）は **posture に関わらず** `ask`/`deny` に解決する。
  observe-only は `Tainted` の摩擦を測るための posture であって、判定不能を抑止する
  許可ではない（CLAUDE.md §3）。強制する側だけが posture を無視するので、
  observe-only が「抑止した」と数える台帳の意味は保たれる。
