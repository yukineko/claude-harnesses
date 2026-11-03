# fde

Forward Deployed Engineer 用の harness。外部の target リポジトリに対して、推定した仕様を人間に批准させ、
批准前の変更を拒否し、要件ごとの RED / GREEN 証跡を target の外に記録する。Rust 製の決定論バイナリと
`/fde` skill から成る。

## 状態(導入状況)

**rollout・enable 済み。** 2026-11-03 に一度 parked(rollout と enable の保留)とされたが、同日のユーザー裁定で
解除され、`scripts/rollout-plugins.sh` で配布・有効化された(`scripts/parked-plugins.json` の経緯メモを参照)。
稼働ハーネスに反映されるのは `scripts/rollout-plugins.sh` を実行した版だけである。

## 対象プラットフォーム

**Unix 系(WSL を含む)が対象。Windows は対象外。**(2026-11-03 のユーザー裁定。制限ではなく対象範囲の定義)
`src/target.rs` は `std::os::unix` を使い、CMD は `/bin/sh -c` で実行する。

## 役割分担

- **LLM(`/fde` skill と委譲先 agent)**: target の読解、要件素材と逐語引用の作成、テスト記述、実装。
- **`fde` バイナリ**: target の解決、spec の登録と批准の記録、批准前変更の拒否(PreToolUse gate)、
  証跡の記録と完全性の判定。
- **specforge**: `specforge draft` だけを再利用して下書き spec(Spec IR TOML)を作る。config と出力は
  fde の state dir 側に置き、target には置かない。`specforge ratify` / `specforge queue` は使わない。
- **tdd / condukt**: RED / F→P オラクルの証跡(`tdd red` / `tdd green` / `tdd oracle`)と、実装の委譲
  (`condukt` / `condukt-loop`)。テスト agent と実装 agent は分ける。

## target の解決

`TARGET` はどのサブコマンドでも省略可能な位置引数。

- 省略時は、呼び出し cwd を含む git top-level(`git rev-parse --show-toplevel` を cwd で実行)。
  cwd が git work tree の外なら判定不能(exit 2)。素の cwd へのフォールバックは無い。
- 相対パスは呼び出し cwd 基準。パスは実在し、symlink 解決後の正規パスにし、それを含む git top-level 全体が target。
- 継承された `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` などは probe から除かれる。
- state key は正規化した top-level パスの SHA-256(16 進)。同じ repo を別の書き方・別の下位ディレクトリから
  指しても同じ state を共有する。

## コマンドと終了コード

| コマンド | 内容 | 終了コード |
|---|---|---|
| `fde resolve [TARGET]` | `{"root", "state_key"}` を JSON で出力 | 0 解決 / 2 判定不能 |
| `fde begin [TARGET] --spec FILE` | 下書き spec を登録。target は未批准になる | 0 登録 / 1 拒否(spec が無い・ファイルでない・target の中にある)/ 2 判定不能 |
| `fde ratify [TARGET] [--spec FILE]` | 登録 spec の批准を記録(内容の SHA-256 を fingerprint とし、target の HEAD コミットと dirty かどうかも記録する) | 0 記録 / 1 拒否(未登録、または FILE が登録 spec でない)/ 2 判定不能(HEAD・dirty が判定できない場合を含む。例: コミットが 1 つも無い repo。このとき何も記録しない) |
| `fde status [TARGET]` | 批准状態の照会 | 0 批准済み / 1 未批准(未登録・批准後の spec 変更を含む)/ 2 判定不能(state が読めない・壊れている、批准記録後に spec が読めない・無い) |
| `fde gate` | PreToolUse hook の入口(stdin で payload を読む) | 0 allow / 2 deny |
| `fde evidence red\|green\|pass --req ID --cmd CMD [TARGET]` | target root で CMD を実行し、終了ステータスで記録可否を決める | 0 記録 / 1 拒否 / 2 判定不能 |
| `fde evidence verify [TARGET]` | 全要件の証跡が揃っているか | 0 完了 / 1 欠け(欠けた要件を stderr に列挙)/ 2 判定不能(未批准・要件ゼロ・ledger 不整合、および下の「HEAD の来歴」規則を満たさない・判定できない場合を含む) |
| `fde report [TARGET]` | 人間向けの一覧(未批准でも `ratification: UNRATIFIED` を出力。批准時 HEAD と clean/dirty、現在の HEAD、要件ごとの各記録の種類・HEAD・clean/dirty を含む) | 0 完了(批准済みかつ全要件が RED→GREEN)/ 1 未完了(未批准〈未登録・批准後の spec 変更を含む〉、または unresolved・unmeasured・PASS のみの要件がある。理由を stderr に出す)/ 2 判定不能(store・spec・ledger が読めない・壊れている・不整合、要件ゼロ、「HEAD の来歴」規則を満たさない・判定できない、stdout へ書けない) |

- exit 2 は常に「判定不能」(gate では deny)で、pass ではない。受け取ったら止めて人間に聞く。
- 非 0 終了では必ず stderr に理由が出る。引数エラー(clap)も exit 2。
- **`fde report` の終了コードは「批准済みかつ全要件が RED→GREEN で検証済みか」の判定である**(2026-11-03 のユーザー裁定:
  「Exit 0 only when it is ratified and every requirement is verified RED→GREEN」)。一覧は従来どおり全ケースで stdout に
  出る(未批准なら `ratification: UNRATIFIED` / `evidence: not evaluated`)。exit 0 は批准済みかつ全要件が RED→GREEN の
  ときだけ。未批准(未登録・未批准・批准後の spec 変更)や、unresolved・unmeasured・PASS のみの要件があれば exit 1 で、
  理由(未批准の理由、または該当要件)を stderr に出す。
- **`fde evidence verify` との違い**: verify は PASS(最初から通る要件)も完了と数え、未批准を 2(判定不能)と答える。
  report は PASS のみの要件を未完了(1)とし、未批准を 1(`fde status` と同じく「否と判定済み」)と答える。
  したがって verify が 0 でも report が 1 のことがある(PASS のみの要件がある場合)。
- `evidence` の拒否(1)の条件: 未批准(批准後の spec 変更を含む)、ID が批准 spec に無い、同じ ID・同じ CMD の
  RED が無い GREEN、RED 後の PASS、CMD の終了ステータスが種類と矛盾(RED は非 0、GREEN / PASS は 0 が必要)。
  判定不能(2)の条件: store・spec・ledger が読めない・壊れている、HEAD を記録していない旧形式の批准(再批准が必要)、
  現在の HEAD が批准時 HEAD の子孫(または同一)でない・比較できない(両コミットを stderr に出す)、HEAD・dirty が
  判定できない、CMD がシグナルで終了・タイムアウト(1 時間)・起動失敗・126/127 で終了。いずれも何も記録しない。

### HEAD の来歴(forward history のみ)

- `fde ratify` は target の HEAD(`git rev-parse --verify HEAD`、40 桁小文字 hex)と dirty(`git status --porcelain`
  が何か出力したか。未追跡ファイルは含み、ignore されたファイルは含まない)を `ratification.json` に記録する。
- `fde evidence red|green|pass` の各記録も、CMD を実行する**直前**に観測した HEAD と dirty を持つ。
- R = 批准時 HEAD、C = 現在の HEAD、H = 数えられる(現在の fingerprint の)各記録の HEAD とする。
  - `evidence red|green|pass`: CMD 実行前に R が現在の HEAD の祖先または同一であること
    (`git merge-base --is-ancestor`)。祖先でない(reset・ブランチ切り替え・履歴の書き換え)か比較できなければ 2。
  - `evidence verify` / `report`: C が判定でき、R ≤ C、かつ数えられる全記録で R ≤ H ≤ C であること。
    1 つでも満たさない・比較できない(例: リポジトリに無いコミット)なら 2(pass / 完了とは言わない)。
- **dirty は記録・表示するが、それだけでは失格にしない**。ただし dirty な記録はコミットからは再現できない。
- HEAD を持たない旧形式の `ratification.json`(schema 1)は `fde status` では従来どおり読める(批准済みのまま)が、
  `evidence red|green|pass|verify` と `report` は再批准を求めて 2 になる。HEAD を持たない記録(旧形式 ledger
  由来)が数えられる場合も verify / report は 2。

## `/fde` のフロー

`/fde [target-dir]`。詳細手順は `skills/fde/SKILL.md`。

1. **把握**: target を読み、`file:line` の逐語引用付きで要件素材を state dir 側に書く。`specforge draft` で
   Spec IR TOML の下書きを作り、`fde begin` で登録する。
2. **批准**: 推定 spec を根拠とともに `AskUserQuestion` で人間に示す。自答しない(自律モードでも escalate)。
   明示的な承認があったときだけ、人間自身が `fde ratify` を実行する(モデルは実行しない)。`fde status` が 0 になるまで先へ進まない。spec を変えると批准は外れる。
3. **RED**: テスト agent が要件ごとにテストを書き、`fde evidence red` と `tdd red` で失敗を記録する。
   最初から通る要件は `fde evidence pass` で適合済みとして記録する。
4. **fix**: `condukt` / `condukt-loop` に実装を委譲する(テスト agent とは別)。target root に統合後、RED と同じ CMD で
   `fde evidence green`、`tdd green`、`tdd oracle`。
5. **報告**: `fde evidence verify`(PASS も完了と数える)と `fde report`(批准済みかつ全要件 RED→GREEN のときだけ 0)の
   終了コード、`fde report` の unresolved(RED のみで GREEN 無し)と unmeasured(証跡なし)、やっていないことを逐語で報告する。

## gate(PreToolUse)

`hooks/hooks.json` で matcher `Bash|Edit|Write|MultiEdit|NotebookEdit` に `fde gate` を登録する。
`fde begin` 済みで未批准(または状態が判定不能)の target を「保護対象」とし、その範囲(root と、その repo の
全 worktree)への編集を拒否する。

- Edit 系: 対象パスを正規化し、保護範囲の中なら deny。
- Bash: 正規化した cwd が保護範囲の中、またはコマンド文字列が root / worktree のパスを文字列として含むなら deny。
  例外(2026-11-03 のユーザー裁定): 状態が「未批准」の target に対しては、`fde status` / `fde report` /
  `fde begin` の**ちょうど 1 回の単独呼び出し**だけ allow する。判定は保守的な許可リストで、コマンド全体が
  英数字と `-_./=:,@+%` だけの語を空白/タブで区切ったもので、第 1 語がちょうど `fde`、第 2 語が
  `status` / `report` / `begin` のいずれか、以降の語が `ratify` を含まないときだけ通る(連結・リダイレクト・
  置換・引用符・環境変数の前置・パス指定の `fde`・ラッパーはすべて deny)。`fde ratify` は人間専用で、
  未批准の間は deny のまま。人間が承認したら、人間自身が Claude Code のプロンプトで `! fde ratify [TARGET]`
  または自分の端末で実行する。状態が判定不能な target、Edit 系ツールにはこの例外は適用されない。
- 空・壊れた payload、読めない state、matcher 外のツール(登録 target がある場合)、panic はすべて deny。
- 登録 target が 1 つも無ければ、すべて allow。

## state の配置

state dir は環境変数 `FDE_STATE_DIR`(設定済みかつ空でない場合)、それ以外は `~/.fde`。解決できなければ判定不能。

```
<state_dir>/targets/<state_key>/
  registration.json   {schema: 1, root, spec}                             fde begin が書く
  ratification.json   {schema: 2, spec, fingerprint, head, dirty}         fde ratify が書く
  evidence.json       {schema: 2, records: [{fingerprint, req, kind, cmd, exit_code, head, dirty}, ...]}
                                                                          fde evidence red/green/pass が追記
```

- `head` は 40 桁小文字 hex のコミット ID、`dirty` は bool。
- 旧形式: `ratification.json` の schema 1(`{schema, spec, fingerprint}`)は読めるが evidence 系では判定不能
  (上記)。`evidence.json` の schema 1(記録に `head` / `dirty` なし)も読め、次の追記で schema 2 として書き直される
  (旧記録は `head` なしのまま)。未知の schema、schema 2 で `head` / `dirty` の片方だけを持つ記録は判定不能。

- いずれも一時ファイル + rename で原子的に書く。`fde begin` は既存の `ratification.json` を先に消す。
- ledger の各記録は記録時の批准 fingerprint を持ち、現在の fingerprint と一致するものだけが判定に数えられる。
- 要件の状態: RED の CMD に同じ CMD の後続 GREEN が無ければ unresolved、GREEN(RED の後)か PASS があれば complete、
  どれも無ければ unmeasured。
- specforge の config・要件素材・下書き spec も state dir 側(例: `<state_dir>/specforge/<state_key>/`)に置く。

## 制限

- 対象プラットフォームは Unix 系(WSL 含む)。Windows は対象外(上記「対象プラットフォーム」。制限ではなく対象範囲)。
- **gate の Bash 判定は文字列照合**。コマンドの構文解析はしない。外から相対パスで target を指す、正規形でない
  symlink 経由で指す、といった書き方は検出しない。
- **登録済み target を削除すると**、その state dir(`<state_dir>/targets/<state_key>/`)を消すまで gate は編集を deny し続ける。
- **HEAD は前進のみ許す**。批准後に HEAD を reset・別ブランチへ切り替え・履歴の書き換えをすると、批准をやり直しても
  批准時 HEAD と各記録の HEAD・現在の HEAD の関係が崩れたままなら verify / report は 2 のまま。同じ spec のまま
  (fingerprint が同じまま)再批准しても既存の記録は数えられ続けるため、ledger(state dir 側の `evidence.json`)を
  どう扱うかは人間が判断する必要がある。HEAD を持たない旧形式の記録も同様に数えられ続ける。
- **dirty は失格条件ではない**。dirty な tree で取った証跡も数える(report に `dirty` と出る)が、その状態は
  コミットからは再現できない。
- SHA-256 object format の repo(64 桁のコミット ID)は HEAD を判定できないため、ratify・evidence は 2 になる。
- 同時に 2 つの記録が走ると片方の追記が失われうる(最後の rename が勝つ)。ledger が「より不完全」になる方向にしか
  起きない。
- `specforge draft` の再利用で target が変わらないことは stub agent で確認しただけで、実 LLM agent については未確認。
