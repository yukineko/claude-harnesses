## north_star
fail-open を『後から検出する』のをやめ、『そもそも書けない』側へ移す。harness_core の verdict 型は出力側でこれを既に達成している — Clean は private witness でしか作れず、Determination には unwrap_or も ok も Default も無いので、『判定できなかった』を『問題なし』へ潰す最短経路が型として存在しない。同じ技法が入力側にはまだ適用されていない。根拠(測定日 2026-07-22 / 測定点 56872974): (1) ルート Cargo.toml にも各 crate の Cargo.toml にも lints セクションが1つも無い(grep -n 'workspace.lints|[lints]' が全 Cargo.toml で無ヒット)。clippy の unwrap_used/expect_used は既定で off なので、gate crate でも素通りする。代わりに deny(clippy::panic) が15ファイルに手貼りされている(grep -rln で実測)。(2) GATE_CRATES の src 配下に unwrap_or(false|true) / unwrap_or_default() / .ok() が126箇所、生の fs::read_dir / fs::read_to_string / Command::new が97箇所(同じ grep、テストコードを含む)。harness_core は verdict.rs を持つが fs/proc の境界ラッパを持たない。shell.rs は素の Command を返し、git_probe.rs は bool を返す。(3) 既にある機械ゲートは check-fail-open.py だが、これは行単位のテキスト走査であり、自身の docstring が『a line-level scan cannot tell whether the caller treats None as fail-open』と限界を明記している。正規表現に見える形しか捕まらない。つまり検出器は本質的に上限を持っており、その上限の外側を型で閉じるのが次の一手である。

## definition_of_done
- ワークスペース直下の Cargo.toml が clippy の lints セクションを持ち、gate crate 側の Cargo.toml がそれを継承する設定を持つ。unwrap_used と expect_used が gate crate で deny になっていることを、違反を1件わざと書いて clippy が非0で終了するのを観測してから消す (RED を先に見る)ことで確認している。手貼りされている15ファイル分の deny 指定が ワークスペース設定へ集約され、重複したまま残っていない。
- enforce の置き場所は local である。clippy ゲートを GitHub の required status check として 登録しない。これは backlog 7ecf3797 の完了条件に書かれていた『clippy ジョブが required status check として列挙される』を意図的に採用しないという判断であり、理由は CLAUDE.md 第7節(ブロックと許可を決める権限を外部サービスに預けない)。不採用の理由が backlog 項目と charter の両方に 明文で残っており、散文が実挙動と一致している。
- harness_core が fallible な入力境界のラッパを提供し、その返り値が既存の三値型 Determination である。少なくともディレクトリ走査・ファイル読み出し・subprocess 実行の3経路を覆う。新しい三値型を作らず既存のものを再利用していること、および Result を bool へ潰す近道を 型として提供していないこと(Default も From<bool> も unwrap_or も生えていないこと)を コンパイル失敗テストで固定している。
- GATE_CRATES 内で標準ライブラリの生のディレクトリ走査・ファイル読み出し・subprocess 実行を 直接呼んでいる箇所が機械的に検出され、検出時に local のゲートが非0で終了する。段階移行の ための許可リストを持ってよいが、その件数は baseline として固定され、増える方向の編集が ゲートで止まる(ratchet)。baseline の初期値は測定コマンドと測定点つきで記録されている。
- アンチ空虚の対照実験を記録している。上記の型とゲートが実在する fail-open を実際に捕まえる ことを、既知の未修正インスタンス少なくとも1件に対して観測している(導入前は緑、導入後は赤、修正後にまた緑)。何も検出しない検出器は常に緑なので、これが無ければ他の4項目は 『通ること』しか証明しない。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
出力側は閉じ、入力側が開いている。harness_core::verdict は Clean を private witness でしか
作れなくし、Determination から unwrap_or / ok / unwrap_or_default を外し、Default も From<bool> も
生やさないことで『判定不能を問題なしへ潰す』最短経路を型として消した。21ファイルがこの共有型へ収斂済み。
しかし fallible な入力側 — ディレクトリ走査・ファイル読み出し・subprocess 実行 — は素の std のまま。
harness_core は verdict.rs を持つが fs/proc の境界ラッパを持たず、shell.rs は素の Command を返し、
git_probe.rs は bool を返す。つまり Result を bool へ潰す近道が入口側には残っている。

現在ある機械ゲート check-fail-open.py は行単位のテキスト走査で、自身の docstring が
『a line-level scan cannot tell whether the caller treats None as fail-open』と限界を明記している。
正規表現に見える形しか捕まらない以上、検出器の上限の外側は型でしか閉じられない。

サイズ測定(測定日 2026-07-22 / 測定点 56872974):
- GATE_CRATES の Cargo.toml と workspace 直下の Cargo.toml に lints セクションは1つも無い
  (grep -n 'workspace.lints|[lints]' が全 Cargo.toml で無ヒット)。deny(clippy::panic) は15ファイルに手貼り。
- .unwrap() / .expect( の総数は6 crate で526件だが、#[cfg(test)] より前(production)に限ると
  blastguard 0 / propguard 0 / specguard 7 / stuckguard 2 / mutategate 0 / overwatch 1 の合計10件だけ。
  つまり workspace lints を deny で入れる実コストは『テスト内を allow にする設定 + production 10箇所』で、
  s〜m 相当。爆発しない。
- 生の fs::read_dir / fs::read_to_string / Command::new は GATE_CRATES で97箇所(テスト込み)。
  こちらは DoD3/DoD4 の対象で、許可リスト + ratchet による段階移行が要る。

したがって最初の一手は DoD1+DoD2 の束 — workspace lints を入れて gate crate で unwrap_used/expect_used を
deny にし、テスト内は allow にし、production 10箇所を潰し、手貼り15箇所を集約し、enforce を local に置く。
先に違反を1件わざと書いて clippy が非0で終了する RED を観測してから始めること。
DoD3(harness_core の fs/proc ラッパ)・DoD4(直接呼び出しの検出と ratchet)・DoD5(対照実験)は
この束が入ってからの方が安く効くので次の一手に回す。

## next_action

## parked
- [達成 2026-07-22 / 49e8daf7 / outcome #40] 慢性赤 CI 検知の blocking 化(旧 north_star)。pre-push は rc=1 と未知 rc で exit 1、rc=3 のみ carve-out として advisory。利害関係のない agent が書いた6件のテストが RED->GREEN で固定済み。
- mutation gate が現在まったく機能していない(実測 2026-07-22): mutation workflow が2連続失敗中で、原因は specguard の ack_succeeds_when_new_commit_exists と ack_blocks_when_raised_at_poisoned_even_with_healthy_head が CI でだけ panic すること(ローカルは 12/12 pass)。cargo-mutants の baseline が取れず exit 4 で fail-closed するため、kill-rate という検出機構が完全に死んでいる。同型の再発: donegate の backlog b0a794f6。3回目の失敗で慢性閾値に届き、今日入れた blocking gate が実際に push を止める側に回る。
- ゲート自身のテストがどこからも実行されない(backlog c3a98510 p1)。pre-commit の契約テスト・bypass ledger のテスト・git hook coverage のテストが どの workflow からも呼ばれず pre-commit のスキャナ列にも無い。他の checker は全部 CI job を持つのに hook 本体のテストだけ持たない非対称。
- 型で閉じたあとの次の層(今回は着手しない): 単調性 proptest で入力の質を下げて verdict が permissive 側へ動かないことを強制する(backlog a7d41587)、環境を変異させて undetermined 経路を機械的に踏むフォールト注入ハーネス(66e305b5)、Undetermined の発生量を overwatch へ記録して本番で観測可能にする(6d493e39)。いずれも今回の型ラッパが入ってからの方が安く効く。
- 旧 DoD2 GitHub required status check で merge をブロックする件: 第7節により機構撤去。CI ruleset は advisory へ降格し、enforce は local pre-commit へ移管済み。再登録は第7節に反するため意図的に行わない。今回の north_star の DoD2 も同じ判断を継承する。
- [達成 2026-07-22] 慢性的に赤い2 workflow(semver-checks・build & commit plugin binaries)の実修理(旧 north_star、outcome #39 Forward)。
- [達成 2026-07-22] fail-open ゲート機構の型・enforce・host 非依存化(旧 north_star)。DoD1 は構造適合 6 crate で完了、check-fail-open --all は 0 件、enforce は local pre-commit。詳細は git log 43ce376a 周辺。
- [達成 2026-07-22] plugin rollout drift の機械的 blocking 化(旧 north_star)。詳細は git log 75b91230 周辺。drift 37件は 2026-07-22 に canary rollout で解消済み(backlog 6a9f4ac1 close)。
- [達成 2026-07-22] donegate の harness_core verdict 移行と trybuild による型契約の固定(6d4312c5 / 8685f760)。condukt run-20260722-072334 は 4/4 verified で gate PASS。残る限界は backlog 19ccedd8 に記録(donegate が bin-only のため control が gate.rs を コンパイルしない)。
- specguard・stuckguard・overwatch の harness_core verdict 非適合判定(2026-07-22): 型を無理に統合するのではなく、必要なら別の共有型で個別に判断する。今は着手しない。
- 旧 north_star 出荷済み並列衝突ハードニングの validate 閉環。overwatch は drift 無しで live。残るのは runtime-conflict merge-hold contended-skip の時間窓集計 surface と before-after delta の evidence 化。
- backlog 6267bfbe(p1): bypass ledger が git commit --amend を未検証コミットとして誤記録する。誤検知側なので危険ではないが、amend は日常操作なのでゲートの狼少年化を招く。

