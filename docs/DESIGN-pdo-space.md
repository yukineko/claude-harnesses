# DESIGN: PDO Space — hypothesis / violations / lessons を git-native にチーム共有する

> ステータス: **保留（優先度を下げて凍結）**。実データ確認の結果、この repo は現時点で単一開発者運用
> （コミット著者は全員同一人物の表記ゆれ + bot のみ、hypothesis 台帳は作成直後の1件のみ）であり、
> チーム共有インフラより先に**個人運用でのセッション健全性**（記憶の持続・スコープ逸脱・並行セッションの
> 分離）を手当てする方が優先度が高いと判断した。本ドキュメントは将来チーム運用が実際に必要になった
> 時点の参照用に残し、実装は着手しない。個人運用側の再設計は
> [DESIGN-pdo-session-anchor.md](DESIGN-pdo-session-anchor.md) を見よ。

**PDO（仮説駆動の並列開発）の台帳・実行レジストリ・学習ループが、いずれも `~` 配下のマシンローカルな
store に閉じているため、複数人・複数マシンでの自律運転では一切共有されない。この仕様は、既存の
ローカル即応レイヤーはそのまま残し、その上に git-native な**チーム共有レイヤー（Space）**を追加する。**

> **v2 での方針転換**: 当初案は「`.pdo/` を commit/pull する」ことをチーム共有の唯一の手段としていたが、
> それだと**読み取り側も書き込み側も随時 commit・随時 pull が事実上の前提**になり、通常の git 運用
> リズム（機能単位でまとめて commit、ブランチ作業中は数時間 pull しない）と噛み合わないという指摘を
> 受けた。v2 は**読み取りと書き込みを非対称**にする：読み取りは working tree を汚さない軽量
> `git fetch` で常に最新を見に行き（自動・確認不要）、書き込みは今まで通り人間の通常の commit/push
> リズム（`ship` の出荷儀式）に委ねる（自動化しない）。詳細は §4。

---

## 1. 動機

### 1.1 参照した外部知見

[AI-DLC Workflows 2.0](https://zenn.dev/aws_japan/articles/aidlc-workflows-v2-harness-engineering) は
**Intent**（1 回の作業単位。専用ディレクトリに記録が集約される）と **Space**（チーム世界観。複数
Intent が並行実行されつつ、チーム学習だけは横断的に効く）を分離する。Intent はセッション・ラン単位の
使い捨て状態、Space は git などの共有バックエンドに永続化されたチーム資産、という住み分けである。

### 1.2 この harness の現状（証拠）

この harness には PDO（[hypothesis](../crates/hypothesis)）と実行レジストリ
（[overwatch](../crates/overwatch)）という、AI-DLC の Intent/Space に相当する概念が**既にある**。
しかし実体を調べると、「チーム全体」を謳いながら実装はマシンローカルに閉じている：

| クレート | 自称 | 実際の保存先 | git 追跡 | 複数マシン間で共有されるか |
|---|---|---|---|---|
| `compass` | プロジェクトのゴール | `.compass/charter.md`（リポ同居） | される | **される** |
| `hypothesis` | PDO 仮説台帳 | `~/.hypothesis/hypotheses.toml` | されない | **されない** |
| `backlog` | 確定タスクキュー | `~/.backlog/` | されない | **されない** |
| `condukt` | run 状態・cross-task lessons | `~/.condukt/` | されない | **されない** |
| `overwatch` | 「プロジェクト全体の実行レジストリ」 | `~/.local/share/claude-harnesses/<project-key>/overwatch/` | されない | **されない** |
| `harness-core::lessons` | cross-project lesson | `~/.lessons/lessons.jsonl`（意図的にマシンスコープ・project-independent） | されない | **されない**（意図的） |

compass の charter だけが唯一 git 経由でチーム共有される。他はすべて `harness_core::config::base_dir()`
（`~/.<plugin>/`）または同等のホームディレクトリ配下に置かれる。

これは実装ミスではなく、**単一開発者が同一マシン上で複数 Claude Code セッションを並行させる**ケース
（overwatch の dedup/lease、flow の backlog ロック直列化）を主眼に設計されているためで、その用途には
正しい。しかし、次の2点が「チームで PDO を自律運転する」という目的とは食い違う：

- **hypothesis が検証済み／棄却済みにした賭けを、別の開発者（別マシン）の Claude Code が知らずに
  再度同じ賭けを積む。** build ≠ validate の規律はマシン内でしか効かない。
- **overwatch の fleet 相関エラー検知（`violations`/`escalations`）は「複数 task or 複数 session に
  またがる再発」を systemic の条件にしているが、session はすべて同一マシン上でしか観測されない。**
  同じ blastguard 拒否・propguard 失敗パターンがチームの複数人に同時多発していても、各人のマシンでは
  「単発」にしか見えず、systemic 判定のロジック自体は正しいのに**入力が届かない**ため機能しない。

### 1.3 なぜ今のクレート設計を変えずに直せるか

overwatch の `violations.jsonl` は元から **append-only・正規化 signature 付き** で、集計関数
（`detect_recurrence`/`systemic_issues`）は「時刻を引数で受け取る純関数」「順序に依存しない」設計に
なっている（[overwatch 仕様](specs/overwatch.md) 参照）。つまり **保存先を git 追跡下のパスへ変える
だけ**で、fleet 相関検知はコード変更ゼロのままチーム規模で機能するようになる。これは本仕様の中で
最もレバレッジが高い変更である。

---

## 2. 既存 harness との棲み分け（再発明しない）

| 問い | 担当 |
|---|---|
| ゴールは何か（team-shared, 既存） | `compass`（`.compass/charter.md`） |
| 誰が今このキーを保持しているか（real-time, マシンローカルのまま） | `overwatch` lease/heartbeat |
| **チームは何を検証済み／棄却済みと知っているか（team-shared, 新規）** | **PDO Space（本仕様）** |
| **チームはどの失敗パターンを繰り返しているか（team-shared, 新規）** | **PDO Space（本仕様）** |
| 個々のマシンでの cross-project な教訓（意図的にマシンローカル、現状維持） | `harness-core::lessons` |

本仕様は新しいクレートを作らない。既存の hypothesis / overwatch / condukt / harness-core に
**保存先の追加（オプトイン）** という形で乗せる。

---

## 3. 設計原則

1. **加算的（additive）・オプトイン** — 既定は現状維持（ローカルのみ）。`.pdo/` への書き出しは
   config で明示的に有効化するまで発火しない。既存インストールを壊さない。
2. **live coordination はローカルのまま** — overwatch の lease/heartbeat（TTL 1800 秒の
   liveness 判定）は git に向かない（git はリアルタイム同期の道具ではない）。git 化するのは
   **低頻度・追記専用・チーム学習として価値が持続するデータ**（仮説の遷移、violation、lesson）
   に限る。「今誰が何を握っているか」の live 判定は今まで通りマシンローカル。
3. **フォーマットは append-only JSONL を既定にする** — TOML/JSON スナップショット全体書き換えは
   git マージで衝突しやすい。追記専用の1行1イベントは、3-way merge が素直に union できる上、
   既存の集計関数群（`overwatch::violation`、`harness-core::lessons`）が「順序に依存しない
   集合演算」として既に書かれているため、マージ順の乱れが結果に影響しない。
4. **write は誰も自動 commit/push しない** — バイナリは作業ツリーの `.pdo/*` ファイルを書くだけ
   （`.compass/charter.md` が今まで通りそうしているのと同じ扱い）。commit/push は既存の
   `ship` プラグインの出荷儀式・人間の通常の git ワークフローに委ねる。新しい自動化は追加しない。
5. **read は working tree を変更しない `git fetch` で自動化してよい** — 「読む」ことは非破壊・
   ローカルブランチ非変更・確認不要な操作（このハーネスの安全分類でいう "Regular"）なので、
   SessionStart のたびに軽量 `git fetch` + 該当 path の読み取りを自動実行してよい。これは
   「commit/push を自動化しない」（原則4）とは非対称であり、意図的である（§4）。
6. **fail-soft** — `.pdo/` が無い・書けない・パース不能でも、既存のローカル store への読み書きは
   従来通り機能する（劣化するのは「チーム共有」の部分だけ）。オフライン・fetch 失敗時も同様に
   ローカルのみへ黙って縮退する。
7. **同一マシン上の並行書き込みは既存の advisory lock を再利用する** — `harness-core::lessons`
   の `acquire_lock`/`LockGuard`（create_new によるアトミックなロックファイル、RAII 解放）と
   同じプリミティブを使う。新しいロック機構を作らない。

---

## 4. アーキテクチャ

読み取りパス（自動・非破壊）と書き込みパス（人間ゲート・低頻度）を非対称に設計する。

```
 ローカル即応レイヤー（現状維持）                  リモート（origin）
 ~/.hypothesis/  ~/.condukt/                       origin/<team.remote_ref>
 ~/.local/share/claude-harnesses/…                 └─ <repo>/.pdo/
 （list / rat / autonomy-check 等の                      hypotheses.jsonl
  CLI はこちらを読む。今まで通り）                        violations.jsonl
        │                                                 lessons.jsonl
        │ 1) 追記（オプトイン、write）
        ▼                                                      ▲
 working tree の <repo>/.pdo/*.jsonl  ── 3) 通常の commit/push ─┘
  （即座には push しない。ship の         （人間の判断・既存の出荷儀式のまま。
   出荷儀式・通常のブランチ運用で           本仕様は新しい自動化を足さない）
   いずれ mainline に乗る）

 SessionStart フック（read）
   2) 軽量 `git fetch origin <team.remote_ref>` のみ実行（working tree・現ブランチは無変更）
      → `git show origin/<team.remote_ref>:.pdo/<file>` でリモート最新の内容だけを読む
      → ローカル working tree の .pdo/*（自分がまだ push していない分）と union して描画
      → fetch 失敗・オフライン・git 未初期化のときは黙ってローカルのみにフォールバック（fail-soft）
```

- **書き込み（1→3）**: 各バイナリの既存コマンド（`hypothesis validate`、`overwatch
  record-violation`、`stuckguard` エスカレーション経由の lesson 記録）が、ローカル store への
  書き込みに加えて working tree の `.pdo/*.jsonl` にも同じイベントを追記する（config で有効時のみ）。
  そこから mainline へ乗るのは、今まで通り人間が行う commit/push（`ship` の出荷儀式）のタイミング。
  **本仕様はここを自動化しない**（§8）。
- **読み取り（2）**: SessionStart 系フック（`hypothesis session-start`、`overwatch status`、
  `condukt replan handoff`）は、まず `git fetch origin <team.remote_ref>` を実行し（working tree
  を一切変更しない読み取り専用のネットワーク操作。ブランチの merge・checkout は行わない）、
  `git show origin/<ref>:.pdo/<file>` でリモートの最新版を直接読む。これを **working tree に既にある
  自分の `.pdo/*`（まだ push していない自分の直近の学び）と союз（union）して描画する**。
  結果として、**開発者は自分のブランチを pull しなくても、チームが mainline に push 済みの学びを
  毎セッション自動的に受け取る**。fetch が失敗（オフライン・リポジトリでない・remote 未設定）しても
  ローカル store のみで従来通り動作する（fail-soft）。
- **fetch 頻度の制御**: `git fetch` はネットワーク往復を伴うため、SessionStart のたびに毎回叩くのではなく
  `team.fetch_ttl_secs`（既定 300 秒、overwatch の `LEASE_TTL_SECS` と同じ「TTL キャッシュ」の考え方）
  内は前回 fetch 結果をキャッシュして再利用する。

---

## 5. クレート別の変更内容

### 5.1 hypothesis — イベント追記を team ledger にも書く

- **新規**: `store::append_event(&Event)` を追加し、`add` / `validate` / `reject` /
  `await-measurement` / `assume` / `tested` / `confidence` の各遷移で、既存の TOML スナップショット
  更新に加えて `Event { id, kind, hypothesis_id, payload, session_id, ts }` を
  `.pdo/hypotheses.jsonl`（config で有効時）に追記する。フォーマットは
  `harness-core::lessons::Lesson` と同じ「1行1 JSON、idempotent-by-id ではなく append-only
  event log」。
- **新規**: `hypothesis session-start` は、ローカル TOML の `open`/`awaiting-measurement` に加えて
  ①working tree の `.pdo/hypotheses.jsonl`（自分がまだ push していない分）と
  ②`git fetch` 経由でリモートから読んだ `.pdo/hypotheses.jsonl`（§4）を union して replay し、
  「他マシンで既に `validated`/`rejected` になっている同一テキストの仮説」があれば
  `[team: already <status> on <run>]` として警告注入する（テキストの決定論的 ID は既存の
  `hypothesis::new_id`（FNV-1a）をそのまま流用でき、同一文言は同一 ID になるため突合は自明）。
  これは `git pull` していなくても、直近 `team.fetch_ttl_secs` 以内の fetch キャッシュから得られる。
- **既存の `hypotheses.toml` は変更しない** — ローカルの高速パスとして維持し、後方互換を保つ。

### 5.2 overwatch — violations.jsonl の保存先をリポジトリ配下へ（最小変更・最大効果）

- `record-violation` の書き込み先を、config で `team_violations = true`（既定 false）のとき
  `<repo>/.pdo/violations.jsonl` にも複製する。**`normalize_signature`・`detect_recurrence`・
  `systemic_issues` は一切変更しない**（既に純関数・順序非依存）。
- `violations`/`escalations` サブコマンドは、ローカル `violations.jsonl` と working tree の
  `.pdo/violations.jsonl` に加え、`git fetch` 経由でリモートから読んだ `.pdo/violations.jsonl`
  （§4、`team.fetch_ttl_secs` でキャッシュ）の三者の和集合を同じ集計関数に渡す。
  `distinct_sessions`/`distinct_tasks` の算出はイベント由来の `session_id`/`task` フィールドを
  そのまま使うため、**他マシンの session_id が混ざることで systemic 判定の分母が自然にチーム規模へ
  広がる**（コード変更なし、入力ソースが fetch 経由で増えるだけ。呼び出し側は pull 不要）。
- lease/heartbeat（`begin`/`heartbeat`/`reap`、`LEASE_TTL_SECS`）は変更しない（3.2 節参照）。

### 5.3 condukt — cross-task lessons に project-scoped overlay を足す

- `harness-core::lessons` は「マシンスコープ・project-independent」という現行の意図的設計を
  **変更しない**（単一開発者が複数プロジェクトを跨いで学ぶ用途は残す）。
- 追加で `harness-core::lessons::project_store_path()`（例: `<repo>/.pdo/lessons.jsonl`）を新設し、
  `stuckguard` のエスカレーション時に**両方**へ追記する（グローバル store は今まで通り、project
  store は config で有効時のみ）。
- `condukt replan handoff` の検索は、グローバル store ∪ working tree の project store ∪
  `git fetch` 経由でリモートから読んだ project store（§4）の和集合に対して既存の
  `search`/`search_default`（lexical Jaccard）をそのまま適用する。ロジック変更なし、入力ソース追加
  のみ。

### 5.4 harness-core — 共有プリミティブ

- 新モジュール `harness_core::team_store`（仮称）に、`.pdo/` 配下の JSONL への
  「repo-root からの相対パス解決」「advisory lock を使った追記」「fail-soft load」を集約する。
  `lessons.rs` の `acquire_lock`/`append_at`/`load_at` パターンをジェネリック化して再利用し、
  hypothesis/overwatch/condukt が個別に実装しない。
- repo-root の解決は `git rev-parse --show-toplevel` 相当（既存クレートが git 情報を読む箇所が
  あればそれに合わせる。無ければ cwd から `.git` を上方向探索）。git リポジトリでない場合は
  `.pdo/` 書き込みを黙ってスキップ（fail-soft）。
- **新規**: `harness_core::team_store::fetch_remote(remote_ref: &str, ttl_secs: u64) ->
  Option<TeamSnapshot>` を追加する。内部で `git fetch --quiet <remote> <branch>` を子プロセスとして
  実行し、成功したら `.pdo/hypotheses.jsonl`/`violations.jsonl`/`lessons.jsonl` それぞれを
  `git show <remote>/<branch>:.pdo/<file>` で読んで `TeamSnapshot` にまとめる。working tree・
  `HEAD`・現在のブランチには一切触れない（`checkout`/`merge`/`pull` を呼ばない）。結果は
  `<base_dir>/team-fetch-cache.json`（既存 store と同じ home-dir 配下）に `fetched_at` 付きで
  キャッシュし、`ttl_secs` 以内の再呼び出しはネットワークを叩かずキャッシュを返す（overwatch の
  `LEASE_TTL_SECS` と同じ TTL キャッシュの考え方）。`git` バイナリが無い・fetch がタイムアウト・
  remote 未設定・オフラインなど、あらゆる失敗は `None`（fail-soft）で、呼び出し側はローカル store
  のみで従来通り動作する。

---

## 6. 設定（新規、すべて既定 false = 現状維持）

各クレートの既存 `config.toml` に追記する形にする（新しい config ファイルは作らない）。

```toml
# ~/.hypothesis/config.toml に追記
[team]
enabled = false   # true で .pdo/hypotheses.jsonl への複製書き込み・fetch 読み込みマージを有効化

# ~/.overwatch/config.toml 相当（overwatch.toml）に追記
[team]
violations_enabled = false   # true で .pdo/violations.jsonl への複製書き込みを有効化

# ~/.condukt/config.toml に追記
[team]
lessons_enabled = false   # true で .pdo/lessons.jsonl への複製書き込み・検索マージを有効化

# 3クレート共通（harness-core::team_store が読む。個別 config.toml のどれかに [team.fetch] として
# 置くか、専用の ~/.harness/team.toml に集約するかは実装時に決める。§13 オープン論点）
[team.fetch]
remote      = "origin"   # fetch 先リモート名
branch      = "main"     # 読み取り対象ブランチ（team ledger が収束する mainline）
ttl_secs    = 300        # このTTL以内は fetch を再実行せずキャッシュを使う
```

`.pdo/` を git 管理するかどうかはプロジェクト側の判断（`.gitignore` に入れれば「ローカルのみ拡張」
としても使える＝オプトインの意味がここでも効く）。`[team.fetch]` は read 専用の設定であり、write
（`[team].enabled` 等）とは独立に有効化できる — たとえば「チームの学びは受け取りたいが自分のは
まだ共有したくない」という非対称な運用も可能。

---

## 7. マージ衝突耐性

§4 の非対称設計により、衝突が問題になりうるのは**書き込み側（自分の working tree の `.pdo/*` を
mainline へ commit/push する瞬間）だけ**である。読み取り側（`git fetch` + `git show`）はブランチを
一切 merge しないため、そもそも衝突が発生しない。

- **append-only 前提が壊れない限り、書き込み側の git 3-way merge は自動解決する。** 双方が末尾に
  別々の行を足すだけの diff は、行単位の merge で衝突しない。
- **衝突する唯一のケース**は、リベースせず同じ物理行位置を編集したような非標準操作（通常は起きない、
  追記のみのファイルなので発生条件がそもそも狭い）。発生したら通常の git conflict marker 解決に委ねる
  （新しい自動解決ロジックは持たない — HOTL でよい）。
- **重複行**（同じイベントが2回書き込まれる、または fetch キャッシュと working tree の union で
  同一イベントが二重に見えるなど）は許容する。読み込み側の集計関数はすべて idempotent-by-id
  （hypothesis の `new_id`）または再発検知が signature ベースの集合演算（overwatch の
  `normalize_signature`）なので、重複があっても結果は変わらない。

---

## 8. 非目標（本仕様に含めない）

- **overwatch の lease/heartbeat を分散システム化すること。** 複数マシン間のリアルタイム排他制御は
  git では実現できない。本仕様は「チームの誰が何を学んだか」の共有に限定し、「今この瞬間誰が
  何を握っているか」の live coordination はマシンローカルのまま維持する。
- **hypothesis/backlog/condukt run state のフル git 化。** run state・worktree・backlog キューの
  ような高頻度書き込みの状態は本仕様の対象外（merge 衝突コストが便益を上回る）。
- **Intent ID の統一（hypothesis id / backlog item id / condukt run id / overwatch lease key を
  1本の ID に紐付け直すこと）。** 価値はあるが本仕様とは直交する別提案とする（将来課題として
  ここに記録するのみ）。
- **review-worthiness スコアに応じたゲートの動的スキップ（scope-adaptive gating）。** 別途検討中の
  提案であり、本仕様のスコープには含めない。
- **`.pdo/` への自動 commit/push。** 明示的に非目標（3.4 節、原則4）。read 側の `git fetch` は
  working tree・現在のブランチ・`HEAD` を一切変更しないため対象外（原則5）——「fetch は自動化するが
  pull（merge を伴う取り込み）は自動化しない」という区別を本仕様全体で一貫させる。
- **working tree のブランチを自動で pull/merge/rebase すること。** 読み取りは常に `git show
  <remote>/<ref>:<path>` によるリモート参照の直接読みで完結させ、ユーザーのローカルブランチには
  触れない。

---

## 9. 用語の修正（ついでに直す）

`docs/GLOSSARY.md` は PDO を「Parallel Development Orchestration」と定義するが、
`crates/overwatch/README.ja.md` は「PDO（Pending Data Object）」と別の展開を書いている。
本仕様の実装と合わせて、後者を GLOSSARY.md の定義に統一する（overwatch は PDO の
aggregator であって PDO そのものの別定義ではない）。

---

## 10. 段階的ロールアウト

0. **Phase 0 — harness-core::team_store の fetch/write プリミティブ**: `fetch_remote`（§5.4）と
   advisory-lock 付き追記の共通実装のみを先に作る。他クレートはまだ呼ばない。単体テストで
   「fetch はブランチ/HEAD を変更しない」「TTL キャッシュが効く」「git 不在/オフラインで `None`」を
   担保してから Phase 1 以降が乗る。
1. **Phase 1 — overwatch violations**（最小変更・最大効果）: `.pdo/violations.jsonl` への複製書き込み
   と fetch 読み込みマージのみ実装。core ロジック不変なので最もリスクが低い。
2. **Phase 2 — hypothesis**: イベントログ追記 + SessionStart の team-aware 警告（fetch 経由）。
3. **Phase 3 — condukt lessons project overlay**: stuckguard エスカレーション時の複製書き込み +
   replan handoff の検索ソース追加（fetch 経由）。
4. 各 phase 独立に PR 化し、`--canary` 不要（GATE_CRATES ではない）だが通常の
   `check-plugin-versions.py` / `check-version-bumped.py` は適用する。

---

## 11. 受け入れ基準（done_criteria）

- [ ] `.pdo/` 未設定（既定）のとき、3クレートいずれも既存の振る舞い・出力・テストが**一切変わらない**
      （回帰テストで担保）。
- [ ] `team.enabled=true` の hypothesis で、あるマシンが `validate` して push 済みの仮説を、
      **`git pull`/`merge` を一度も実行していない**別ワークツリーの `hypothesis session-start` が
      `git fetch` 経由だけで `[team: already validated]` として検出する（統合テストで2つの独立した
      store dir + bare remote をシミュレートして再現。**pull していないことをテストで明示的に
      確認する**のが本仕様のコア主張の検証になる）。
- [ ] 上記シナリオで、fetch 後もローカルの現在ブランチ・`HEAD`・working tree のコード側ファイルは
      一切変更されていない（`git status`/`git rev-parse HEAD` が fetch 前後で不変）。
- [ ] `team.fetch.ttl_secs` 内に `hypothesis session-start` を複数回呼んでも、`git fetch` の実プロセス
      起動は1回だけ（TTL キャッシュのヒットをモック/カウンタで確認）。
- [ ] `team.violations_enabled=true` の overwatch で、2つの異なる `session_id`/`task` から
      同一 signature の violation が `.pdo/violations.jsonl`（一方は fetch 経由のリモート、他方は
      working tree）に記録されたとき、`overwatch escalations` がそれを systemic と判定する
      （`distinct_sessions > 1` 経路）。
- [ ] `.pdo/` 配下の JSONL に手で不正行・空行・重複 id 行を混ぜても、読み込み側は panic せず
      fail-soft に振る舞う（既存 `lessons.rs` のテストパターンを踏襲）。
- [ ] git リポジトリでない cwd（`.git` が見つからない）・remote 未設定・`git` バイナリ不在・
      fetch タイムアウトのいずれでも、`team.*_enabled=true` にしても書き込み/fetch は黙って
      スキップされ turn を壊さない（`fetch_remote` が `None` を返す全経路を個別にテスト）。
- [ ] 変更した各クレート（hypothesis / overwatch / condukt / harness-core）の version を
      `Cargo.toml` / `plugin.json` / `marketplace.json` で lockstep に bump し、
      `check-plugin-versions.py` と `check-version-bumped.py` を green にする。
- [ ] `docs/GLOSSARY.md` と `crates/overwatch/README.ja.md` の PDO 展開表記を統一する。

---

## 12. リスク・トレードオフ

- **「チームで揉まれた `.pdo/*.jsonl` がリポジトリの diff ノイズになる」** — 頻度は「仮説の状態遷移」
  「fleet 違反」「stuckguard エスカレーション」程度で、通常のコード変更ほど高頻度ではないと想定するが、
  実運用で無視できないノイズになるようなら `.pdo/` を `.gitignore` して「チーム共有オフのローカル
  拡張」に倒せる（オプトインの逃げ道として機能する）。
- **「検証済みと表示された仮説が実は別ブランチの実験で、まだ自分のブランチには関係ない」** —
  team-aware 警告は advisory（ブロックしない）。hypothesis の struct 自体を書き換えるのではなく
  session-start の注入テキストに留めるため、誤検出の実害は「余計な一行の注意書き」に限定される。
- **「fleet 相関検知の閾値（既定 3件/24h）がチーム規模で緩すぎる/厳しすぎる」** — 既存の
  `RecurrencePolicy { threshold, window_secs }` は CLI フラグで調整可能なため、team 化後の運用で
  再チューニングが必要になる可能性がある（実装後の観測課題として明記するのみで、本仕様では既定値を
  変えない）。

---

## 13. オープン論点（実装前に決めること）

1. `.pdo/` の repo-root 解決は `git` コマンド呼び出しに依存してよいか、それとも純 Rust
   （`.git` ディレクトリの上方向探索）に留めるべきか（既存クレートの git 依存方針に合わせる）。
   `fetch_remote`（§5.4）は `git show`/`git fetch` の子プロセス起動が前提になるため、既に
   `git` バイナリへの依存は避けられない——この論点は repo-root 解決を同じ前提に乗せてよいかの確認。
2. team 警告の inject 文字数上限（既存の `inject_limit` 予算を食い合わないか）。
3. `.pdo/` を新規リポジトリで生成するタイミング（初回書き込み時に `mkdir -p` するだけで良いか、
   `hypothesis install`/`overwatch install` 相当のセットアップコマンドを設けるか）。
4. `team.fetch.branch` の既定値をどう決めるか（`main` 固定か、`git symbolic-ref
   refs/remotes/origin/HEAD` 等でリポジトリのデフォルトブランチを自動検出するか）。ユーザーが
   `main`/`master`/`trunk` などブランチ名の違うプロジェクトを跨いで使うことを考えると自動検出が
   望ましいが、実装コストとのトレードオフ。
5. `git fetch` のネットワーク呼び出しが SessionStart のレイテンシに与える影響をどこまで許容するか
   （TTL 300 秒は仮の既定値。`stop-gate-latency.md` に倣った実測での調整が必要）。
6. fetch 失敗（オフライン等）を毎回無音でフォールバックするか、頻度を抑えて一度だけ
   `additionalContext` に「team fetch 失敗中」と出すか（`daily` クレートの once-per-day 抑制
   パターンが流用できる）。
