# DoD9 の次の勾配 — harness_core::verdict 低採用 3 クレートの fail-open 監査

backlog `67e95a19`。対象: `budgetguard` / `gauge` / `reviewgate` — 3クレートとも
`harness_core::verdict` の採用ファイル数が最少（各 1 ファイルのみ、という前提でのチケット起票）。
本監査は「判定不能 (IO失敗/ロック失敗/subprocess異常終了/パース不能/panic/空集合) が
clean/allow に collapse していないか」を実コードで検証する read-only 監査。

## 1. 測定情報

- 測定日: 2026-09-24
- 測定点: `git rev-parse --short HEAD` → `a2fbbbe2`（監査本体のgrep/読了を行った時点。作業ツリー:
  `/Users/yuki/src/.harness-worktrees/session-2444be10`、branch `session-2444be10`）。
  **この worktree は監査中も他セッション（バックグラウンドの flow/backlog 自動化とみられる）が
  同じ branch へ commit を積み続けており、HEAD は静止していない**（レポート最終化時点で再測定すると
  `4d503182` まで進んでいた: `chore(backlog): ...` 系のみで、`crates/budgetguard` /
  `crates/gauge` / `crates/reviewgate` への差分は `git diff --stat a2fbbbe2 4d503182 --
  crates/budgetguard crates/gauge crates/reviewgate` で無出力＝ゼロと確認済み。したがって
  対象3クレートについては`a2fbbbe2`時点の読了内容がそのままレポート最終化時点でも有効）。
  CLAUDE.md §8 の「別セッションは常に存在する前提で動く」を字義通り再確認した形。
- 監査対象の3クレートへの差分は測定点直前に存在しない（`git diff --stat` / `git diff --stat HEAD~3 HEAD -- crates/budgetguard crates/gauge crates/reviewgate` が両方とも無出力で確認済み）。作業ツリーには他セッション由来と見られる未コミット差分（`scripts/check-fail-open-mutation.py` 等）があるが、対象3クレートの外側であり本監査に影響しない。

測定コマンド（実際に実行したもの、逐語）:

```
grep -rln 'harness_core::verdict' crates/*/src/   # ベースラインの採用ファイル一覧
grep -n 'harness_core::verdict' crates/budgetguard/src/*.rs crates/gauge/src/*.rs crates/reviewgate/src/*.rs
grep -n "corrupt\|Corrupt" crates/budgetguard/src/gate.rs crates/budgetguard/src/main.rs
grep -rn 'cache::assess\|CacheHealth' crates/budgetguard/src/*.rs
grep -n "fn known_records\|fn load_all" crates/gauge/src/main.rs crates/harness-core/src/session.rs
grep -rln "gauge" crates/harness-status/src/*.rs crates/*/src/*.rs
grep -n "gauge" crates/condukt/src/state.rs
grep -n '\.ok()\|unwrap_or\|if let Ok(' crates/reviewgate/src/review.rs
grep -n '\.ok()\|unwrap_or\|if let Ok(' crates/reviewgate/src/main.rs
grep -n "diff_text\|ChangeScan\|changed_files\|truncated" crates/reviewgate/src/review.rs crates/reviewgate/src/main.rs
```

**チケット前提の訂正**: 「各クレート `harness_core::verdict` の採用は1ファイルのみ」という前提は
budgetguard については不正確。実測 `grep -n 'harness_core::verdict' crates/budgetguard/src/*.rs`:

```
crates/budgetguard/src/config.rs:13:use harness_core::verdict::Determination;
crates/budgetguard/src/gate.rs:34:use harness_core::verdict::Determination;
crates/budgetguard/src/main.rs:42:use harness_core::verdict::Determination;
```

（`crates/budgetguard/src/cache.rs:24` はコメント内の言及のみでヒットにカウントされない）— 3ファイルで実採用。
gauge (`main.rs` のみ) と reviewgate (`review.rs` のみ) はチケットの前提どおり1ファイル。
この訂正はチケットの結論（「低採用3クレートを監査する」）自体は覆さない — budgetguard は
実際に監査すると後述のとおり非常に手厚く tri-state 化・テスト済みで、3クレート中もっとも
健全だった。低採用というラベルと実装の健全性は別軸である。

## 2. 対象と非対象

読了（全文）:
- `crates/budgetguard/src/{cache,config,gate,lock,main}.rs`（`install.rs` はコード規模63行、hooks インストールのみで verdict 非関連と判断しgrep確認のみ）
- `crates/gauge/src/{config,main,model,report,store,window}.rs`（`install.rs` は同様の理由で未読了・grep確認のみ）
- `crates/reviewgate/src/{config(前半),git,main(抜粋),model,review,state}.rs`

非対象（今回読んでいない、または部分読了）:
- `crates/budgetguard/src/install.rs`（settings.json への hook merge。verdict路的でない）
- `crates/gauge/src/install.rs`（同上）
- `crates/reviewgate/src/install.rs`（同上）
- `crates/reviewgate/src/config.rs` の後半（`load` 以降の各フィールド代入・`trust` 連携以外の部分、および末尾のテスト群）— `load` の構造とデフォルト値の安全性は確認したが、個々のフィールドの sanitize（例: `max_attempts=0` 等の境界値）までは未検証
- `crates/reviewgate/src/review.rs` の1293行のうち、`decide_subprocess`/`decide_truncated`/`decide_scan_failed`/`attribute`/`evaluate` の主要経路と関連テストは読んだが、`inject_reason`/`subprocess_reason`/`run_reviewer`（subprocessの起動そのもの）などメッセージ整形・タイムアウト実装の細部は未精読
- 外部クレート（`harness-core::ledger` / `harness-core::session` / `harness-core::attribution` / `harness-core::git_probe` / `harness-core::pricing` / `condukt::state`）は「消費経路の追跡」に必要な範囲だけ該当箇所を読んだ。それらクレート自体の網羅監査ではない

## 3. 所見（crate ごと）

### 3.1 budgetguard — CONFIRMED 1件、UNVERIFIED 1件

このクレートは3クレート中もっとも tri-state 化が進んでおり（`Determination<Config>` /
`Determination<Option<f64>>` / `Verdict` 三値、lock.rs の `held()` 明示化、cache.rs の
`CacheHealth` 三値）、ほぼ全経路にテストが付いている。唯一、明確な穴が残る。

#### [CONFIRMED] 台帳(ledger.json)破損時、当日累計が「measured」として verdict に渡る

逐語引用:

```
crates/budgetguard/src/gate.rs:148-164
    let day_usd = match Ledger::load_checked(&cfg.state_dir) {
        Ok(mut ledger) => {
            let day_usd = ledger.record(session_id, today, session_usd);
            let _ = ledger.save(&cfg.state_dir);
            day_usd
        }
        Err(_corrupt) => {
            // The on-disk ledger is unparseable. Do NOT overwrite it (that would
            // erase the day's accumulated spend and fail the budget open). Leave
            // the file untouched and fall back to this session's own cost as the
            // day total — conservative: never under-reports below this session.
            eprintln!(
                "budgetguard: ledger.json is corrupt; preserving it and skipping \
                 update (day total falls back to this session's cost)"
            );
            session_usd
        }
    };
    drop(_guard);

    let verdict = verdict(cfg, session_usd, day_usd);
```

`harness_core::ledger::Ledger::load_checked`（`crates/harness-core/src/ledger.rs:57-63`）は
「ファイル不在→`Ok(default)`、存在してパース不能→`Err(LoadError::Corrupt)`」という契約で、
`Corrupt` は CLAUDE.md §3 が名指す「パース不能＝判定不能」そのもの。しかし上のコードは
`Err(_corrupt)` を `Determination::Undetermined` 相当として扱わず、`session_usd`（このセッション
自身のコストのみ）を「measured な day_usd」として `verdict()` に渡している。

消費経路の追跡: `verdict(cfg, session_usd, day_usd)` (`crates/budgetguard/src/gate.rs:341-374`) は `day_usd >=
cfg.daily_block_usd` で日次ブロックを判定する通常の比較関数であり、`Undetermined` を特別扱いしない
（`day_undetermined_verdict` を経由していない）。この `day_usd` は本来「今日、この他の全セッションの
合計でいくら使ったか」だが、台帳が壊れている状況では「他のセッションが今日どれだけ使ったか」は
**測定不能**であり、このセッション自身のコストだけを day_usd とみなすのは実際の当日累計を
過小評価しうる（他セッションの蓄積があれば必ず過小評価になる）。`daily_block_usd` が武装されていて、
真の当日累計がそれを超えているのに、この session の単体コストがそれ未満であれば、
`verdict()` は `Allow` を返す — 判定不能が clean へ collapse する経路。

同じファイル内に、ロック未取得時（`_guard.held() == false`、`crates/budgetguard/src/gate.rs:132-146`）の**正しい**双子の
処理が存在する（`day_undetermined_verdict` を呼び、`day_usd: None` として明示的に undetermined 扱い
する）。台帳破損はロック未取得と同格の「当日累計が測れない」事象でありながら、対称的な扱いを
受けていない — `gate-crate-audit-mirror-gaps`（過去に観測された monorepo 全体のパターン: 修正が
片方の mirror にだけ着地し、双子の経路に伝播しない）と一致する構造。

テスト網羅性: `crates/budgetguard/src/gate.rs` のテストモジュールに `corrupt`/`Corrupt` を
exercise するテストは無い（`grep -n "corrupt\|Corrupt" crates/budgetguard/src/gate.rs
crates/budgetguard/src/main.rs` の結果は `crates/budgetguard/src/gate.rs:149` と `:155` のみ＝実装行そのもので、
専用テストはゼロ）。ロック未取得の双子（`a_contended_ledger_lock_makes_the_day_undetermined_and_blocks`,
`crates/budgetguard/src/gate.rs:960行付近`）には手厚いテストがある一方、台帳破損側は未検証のまま埋め込まれている。

verdict: **CONFIRMED**
提案する三値化の形: `Err(_corrupt)` の腕を `day_undetermined_verdict(cfg, session_usd, "ledger.json
is corrupt")` へ差し替える（ロック未取得腕と同じ関数）。`GateResult.day_usd` も `None` にする
（`usd()` ヘルパーが `unknown` と印字するようになる）。既存の「台帳ファイル自体は上書きしない」という
安全策（消えたら過去の全履歴が失われる）はそのまま維持してよい — 変えるべきは「破損時に session_usd
を day_usd として verdict へ渡す」の一点。

#### [UNVERIFIED] `gate_run` の空 transcript_path/session_id 早期リターン

```
crates/budgetguard/src/main.rs:133-135
    if input.transcript_path.is_empty() || input.session_id.is_empty() {
        return;
    }
```

この早期リターンは`gate_run`内で最終的に `emit_and_exit(None)` へ落ち、`None => exit(0)` で
サイレント allow になる（`crates/budgetguard/src/gate.rs:718-721`）。これは形としては「空文字列 = 判定不能っぽい入力を
無条件 allow」に見えるが、消費者を追うと `HookInput::parse` はClaude Code本体がhook実行時に渡す
stdin JSON の必須フィールドであり、この2フィールドが空になるのはhookプロトコル自体が壊れた
（Claude Code側のバグ、あるいは手動でstdinを与えずCLI実行した）場合に限られる、と推測される。
これは「budgetguardが測定に失敗した」ケースではなく「そもそも測定対象が存在しない/hook契約外の
呼び出し」であるように見えるが、`HookInput::parse`自体の実装（`harness-core`側、今回未精読）を
確認していないため、「本当に判定不能な入力（例: transcript_pathが本来非空のはずが何らかの理由で
欠落）を含むかどうか」を確定できない。**消費者=hook契約の起点自体を追い切れなかったため
UNVERIFIED**とする。severity は低いと見るが、断定はしない。

#### 既に fail-closed な経路（budgetguard）— 修正対象ではない

- `crates/budgetguard/src/lock.rs:52-96`（`LedgerLock::acquire_with_timeout`）: ロック取得失敗・stale判定不能を
  すべて `held: false` として報告し、呼び出し側に「シリアライズできなかった」ことを伝える。
  `is_stale`（`crates/budgetguard/src/lock.rs:98-116`）も `metadata` の NotFound以外のエラーを「消えていない」側に倒す
  （`Err(_) => return false`、コメント: 「Any OTHER error... means we could not tell whether the
  owner is alive. Treating that as "vanished" would steal a LIVE session's lock」）。
- `crates/budgetguard/src/cache.rs` 全体: `CacheHealth` 三値（`NotYetMeasurable`/`Healthy`/`Degraded`）、
  `effective_threshold`（`crates/budgetguard/src/cache.rs:100-106`、`0.0`/NaN/範囲外を全てデフォルトへclamp、
  「floorless clamp は常にpassを意味する」を明示的にテストで固定：`a_zero_or_nan_threshold_cannot_disable_the_check`）。
- `crates/budgetguard/src/config.rs:117-213`（`Config::load_checked` / `locate`）: 「ファイル不在=Known(default)」と
  「ファイル存在するがread/parse失敗=Undetermined」を明確に分離し、`an_unparseable_config_is_undetermined_not_an_unconfigured_gate`
  などのテストで固定。`disabled_env`（`crates/budgetguard/src/config.rs:219-221`）の `unwrap_or(false)` はコメントで
  明示されている通り**制限側**（false=無効化されていない=gateは武装されたまま）。
- `crates/budgetguard/src/gate.rs` の `undetermined_verdict` / `day_undetermined_verdict`（ロック未取得側）/
  `config_undetermined_result` / `session_cost`（`Determination<Option<f64>>` 三値）:
  いずれもコストや設定が測れない場合、武装された閾値の有無に応じてBlock/Warn/Allowを正しく
  切り分けている。`session_cost`のサブエージェント未読取テスト
  (`unreadable_subagents_dir_is_undetermined_and_blocks_when_a_limit_is_armed`) は
  「読めないサブエージェント transcript を $0 として prices しない」ことをエンドツーエンドで固定。
- `crates/budgetguard/src/main.rs:115-127`（`gate_command`）: panicは `harness_core::gate::run::run_guarded` 経由で
  fail-closed-BLOCK（`stop_hook_active`でbound）。`BUDGETGUARD_DISABLE`はガードの外側でチェックされ
  「エスケープハッチは生き続ける」設計。

### 3.2 gauge — CONFIRMED 1件、UNVERIFIED 2件

gauge は「ゲート（block/allow）を持たない observability ツール」であり、自身の読みコマンド
（`report`/`session`/`subagents`/`status`）はすでに `Determination` を正しく使って「読めない store」
を `unknown` かつ非ゼロ終了として扱っている（後述）。低採用（`harness_core::verdict` の直接 `use` が
main.rs 1箇所のみ）というラベルの裏で、**config.rs がその規律から完全に外れている**のが本質的な穴。

#### [CONFIRMED] `Config::load` が read/parse 失敗を無音で `Config::default()` に握り潰し、condukt の cost 台帳を汚しうる

逐語引用（根本原因）:

```
crates/gauge/src/config.rs:82-112
        if let Some(path) = chosen {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(fc) = toml::from_str::<FileConfig>(&text) {
                    if let Some(v) = fc.enabled {
                        cfg.enabled = v;
                    }
                    ...
                    if let Some(rows) = fc.pricing {
                        cfg.pricing = rows
                            .into_iter()
                            .filter_map(|r| { ... })
                            .collect();
                    }
                }
            }
        }
        cfg
```

`read_to_string`・`toml::from_str` のいずれかが失敗しても分岐が無く、`cfg`（＝`Config::default()`、
`pricing: Vec::new()`）がそのまま返る。budgetguardの`crates/gauge/src/config.rs`が同型のバグを`load_checked`＋
`Determination`で修正済みであるのに対し、gaugeの`crates/gauge/src/config.rs`には`load_checked`相当が一切存在しない
（`grep -n 'harness_core::verdict' crates/gauge/src/config.rs` はヒットなし）。

消費経路の追跡（実際に downstream の verdict/記録に到達することを確認):

1. `crates/gauge/src/main.rs`（`subagents_cmd`）:
   ```
   let cfg = Config::load(&root);
   ...
   let cost_of =
       |s: &usage::SubAgentUsage| -> f64 { pricing::session_cost(s.models.iter(), &cfg.pricing) };
   ...
   let arr: Vec<serde_json::Value> =
       subs.iter().map(|s| subagent_json(s, cost_of(s))).collect();
   println!("{}", serde_json::to_string(&serde_json::Value::Array(arr))...);
   ```
   （`crates/gauge/src/main.rs` の `subagents_cmd` 関数、`--json` 経路。exit codeは0、正常応答として出力される —
   `Determination::Undetermined`分岐〈`usage::subagent_usage`の失敗〉とは別の、config起因の経路）。

2. `crates/harness-core/src/pricing.rs:36-63`: `rate_for`はoverrideに一致しなければ`builtin_rate`
   を使い、`builtin_rate`はfable/opus/sonnet/haiku以外の未知モデルIDに対して
   `Rate { input: 0.0, output: 0.0 }`を返す（`crates/harness-core/src/pricing.rs:11`「An unrecognized model contributes 0
   (so an unknown id never invents cost)」— これ自体は意図的設計）。よって、operatorが
   `[[pricing]]`でカスタムモデルの単価を登録していた場合、gauge.tomlの構文エラー1つで
   その override が消え、該当モデルの`cost_usd`が黙って$0.00になる。

3. `crates/condukt/src/state.rs:2241-2254`（`resolve_agent_cost`）:
   ```
   /// exact `agentId` match against `gauge subagents --json`, replacing the fragile
   /// description-string matching the SKILL.md prose used previously. Mirrors the
   /// `fugu_fingerprint` / `record_runs` soft-probe precedent in `crates/gauge/src/main.rs`: any
   /// failure (gauge absent, non-zero exit, unparseable/empty stdout, no matching
   /// id) falls through to `None` so the caller can fall back to the manually
   /// recorded `cost_usd` — never a hard error.
   pub fn resolve_agent_cost(agent_id: &str) -> Option<f64> {
       let out = std::process::Command::new("gauge")
           .args(["subagents", "--json"])
           .output()
           .ok()?; // spawn failed (not on PATH) → soft-skip
       if !out.status.success() {
           return None;
       }
       let raw = String::from_utf8_lossy(&out.stdout);
       parse_agent_cost(&raw, agent_id)
   }
   ```
   このコメントが列挙する「failure」（プロセス起動失敗・非ゼロ終了・パース不能・stdout空）は
   すべて検知される。しかし今回の finding は**そのいずれでもない**: gaugeは exit 0 で、
   正しい形のJSONを返す。ただし中の`cost_usd`の値が、config破損により本来より低い（最悪$0）。
   `resolve_agent_cost`にはこれを見分ける手段が構造的に無い — 「exit 0 + パース可能なJSON」を
   「信頼できる実測値」として扱うのが設計そのものだからである。
   `crates/condukt/src/state.rs:127`（`Task`構造体）のコメント「Observed USD cost of executing
   this task (e.g. from `gauge`)」、および`crates/condukt/src/state.rs:2116`付近「overriding `cost_usd` on success」
   が示すとおり、この値はconduktの永続状態（task実行の「観測されたコスト」）に上書き反映される。

verdict: **CONFIRMED**（ただし severity の性質は budgetguard/reviewgate の finding とは異なる —
Stop hookのblock/allow判定を動かすものではなく、condukt側の「観測コスト」という記録値を汚す。
記録が汚れた結果、condukt側の何らかの意思決定〈例: コストベースのルーティング／予算追跡〉が
その値を再利用するかどうかは condukt 側の追加調査が必要で、今回は追っていない）。

提案する三値化の形: `crate::config::Config::load_checked(root: &Path) -> Determination<Config>`を
新設し、budgetguardの`load_checked`と同型（ファイル不在=Known(default)、存在するがread/parse失敗=
Undetermined）にする。呼び出し側（`record_hook`/`report_cmd`/`session_cmd`/`subagents_cmd`/`status`）
は`Undetermined`のとき、少なくとも`subagents_cmd --json`は`gauge: sub-agent usage unknown`と同じ
パターンで非ゼロ終了・`{"status":"unknown",...}`を返すべき（既存の`usage::subagent_usage`の
`Undetermined`処理とまったく同じ型のガードを、config読み込みにも追加する）。

#### [UNVERIFIED] `Config::load` の `enabled` フィールドが read/parse 失敗時に `true`（デフォルト）へ倒れる

同じ `crates/gauge/src/config.rs:82-112` の swallow が `cfg.enabled` にも及ぶ。`record_hook`
(`crates/gauge/src/main.rs`、`if !cfg.enabled { return; }`)は、operatorが明示的に
`enabled = false`（recordingを止める意図）としたgauge.tomlが構文エラーで丸ごとパース不能になった
場合、無音で`enabled = true`（デフォルト）へ戻り、recordingを**再開**する。

これがCLAUDE.md §3の「判定不能はclean/allowに解決してはならない」の文言に反するかどうかは、
「gaugeにとっての『制限側』とは何か」の解釈に依存する。recordingの継続は
(a) プライバシー/操作者意図の観点では permissive 方向（望まれない記録が続く）
(b) budgetguard側のコスト計測精度の観点では restrictive 方向（recordingが止まるより
    続いた方が budgetguard の transcript fallback 精度に寄与する。実際 gate.rs の
    `session_cost` は gauge の record が無くても常に transcript を re-parse するフォールバックを
    持つため、gauge の recording 停止/継続いずれでも budgetguard 自体の verdict は変わらない）
のどちらとも取れ、明確な「clean/allowへのcollapse」の consumer を追い切れなかった。
**UNVERIFIED**として記録し、fixerの判断に委ねる。

#### [UNVERIFIED] `report_cmd`の`--since`フィルタが`day()`不明セッションを黙って除外

```
crates/gauge/src/main.rs (report_cmd)
    if let Some(s) = since.as_deref() {
        records.retain(|r| r.day().map(|d| d.as_str() >= s).unwrap_or(false));
    }
```

`r.day()`が`None`（タイムスタンプ欠落等）の場合`unwrap_or(false)`でレコードごと集計から除外される
ため、`gauge report --since ...`の合計コスト・トークン数は当該セッション分だけ過小になりうる。
ただしこれは人間向けレポートのフィルタであり、この値を「verdict」として消費する下流（block/allowや
記録値の上書き）を確認できなかった。`gauge status`（フィルタなし全件合計）や`gauge session`は
この経路を通らない。**UNVERIFIED**、severityは低いと見るが未確認。

#### 既に fail-closed な経路（gauge）— 修正対象ではない

- `crates/gauge/src/main.rs`（`record_hook`）: `est.aggregate.complete()`の`Determination::Undetermined`分岐
  （サブエージェントspendが読めない場合）は記録そのものを拒否し、理由をstderrに出す
  （「Persisting it would write that under-count into the canonical record budgetguard's gate
  prefers as its cost source」）。
- `crates/gauge/src/main.rs`（`known_records`関数）: `store::load_all`の`Undetermined`を`report`/`session`双方の
  読みコマンドから呼び出し、`std::process::exit(1)`＋「an unreadable store must never render as an
  empty report」という明示コメント。
- `crates/gauge/src/main.rs`（`subagents_cmd`）: `usage::subagent_usage`の`Undetermined`分岐は非ゼロ終了＋
  `{"status":"unknown",...}`、コメントで「crates/condukt/src/state.rs:1291」への影響を名指しして
  正当化している（このコメントが condukt 側の実消費経路の実在を裏付ける一次証拠でもある）。
- `crates/gauge/src/main.rs`（`status`関数）: `store::load_all`の`Undetermined`を`unknown (...)`表示に倒す
  （「`unknown`, not `0`... Only a store we actually read may produce a number」）。
- `crates/gauge/src/window.rs`: 「Unset (no file, or unparseable): every caller treats this as `None` and falls
  back to showing continuous-uptime only」と明記された、意図的なfail-soft設計（block/allowを
  持たない付随的なETA表示機能であり、CLAUDE.md §3の対象であるverdict経路ではない）。
- `find_transcript`（main.rs）: `projects`ディレクトリが読めるが個々のエントリが読めない場合は
  `eprintln!`で警告しつつ`continue`（黙って無視しない）。

### 3.3 reviewgate — CONFIRMED 1件（2つの発現経路）、UNVERIFIED 2件

3クレート中もっとも severity が高い finding。reviewgate は Stop hook で「レビュー済みでない diff を
block する」ゲートそのものであり、本 finding は block/allow を直接動かす。

#### [CONFIRMED] `diff_text`/`run_diff` の git subprocess 失敗が無シグナルで消え、`changed_files`が確認した「変更あり」を「レビュー対象なし→allow」に反転しうる

逐語引用（根本原因、2箇所）:

```
crates/reviewgate/src/git.rs:135-143
fn run_diff(root: &Path, base: &[&str], files: &[String], out: &mut String) {
    let mut args: Vec<&str> = base.to_vec();
    args.extend(files.iter().map(String::as_str));
    if let Ok(o) = Command::new("git").current_dir(root).args(&args).output() {
        if o.status.success() {
            out.push_str(&String::from_utf8_lossy(&o.stdout));
        }
    }
}
```

```
crates/reviewgate/src/git.rs:99-118
pub fn diff_text(root: &Path, files: &[String], max_bytes: usize) -> DiffText {
    let mut s = String::new();

    run_diff(root, &["diff", "--"], files, &mut s);
    run_diff(root, &["diff", "--cached", "--"], files, &mut s);

    // untracked files among `files`: include their contents as "new file" diffs.
    let mut others = Vec::new();
    let mut args: Vec<&str> = vec!["ls-files", "--others", "--exclude-standard", "--"];
    args.extend(files.iter().map(String::as_str));
    if let Ok(o) = Command::new("git").current_dir(root).args(&args).output() {
        if o.status.success() {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
```

`run_diff`は`Command::output()`がErrでも、`status.success()`がfalseでも、**何のシグナルも
返さず・記録せず**、単に`out`に何も追記しない。`diff_text`が返す`DiffText`には`truncated: bool`
というフィールドはあるが、これは「バイト数上限で切った」ことだけを表し、「subprocessが失敗して
中身が丸ごと・部分的に欠落した」ことを表現する手段が構造的に存在しない。

消費経路の追跡（`evaluate`、実際にblock/allowへ到達することを確認）:

```
crates/reviewgate/src/review.rs:171-186 (先頭)
    let changed = match crate::git::changed_files(root) {
        crate::git::ChangeScan::NotRepo => return allow("no-git", st),
        crate::git::ChangeScan::Failed => {
            ...
            return decide_scan_failed(cfg, prior_attempts);
        }
        crate::git::ChangeScan::Files(v) => v,
    };
```

```
crates/reviewgate/src/review.rs:203-234
    let files = reviewable_files(cfg, &changed);
    if files.len() < cfg.min_changed_files {
        return allow("no-reviewable-changes", st);
    }

    let crate::git::DiffText {
        text: diff,
        truncated,
    } = crate::git::diff_text(root, &files, cfg.max_diff_bytes);
    if diff.trim().is_empty() {
        return allow("empty-diff", st);
    }
    ...
    if truncated {
        return with_note(decide_truncated(cfg, files, prior_attempts), note);
    }

    let hash = hash_diff(&diff);
    if !st.last_hash.is_empty() && st.last_hash == hash {
        return allow("already-reviewed", st);
    }
```

**発現経路A（全欠落 → 即allow）**: `changed_files`が`Files(v)`（変更ファイルあり、例:
tracked な既存ファイルへの変更のみで untracked 新規ファイルは無い）を正しく確認した直後、
`diff_text`内の`run_diff`（unstaged側・staged側の両方）がsubprocess失敗（非ゼロ終了／spawn失敗:
例えば`.git/index.lock`競合、ディスクフル、権限問題、gitバイナリのクラッシュ）した場合、
`others`（untrackedファイル一覧、対象がすべてtrackedなら空）も空なので、最終的な`diff`文字列は
空になる。`diff.trim().is_empty()`が`true`となり、`crates/reviewgate/src/review.rs:209`の`return allow("empty-diff", st)`
が実行される — **`changed_files`が確認した実在の変更が、一切レビューされずにStopが許可される**。

**発現経路B（部分欠落 → 不完全diffが「レビュー済み」として確定する）**: `run_diff`の2回の呼び出し
（unstaged・staged）のうち片方だけが失敗した場合、`diff`は非空だが**実際の変更の一部を欠いた**
テキストになる。これは`truncated`フラグの対象外（バイト数超過による切り詰めではない）ため、
`crates/reviewgate/src/review.rs:232`の truncation guard を素通りする。この不完全な`diff`が`hash_diff`でハッシュ化され
(`crates/reviewgate/src/review.rs:236`)、Inject/Subprocessいずれのモードでも当該 diff はレビュー済みとして
`last_hash`に記録される。次回以降、真の（欠落分を含む）diffが偶然同じハッシュになることは
考えにくいが、**このセッションで実際に発生した変更の一部が、一度もレビューされずに
「レビュー済み」の実績として確定する** — `crates/reviewgate/src/review.rs`冒頭のdocstringが明示的に警告する
「Truncation guard」の被害（「a dropped tail is *unreviewed*... must NOT be silently allowed
（that would let the tail bypass the gate)」）と同型の被害が、truncationとは別の原因（subprocess
失敗）で、truncation guardの保護対象外として発生する。

**この finding が高確度である理由**: `crates/reviewgate/src/git.rs`のファイル冒頭コメント（`crates/reviewgate/src/git.rs:1-10`）および
`crates/reviewgate/src/review.rs`の`decide_scan_failed`が生成する日本語メッセージ
（`crates/reviewgate/src/review.rs:462-476`、逐語:
「空の diff を『変更なし』と解釈して無言で通過させると、未レビューの変更が gate をすり抜けて
しまいます」）は、**まさに本findingで指摘している事象そのもの**を、`changed_files`の文脈で
明示的に説明し、対策済みである。つまり実装者は、この失敗モード自体は正しく認識・修正している
——だが`changed_files`（変更ファイル**一覧**の取得）にだけ対策し、`diff_text`（変更**内容**の取得。
`changed_files`成功後に呼ばれる下流の別subprocess群）という双子の経路には同じ対策が伝播していない。
`gate-crate-audit-mirror-gaps`の典型例。

テスト網羅性: `crates/reviewgate/src/git.rs`のテスト（`changed_files`のtri-state関連: 
`non_repo_dir_is_notrepo`, `clean_repo_is_empty_files_not_failed`,
`collect_reports_error_vs_empty_success`）は`changed_files`のみを対象とし、`diff_text`/`run_diff`
の失敗系を exercise するテストは存在しない（`truncate_on_boundary`関連のテスト3件のみが
`diff_text`周辺の唯一のテストで、いずれもバイト数切り詰めの検証であり subprocess 失敗は
未シミュレート）。

verdict: **CONFIRMED**（3クレート中もっとも severity が高い — Stop hookのblock/allowを直接
反転させる経路であり、かつ「レビュー済み」という将来にわたる確定状態〈`last_hash`〉を汚しうる）。

提案する三値化の形: `run_diff`の戻り値を`bool`（成功/失敗）にし、`diff_text`はそれを集約して
`DiffText { text, truncated, incomplete: bool }`のように第三の状態を追加するか、あるいは
`ChangeScan`と同型の `enum DiffScan { NotRepo, Failed, Text(DiffText) }` に統合する。
`evaluate`側では`incomplete`（または`Failed`）を`truncated`と同じ扱い——`decide_truncated`と
同型の bounded block（give-up付き、`REVIEWGATE_DISABLE`/`reviewgate skip`のエスケープハッチ明示）
——にルーティングし、`empty-diff`allowの手前でこのチェックを先に評価する（`truncated`チェックが
hash短絡の手前に置かれているのと同じ理由・同じ位置関係で)。

#### [UNVERIFIED] `Config::load`内の1個の壊れた glob パターンが黙って include/exclude セットから脱落する

```
crates/reviewgate/src/config.rs:74-82（build_set関数、抜粋）
        if let Ok(glob) = Glob::new(g) {
            b.add(glob);
            any = true;
        }
```

複数の`include`globのうち1個だけがtypoで不正な場合、そのglobだけが無音で脱落し、他の有効なglobは
残る（`any=true`のまま）。結果、operatorが意図した include 範囲より**狭い**集合でレビュー対象が
決まる可能性がある——「include未設定」時は`unwrap_or(true)`で全ファイルが対象になる
（`crates/reviewgate/src/review.rs:63`）ため、これは未設定時よりむしろ狭いという、直感に反する方向の縮小である。
ただし、これが実際にreview対象からファイルを取りこぼす具体的なシナリオ（操作者が複数include
globを設定し、かつそのうち1個だけがtypoで、かつ他のglobにはマッチしないファイル種別を変更する、
という複合条件）は狭く、かつ`reviewgate.toml`は`harness_core::trust::is_trusted`のゲートを
通過した信頼済みプロジェクトのみが読まれる（`crates/reviewgate/src/config.rs:191-212`）ため悪用可能性も低い。
消費先（`crates/reviewgate/src/review.rs:203`の`min_changed_files`判定・`reviewable_files`フィルタ）は追えたが、
「これが実運用で観測されうるほど現実的か」を判断する材料が不足しており**UNVERIFIED**とする。

#### [UNVERIFIED] `run_reviewer`: reviewer subprocess が非ゼロ終了しても stdout が「lgtm」で始まっていれば Clean になる

逐語引用:

```
crates/reviewgate/src/review.rs:630-636 (run_reviewer)
            if !status.success() && out.trim().is_empty() {
                return Verdict::undetermined(format!("exit {:?}", status.code()));
            }
            classify(&out)
```

```
crates/reviewgate/src/review.rs:648-660 (classify)
fn classify(out: &str) -> Verdict {
    let t = out.trim();
    if t.is_empty() {
        return Verdict::from_findings(vec![]);
    }
    let first = t.lines().next().unwrap_or("").trim();
    if first.eq_ignore_ascii_case("lgtm") || first.to_ascii_lowercase().starts_with("lgtm") {
        return Verdict::from_findings(vec![]);
    }
    Verdict::violation(t.to_string())
}
```

`!status.success()`（非ゼロ終了）は`out.trim().is_empty()`のときだけ`Undetermined`に倒れる。
reviewerプロセスが失敗しつつ、クラッシュ直前に何かstdoutへ書き出し、その1行目がたまたま
（大文字小文字を問わず）`lgtm`で始まっていた場合、`classify`は終了ステータスを見ずに
`Verdict::from_findings(vec![])`（＝clean）を返す。これは`decide_subprocess`
（`crates/reviewgate/src/review.rs:296-354`、「fail-closedな経路」リスト参照）が「reviewerの失敗＝reviewed and clean
ではない」と明示的に固定している不変条件の抜け道になりうる。

消費経路: `classify`の戻り値は`decide_subprocess`へ渡り、`Verdict::Clean`はそのまま
`allow`系の決定へ流れる（`decide_subprocess`のVerdict分岐、`crates/reviewgate/src/review.rs`内）。

**UNVERIFIED とする理由**: この経路が実運用で到達可能か（＝operatorが設定する`reviewer_cmd`が、
非ゼロ終了しつつstdoutの1行目に`lgtm`で始まる文字列を書くことが現実的に起こりうるか）を、
今回のスコープ内の`reviewgate.toml`のデフォルト`reviewer_cmd`設定や実際のreviewer実装を
確認しないまま判断することはできなかった。`reviewer_cmd`自体は`crates/reviewgate/src/config.rs`側で読んだ範囲では
文字列コマンドラインとしてのみ扱われ、具体的な既定コマンド（外部LLM CLIなど）の出力仕様までは
追っていない。パターン自体は実在し、消費経路（`decide_subprocess`→clean allow）も追跡できたが、
「到達可能性」を実測できていないため CONFIRMED とはしない。

#### 既に fail-closed な経路（reviewgate）— 修正対象ではない

- `crates/reviewgate/src/git.rs:33-59`（`changed_files`）: `ChangeScan`三値（`NotRepo`/`Failed`/`Files`）。
  `probe_repo`の`Undetermined`は`Failed`へ、`collect`のいずれかの失敗も`Failed`へ集約する
  （`ok = ... && ... && ...`の短絡評価、`!ok`で`Failed`）。テスト`clean_repo_is_empty_files_not_failed`
  が「空だが成功」と「失敗」を明確に区別することを固定。**これが今回の finding の "正しい双子"**。
- `crates/reviewgate/src/review.rs:171-184`（`evaluate`冒頭）: `ChangeScan::Failed`を`decide_scan_failed`へルーティング
  （bounded block、give-up後も明示的allowで理由をstderrに出す）。
- `crates/reviewgate/src/review.rs:106-120`（`Attribution`型）と`attribute`関数: transcriptが読めない場合、「narrow」
  （＝自分の変更でないものを除外）を行わず全件を対象に残す。コメント逐語：
  「Narrowing here would rewrite "I cannot tell whose these are" into "none of these are mine" —
  判定不能 resolved to the permissive side, which would silently switch the whole gate off on any
  transcript hiccup」。
- `crates/reviewgate/src/review.rs:296-354`（`decide_subprocess`）: reviewer subprocessの`Verdict::Undetermined`
  （クラッシュ・タイムアウト・パース不能出力）は**Allowに倒れない**。bounded block
  （`max_attempts`）の後、明示的なloudなgive-upのみ許可する。コメント逐語：「A reviewer that
  itself fails... is NOT the same as "reviewed and clean"」。
- `crates/reviewgate/src/review.rs:398-421`（`decide_truncated`）: 本findingが「あるべき姿」として提案している設計の
  実例そのもの — バイト数超過による切り詰めを`truncated`フラグで検出し、bounded blockへ回す。
  今回の finding は、この設計思想を`diff_text`のsubprocess失敗にも同じ形で拡張すべき、という
  提案である。
- `crates/reviewgate/src/config.rs:191-212`（`Config::load`のtrustゲート）: プロジェクトの`reviewgate.toml`は
  `harness_core::trust::is_trusted(root)`を通らない限り読まれず、home configへフォールバックする
  （`reviewer_cmd`がsubprocessとして実行されることへの、信頼されていないリポジトリからの
  コード実行防止）。
- `crates/reviewgate/src/main.rs:138-146`（`review_command`のpanicガード）: `harness_core::gate::run::run_guarded`
  経由でfail-closed-BLOCK、`stop_hook_active`でbound。CLAUDE.md §1が名指しする「never break the
  turn」的な正当化文言を明示的に否定するdocstringを持つ（「is deliberately NOT a "never break the
  turn" guard」）。

**注記（config.rsのload全体について）**: `crates/reviewgate/src/config.rs:214-216`
（`if let Some(path) = chosen { if let Ok(text) = ... { if let Ok(fc) = ... {`）は budgetguard/gauge
と同型の「read/parse失敗を無音でdefaultに倒す」実装だが、`Config::default()`自体が
`enabled: true, mode: Inject`という**能動的にレビューし続ける**設定であるため（`crates/reviewgate/src/config.rs:152-168`、
モジュール冒頭コメント「Safe by default: with no config the gate reviews ordinary source changes」）、
現状は「たまたま安全な default に倒れているために fail-open として顕在化していない」壊れやすいパターン
である。これは「既に fail-closed」と「CONFIRMED finding」の中間——**今は安全だが、将来 `Config::default()`
がより緩い方向に変更された瞬間に無音の fail-open になる**構造的な脆さとして、修正対象ではないが
記録しておく。加えて`crates/reviewgate/src/config.rs:185`のコメント「Any parse error silently falls back (the gate must
never crash a turn)」は、CLAUDE.md §1が名指しする正当化フレーズ（"never break/crash a turn"）と
同系統の文言であり、レビュー時は要注意。

## 4. 既に fail-closed な経路（不合格条件・サマリ）

修正者はこのリストにある関数・分岐を「fail-open だから直す」と誤検出してはならない:

- budgetguard: `lock.rs`全体, `cache.rs`全体, `config.rs`の`load_checked`/`locate`/`disabled_env`,
  `gate.rs`の`undetermined_verdict`/`day_undetermined_verdict`(ロック未取得腕)/
  `config_undetermined_result`/`session_cost`, `main.rs`のpanicガード配線
- gauge: `main.rs`の`record_hook`(`est.aggregate.complete()`処理), `known_records`,
  `subagents_cmd`の`usage::subagent_usage`処理, `status`の`store::load_all`処理, `window.rs`全体
  （意図的fail-soft、verdict対象外）, `report.rs`（純粋整形、エラー経路なし）
- reviewgate: `git.rs`の`changed_files`/`collect`/`ChangeScan`, `review.rs`の
  `decide_scan_failed`/`decide_truncated`/`decide_subprocess`/`Attribution`/`attribute`,
  `config.rs`の`is_trusted`ゲート, `main.rs`のpanicガード配線

## 5. 測れなかったこと

- **condukt側の`cost_usd`の最終消費**: gauge findingの消費経路を`crates/condukt/src/state.rs`の
  `resolve_agent_cost`/`Task.cost_usd`更新箇所まで追ったが、そこから先——この値がconduktの
  意思決定（コストベースのルーティング、予算ベースの打ち切り等）にどう使われるか、あるいは単なる
  記録・表示に留まるか——は追っていない。conduktクレート自体は今回のスコープ外。
- **`harness_core::attribution::attribute_from_transcript`の内部実装**: reviewgateの`attribute`
  関数が呼ぶ先の実装（transcript解析でどのファイルが「このセッションのもの」と判定されるか）は
  未読了。`Undetermined`に倒れる条件の網羅性は確認していない。
- **`harness_core::hook::HookInput::parse`の実装**: budgetguard/gauge/reviewgateいずれも
  hook起動の入口で依存するが、実装（`harness-core`側）は読んでいない。「空文字列フィールドの
  発生条件」（3.1のUNVERIFIED項）を確定できなかった直接の理由。
- **`harness_core::trust::is_trusted`の内部実装**: reviewgateの`Config::load`がプロジェクト設定の
  信頼判定に使うが、この関数自体（`crates/harness-core/src/trust.rs`と思われる）は未読了。
  MEMORY上、`is_trusted`が`HARNESS_TRUST_ALL`のようなショートサーキットを持つ既知の懸念
  （`is-trusted-shortcircuits-trust-all`）があるが、今回は読み側の消費（`reviewgate::config::load`
  がtrust結果をどう使うか）だけを確認し、`is_trusted`自体の判定ロジックは監査していない。
- **サンドボックス内での動作再現**: 本監査は静的な読み込みのみで行った。`cargo test`/`cargo build`
  は指示により実行していない。したがって「テストが無い」という記述は`grep`によるテスト関数名の
  不在確認に基づくものであり、それらのテストが実際に**通る**（あるいは red になる）ことは未確認。
  budgetguard finding・reviewgate findingとも、修正時は「まず red を観測してから green にする」
  （CLAUDE.md §2 の F→P オラクル）に従うこと。
- **他2クレート（reviewgateの`install.rs`等、3クレートの`install.rs`全て）**: hookインストール
  ロジック自体にverdict経路が存在するかどうかは、ファイル冒頭のコメントとサブコマンド名から
  「settings.jsonのmerge処理のみ」と推測したに留まり、全文は読んでいない。
- **`reviewgate.toml`の実際の`reviewer_cmd`既定値・実装**: `run_reviewer`/`classify`の
  「非ゼロ終了だがstdout先頭が`lgtm`」finding（3.3のUNVERIFIED項）の到達可能性判定に必要だが、
  operatorが設定する実際のreviewer CLIの出力仕様（部分失敗時にstdoutへ何を書くか）は監査していない。
