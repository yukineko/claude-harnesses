> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# compass 仕様

## 概要

`compass` は condukt の**上流**に座り「何をやるか」を決める再オリエンテーション層である。ゴール（北極星）と
完成定義を彫り直し、現状との gap を読み、そこから導いた右サイズの一手だけを condukt へ渡す。判定（ゴールを彫る・
gap を読む・一手を選ぶ）は `/compass` skill 側の LLM 労働に置き、バイナリは状態維持と決定論的 context 生成に徹する
（`main.rs` doc-comment: *"the binary keeps state and renders deterministic context; the LLM (skill) does the judging"*）。
バイナリ自身は LLM も `AskUserQuestion` も呼ばず、**subscription で完結**する。ゴールと完成定義はリポ同居の生きた
一枚 `.compass/charter.md`（`charter::Charter`）に保持し、carve 状態・outcomes・opportunities・discovery も同じ
`.compass/` 配下に永続化する。`main.rs` の module doc は自身を **scaffold** と宣言し、`nudge`/`breadcrumb`/`gap`/
`route` を dispatch に配線済みだが一部は後続タスクまで stub と位置づける（実際には現状ほぼ全サブコマンドが実装済み）。

## 不変条件

- **LLM 非呼び出し（ハード不変条件）** — バイナリはどのサブコマンドでも LLM も `AskUserQuestion` も呼ばない。
  意味判断（`gap` の delta 導出・carve の C3〜C5・`route` の分解）は skill が行い、バイナリは決定論的な
  組み立て（`gap::assemble_gap_inputs`）・size triage（`route::route`）・状態維持のみを担う（`gap.rs`/`route.rs`
  の *"A Rust binary cannot call an LLM"* 制約 doc）。
- **hook は非ブロッキングで常に exit 0** — SessionStart `nudge` と Stop `breadcrumb` はエラー時も exit 0（再接地
  hook がターンを壊してはならない）。`nudge_command` は `nudge_verdict` を出すだけで常に `Ok(())`。`breadcrumb_command`
  は payload 不正・transcript 無し・明示ブロック無しのいずれでも黙って `Ok(())` を返し、書き込みも best-effort（`let _ =`）。
- **breadcrumb は推測しない** — `breadcrumb::extract_next_action` は assistant 最終応答中の明示的 ```` ```compass-next ````
  フェンスブロックのみを `charter.next_action` へ書き戻す。ブロックが無ければ何もしない（`main.rs`: *"never guesses"*）。
- **config は fail-soft** — `Config::load` はファイル欠落・TOML parse エラーいずれも既定値へ黙って fallback
  （`Freshness{stale_commits:20,stale_days:14,check_dod_refs:true}` / `Carve{max_rounds:4}` / `Routing{right_size:["s","m"]}`）。
  全 section・全 field が `#[serde(default)]`。
- **charter parse は tolerant** — `Charter::parse` は `## <field>` 見出しをキーに折り込み、未知見出しは無視・欠落
  field は default のまま。`nudge_verdict` は parse 失敗を `fresh`（＝黙認）として扱い、session start / driver を
  wedge しない。`save`↔`load` は構造 field を lossless round-trip し、正規化形は不動点。
- **outcome は計測証拠必須（build ≠ validate）** — `outcome::record` は evidence を trim して空を除去し、非空が
  残らなければ `anyhow::bail!("requires measured evidence …")`。空証拠の run は何も書かない。record は charter の
  `north_star`/`current_gap` を snapshot し、単調増加 `seq` を振る。
- **discovery / opportunity は fail-soft、cross-session dedup** — `discovery` 全サブコマンドは store エラーを飲んで
  exit 0（skills/hooks から呼ばれ turn を壊さない契約）。`discovery::filter_undiscovered_by_others` が `gap` と
  `route` handoff の両所で他 session が既に `Discovered` にした opportunity を落とす。store 欠落/破損時は誰も落とさず
  byte-equivalent（DoD#4）。
- **決定論** — `route::route` の triage・`centrality_cmp`（グラフ in-degree の決定論プロキシ）・`c3screen::c3_lexical_screen`・
  `outcome::pivot_signal` / `suggest_verdict`・`harness_core::score` はすべて純関数で入力に対し決定論的。
- **advisory ゲートはブロックしない** — `c3-screen`（C3 語彙スクリーン）と `suggest-verdict`（既定 verdict）は skill の
  判定を SUPPLEMENT するのみで、skill は `outcome --verdict` で上書き可能。

## 振る舞い

サブコマンドは `clap` の `Command` enum で定義（`main.rs`）。

- **`nudge [--json]`（SessionStart hook）** — `nudge_verdict` が C1（`charter.md` 欠落／`north_star`・DoD 空＝blurry）
  ＋ C2（`freshness::check` の drift floor）を1つの `NudgeVerdict{fresh, reason}` に畳む。既定は stale 時のみ1行 nudge、
  `--json` は `{"fresh":bool,"reason":string|null}` を出し下流 driver（flow）が同じ floor で gate できる。常に exit 0。
- **`breadcrumb`（Stop hook）** — stdin の Stop payload を `HookInput::parse`、`last_assistant_message` が transcript
  JSONL の最終 assistant text を拾い、`compass-next` ブロックを抽出して `next_action` へ書く。常に exit 0。
- **`evaluate` / `apply --answer <JSON>` / `carve-reset`（carve ループ §11）** — `evaluate` は `gather::gather` の
  bundle 上に `CompassGates`（C1/C2 floor のみ）を `interrogate::evaluate` で回し `{open_questions,status,round}`
  （`CarveView`）を JSON 出力、`CarveState` を init-or-load して `.compass/carve-state.json` に永続化。skill が C3〜C5
  を上乗せ・回答を `apply` で1問ずつ High-authority fragment として畳み込む。`carve-reset` は state を冪等に削除。
- **`charter [--write <JSON>]`** — 既定は resolved config＋parse 済み charter を表示。`--write` は skill 合成 charter
  （`north_star`/`definition_of_done`/`measuring_stick`/`current_gap`/`next_action`/`parked`）を `.compass/charter.md` へ永続化。
- **`gap [--write <TEXT>]`（§3）** — 既定は `gap::assemble_gap_inputs`（DoD／git activity／progress excerpt／
  measuring_stick）＋ `outcome::latest`（`last_outcome`）＋ active outcome 配下の opportunity per-bet gap slot を
  weight 降順で組み立て JSON 出力。`--write` は skill 産の gap テキストを `current_gap` へ書き戻す。
- **`route [--file <path>]`（§13）** — condukt の `Decomposition` JSON を size で triage。`route::route` は right-size
  （既定 `s`/`m`）1件＋その coupled 依存を `to_condukt`、残りを `parked`、右-size-0 端は `RouteEdge::{GoalTooBig(全l/xl),
  OnlyNoise(全xs)}` を立てる。未知 size は "needs attention" で park せず condukt へ。parked を `write_parked_to_taskprog`、
  `condukt_handoff` 課題テキストを出力。
- **`outcome --verdict <forward|unchanged|backward> --evidence <…>`（§7）** — 計測証拠付き verdict を
  `.compass/outcomes.json` へ append（`Verdict` は snake_case serde）。
- **`pivot-check`** — `outcome::pivot_signal`（末尾連続 non-forward streak ≥ `DEFAULT_PIVOT_THRESHOLD`=3 で pivot）を
  `{"recommendation","streak","threshold","reason"}` で出力、常に exit 0（flow の gate 用）。
- **`c3-screen` / `suggest-verdict` / `score`（advisory）** — `c3-screen` は DoD の曖昧・非観測可能項目を
  `{"flagged":[{"index","item","reason"}]}` で出す（数字・measurable keyword・path-like token いずれかがあれば observable）。
  `suggest-verdict --tests-delta/--regressions/--gap-closed` は `outcome::suggest_verdict`（regressions>0→backward、
  tests_delta>0∥gap_closed→forward、他 unchanged）を `{"suggested":…}` で出す。`score --severity/--effort/--lens/
  --goal-proximity` は `harness_core::score` を公開し `{"score":f64}` を出す（advisory と異なり不正 enum は非0 exit）。
- **`opportunity add|list` / `discovery record|select|list`** — active outcome（charter `north_star` snapshot、`--outcome`
  上書き）配下の named bet を `.compass/opportunities.json` へ記録/一覧（`add` は空 title 拒否＋副次 discovery emit）。
  discovery は machine-scope 共有 store（`harness_core::discovery`）の行を追記/選択/一覧。いずれも fail-soft・常に exit 0。

### module 責務

- **`charter`** — `.compass/charter.md` の構造 charter（`Charter`：6 field、全 `#[serde(default)]`）。Markdown
  `## <field>` 見出しスキームで `parse`/`to_markdown`/`load`/`save`、固定見出し順で lossless round-trip。
- **`config`** — `.compass/config.toml`（`Config`＝`Freshness`/`Carve`/`Routing`）を built-in default 上に読む。
  `load` は欠落・parse エラーで既定へ fallback。
- **`freshness`** — C2 決定論 floor。`check` が commit-divergence（`stale_commits` 超過／未コミット）・elapsed-days
  （`stale_days`／`SECS_PER_DAY`）・DoD-ref 欠落（`check_dod_refs` 時）・next_action divergence の4シグナルを集約し
  `Freshness{stale, reasons}` を返す。非 git repo はシグナル無し（trip しない）。
- **`gates`** — compass の `RigorGates` 実装（C1 存在＋C2 鮮度のみ）。C3〜C5 は skill 判定なので `CompassGates::evaluate`
  は返さない。C1 失敗時は C2 を回さず早期 return。
- **`gather`** — 「ゴール vs 現状」の provenance-tagged `Fragment` bundle を組む。charter=High、git activity
  （直近 `RECENT_COMMITS`=30 subject＋uncommitted）／taskprog progress／deepwiki=Mid。欠落 source は skip（never error）。
- **`gap`** — skill が推論する gap 入力の決定論的 ASSEMBLY（`GapInputs`/`OpportunityGap`）＋ skill 産 gap の write-back
  （`persist_gap`）。意味判断は持たない。
- **`route`** — size triage（B案＝焦点保護）。`Decomposition`/`Task`/`Routing`/`RouteEdge` と `route`/`centrality_cmp`
  （in-degree プロキシ）/`right_size_zero_edge`/`write_parked_to_taskprog`/`condukt_handoff`。
- **`outcome`** — 計測ループの決定論コア。`Verdict`/`Outcome`/`record`（証拠必須）/`latest`/`pivot_signal`/
  `PivotSignal`/`Recommendation`/`OutcomeFacts`/`suggest_verdict`。`.compass/outcomes.json` へ atomic-write。
- **`opportunity`** — PDO OST の named-bet store（`Opportunity`。`DEFAULT_WEIGHT`=1.0、legacy 互換）。active outcome
  配下に append し weight 降順 `list_under_ranked` で並べる。`.compass/opportunities.json`、空 title 拒否。
- **`discovery`** — `harness_core::discovery` 上の compass 薄層。session-id 解決（`CLAUDE_CODE_SESSION_ID`／既定 `local`）・
  record/select/list コマンド本体・opportunity-add hook・cross-session dedup（`filter_undiscovered_by_others`）。全 fail-soft。
- **`carve`** — interrogate `CarveState` の永続化（`.compass/carve-state.json`、atomic-write）と `{open_questions,status,round}`
  JSON view。carve ループ自体は driver せず skill が回す。1 repo 1 carve、破損 state は `None`（再 init）。
- **`c3screen`** — C3「観測可能な DoD」の advisory 語彙スクリーン。`C3Flag`/`c3_lexical_screen`（純）。vague token が
  あり observability signal 無しの項目のみ flag、under-flag 寄り。
