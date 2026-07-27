# autoflow verdict-path audit — `backlog.rs` / `condukt.rs` / `lock.rs`

read-only 監査。**このドキュメントは `crates/autoflow/` を1行も変更していない。**

- 監査対象: `crates/autoflow/src/backlog.rs`（167 行）/ `crates/autoflow/src/condukt.rs`（352 行）/
  `crates/autoflow/src/lock.rs`（272 行）。call site として `crates/autoflow/src/main.rs`（683 行）と
  `crates/autoflow/src/delegation_audit.rs`（548 行）を追跡した。
- 測定点: commit `1601b835`（autoflow 0.1.16）、測定日 2026-07-28、作業ツリーは clean。
- 行数の測定コマンド: `wc -l crates/autoflow/src/*.rs`。
- テスト: `. "$HOME/.cargo/env" && cargo test -p autoflow` = 49 + 4 + 2 = **55 passed / 0 failed**
  （無変更のまま green。`tests/precompact_lock.rs` は `target/debug/backlog` の存在を前提とするので、
  事前に `cargo build -p backlog` が必要。これはテスト自身が panic メッセージで要求している前提であり、
  autoflow 側の欠陥ではない）。

## 0. 方法と用語

**signature は証拠にならない**（repo の教訓「erasure lives at callsite」）。よって全関数について
「返り値の型」ではなく「判定不能がどの値へ写るか」と「その値を main.rs 側の分岐がどう読むか」を
追跡した。さらに、静的読解だけでは*予測*にとどまるので、**11 本の fault injection を実際に実行して
観測**した（§4.9 に手順）。以下で「観測」と書いた行は実行結果であり、「未観測」と書いた行は
静的読解のみである。両者を混ぜていない。

方向のラベル:

| ラベル | 意味 |
| --- | --- |
| **A** = permissive-A | 誤って進行を許す（既に driver が走っているのに二重駆動する） |
| **B** = permissive-B | 誤って「もう作業が無い」と完了扱いにしてループを止める |
| **R** = restrictive | 安全側（判定不能を stand down / 非発火へ倒す） |

集計の単位は**関数ではなく失敗経路（sink）**である。1 つの関数が方向の異なる sink を複数持つため
（例: `backlog_driver_active` は subprocess 失敗を R へ、binary 不在を A へ倒す）。
逆に、同一の hazard が producer と consumer の 2 行に現れることもある
（`find_backlog_binary` の `None` と、それを受ける `backlog_driver_active` の `return false;`）。
**その重複は 1 件として数える。** §1 の表は関数ごとの網羅列挙なので行数とは一致しない。
数えた実体は以下（各項目は §3 / §4 の該当節へ対応）:

| ID | 方向 | sink | 節 |
| --- | --- | --- | --- |
| A-1 | A | `crates/autoflow/src/backlog.rs:96` 「.ok()?」 → `crates/autoflow/src/lock.rs:45` 「return false;」（未文書の残余） | §4.5 |
| A-2 | A | `crates/autoflow/src/condukt.rs:103` 「None => return false,」（advisory 非発火・文書化済み） | §4.8 |
| B-1 | B | `crates/autoflow/src/backlog.rs:66` 「serde_json::from_slice(&output.stdout).unwrap_or_default()」 | §4.1 |
| B-2 | B | `crates/autoflow/src/backlog.rs:73` 「i.status == "pending"」 | §4.2 |
| B-3 | B | `crates/autoflow/src/backlog.rs:58` 「return vec![];」（非0終了） | §4.3 |
| B-4 | B | `crates/autoflow/src/backlog.rs:61` 「eprintln!("autoflow: could not run backlog list: {e}");」（spawn 失敗） | §1.1 |
| B-5 | B | `crates/autoflow/src/backlog.rs:26` 「None => return vec![],」（binary 不在・carve-out） | §4.8 |
| B-6 | B | `crates/autoflow/src/condukt.rs:145` 「serde_json::from_str::<RunState>(&t).ok()」 | §4.4 |
| B-7 | B | `crates/autoflow/src/condukt.rs:172` 「.ok()?」 | §4.6 |
| B-8 | B | `crates/autoflow/src/condukt.rs:166` 「.unwrap_or_default()」（未観測） | §4.7 |
| R-1 | R | `crates/autoflow/src/lock.rs:57` 「_ => true,」 | §3.2 |
| R-2 | R | `crates/autoflow/src/lock.rs:80` 「None => true,」 | §3.1 |
| R-3 | R | `crates/autoflow/src/lock.rs:81` 「.unwrap_or(false)」（`stale` 欠落 → active） | §3.1 |
| R-4 | R | `crates/autoflow/src/lock.rs:115` 「_ => false,」 | §2 |
| R-5 | R | `crates/autoflow/src/lock.rs:120` 「let Some(v) = parse_status_json(stdout) else {」 | §3.3 |
| R-6 | R | `crates/autoflow/src/lock.rs:124` 「if v.get("stale").and_then(」（stale レコードは非駆動） | §3.3 |
| R-7 | R | `crates/autoflow/src/lock.rs:101` 「if session_id.is_empty() {」 | §3.4 |
| R-8 | R | `crates/autoflow/src/condukt.rs:153` 「fn save_run(path: &Path, run: &RunState) -> std::io::Result<()> {」 | §3.4 |
| R-9 | R | `crates/autoflow/src/condukt.rs:115` 「None => return,」 | §2 |
| R-10 | R | `crates/autoflow/src/backlog.rs:119` 「.unwrap_or(root)」 | §3.4 |

- **A: 2 件**（A-2 は docstring で意図が明示された advisory 抑止、A-1 が未文書の残余）
- **B: 8 件**（うち B-5 は carve-out として一貫、B-8 は未観測）
- **R: 10 件**

## 1. verdict を返す全関数と失敗経路

### 1.1 `crates/autoflow/src/backlog.rs`

| 関数（行） | 返り値 | 失敗経路（逐語引用） | 判定不能の写り先 | 方向 |
| --- | --- | --- | --- | --- |
| `crates/autoflow/src/backlog.rs:23` 「pub fn find_open(cwd: &Path) -> Vec<BacklogItem> {」 | `Vec<BacklogItem>` | binary 不在: `crates/autoflow/src/backlog.rs:26` 「None => return vec![],」 | 空 vec | B |
| 同上 | | 非0終了: `crates/autoflow/src/backlog.rs:54` 「"autoflow: backlog list exited {}: {}",」 の直後 `crates/autoflow/src/backlog.rs:58` 「return vec![];」 | 空 vec（stderr に診断あり） | B |
| 同上 | | spawn 失敗: `crates/autoflow/src/backlog.rs:61` 「eprintln!("autoflow: could not run backlog list: {e}");」 | 空 vec（stderr に診断あり） | B |
| 同上 | | **parse 失敗**: `crates/autoflow/src/backlog.rs:66` 「serde_json::from_slice(&output.stdout).unwrap_or_default()」 | 空 vec（診断なし） | B |
| 同上 | | status 語彙の不一致: `crates/autoflow/src/backlog.rs:73` 「i.status == "pending"」 | 空 vec（診断なし） | B |
| `crates/autoflow/src/backlog.rs:78` 「pub(crate) fn find_backlog_binary() -> Option<PathBuf> {」 | `Option<PathBuf>` | cache dir の read_dir 失敗: `crates/autoflow/src/backlog.rs:96` 「.ok()?」 | `None`（「未インストール」と同一の値） | A |
| 同上 | | 候補ゼロ: `crates/autoflow/src/backlog.rs:103` 「candidates.pop()」 | `None` | A |
| `crates/autoflow/src/backlog.rs:116` 「pub(crate) fn repo_project_path(cwd: &Path) -> String {」 | `String` | canonicalize 失敗: `crates/autoflow/src/backlog.rs:119` 「.unwrap_or(root)」 | 生の絶対パス（**定数ではない**） | R |

補足（表に入らない引用）:

- 非0終了の枝は意図が明示されている: `crates/autoflow/src/backlog.rs:50` 「// Non-zero exit: surface it rather than silently reporting "no work"」。
  ただしこのコメント自身が `crates/autoflow/src/backlog.rs:52` 「// error). Still fail-soft to an empty vec — never break the turn.」 と続けており、
  **stderr へ出すだけで返り値の verdict は空 vec のまま**である。§4.3 の観測はこの記述どおりの挙動を示した
  （実装とコメントの食い違いは無い）。
- `find_open` の docstring は `crates/autoflow/src/backlog.rs:22` 「pending work. Fail-soft throughout — autoflow must never break a turn.」。
  この「never break a turn」は CLAUDE.md が「判定を持つコードの docstring に書いた時点で赤信号」と
  名指ししている語であり、実際この関数の戻り値は §2 で Stop の完了判定に直結している。
- `find_backlog_binary` の PATH 探索は `crates/autoflow/src/backlog.rs:82` 「.is_ok()」 で
  **spawn の成否しか見ていない**（終了ステータスを見ない）。壊れた `backlog` が PATH にあると `Some` を返すが、
  その後の実害は `crates/autoflow/src/lock.rs:57` 「_ => true,」 の fail-closed に吸収されるため A には数えない（§3.2）。
- `crates/autoflow/src/backlog.rs:102` 「candidates.sort();」 は辞書順ソートなので、
  version dir が増えたとき `0.10.0` より `0.9.0` が後にくる。verdict 経路ではないため本監査の対象外だが、
  事実として記録する。

### 1.2 `crates/autoflow/src/condukt.rs`

| 関数（行） | 返り値 | 失敗経路（逐語引用） | 判定不能の写り先 | 方向 |
| --- | --- | --- | --- | --- |
| `crates/autoflow/src/condukt.rs:38` 「pub fn find_pending(cwd: &Path) -> Vec<TaskState> {」 | `Vec<TaskState>` | run-state を得られない: `crates/autoflow/src/condukt.rs:41` 「None => return vec![],」 | 空 vec | B |
| 同上 | | 永続化失敗: `crates/autoflow/src/condukt.rs:59` 「"autoflow: failed to persist condukt run state to {}: {e}",」 | 続行（stderr に診断あり） | R |
| `crates/autoflow/src/condukt.rs:90` 「pub fn has_completed_tasks_for_run(cwd: &Path, run_id: &str) -> bool {」 | `bool` | read / parse 失敗: `crates/autoflow/src/condukt.rs:103` 「None => return false,」 | `false`＝「完了していない」 | A（advisory 抑止・文書化済み） |
| `crates/autoflow/src/condukt.rs:112` 「pub fn mark_running(cwd: &Path, task_ids: &[&str]) {」 | なし | run-state を得られない: `crates/autoflow/src/condukt.rs:115` 「None => return,」 | 無操作 | R |
| `crates/autoflow/src/condukt.rs:138` 「fn load_latest(cwd: &Path) -> Option<(PathBuf, RunState)> {」 | `Option` | read 失敗: `crates/autoflow/src/condukt.rs:143` 「let run = std::fs::read_to_string(&path)」 の `.ok()` | `None` | B |
| 同上 | | **parse 失敗**: `crates/autoflow/src/condukt.rs:145` 「serde_json::from_str::<RunState>(&t).ok()」 | `None`（「run が無い」と同一の値） | B |
| `crates/autoflow/src/condukt.rs:153` 「fn save_run(path: &Path, run: &RunState) -> std::io::Result<()> {」 | `io::Result` | IO / serialize 失敗 | 呼び出し側へ伝播 | R |
| `crates/autoflow/src/condukt.rs:163` 「fn now_secs() -> i64 {」 | `i64` | SystemTime 失敗: `crates/autoflow/src/condukt.rs:166` 「.unwrap_or_default()」 | `0` | B（未観測） |
| `crates/autoflow/src/condukt.rs:170` 「fn latest_run_file(project_dir: &Path) -> Option<PathBuf> {」 | `Option<PathBuf>` | read_dir 失敗: `crates/autoflow/src/condukt.rs:172` 「.ok()?」 | `None`（「run file 無し」と同一の値） | B |
| 同上 | | 候補ゼロ: `crates/autoflow/src/condukt.rs:183` 「entries.pop()」 | `None` | B |

補足:

- `now_secs()` が `0` を返すと `crates/autoflow/src/condukt.rs:48` 「unwrap_or(i64::MAX)」 の隣の
  `age` が負になり、`crates/autoflow/src/condukt.rs:49` 「if age > STUCK_SECS {」 が成立しないため
  **中断された running タスクが二度と pending へ戻らない**。結果として `find_pending` の集合が空になりうる（B）。
  ただし到達には system clock が 1970 以前である必要があり、**実機で観測していない**。
- `save_run` の docstring は `crates/autoflow/src/condukt.rs:149` 「/// Persist run-state. Returns the IO/serialize error instead of swallowing it,」 で、
  実装（`?` による伝播）と一致している。呼び出し側 2 箇所も `if let Err(e)` で受けて診断を出す。R と判定した根拠。
- `RunState` は `crates/autoflow/src/condukt.rs:31` 「pub tasks: Vec<TaskState>,」 に `#[serde(default)]` が付くが、
  `TaskState` の `id` / `status` には default が無い。したがって**1 タスクでも `status` を欠くと run 全体が parse 失敗**し、
  空 vec へ潰れる（§4.4 で観測）。

### 1.3 `crates/autoflow/src/lock.rs`

| 関数（行） | 返り値 | 失敗経路（逐語引用） | 判定不能の写り先 | 方向 |
| --- | --- | --- | --- | --- |
| `crates/autoflow/src/lock.rs:43` 「pub fn backlog_driver_active(cwd: &Path) -> bool {」 | `bool` | binary 不在: `crates/autoflow/src/lock.rs:45` 「return false;」 | `false`＝「driver なし」 | A（carve-out。§4.5 の残余あり） |
| 同上 | | spawn 失敗 / 非0終了: `crates/autoflow/src/lock.rs:57` 「_ => true,」 | `true`＝stand down | **R（意図的 fail-closed）** |
| `crates/autoflow/src/lock.rs:74` 「fn driver_active_from_status(stdout: &str) -> bool {」 | `bool` | 解釈不能な stdout: `crates/autoflow/src/lock.rs:80` 「None => true,」 | `true`＝stand down | **R（意図的 fail-closed）** |
| 同上 | | `stale` フィールド欠落: `crates/autoflow/src/lock.rs:81` 「.unwrap_or(false)」 | `!false` = `true`＝stand down | **R（意図的 fail-closed）** |
| `crates/autoflow/src/lock.rs:100` 「pub fn this_session_holds_lock(session_id: &str, cwd: &Path) -> bool {」 | `bool` | session id 空 / binary 不在 / spawn 失敗 / 非0終了: `crates/autoflow/src/lock.rs:115` 「_ => false,」 | `false`＝marker を書かない | R |
| `crates/autoflow/src/lock.rs:119` 「fn holds_lock_from_status(stdout: &str, session_id: &str) -> bool {」 | `bool` | parse 不能 → `crates/autoflow/src/lock.rs:120` 「let Some(v) = parse_status_json(stdout) else {」 | `false` | R |
| 同上 | | stale レコード: `crates/autoflow/src/lock.rs:124` 「if v.get("stale").and_then(」 | `false` | R |
| `crates/autoflow/src/lock.rs:141` 「fn parse_status_json(stdout: &str) -> Option<serde_json::Value> {」 | `Option<Value>` | `none` / 空: `crates/autoflow/src/lock.rs:143` 「trimmed.is_empty()」 | `None` | 呼び出し側で分岐（§3.3） |
| 同上 | | parse 失敗: `crates/autoflow/src/lock.rs:146` 「serde_json::from_str(trimmed).ok()」 | `None` | 呼び出し側で分岐（§3.3） |

## 2. call site の追跡（main.rs / delegation_audit.rs）

`grep -rn <fn> crates/autoflow/src crates/autoflow/tests` による網羅。テスト内の呼び出しは除く。

| 関数 | call site（逐語引用） | Stop hook 分岐でどちらへ効くか | 方向 |
| --- | --- | --- | --- |
| `lock::backlog_driver_active` | `crates/autoflow/src/main.rs:149` 「if lock::backlog_driver_active(&cwd) {」 → `crates/autoflow/src/main.rs:150` 「return;」 | `true` なら Stop 全体を即 return（何も出力しない＝この tick は駆動しない）。`false` なら以降の全分岐へ進む | `true`=R / `false`=A |
| `condukt::find_pending` | `crates/autoflow/src/main.rs:187` 「let pending = condukt::find_pending(&cwd);」 → `crates/autoflow/src/main.rs:188` 「if !pending.is_empty() {」 | 非空なら block（/condukt 継続）。空なら backlog 分岐へ落ちる | 空=B |
| `condukt::mark_running` | `crates/autoflow/src/main.rs:205` 「condukt::mark_running(&cwd, &ids);」 | 失敗しても verdict を変えない。running へ移らなければ次 tick も同じ pending を観測 → `decide_progress` が停滞を検知して**可視の** EscalateStuck へ | R |
| `backlog::find_open`（Stop） | `crates/autoflow/src/main.rs:265` 「let open = backlog::find_open(&cwd);」 → `crates/autoflow/src/main.rs:266` 「if open.is_empty() {」 → `crates/autoflow/src/main.rs:267` 「s.phase = Phase::Done;」 → `crates/autoflow/src/main.rs:268` 「state::save(&cfg.state_dir, &session_id, &s);」 | **空 vec が「もう作業が無い」として `Phase::Done` に写る**。以後このセッションでは `crates/autoflow/src/main.rs:346` 「Phase::Done => {}」 により恒久的に何もしない | 空=**B** |
| `backlog::find_open`（SessionStart） | `crates/autoflow/src/main.rs:425` 「let open = backlog::find_open(&cwd);」 → `crates/autoflow/src/main.rs:426` 「if !open.is_empty() {」 | 空なら /flow の提案を出さずに終了（advisory の非発火） | 空=B（advisory） |
| `condukt::has_completed_tasks_for_run` | `crates/autoflow/src/delegation_audit.rs:192` 「crate::condukt::has_completed_tasks_for_run(cwd, rid)」 → `crates/autoflow/src/delegation_audit.rs:193` 「if !any_completed {」 → `crates/autoflow/src/delegation_audit.rs:194` 「return false;」 | `false` は `crates/autoflow/src/main.rs:173` 「&& delegation_audit::missing_delegation_record(&input.transcript_path, &cwd)」 の右辺を偽にし、Tier 2 advisory の block を起こさない | A（advisory 抑止） |
| `lock::this_session_holds_lock` | `crates/autoflow/src/main.rs:460` 「if !lock::this_session_holds_lock(session_id, cwd) {」 → `crates/autoflow/src/main.rs:461` 「return; // flow loop is not driving THIS session → nothing to resume」 | `false` なら resume marker を書かない。誤って書くと駆動していないセッションへ「継続せよ」を注入するので、`false` が保守側 | R |
| `compass::charter_freshness` | `crates/autoflow/src/main.rs:276` 「if let Some(v) = compass::charter_freshness(&cwd) {」 | `None` は「判定不能→従来どおり進む」。**§5 のとおりスコープ外** | （対象外） |

`find_pending` が空になった後の流れを念のため明示する: `crates/autoflow/src/main.rs:263` 「} else {」 の枝に入り、
そこで `find_open` も空なら `Phase::Done` へ確定する。つまり **condukt 側の判定不能と backlog 側の判定不能が
両方とも「空集合」に写るため、2 つの独立した観測失敗が合流して 1 つの「完了」を作る**。

## 3. 既に fail-closed に設計されている経路（**次タスクはこれを壊してはならない**）

以下は「判定不能を制限側へ倒す」設計が**すでに実装され、docstring とテストで固定されている**。
`unwrap_or(false)` という見た目だけで fail-open と判定すると、意図的な設計を破壊する。

### 3.1 `lock::driver_active_from_status` — 判定不能は `true`（stand down）

根拠の逐語引用:

- `crates/autoflow/src/lock.rs:30` 「**Cannot-determine resolves to `true` (stand down).** The two answers are」
- `crates/autoflow/src/lock.rs:33` 「again next turn). So a `backlog` invocation that fails to run, exits」 に続く `crates/autoflow/src/lock.rs:35` 「may be active".」
- `crates/autoflow/src/lock.rs:63` 「* `none` → nothing is driving. This is the ONLY output that means "free":」
- `crates/autoflow/src/lock.rs:64` 「backlog prints it as a positive observation, never as a fallback.」
- `crates/autoflow/src/lock.rs:70` 「* anything else (empty, non-JSON) → we cannot interpret the answer, so we」
- `crates/autoflow/src/lock.rs:71` 「report active and stand down.」

`crates/autoflow/src/lock.rs:81` 「.unwrap_or(false)」 は **`stale` フィールドの有無**に対する既定値であり、
先頭の `!` によって「`stale` が読めない＝生きている driver」へ倒れる。**これは fail-closed であって fail-open ではない。**
`undetermined` を表す JSON オブジェクトが `stale` を持たないことも意図的で、
`crates/autoflow/src/lock.rs:69` 「`undetermined` object backlog emits when it could not read its registry).」 が根拠。

テストによる固定:

- `crates/autoflow/src/lock.rs:166` 「fn driver_active_from_status_uninterpretable_output_stands_down() {」
- `crates/autoflow/src/lock.rs:169` 「"empty stdout is not an observation of an idle queue"」
- `crates/autoflow/src/lock.rs:173` 「"unparseable stdout is not an observation of an idle queue"」
- `crates/autoflow/src/lock.rs:207` 「fn driver_active_from_status_undetermined_object_is_active() {」

### 3.2 `lock::backlog_driver_active` の subprocess 失敗枝

`crates/autoflow/src/lock.rs:57` 「_ => true,」 は spawn 失敗と非0終了の両方を stand down へ倒す。
根拠コメント: `crates/autoflow/src/lock.rs:55` 「// Spawned but failed, or could not be spawned: we did not get an」 /
`crates/autoflow/src/lock.rs:56` 「// answer. Stand down rather than assume the coast is clear.」

**観測（§4.9 の LOCK-1）**: `backlog lock status` が exit 3 で終わる環境では、run-state に pending タスクが
1 件あっても autoflow は何も出力せず phase も `record_requested` のまま据え置かれた。設計どおり。

### 3.3 `lock::parse_status_json` の `None` は call site 側で曖昧性が解消されている

`parse_status_json` の `None` は「`none`（＝free）」と「解釈不能」の両方を意味するが、
`driver_active_from_status` は **`parse_status_json` を呼ぶ前に** `crates/autoflow/src/lock.rs:76` 「if trimmed == "none" {」 で
free を先に捌く。したがって `crates/autoflow/src/lock.rs:80` 「None => true,」 に到達する `None` は
解釈不能だけである。**`parse_status_json` を「三値化」しようとして呼び出し順を変えると、この分離が壊れる。**
`holds_lock_from_status` 側では同じ `None` が `false`（marker を書かない＝保守側）へ倒れており、
1 つの `Option` が call site ごとに正しい向きへ解決されている。

### 3.4 その他の R 判定

- `crates/autoflow/src/lock.rs:101` 「if session_id.is_empty() {」 → 不明セッションに state を紐付けない。
- `crates/autoflow/src/condukt.rs:153` 「fn save_run(path: &Path, run: &RunState) -> std::io::Result<()> {」 —
  エラーを握り潰さず伝播し、呼び出し側 `crates/autoflow/src/condukt.rs:57` 「if let Err(e) = save_run(&path, &run) {」 が診断を出す。
- `crates/autoflow/src/backlog.rs:119` 「.unwrap_or(root)」 — canonicalize 失敗を**定数へ潰さない**。
  旧実装の `"unknown"` 定数フォールバックが衝突源だったことは
  `crates/autoflow/src/backlog.rs:115` 「failure falls back to the raw absolute path — still unique, never a constant.」 に記録されている。
- （監査対象3ファイル外・参考）`crates/autoflow/src/main.rs:358` 「fn is_autonomous() -> bool {」 の
  `crates/autoflow/src/main.rs:365` 「.unwrap_or(false)」 は、失敗時に「非自律＝ユーザーに確認する」へ倒す
  意図的 fail-safe。しかもこれは **EscalateStuck の文面を選ぶだけ**で block するか否かを変えない。

## 4. 恒久的に permissive 側へ倒れる経路（確定リスト — 次タスクの RED 設計図）

すべて **fault injection を実行して観測済み**（P-7 のみ未観測、その旨を明記）。
観測は `target/debug/autoflow stop` に `{"session_id":…,"cwd":…,"transcript_path":""}` を stdin で与え、
`HOME` を temp dir に、`PATH` を細工した状態で実行して、stdout（block の有無）と
`$HOME/.autoflow/state/<session>.json` の `phase` を読んだもの。

### 4.1 P-1（B）`crates/autoflow/src/backlog.rs:66` 「.unwrap_or_default();」 の parse 失敗が空 vec へ潰れる

- 経路: `crates/autoflow/src/backlog.rs:66` 「serde_json::from_slice(&output.stdout).unwrap_or_default()」
  → `crates/autoflow/src/main.rs:265` 「let open = backlog::find_open(&cwd);」
  → `crates/autoflow/src/main.rs:266` 「if open.is_empty() {」
  → `crates/autoflow/src/main.rs:267` 「s.phase = Phase::Done;」
- fault injection: `backlog` という名前の shell スクリプトを PATH の先頭に置く。
  `lock status` には `none` を、`list` には `not json at all` を出力して **exit 0** で終わる。
- 観測（FAULT D）: stdout `<EMPTY>` / stderr `<empty>` / `phase after: done`。
  **診断が一切出ないまま完了扱いになる。**
- 同 sink の別入力（FAULT E）: `list` が配列ではなく `{"items":[…]}` を返す（schema drift）→ 同じく `done`。
- 同 sink の別入力（FAULT G）: `list` が exit 0 で**何も出力しない** → 同じく `done`。
- 対照（CONTROL）: `list` が正しい 1 件配列を返すと block（「残課題バックログに 1 件の未完了課題があります。」）+ `phase after: continuing`。

### 4.2 P-2（B）`crates/autoflow/src/backlog.rs:73` 「i.status == "pending"」 の client 側フィルタが語彙のずれを空集合へ写す

- 経路: `crates/autoflow/src/backlog.rs:73` 「i.status == "pending"」 → 同上の Done sink。
- fault injection: `list` が exit 0 で `[{"id":"a","title":"T","status":"open"}]` を返す。
- 観測（FAULT I）: `phase after: done`。`status` フィールドを完全に省いた場合（FAULT J）も
  `#[serde(default)]` により `""` となり、同じく `done`。
- 注: backlog CLI の status 語彙は現在 `pending` であり、`open` は存在しない。つまりこれは
  「将来の語彙変更」ではなく「**サーバ側フィルタが効かずクライアント側だけが判定する状況**」を突く RED になる。

### 4.3 P-3（B）非0終了は stderr へ出るが verdict は変わらない

- 経路: `crates/autoflow/src/backlog.rs:58` 「return vec![];」 → 同上の Done sink。
- fault injection: `lock status` は `none` で exit 0、`list` は正しい 1 件配列を出しつつ **exit 3**。
- 観測（FAULT F）: stderr に `autoflow: backlog list exited exit status: 3:` が出た**が**、
  stdout は `<EMPTY>`、`phase after: done`。
  つまり `crates/autoflow/src/backlog.rs:50` 「// Non-zero exit: surface it rather than silently reporting "no work"」 は
  **stderr について真**であり、**Stop の verdict については依然「no work」**である。
  次タスクはこの区別を壊さないこと（コメントは嘘を書いていない。verdict が足りないだけである）。

### 4.4 P-4（B）condukt run-state の read/parse 失敗が空 vec へ潰れる

- 経路: `crates/autoflow/src/condukt.rs:145` 「serde_json::from_str::<RunState>(&t).ok()」
  → `crates/autoflow/src/condukt.rs:41` 「None => return vec![],」
  → `crates/autoflow/src/main.rs:187` 「let pending = condukt::find_pending(&cwd);」
  → `crates/autoflow/src/main.rs:188` 「if !pending.is_empty() {」 が偽 → backlog 分岐 → `Phase::Done`
- fault injection（`$HOME/.condukt/state/<project_key>/run-*.json` を壊す。`backlog` は PATH に無い）:
  - FAULT A: 内容を `{ this is not json ` にする → 観測 `phase after: done`、stderr 空。
  - FAULT B: **valid JSON だが 1 タスクだけ `status` を欠く** → 観測 `phase after: done`。
    健全なタスクを含む run 全体が不可視になる点が重要。
  - FAULT C: 末尾 8 バイトを切り落とす（書き込み中断の模擬）→ 観測 `phase after: done`。
  - CONTROL: 正しい run-state（pending 1 件）→ block（「condukt に残課題が 1 件あります:」）+ `continuing`。

### 4.5 P-5（A）`crates/autoflow/src/backlog.rs:96` 「.ok()?」 の read_dir 失敗が未インストールと同一の `None` へ潰れる

**これが唯一の permissive-A（二重駆動）方向の未文書経路である。**

- 経路: `crates/autoflow/src/backlog.rs:96` 「.ok()?」 → `None`
  → `crates/autoflow/src/lock.rs:44` 「let Some(binary) = find_backlog_binary() else {」
  → `crates/autoflow/src/lock.rs:45` 「return false;」
  → `crates/autoflow/src/main.rs:149` 「if lock::backlog_driver_active(&cwd) {」 を素通り
  → `crates/autoflow/src/main.rs:187` 「let pending = condukt::find_pending(&cwd);」 以降で /condukt を自動駆動。
- docstring の carve-out はこの経路を**カバーしていない**。根拠:
  `crates/autoflow/src/lock.rs:37` 「The one deliberate exception is `backlog` not being installed at all. That」 /
  `crates/autoflow/src/lock.rs:38` 「is an observation, not a failure to observe: with no `backlog` binary there」。
  つまり免責の前提は「binary が存在しない」という**観測**だが、`.ok()?` は
  「dir を列挙できなかった」という**観測の失敗**を同じ `None` に写す。
- fault injection（対照つき。PATH からは `backlog` を外す）:
  1. `$HOME/.claude/plugins/cache/yukineko/backlog/0.1.0/bin/backlog` に、
     `lock status` で `{"kind":"driver-presence",…,"drivers":[{"session_id":"other-live-session"}]}` を返す
     実行可能スクリプトを置く（＝**別セッションが実際に駆動中**の状態）。
  2. run-state には pending タスクを 1 件置く。
  3. CONTROL: cache dir を mode 755 のままにする → 観測: **stdout `<EMPTY>` / `phase after: record_requested`**（正しく stand down）。
  4. FAULT H: cache dir を **mode 111（実行可・読み取り不可）** にする。
     この状態でも cached binary は起動でき（実測で `lock status` が driver-presence を返すことを確認）、
     `os.listdir` だけが `Permission denied` になる。
     → 観測: **block を出力**（「condukt に残課題が 1 件あります:」）+ `phase after: continuing`。
- 意味: **同一マシン・同一の live driver・同一の実行可能な backlog binary で、
  ディレクトリの読み取りビットだけが違うと、stand down と二重駆動に判定が割れる。**

### 4.6 P-6（B）condukt run-state ディレクトリの列挙失敗が「run file 無し」と同一の `None` へ潰れる

- 経路: `crates/autoflow/src/condukt.rs:172` 「.ok()?」 → `crates/autoflow/src/condukt.rs:142` 「let path = latest_run_file(&project_dir)?;」 → P-4 と同じ Done sink。
- fault injection: run-state dir（`$HOME/.condukt/state/<project_key>/`）を **mode 111** にする。
  ファイル自体は健在で pending タスクを 1 件持つ。
- 観測（FAULT K）: `phase after: done`、stderr 空。CONTROL（mode 755）は block + `continuing`。

### 4.7 P-7（B・**未観測**）`now_secs()` の `0` フォールバック

- 経路: `crates/autoflow/src/condukt.rs:166` 「.unwrap_or_default()」 → `age` が負 →
  `crates/autoflow/src/condukt.rs:49` 「if age > STUCK_SECS {」 が常に偽 → 中断された running タスクが復帰しない。
- 到達には system clock を 1970 以前へ戻す必要があり、**この監査では観測していない**。
  RED を書くなら `now_secs` を注入可能にする（時刻源を引数化する）必要があり、それは実装変更を伴う。
  他 6 件より優先度は低い。

### 4.8 文書化済みだが permissive 方向の経路（**修正前に人間の判断を要する**）

以下は「意図的 fail-soft」と docstring に明記されているため、§4.1〜4.7 と**同列に扱わない**。
RED を書く前に、契約を変えるべきか否かの判断が要る。

- `crates/autoflow/src/condukt.rs:88` 「would have written. Fail-soft: an unreadable/unparseable/missing run file」 /
  `crates/autoflow/src/condukt.rs:89` 「returns `false`.」 と
  `crates/autoflow/src/delegation_audit.rs:163` 「unmet (fail soft: undetermined never fires this advisory).」。
  効果は Tier 2 advisory の非発火のみで、ループの継続/完了判定は動かさない。
- `crates/autoflow/src/backlog.rs:26` 「None => return vec![],」（backlog 未インストール）。
  §4.5 と違い、こちらは「queue が存在しない」という観測に基づく carve-out として一貫している。

### 4.9 再現手順（共通）

1. `. "$HOME/.cargo/env" && cargo build -p autoflow`（`target/debug/autoflow` を得る）。
2. temp dir を `HOME` にし、`$HOME/repo/.git/` を作る。
3. `$HOME/.condukt/state/<project_key>/run-20260101-000000-1.json` を置く。
   `<project_key>` は `<sanitized basename>-<fnv1a32 hex of canonical root>`
   （`crates/harness-core/src/projkey.rs:24` 「pub fn project_key(root: &Path) -> String {」）。
4. `$HOME/.autoflow/state/<session>.json` に `{"phase":"record_requested"}` を書く
   （Stop の状態機械を継続分岐へ入れるため）。
5. `PATH` を `/usr/bin:/bin`（＋必要ならフェイク `backlog` を置いた dir）に絞って
   `echo '<payload>' | target/debug/autoflow stop` 相当を実行する。
6. stdout の block 有無と、手順 4 のファイルの `phase` を読む。

## 5. スコープ外: `crates/autoflow/src/compass.rs` の `charter_freshness`

`crates/autoflow/src/compass.rs:45` 「pub fn charter_freshness(cwd: &Path) -> Option<Verdict> {」 の
`None`（compass 不在・エラー・解釈不能・timeout）は「判定不能→呼び出し側は従来どおり進む」へ写るが、
これは **docstring で fail-soft 契約が明示されている**ため本監査のスコープ外とし、**変更提案を含めない**。

根拠の逐語引用:

- `crates/autoflow/src/compass.rs:11` 「return `None`, and the caller preserves today's behavior — a repo that」
- `crates/autoflow/src/compass.rs:12` 「doesn't use compass keeps auto-driving as before. This module only READS」
- `crates/autoflow/src/compass.rs:42` 「`None` means "can't tell" — compass absent, errored, or emitted unparseable」
- `crates/autoflow/src/compass.rs:43` 「output — and the caller should preserve its prior behavior (proceed). A」

call site（`crates/autoflow/src/main.rs:276` 「if let Some(v) = compass::charter_freshness(&cwd) {」）も
この契約どおり `Some` のときだけ stand down する。**次タスクはここに手を入れないこと。**

## 6. 次タスクへの引き継ぎ

- **最も再現しやすい RED**: §4.4 の FAULT B（valid JSON だが 1 タスクが `status` を欠く run-state）。
  外部バイナリのフェイクが不要で、temp `HOME` とファイル 1 本だけで `Phase::Done` を観測できる。
- **最も影響が大きい欠陥**: §4.5 の P-5（唯一の permissive-A。二重駆動を実際に起こす）。
- **壊してはならないもの**: §3 の 10 経路、特に `driver_active_from_status` の
  `crates/autoflow/src/lock.rs:80` 「None => true,」 と `crates/autoflow/src/lock.rs:81` 「.unwrap_or(false)」。
  これらは判定不能を stand down へ倒す**意図的 fail-closed**であり、既存テスト 4 本が固定している。
