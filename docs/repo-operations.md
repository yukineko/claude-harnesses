# リポジトリ運用手順（plugin rollout / version bump / レビュー窓口）

> **このファイルは特定の作業（プラグイン改修・バージョン管理・レビュー統合・特定ゲートスクリプトの
> 詳細）をするときだけ読む。** 全 agent が毎ターン読むべき汎用ドクトリン（fail-open/fail-closed の
> 哲学・判断と観測の関係・worktree 分離義務など）は [`CLAUDE.md`](../CLAUDE.md) の
> 「最上位の方針」節を参照。ここに書かれているのは「どうやるか」の手順であり、「なぜそうするか」の
> ドクトリンではない。

## ゲートスクリプトの詳細（`cargo fmt`/`clippy` 以外）

- **prompt-injection 防御ゲート（1本）** — prompt に load される資産に改竄が植わっていないか機械検出する:
  - `python3 scripts/check-prompt-injection.py`（injectguard）— skills/agents/hooks/CLAUDE.md/.compass/docs
    に隠蔽・検証バイパス・egress 文言が植わっていないか走査（防御 framing は除外）。ローカルは
    `git config core.hooksPath .githooks` で pre-commit を有効化（速い advisory 層。CI の `injectguard` job が
    非バイパスの本ゲート）。
- **file-growth ratchet（pre-commit、`file-growth`）** — `python3 scripts/check-file-growth.py`。
  **staged（index）** の追加/変更 `.rs` / `.py` を `git show :<path>` の行数（末尾改行なしの最終行も
  1 行と数える）で検査し、2000 行を超えたら block する。既に 2000 行を超えていたファイルは
  `scripts/check-file-growth.baseline`（`<max_lines> <path>`、`#` 行・空行は無視）に導入時点の行数を
  上限として記録してあり、その上限を超えたら block する。**上限は下げる方向にしか動かない**:
  staged の baseline が HEAD の baseline より上限を上げると block。HEAD の baseline に無い 2000 超の
  新規エントリ（HEAD に baseline 自体が無い場合を含む）は、**HEAD の tree で既にその行数以上ある
  ファイルの追認**としてだけ許す（上限 ≤ `git show HEAD:<path>` の行数）。HEAD にそのパスが無い・
  上限が HEAD の行数を超える・HEAD が unborn のときは block（成長の追認は不可）。2000 以下の新規
  エントリは締める方向なので可。HEAD:<path> を読む git が失敗したら判定不能（exit 2）。
  下げる・消すのは可。直し方はファイルを分割して 2000 行未満にし、同じ commit でエントリを下げるか
  消すこと。working tree だけの変更・stage していないファイルは見ない。exit 0 = pass / 1 = 違反
  （path・行数・上限を表示）/ 2 = 判定不能（git 失敗・baseline の解釈不能行・重複エントリ）で、
  2 も block する。テスト: `python3 -m unittest scripts/test_check_file_growth.py`。
- **Continuous-Audit 自動起動導線** — `git config core.hooksPath .githooks` を有効化していれば
  `.githooks/pre-push` が GATE_CRATES（blastguard/propguard/specguard/stuckguard/mutategate/overwatch）配下の
  変更を検知し、`scripts/continuous-audit.sh --dry-run` を勧める advisory メッセージを出す
  （この advisory 自体は push を止めない。ただし hook 全体は下記の再確認を含むブロッキングチェックで
  非0終了しうる）。cron 定期実行の雛形は `scripts/continuous-audit.cron.example` を参照。
- **pre-push の ref 再確認（backlog d222b3e3）** — `.githooks/pre-push` は最後の `exit 0` の直前に、
  stdin の各行（local_sha が全ゼロの delete 行は除く）の local_ref を `git rev-parse --verify --quiet`
  で引き直し、local_sha と比較する。ref が動いていた・解決できなかった場合は、ref 名・certified sha・
  current sha を表示して非0で block する（fail-closed、CLAUDE.md §3）。理由: https では git は hook が
  返った**後で** ref を名前で読み直して送るので、hook 実行中に ref が動くと remote は hook が検査して
  いない commit を受け取る。実測 2026-10-25: hook が 2f8e4209 を検査した push で、0241c62f と 4d3dd426
  が未検査のまま origin に届いた（`scripts/test_prepush_ref_moved_during_hook.py`）。**この再確認は窓を
  狭めるだけで閉じない** — 再確認から transport の読み直しまでの間に ref が動けば同じことが起きる。
  残余の窓は残っており、race は閉じていない。
- **pre-push のフェーズ計時（observability のみ）** — `.githooks/pre-push` は各フェーズの wall-clock 秒
  （`date +%s`）を測る: `bypass-ledger` / `nudges` / `rollout`（スクリプトがある時のみ）/
  push される sha ごとの `typecheck(<sha8>)`（worktree 作成＋`cargo check`）・`compile-fail(<sha8>)`
  （trybuild target ごとの内訳つき。pass cache で飛ばした target は秒数の代わりに `skipped`）・`specguard(<sha8>)`（gate crate を触る range か、その diff が取れなかった時のみ）/
  `cleanup`（検査用 worktree の削除）/ `ref-recheck`。EXIT trap で、block や失敗で抜けた場合も含めて
  その時点までの計測を `pre-push timing: ...; total <N>s (exit <rc>)` の 1 行で stderr に出し、
  `$(git rev-parse --git-common-dir)/prepush-timing.jsonl` に 1 push 1 行の JSON（`start` / `shas` /
  `phases` / `total` / `exit`）を追記する。`date` が使えない時の秒数は `?`（JSON では `null`）。
  ログを書けない時は 1 行の警告だけを出す。**判定を持たない**: 下流消費者は stderr を読む人間と、
  遅い push を診断する時に jsonl を読む人間だけで、これを読むゲートは無い。block/allow・exit code・
  フェーズ順は変えない（trap は入った時の終了コードで exit し直す）。cargo の
  `Blocking waiting for file lock` は記録しない: hook の cargo 呼び出しはすべて `--quiet`
  （または stderr を捕捉して失敗時にしか出さない `check-compile-fail.py` 経由）で、cargo はその
  status 行を `--quiet` で抑止するため、拾うには利用者に見える出力を変える必要がある。
- **pre-commit のゲート別計時（observability のみ、#1551）** — `.githooks/pre-commit` も同じ方式で
  ゲート（scanner）ごとに 1 フェーズを測り、EXIT trap で `pre-commit timing: <label> <N>s, ...; total <N>s (exit <rc>)`
  の 1 行を stderr に出し、`$(git rev-parse --git-common-dir)/precommit-timing.jsonl` に 1 回 1 行の JSON
  （`start` / `phases`〈`phase` / `secs` / `rc`=そのゲート自身の終了コード、scanner 欠落時は `null`〉/ `total` / `exit`）を追記する。
  時計は開始時に `date +%s` が使えればそれ、使えなければ shell の `$SECONDS`（bash/ksh/zsh）、どちらも無ければ
  秒数は `?`（JSON では `null`）。ログを書けない時は 1 行の警告だけ。**判定を持たない**（下流消費者は
  stderr と jsonl を読む人間だけ）: block/allow・exit code・ゲート順は変えない。
- **pre-commit / pre-push の範囲（2026-11-08 ユーザー裁定）** — hook は他の worktree に干渉せず、
  検査はその時の変更範囲に限る。pre-commit の全リポジトリ走査型スキャナは `--staged` で stage された
  差分だけを見る（または差分が入力に触れたときだけ走る。`scripts/staged_scope.py`）。pre-push は
  使い捨て worktree・共有 target dir（旧 `$GIT_COMMON/prepush-typecheck-target`。撤去後もディスクに
  残っている場合は手で消してよい）・`target-cleanup` フェーズを持たず、push される commit が触った crate
  だけを、push される sha が clean な HEAD のときにその場で検査する。同じ tree・同じ test id の pass は
  `scripts/test-pass-record.py`（`<git common dir>/test-pass-record/`）に記録し、記録があれば再実行しない。
  記録が無い・壊れている・tree が違うときは実行し、その場で実行できないなら block する。
  `gate-bypass.py` の到達判定はこの worktree の HEAD だけを見る（他 worktree の HEAD は見ない）。
- **pre-push の crate 単独 typecheck（issue #1641 / backlog 38a44b30）** — `typecheck` フェーズは
  range に応じた `cargo check --all-targets`（`--workspace` または `-p a -p b ...`）の後に、range が触った
  各 `crates/<dir>`（push される sha に `Cargo.toml` があるもの）を `cargo check --all-targets --quiet -p <crate>`
  で **1 crate ずつ単独に**検査する。複数 package を 1 回で検査すると Cargo が共有依存（harness-core の
  `fault-injection` 等）の feature を統合するため、自分の manifest に feature が欠けた crate のテスト
  target（例: tdd の E0432）が通ってしまうからである。Cargo.lock を触る range（lockstep bump のたび）で
  `--workspace` に広がった場合も同様に走る。どれか 1 つでも失敗すれば `typecheck` フェーズ内で block。
  test id は crate ごとの `cargo-check:<dir>` で、その tree にその id の pass があるときだけ再実行しない
  （`cargo-check:workspace` や複数 crate の id の pass ではスキップしない。単一 crate の狭い range では
  前段の検査がまさに同じコマンド・同じ id なので二重に走らせない）。range が計算できないときは crate 集合が
  不明なので全 workspace member を単独検査する。dir が 1 package に対応しない・metadata が読めないときは block。
  追加コスト（2026-11-11 実測、rev 329b4700 + 本修正、warm な共有 target dir）: `--workspace` 検査直後の
  `cargo check --all-targets --quiet -p blastguard` 1 回目 8.9s、2 回目 0.16s。
- **pre-push の fail-open mutation（issue #1584 / backlog 4f19b628）** — push される range が
  `crates/<gate>/`（GATE_CRATES の 7 crate）を触ったとき、その gate crate ごとに
  `scripts/check-fail-open-mutation.py --crate <gate> --keep-going` をその場で実行する（test id
  `fail-open-mutation:<gate>`、同じ tree で pass 記録があれば再実行しない）。range が計算できない時と
  スクリプト自身が変わった時は全 gate crate。root の Cargo.toml / Cargo.lock だけの変更では走らない。
  粒度は crate であって、変更された個々の `Undetermined` arm ではない（スクリプトに arm 単位の選択子は
  無く、scenario が登録されていない arm はここでは測られない）。非0終了（NOT CAUGHT / inconclusive /
  error / revert 失敗 / crash）・スクリプト不在・python3 不在はすべて block。

## 記録そのものの定期監査 — `scripts/record-audit.py`（backlog 0f55003a）

上のゲート群はすべて**コード**を検査する。**それらのゲートが書き出す記録**を検査する層は無かった。
記録は次の監査の入力なので、腐ると次の監査は「役に立たない」ではなく「成立しない」— 書き込みが
止まった台帳から `clean` を結論することになる。

`record-audit.py` は 4 つの記録健全性を測り、閾値超過を `overwatch review-queue` に上げる:

| 次元 | 何を見るか | 閾値（測定 2026-07-31 / `c3f29681`） |
|---|---|---|
| `doc-claim-drift` | docs/ と CLAUDE.md の `path:line` 主張のうち実体と食い違う数 | **ratchet** 140（増加で breach） |
| `audit-convergence` | `overwatch audit-metrics` の tri-state `converging` | false で breach |
| `review-queue-depth` | 人間レビュー面に溜まった未処理エントリ数 | 20 |
| `stale-undisposed` | fix は landed したが disposition が無い finding 数 | 5 |
| `backlog-rot` | 21日超 pending ＋ 再浮上して再 fail した項目 | 0 |

**なぜこの 4 つか**（どれも実測。仮定ではない）: `check-doc-claims.py` は 140 件のドリフト主張を
抱えたまま verdict `clean` を返す（全件 exempt で、exempt が clean として描画される）。
`audit-metrics` は 6 ラウンド連続で `converging: NO`（new findings 5→1→13→0→14→15）だが誰も読まない。
`review-metrics` は stale 件数を println して exit 0 する（`disposition_cli.rs`）。`backlog fail` は
項目を退役させず 2 日 defer するだけで、`Task::is_pending` が再び数える（`task.rs`）。

**exit code は三値**: `0` = 全次元を測定して閾値内 / `1` = 閾値超過（finding を記録済み）/
`2` = **測定できなかった次元がある**。2 が 1 に優先する — 測れなかった次元は通った次元ではない
（CLAUDE.md 第3節）。probe の非0終了・parse 不能・binary 不在・timeout はすべて `2` であって `0` ではない。

**定期実行は `daily`（ローカル）** — CI schedule は使わない。ゲートを止める権限だけでなく
**breach の可視性まで外部サービスに預ける**ことになるためで、advisory であっても割に合わない
（CLAUDE.md 第7節）。`~/.daily/config.toml` に登録済み。雛形は:

```sh
python3 scripts/record-audit.py --print-daily-task   # 貼り付ける stanza を出力
python3 scripts/record-audit.py --no-escalate        # 手で1回だけ測る（queue に何も書かない）
```

> **`~/.daily/config.toml` を新規作成するときの罠**: `daily` はタスクが 1 つも登録されていない間だけ
> 組み込みの `security`（cargo-deny）タスクへフォールバックする（`crates/daily/src/main.rs` の
> `effective_tasks`）。record-audit だけを書いた config を作ると **cargo-deny 監査が黙って退役する**。
> 現行 config は security を明示的に引き継いでいる。

**記録は 2 箇所に残る**: breach は `overwatch review-queue`（`record-audit:<次元>:<初回 breach の epoch>`
という episode 付き finding id。既に open なら重複記録しない — `record-finding` は単純 append なので
毎日積むと面それ自体が読めなくなる。episode は `episodes.json` に永続化され、close 後の再 breach は
新しい id になるので queue に再び見える。episode 無しの旧 id `record-audit:<次元>` も open のまま扱う）。
**close は観測で行う**（backlog 89544915）: その実行が次元を `ok` と実測できたときだけ、open な
`record-audit:` finding に `overwatch record-disposition --verdict resolved`（人間の判定ではない自動
verdict。review-metrics の agreement/false-positive 率から除外し `auto_resolved` に別計上）を evidence
付きで記録する。breach 中・測定不能・queue 読めない・disposition 書き込み失敗はどれも close せず、
`NOT closed` を stderr に出して exit 2。
全実行の数値は `~/.record-audit/observations.jsonl`（append-only の trend 台帳。
`RECORD_AUDIT_STATE_DIR` で差し替え可）。**台帳への追記に失敗した実行は exit 2** — 記録を残さなかった
実行は、この job が生む唯一の成果物である trend に穴を空けている。

**store は cwd で解決される（重要）**: `overwatch` も `backlog` も store を cwd から引くので、
linked worktree から走らせると**全次元が 0 を返す**（存在しない store を「健全」と報告する）。
`record_audit.record_root()` が `git rev-parse --git-common-dir` で main worktree を解決してそこを読む。
解決できなければ REPO へフォールバックせず **undetermined** にする。テストは
`scripts/test_record_audit.py`（45 ケース。13 の fail-open 変異がそれぞれ名前の付いたテストで殺されることを確認済み）。

## gate が壊れたら何が漏れるか — `scripts/gate-severity-probe.py`（GitHub #1552 / backlog dfa2c9d3）

「どの gate を毎 commit で回すか」を決める前に、gate ごとの重大度を**実測**する道具（判断材料であって
ゲートではない。pre-commit / pre-push には配線しない）。gate ごとに代表違反 fixture を 1 つ注入し、
ユーザー定義（2026-11-06）の 3 指標を出す。time-to-detect は対象外。契約は
`docs/specs/gate-severity-probe.md`。

```sh
python3 scripts/gate-severity-probe.py --json                          # 全 gate・全層（遅い。下記）
python3 scripts/gate-severity-probe.py --json --gate launcher-exec-bit # 指定 gate だけ（繰り返し可）
python3 scripts/gate-severity-probe.py --json --commit-layers-only     # pre-push / cargo-test を省く（約 1.5 分）
python3 scripts/gate-severity-probe.py --json --cargo-target-dir ~/.cache/gsp-target  # cargo 成果物を再利用
```

| key | 意味 |
|---|---|
| `fires` | gate 単体で代表違反を今ブロックするか（`true` / `false` / `"undetermined"`） |
| `caught_by` | その gate **だけ**を外したとき、なお止める層（他の pre-commit gate のラベル・`commit-msg`・`pre-push`・`cargo-test`） |
| `leak` | `caught_by` が空のときの到達点（`push` / `rollout` / `undetermined`）。他層が止めるなら `null` |
| `security_relevant` | **人間が宣言した**一覧（`SECURITY_RELEVANT`。現在 injectguard / secret-guard）に入っているか。データから推論しない |
| `undetermined_layers` | control（違反なし）で既に止まる・timeout・起動不能だった層。`caught_by` には入れず、未捕捉なら `leak` を `undetermined` にする |

- **隔離**: caller repo には `rev-parse` しかしない。caller の HEAD を temp dir へ `git clone --shared`
  した私有 clone とその中の linked worktree だけで fixture を適用・commit し、終了時に丸ごと消す
  （caller の作業ツリー・index・HEAD・worktree 一覧・bypass ledger・pass 記録は不変。caller の staged /
  untracked 変更は測定対象外）。
- **帰属**: 各 scanner をまず違反なし（何も stage しない）で走らせ、そこで既に止まる gate のブロックは
  fixture に帰属できないので `fires: "undetermined"`。pre-push と cargo-test も同じ crate dir を触る
  neutral commit / clean HEAD を control にする。
- **fixture の作り方**: plugin crate を触る fixture は lockstep bump も同時に入れる（version 系 gate を除く）。
  そうしないと bump-on-change が全部を「捕まえて」しまい、bump すれば通る違反を見誤る。
  fixture の anchor が消えた（ファイル改名等）ときは skip ではなく `fires: "undetermined"` と理由を出す。
- **対象**: 今 hook が走らせている gate だけ（ユーザー裁定 2026-11-11）。hook から外した gate は一覧から消える。
- **所要時間**（実測。2026-11-11、rev `cf954b43`、開発機 Mac 1 台、cargo target は空から）:
  `--commit-layers-only` の全 gate 82 秒、全層の全 gate 546 秒（約 9 分。最長は crossx-constants の
  235 秒＝backlog の cargo test）。`--gate launcher-exec-bit` 単体の全層は約 2 分。
  数字は継承せず、使う前に測り直すこと。
- exit 0 = JSON を出した。exit 2 = probe 自体が走れなかった（未知の `--gate`、caller が git repo でない、
  clone 失敗）で、JSON は出さない（空の clean レポートに見せない）。

## ビルド成果物ディレクトリの上限（`scripts/cap-target-dir.sh`）

`.cargo/config.toml`（`/.cargo/` は gitignore 済みの machine-local 設定）で `target-dir` を
`/tmp/condukt-build` のような**絶対パス**へ逃がしている環境では、その 1 つのディレクトリに
すべてのビルド成果物が積み上がり、誰も回収しない（2026-07-30 実測: 37.1 GiB / 262,954 files）。

`scripts/cap-target-dir.sh` はこれを決定論的に抑える:

- `cargo metadata` で解決した target-dir を `du -sm` で実測し、**閾値を超えたときだけ** `cargo clean`
  して 1 行を stderr に出す。超えていなければ**完全に沈黙**する。
- 閾値は環境変数 `CARGO_TARGET_CAP_MB`（MB 単位、**既定 20000 = 20GB**）。
  **`CARGO_TARGET_CAP_MB=0` で明示的に無効化**できる。
- **ビルドの「前」に走る**（後ではない）。後で clean すると、いま作った成果物を捨てて次回フルリビルドに
  なるだけで、総コストは同じなのに 1 回分無駄が増える。前なら「閾値超過時のそのビルドだけがフル
  リビルド」で済む。
- このスクリプトの失敗はビルドの失敗ではない。`cargo metadata` が失敗する・target-dir が無い・`du` が
  失敗する（部分的にしか歩けず数値が過少になる場合を含む）— いずれもスクリプト自身は何もせず exit 0
  （ただし `cargo` / `du` 自身の診断 stderr は握りつぶさない）。とくに **`cargo metadata` が rc != 0 で失敗したときは
  `$PWD/target` へフォールバックしない**（cargo workspace の外か cargo が壊れているかで、そこにある
  `target/` が cargo の成果物である保証が無く、`cargo clean` も同じ理由で失敗する。判定できない状態で
  破壊的操作を撃たない）。フォールバックするのは **rc == 0 だが出力に `target_directory` が無い**
  場合だけ。

呼び出し元は `scripts/rebuild-plugins.sh`（`--no-clean` 時。既定の clean 経路は自前で `cargo clean`
するため不要）・`scripts/build-plugin-bin.sh`・`scripts/test-changed-crates.sh`（donegate の Stop ゲート）。

## プラグインを改修したときの反映（忘れやすい）

`crates/<name>/` が唯一の正典。`/plugin install` はここを
`~/.claude/plugins/cache/<owner>/<name>/<version>/` に**プレーンコピー**する（git 外）。稼働中の
ハーネスはキャッシュ側を読むので、**repo をビルドしただけでは何も反映されない**:

### 「plugin 更新（`/plugin update` 相当）」を求められたら — `scripts/rollout-plugins.sh` を使う（正典・一発）

**version を上げた／テキスト資産を変えたプラグインを稼働ハーネスへ反映するときは、手動 `cp` ではなく
必ず `scripts/rollout-plugins.sh` を実行する。** このスクリプトは `/plugin update`（UI 専用操作）が
directory marketplace `yukineko` に対してやる2操作 —(1) `crates/<name>/` を新しい
`cache/yukineko/<name>/<version>/` **dir へコピー**、(2) `installed_plugins.json` の
`<name>@yukineko` を新 dir へ **repoint** — を再現し、続けて `rebuild-plugins.sh`（バイナリ swap）と
各 plugin の `sync-plugin-assets.sh`（skills/agents/hooks 同期）まで一括で走らせる。順序も保証する
（version dir + registry pointer を rebuild より先に作る）。

```sh
python3 scripts/check-plugin-versions.py && python3 scripts/check-version-bumped.py   # 先に version 整合を確認
scripts/rollout-plugins.sh --plugin <name> --dry-run   # 動作を確認（何も書かない）
scripts/rollout-plugins.sh --plugin <name>             # 実反映（全 plugin なら無引数）

# GATE クレート（防御ゲート: blastguard / propguard / specguard / stuckguard / overwatch、
# 非プラグインの mutategate も含む）は fleet を守るため、canary 無しの反映を
# **拒否**する（Problem-2.3）。GATE クレートを対象にするときは --canary で段階
# 反映し、各ステージ間で健全性ゲート（raw-spike または systemic recurrence の
# いずれかで rollback）を通す。--plugin 無しの全 plugin 反映も GATE を含むため
# --canary が必須（明示的に外すときのみ --no-canary）。
scripts/rollout-plugins.sh --plugin specguard --canary --dry-run   # GATE クレート: 段階反映を確認
scripts/rollout-plugins.sh --plugin specguard --canary             # GATE クレート: canary 経由で反映
```

- 冪等（version 不変なら no-op）。`--force` で無変更でも再コピー、`--no-rebuild`/`--no-sync` で段階分け。
- **committed HEAD 以外からは反映しない（exit 9, backlog ff8cebd0）**: 2026-11-14、pre-push の
  fail-open mutation（`check-fail-open-mutation.py`）が `crates/blastguard/src/model.rs` を in-place で
  `Decision::Ask(_reason) => Decision::Allow` に書き換えている最中に rollout が走り、その mutant を
  blastguard 0.2.548 として配布した。mutation は今の場所に残す（ユーザー裁定）ので、反映側が拒否する。
  `scripts/rollout_source_guard.py` が判定し、判定不能は拒否:
  (1) コピー前に build 入力（`crates/`・`Cargo.toml`・`Cargo.lock`・`.cargo/`）に staged/unstaged/untracked
  の差分が無いこと・HEAD が起動時のままであること・mutation 中マーカー `<git-dir>/fail-open-mutation.active`
  が無いこと、(2) 各 plugin のコピー直後に、HEAD が追跡する全ファイルが HEAD の blob と一致すること
  （不一致は cache 側を HEAD の内容に戻してから拒否）、(3) registry 書き込み前、(4) `rebuild-plugins.sh` 内で
  `cargo build` の後・**バイナリ swap の前**、(5) rebuild+sync 後（in-flight restore で registry は元へ戻る）に、
  (1) に加えて HEAD と mutation カウンタ `<git-dir>/fail-open-mutation.seq` が不変であることを再確認する。
  mutation スクリプトはファイルを書き換える**前**にカウンタを増やしマーカーを作り、revert をバイト一致で
  確認した**後**にマーカーを消すので、build 中に始まって戻された mutation もカウンタで検出される。
  マーカー/カウンタは自 worktree の git dir だけを見る（他 worktree は参照しない）。`--dry-run` は対象外。
  `rebuild-plugins.sh` を**単体で**叩いた場合はこの検査を通らない（rollout 経由でのみ有効）。
  **残る穴（未対処・明示）**: (a) gitignored ファイルは `git status` に出ず rsync でコピーされ、(2) は HEAD
  追跡ファイルしか比べないので cache 側の余剰ファイルは検出しない。(b) 汚れ検査は上記 build 入力のみで、
  `scripts/`（sync-plugin-assets.sh 等）は対象外。(c) (5) で拒否した場合、registry は戻るが cache の
  version dir には swap 済みバイナリが残る（(4) を通過済みのビルド物）。(d) canary 経路は E2E テスト未整備。
  プロセスが死んで `.active` が残った場合は rollout が拒否し続けるので、`git status` が clean なことを
  確認してから手で消す。
- **GATE クレートは canary 必須**: canary 無しでの反映はエラーで止まる（fleet 防御ゲートを
  無検証で入れ替えないための安全策）。`--no-canary` は明示的な例外指定としてのみ用いる。
- **手動 `cp` は禁止**: cache の binary/asset を手で上書きするだけだと **version dir が旧名のまま残り**
  registry も更新されず、`sync-plugin-assets.sh` が version から dir を誤解決して**古い版が配布される**。
- `/plugin update`（ユーザー UI 操作）は本スクリプトで完全代替できるので、手動 UI 操作は不要。

低レベルの構成要素（通常は上の rollout 経由で十分。個別に叩くのは段階分けのときだけ）:

- バイナリを反映: `scripts/rebuild-plugins.sh`（`--no-clean` で増分）— target のバイナリを live
  キャッシュへ swap する（**既存 dir に swap するだけ。version dir は作らない**）。
  **書き込むのは各 plugin の current version dir だけ**（current = repo の
  `crates/<dir>/.claude-plugin/plugin.json` の `version`。plugin 名→crate dir は plugin.json の
  `name` で解決）。バイナリ・`hooks/*.json`・`.deployed-from.json` のいずれも、**superseded な
  version dir には一切書かない（byte 単位で凍結）**。以前は `cache/*/*/bin/*` を glob して全 version
  dir を上書きしており、旧 version に pin されたセッションが黙って新コードを実行し、canary rollback
  （registry を旧 dir へ repoint）も何も戻せなかった（backlog `8acb117a`）。**帰結として、稼働中の
  セッションは再起動するまで起動時の version dir の旧コードを実行し続ける**（受容済みの仕様）。
  current version が決まらない plugin（該当 crate が無い／`version` が読めない）はどの dir も書かず、
  エラーを出して非0で終わる（「全 dir を書く」へは倒さない。§3）。
- テキスト資産（skills/agents/hooks）を反映: `crates/<name>/scripts/sync-plugin-assets.sh`
  （`--check` で drift 検出）。
- **キャッシュを手編集しない**（git 外で黙って乖離する）。必ず repo を編集 → 上記で同期。

## バージョン整合（**絶対厳守** — 「今動けばいい」は後で壊れる）

各プラグインの version は **3つの正典で常に lockstep** でなければならない。片方だけ上げた状態は
**バグ**として扱う（「今は動く」で放置しない）。ズレると sync-plugin-assets.sh が version から
キャッシュ dir を誤解決し、**古い版がユーザーに配布される**。

### 変更したら必ず version を上げる（**禁忌ルール**）

- **あるプラグインの中身（コード・hooks・skills・agents 等いずれか）を1行でも触ったら、その
  プラグインの version を最低でも micro（patch, `x.y.Z` の Z）上げる。** 触ったのに据え置きは
  **禁忌**。
- version を上げるときは **必ず3ファイル同時**: `crates/<name>/Cargo.toml` の `[package].version`
  ／ `crates/<name>/.claude-plugin/plugin.json` の `version` ／ `.claude-plugin/marketplace.json`
  の当該エントリ `version`（skill-only プラグインは Cargo.toml が無いので後者2つ）。
- 言い換え: **「何か変更したのに plugin version と marketplace version が変わっていない」状態を
  commit してはならない。** 変更の大小を問わず、最低 micro を必ず上げる（意味的に大きければ
  minor/major で判断）。
- これは「今動けばいい」を禁じ、後の drift・古い版配布を根絶するための**徹底順守ルール**。

**強制ゲート（3つとも、commit 前・push 前・CI で回す）:**

```sh
python3 scripts/check-plugin-versions.py            # lockstep: 3ファイルの version が一致するか（exit 1 で drift）
python3 scripts/check-version-bumped.py             # bump-on-change: base(既定 HEAD)から変更のある plugin が bump 済みか
python3 scripts/check-version-bumped.py --base origin/main   # CI/push前は pushed ref と比較
python3 scripts/check-plugin-rollout.py             # rollout drift: source version が installed_plugins.json の registry version へ実際に反映(rollout-plugins.sh実行)済みか
```

`check-plugin-rollout.py` は上の2つとは別の失敗モードを塞ぐ: **source 3ファイルの version 整合と
bump-on-change を両方満たしていても、`rollout-plugins.sh` を実際に実行し忘れれば、その fix はどのセッションにも
一切反映されない。** 過去に一度、5個の plugin（hypothesis/condukt/compass/blastguard/overwatch）で
commit・version-bump 済みの fix が rollout されないまま放置されていたことが手作業の grep+jq 調査で発覚した。
このスクリプトはその調査をスクリプト化したもので、`installed_plugins.json` の `"<name>@yukineko"` registry
version と各 plugin.json の source version を比較し、不一致（＝ rollout 未実行）を機械的に検知する
（registry ファイルが存在しない環境では検査対象なしとして exit 0 で skip）。

`check-version-bumped.py` は `crates/<name>/` に差分がある plugin の plugin.json version が
base より**厳密に上がっている**ことを要求し、上がっていなければ exit 1 で該当 plugin と変更ファイルを
表示する（新規 plugin は base に無いので OK）。**「変更したのに未 bump」を機械的に止めるゲート**。

**共有クレート（`crates/harness-core`）にも同じ規則がかかる**（backlog `32170548`）。harness-core は
plugin ではないので上のループの対象外だったが、**36 plugin のバイナリに静的にリンクされる**ため、
harness-core が動いた時点で出荷済みバイナリ 36 本の中身が変わる — にもかかわらず plugin.json は
どれも動かない。「バイトが変わったのに version が動かない」＝ version が出荷物を同定しない状態なので、
**harness-core 自身の `[package].version` の bump を要求する**（`crates/harness-core/Cargo.toml` のみ。
harness-core は plugin.json も marketplace.json も持たないので lockstep 3ファイルの対象ではない）。
- **リンクされる差分だけ**が bump を要求する。`crates/harness-core/tests/`（独立した integration
  test target）と `*.md` は構造的にバイナリへ入らないので免除され、**その免除は必ず標準出力に announce
  される**（黙って narrow しない）。それ以外は — `src/` 内の `#[cfg(test)]` ブロックのように
  parse なしには判別できないものも含めて — リンク扱いで bump を要求する（§3 の「判定不能は制限側」）。
- **リンク先 36 plugin の bump は要求しない。** 意味論的には最も純粋な読みだが、harness-core 1 commit
  あたり 108 ファイル + marketplace.json 36 行が動き、並行セッションと必ず衝突する（§8）。バイト同定は
  それ無しでも機械的に取れる（下記 `harness_core_version` と `SHARED_SOURCE_PATHS`）。
- テスト: `scripts/tests/version-bumped-shared-crate.sh`（7 ケース。未 bump が落ちること・tests/ 免除が
  announce されること・version が読めない場合に fail-closed に倒れることを含む）。

`rebuild-plugins.sh` が書く `.deployed-from.json` には **`harness_core_version`** が入る（そのバイナリが
リンクした harness-core の version）。`check-plugin-rollout.py` はこれを source tree の値と比較し、
遅れていれば rollout drift として落とす。読めなかった場合に書かれる `"unknown"` も **問題として扱う**
（もっともらしい version を捏造しない）。フィールドが**無い**旧 manifest は問題にしない — 同じ問いは
`SHARED_SOURCE_PATHS`（`crates/harness-core` を含む）による commit 比較が既に完全に答えるので、
fallback は弱くない（この「弱くない」は仮定ではなく
`scripts/test_check_plugin_rollout.py::SharedCrateVersion` の両アームで観測してある）。

- 正典3ファイル（バイナリ付きプラグイン）: `crates/<name>/Cargo.toml` の `[package].version` /
  `crates/<name>/.claude-plugin/plugin.json` の `version` / `.claude-plugin/marketplace.json` の
  当該エントリ `version`。**skill-only プラグイン**（Cargo.toml 無し。例: `scout`）は
  plugin.json + marketplace.json の2つ。
- 正典の向き: **`Cargo.toml == plugin.json` が真**。`marketplace.json` は取り残されやすい（laggard）。
  version を上げるときは **必ず3ファイル同時**に上げる。marketplace.json だけ手で上げ忘れない。
- **すべてのタイミングで徹底チェック**（version を触る commit、rebuild 前、push 前）。
  自動チェッカで機械的に確認する:

  ```sh
  python3 scripts/check-plugin-versions.py   # exit 0 = 全整合 / exit 1 = drift（該当プラグインを表示）
  ```

- **rebuild は version を上げない**（別工程）。`rebuild-plugins.sh` は正典の version をそのまま
  コンパイルして live cache へ swap するだけ。version bump は 3ファイル編集という別の意図的操作。
- **cache の version dir が古い**（例: source は 0.7.0 だが `cache/.../<plugin>/0.6.0/` のまま）のは
  `rollout-plugins.sh`（`/plugin update` 相当）未実行が原因。rebuild-plugins.sh は current
  version dir（0.7.0）にしか書かないので、**0.6.0 dir は凍結されたまま新コードは届かない**
  （0.7.0 dir が無ければ何も更新されない）。正式ロールアウトは `rollout-plugins.sh`
  （copy → repoint → rebuild → sync-plugin-assets.sh の順）で行う。

## 人間のレビュー窓口（統合レビューサーフェス）

観測系の3ストリーム — (1) systemic な gate 違反（`overwatch violations --systemic`）、
(2) canary health-gate のロールバック事象、(3) AI/敵対的レビューの指摘 — は従来別々だった。
`overwatch review-queue` はこれらを **1本の時系列リスト**（新しい順）にまとめ、各行を
`[systemic]` / `[rollback]` / `[ai-finding]` のタグで区別して見せる:

```sh
overwatch review-queue                 # 人間可読の統合リスト（新しい順）
overwatch review-queue --json          # kind 判別子付きの構造化配列
overwatch review-queue --since <ts> --limit <n>
```

（`[escalation]` = condukt の blocked/GATED タスクが人間の回答待ちで滞留している事象も4本目のストリームとして統合表示される。）

- **fail-soft**: いずれかのソースが空/欠落でも他のソースは表示される（コマンド全体はエラーにしない）。
  **review-queue は verdict を返さない純粋な観測・描画レイヤ**なので、欠落ソースは「違反なし」ではなく
  「そのソースは表示されない」として縮退する（判定を持たないから許される。冒頭「最上位の方針」を参照）。
- ロールバック事象は `scripts/rollout-plugins.sh` の canary auto-rollback 時に
  `overwatch record-rollback` で追記される（fail-soft: 記録失敗はロールアウトを止めない）。
- AI 指摘の永続ストア（`overwatch record-finding` の取り込み口）が空なら ai-finding の行は出ず、
  他のソースはそのまま表示される。
