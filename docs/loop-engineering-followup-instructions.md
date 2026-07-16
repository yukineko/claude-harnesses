# 指示文書: loop engineering評価で見つかった2つの未解消ギャップを閉じる

`docs/loop-engineering-evaluation.ja.md`の「気になったギャップ」と、`docs/design-delegation-strategy-measurement.md`の「レビュー所見」で**存在は指摘済みだが未実装のまま残っている**2件を対象にした、次に着手するClaude Codeセッションへの実行指示。両方とも「loopが本当に閉じているか」（＝人間の手打ち・記憶に依存する箇所が残っていないか）という同じ観点のギャップであり、対症療法ではなく決定論的な仕組みで塞ぐことを目的とする。

**着手前に読むこと**（重複説明はしない）:
- `docs/loop-engineering-evaluation.ja.md` — 本ギャップの発端になった評価
- `docs/adversarial-verify-design.md` — タスクAの決定論コアの設計・実装済み部分
- `docs/design-delegation-strategy-measurement.md`（特に末尾「レビュー所見」節）— タスクBの背景
- `docs/fork-subagent-type.md` — fork/監査独立性の前提知識

---

## タスクA: adversarial verifyのSKILL配線（condukt Phase 6）

### 現状

`crates/condukt/src/adversarial.rs`の決定論コア（`Ballot`/`Vote`/`Policy`/`Panel`、`adjudicate`、`plan`、`touches_gate_crate`）は実装・テスト済み。CLIも配線済み:

- `condukt adversarial plan --touched <path>[repeatable] [--size N]` — GATE_CRATES（blastguard/propguard/specguard/stuckguard/mutategate）配下に触れているか、またはglobal switch/high_stakesで判定。engage時はexit 0でPanelPlan（`size`は`[2,5]`にクランプ）、非engage時はexit 1。
- `condukt adversarial adjudicate [--file <path>]` — stdin/fileのJSON配列（`{"skeptic":"...","ballot":"refute|pass|abstain","reason":"..."}`）を受け取り判定。pass=exit 0、block/escalate=exit 1（`outcome`フィールドで区別）。fail-closed（`effective<min_voters`→block、tie→block）。

しかし`crates/condukt/skills/condukt/SKILL.md`のPhase 6（623〜785行目付近）は今も**単一verifier**のまま — `condukt state verifier-model`でworkerとモデルを必ず違えて1体起動するだけで、`adversarial plan`/`adjudicate`への参照は一切無い。つまりGATEクレート自体を変更する完了判定でも、今日時点では単一verifierの自己検証（＝shared blind spotのリスクをそのまま抱えた状態）に留まっている。

### 変更手順

Phase 5.5「Self-consistency 合意形成」（580〜621行目）が同型のfan-outゲートを既に実装しているので、**そのまま構造を流用する**（新しいオーケストレーションパターンを発明しない）:

1. Phase 6の冒頭、単一verifier起動の直前に、Phase 5.5の`condukt consensus plan`呼び出しと同じ形でゲートを挿入する:
   ```sh
   PLAN=$(condukt adversarial plan --touched <changed_file_1> --touched <changed_file_2> ...)
   ```
   exit 0（engage）なら以下のパネル手順へ、exit 1なら**現行の単一verifier手順をそのまま実行**（既存パスは変更しない）。
2. engage時、`PLAN.size`（N）体の独立skeptic subagentを、Phase 5.5の`samples`個の候補実装と同じ流儀で**1メッセージ内に並列Task起動**する。各Taskのdescriptionは`"<t.id>-skeptic<k>"`のような形式。プロンプトは「既定REFUTED。コード上の具体的根拠で反証できたらrefute、崩せなければpass、判断不能ならabstain」を指示し、`{"skeptic":"<id>","ballot":"refute|pass|abstain","reason":"..."}`のJSONを返させる。
3. N件のJSONを配列にまとめ、`condukt adversarial adjudicate`にstdin経由で渡す。exit 0（pass）なら`condukt state set --status verified`、exit 1で`outcome=block`なら`--status failed`、`outcome=escalate`なら人間/上位レビューへ引き渡す既存の経路（condukt自体のblocked/GATEDタスク滞留の仕組み、または`overwatch review-queue`の`[escalation]`ストリーム）に接続する。

### 未確定点（実装前に決める）

- **skeptic間のモデル多様性の割当ルールが未定義。** 単一verifierには`condukt state verifier-model`があるが、N体のskeptic全員に同じモデル多様性保証をどう与えるかの仕様が無い。案: `verifier-model`をN回呼んで結果セットを使う／`--index`引数を足して呼び出しごとに階層をずらす、など。この設計をタスク着手時に決定し、`crates/condukt/src/adversarial.rs`かSKILL側のどちらに置くか明記すること。

### 受け入れ基準

- [ ] Phase 6の冒頭に`condukt adversarial plan`ゲートが追加され、非engage時は既存の単一verifier手順が無変更で実行される（既存テストが無変更でPASSすることで確認）。
- [ ] engage時、N体のskeptic Taskが1メッセージで並列起動され、収集したJSONが`condukt adversarial adjudicate`に渡され、`outcome`に応じて`verified`/`failed`/escalateの3分岐が実装されている。
- [ ] skeptic間のモデル多様性割当ルールが明文化され、実装されている。
- [ ] GATEクレート（例: `crates/propguard/`）配下のファイルを変更するcondukt実行で、実際にパネルが起動することを手動で1回確認する。
- [ ] `docs/adversarial-verify-design.md`の「試作の境界」節の「SKILL配線は未実施」を「実施済み」に更新する。
- [ ] `crates/condukt`のversionをmicro以上bumpし、3ファイルlockstepチェックが通る。

### スコープ外

- `adversarial.rs`の決定論コア自体（`adjudicate`のルール、`plan`のクランプ・発火条件）は変更しない。
- 通常タスク（GATEクレート非該当・global switch off）での挙動は変更しない。

---

## タスクB: delegation記録の「手動・ゲート無し」を埋める

### 現状の穴

`docs/design-delegation-strategy-measurement.md`のレビュー所見で指摘済み: `/flow`が`condukt`をfork/inlineで起動した後、`fugu-router record --delegation <fork|inline>`を呼ぶかどうかは実行LLMの「思い出し」任せで、これを強制する決定論的な仕組みが無い。記録漏れが起きても検知できないため、hypothesis（`fork/inline両方最低3件`）が実際に検証可能になるかは運用の遵守率次第、という構造的な脆さが残っている。

関連する既存実装（今回の調査で確認済み）:
- `fugu-router`のEpisodeストアは`~/.fugu-router/episodes.jsonl`（`sync_repo`設定時は同期先）にappend-onlyで記録される。`delegation: Option<String>`フィールドは`--class flow-delegation`のepisodeでのみ埋まる想定。
- `overwatch`には`record-finding`/`record-rollback`という**fail-soft advisory**の既存パターンがある（ストア書き込み失敗でもターンを壊さない設計）。ただし現状は明示的なCLI呼び出しのみで、自動検知フックには繋がっていない。
- `crates/autoflow`の`Stop`フック（`crates/autoflow/src/main.rs`の`stop_command`）は毎セッション終了時に無条件発火し、`cwd`と`condukt::find_pending`を既に読んでおり、`block()`で advisory メッセージを出す仕組みも既にある。この既存フックが最も自然な差し込み先。

### 設計（2段階。Tier 1を先に実装し、Tier 2は要調査で後回しにしてよい）

**Tier 1 — 「思い出し」を「決定論的チェックの実行」に変える（低リスク・すぐ着手可能）**

flowのSKILL.md Step 3-2の現行の手動記録の呼びかけ（「condukt実行が完了したら...記録する」という散文的リマインダー）は、そのままでは「記録したつもり」を防げない。これを次のように変える:

1. `fugu-router`に軽量な検査サブコマンドを追加する: `fugu-router audit-recent --class flow-delegation --within <seconds>`。直近N秒以内に`class=flow-delegation`のepisodeが記録されていればexit 0、無ければexit 1。
2. flowのSKILL.md Step 3-2〜3-3の記録手順を、「記録すること」という指示文から「記録した**あとに**`fugu-router audit-recent`を呼び、exit 1ならもう一度`record`を試みる、または記録漏れをユーザーに明示的に警告する」という**手順の一部**に変える。これにより、LLMが「記録した」と自己申告するだけで済まなくなり、決定論的なexit codeで自己検証させる。

**Tier 2 — セッション終了フックでの他律的advisory（要調査、Tier 1の後で着手）**

より強い保証がほしい場合、`autoflow`のStop hookに「この`cwd`でconduktがこのセッション中に`verified`/`failed`へ到達したのに、対応する`flow-delegation`のepisodeが見当たらない」場合のfail-soft advisoryを追加する。ただし着手前に次を確認すること（未確認のため断定しない）:

- StopフックのペイロードにセッションのTool呼び出しログ（`transcript_path`等）へのアクセスが含まれるか。含まれるなら「このセッション内で`fugu-router record --delegation`が呼ばれたか」を直接grepできるので、時間窓ヒューリスティックより確実な相関が取れる。
- 含まれない場合は、conduktの完了時刻と`fugu-router`エピソードの記録時刻を短い時間窓（例: 数分）で突き合わせるヒューリスティックにフォールバックする。誤検知（flow経由ではない通常のcondukt実行を誤って警告する）を避けるため、**このチェックは`--class flow-delegation`が期待される文脈でのみ発火させる**設計にすること（発火条件をどう判定するかも要設計）。
- 実装するとしても`overwatch record-finding`/`record-rollback`と同じ**fail-soft**（ストア/検知の失敗がターンを止めない）を厳守する。

### 受け入れ基準

- [ ] `fugu-router audit-recent`が実装され、直近記録の有無をexit codeで返す。
- [ ] `crates/flow/skills/flow/SKILL.md`のStep 3-2〜3-3が、「記録すること」という散文リマインダーから「記録→`audit-recent`で自己検証→未達なら再試行/警告」という手順に更新されている。
- [ ] Tier 2に着手する場合、着手前に上記「要調査」2点を確認し、その結果を`docs/design-delegation-strategy-measurement.md`のレビュー所見に追記してから実装する。
- [ ] `fugu-router`と`flow`のversionをそれぞれmicro以上bumpし、lockstepチェックが通る。
- [ ] `docs/design-delegation-strategy-measurement.md`のレビュー所見の該当項目（「記録ステップが完全に手動でゲートが無い」）を、対応済みである旨に更新する。

### スコープ外

- `fugu-router route`/`decide_bandit`へのdelegation軸の追加（引き続き、hypothesis validate後の別タスク）。
- 既定fork バイアス自体の変更（今回はあくまで検知の仕組みを足すだけ）。
- inlineサンプル枯渇問題（レビュー所見の指摘1点目）への対応 — これは別の設計判断（hypothesisの目安件数見直し等）が必要で、本指示書のスコープ外。

---

## 共通ルール

- 両タスクとも、触ったクレートは必ずversionをmicro以上bump（3ファイルlockstep）。`python3 scripts/check-plugin-versions.py`と`check-version-bumped.py`を通すこと。
- `cargo fmt`・`cargo clippy -p <crate> --all-targets`をgreenにしてからコミットする。
- GATEクレート（今回は`condukt`はGATEクレートではないので該当しないが、念のため）に触れる場合は`scripts/rollout-plugins.sh --canary`が必須になる点に注意。
