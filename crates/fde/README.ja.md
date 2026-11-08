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
| `fde status [TARGET]` | 批准状態の照会 | 0 批准済み / 1 未批准(未登録・批准後の spec 変更を含む。TARGET が存在せず、登録済み target の root〈最も近い既存の祖先を正規化して比較するので symlink 経由の綴りも一致。記録された root そのままの綴りも可〉だった場合も 1 で、stderr に `gone` と `fde forget` を出す)/ 2 判定不能(state が読めない・壊れている、存在しない TARGET が登録済み root でもない、批准記録後に spec が読めない・無い) |
| `fde forget [TARGET]` | **人間専用**。target の state dir `<state_dir>/targets/<state_key>/` を丸ごと消す(登録・批准・証跡)。中身はパースしないので壊れた登録も消せる。target 自体には一切触れない。key は、存在する TARGET(省略時は cwd)なら git top-level。存在する TARGET を git が解決できない(例: `.git` を消して repo でなくなった登録 root)ときは TARGET の正規化パスで引くので、そうなった登録 root も消せ(0)、未登録の非 git ディレクトリは拒否(1、理由に git の失敗を含む)。TARGET 自体を正規化できなければ 2。TARGET 省略(cwd)にはこの代替は無い(2)。存在しない TARGET は最も近い既存の祖先を正規化したパス、次に字句的な絶対パスで引く | 0 削除 / 1 拒否(その key の state dir が無い = 未登録)/ 2 判定不能(解決できない・消せない。消せないときは何も消していない。`targets/` の外へ移した後に削除だけ失敗した場合は、未登録になったが残骸が残っている旨を理由に出す) |
| `fde gate` | PreToolUse hook の入口(stdin で payload を読む) | 0 allow / 2 deny |
| `fde evidence red\|green\|pass --req ID --cmd CMD [TARGET]` | target root で CMD を実行し、終了ステータスで記録可否を決める | 0 記録 / 1 拒否 / 2 判定不能 |
| `fde evidence verify [TARGET]` | 全要件が RED→GREEN で検証済みか(PASS のみの要件は完了に数えない) | 0 完了(全要件が RED の後に GREEN)/ 1 欠け(unresolved・unmeasured・PASS のみの要件を stderr に列挙)/ 2 判定不能(未批准・要件ゼロ・ledger 不整合、および下の「HEAD の来歴」規則を満たさない・判定できない場合を含む) |
| `fde report [TARGET]` | 人間向けの一覧(未批准でも `ratification: UNRATIFIED` を出力。批准時 HEAD と clean/dirty、現在の HEAD、要件ごとの各記録の種類・HEAD・clean/dirty を含む) | 0 完了(批准済みかつ全要件が RED→GREEN)/ 1 未完了(未批准〈未登録・批准後の spec 変更を含む〉、または unresolved・unmeasured・PASS のみの要件がある。理由を stderr に出す)/ 2 判定不能(store・spec・ledger が読めない・壊れている・不整合、要件ゼロ、「HEAD の来歴」規則を満たさない・判定できない、stdout へ書けない) |

- exit 2 は常に「判定不能」(gate では deny)で、pass ではない。受け取ったら止めて人間に聞く。
- 非 0 終了では必ず stderr に理由が出る。引数エラー(clap)も exit 2。
- **`fde report` の終了コードは「批准済みかつ全要件が RED→GREEN で検証済みか」の判定である**(2026-11-03 のユーザー裁定:
  「Exit 0 only when it is ratified and every requirement is verified RED→GREEN」)。一覧は従来どおり全ケースで stdout に
  出る(未批准なら `ratification: UNRATIFIED` / `evidence: not evaluated`)。exit 0 は批准済みかつ全要件が RED→GREEN の
  ときだけ。未批准(未登録・未批准・批准後の spec 変更)や、unresolved・unmeasured・PASS のみの要件があれば exit 1 で、
  理由(未批准の理由、または該当要件)を stderr に出す。
- **PASS のみの要件は完了ではない**(2026-11-04 のユーザー裁定)。`fde evidence pass` は従来どおり PASS を記録する
  が、それだけでは要件を完了にしない。完了は RED の後の GREEN だけ(PASS → RED → GREEN も完了)。verify も report
  も PASS のみの要件を 1 とし、stderr に `<ID> (PASS only, never RED then GREEN)` と出す。report の
  `complete: N of M` 行も RED→GREEN の要件だけを数える。
- **`fde evidence verify` との違い**: 違いは未批准の扱いだけ。verify は未批准を 2(判定不能)と答え、report は
  1(`fde status` と同じく「否と判定済み」)と答える。批准済みの target では両者の 0 / 1 は一致する。
- `evidence` の拒否(1)の条件: 未批准(批准後の spec 変更を含む)、ID が批准 spec に無い、同じ ID・同じ CMD の
  RED が無い GREEN、RED 後の PASS、CMD の終了ステータスが種類と矛盾(RED は非 0、GREEN / PASS は 0 が必要)。
  判定不能(2)の条件: store・spec・ledger が読めない・壊れている、現在の fingerprint で `ratified_head` を持たない
  記録がある、HEAD を記録していない旧形式の批准(再批准が必要)、
  現在の HEAD が批准時 HEAD の子孫(または同一)でない・比較できない(両コミットを stderr に出す)、HEAD・dirty が
  判定できない、CMD がシグナルで終了・タイムアウト(1 時間)・起動失敗・126/127 で終了、**CMD の実行中に HEAD が
  変わった**(後退・前進〈新しい子孫コミット〉・その他どんな変化も。CMD 後に HEAD を観測できない場合を含む。終了
  ステータスより先に判定するので、RED の CMD が非 0 で終わっても 2。前後のコミットを stderr に出す)。いずれも何も
  記録しない。

### HEAD の来歴(forward history のみ)

- `fde ratify` は target の HEAD(`git rev-parse --verify HEAD`、40 桁小文字 hex)と dirty(`git status --porcelain`
  が何か出力したか。未追跡ファイルは含み、ignore されたファイルは含まない)を `ratification.json` に記録する。
- `fde evidence red|green|pass` の各記録も、CMD を実行する**直前**に観測した HEAD と dirty を持つ。さらに記録時点の
  批准時 HEAD を `ratified_head` として持つ(証跡の **epoch**)。
- **数えられる記録**は、fingerprint が現在の批准 fingerprint と一致し、**かつ** `ratified_head` が現在の批准時 HEAD と
  一致するものだけ。それ以外は **stale**(ディスクには残るが、記録時の前提条件・verify・report のすべてで無視され、
  report の `ignored:` 行で件数が出る)。
- したがって**再批准は新しい証跡 epoch を始める**: 別の HEAD で再批准すると以前の記録は残るが無視され、要件は
  unmeasured(1)に戻るので、RED / GREEN / PASS を記録し直す。reset 等で批准した履歴から外れたとき(2)の回復は、
  人間が現在の HEAD で再批准すること。**前進したコミットで再批准した場合も以前の証跡は判定から外れる**ことに注意。
  同じ HEAD での再批准では epoch は変わらず、記録は数えられ続ける。この「再批准は以前の証跡を捨てる」挙動は
  2026-11-04 のユーザー裁定で現状維持と決まった。
- **CMD は HEAD を動かしてはならない**(2026-11-04 のユーザー裁定)。`evidence red|green|pass` は CMD の直前に観測した
  HEAD を CMD の終了直後(終了ステータスの判定より前)にもう一度観測し、読めない・少しでも違う(後退・前進・その他)
  なら 2 で何も記録しない。前進・観測不能も拒否することは 2026-11-05 のユーザー裁定で承認された: 記録の HEAD が CMD の結果が
  属する tree を表さなくなり、また CMD が自分で修正をコミットすれば R ≤ H ≤ C の規則を満たせてしまうため。
- R = 批准時 HEAD、C = 現在の HEAD、H = 数えられる(現在の epoch の)各記録の HEAD とする。
  - `evidence red|green|pass`: CMD 実行前に R が現在の HEAD の祖先または同一であること
    (`git merge-base --is-ancestor`)。祖先でない(reset・ブランチ切り替え・履歴の書き換え)か比較できなければ 2。
  - `evidence verify` / `report`: C が判定でき、R ≤ C、かつ数えられる全記録で R ≤ H ≤ C であること。
    1 つでも満たさない・比較できない(例: リポジトリに無いコミット)なら 2(pass / 完了とは言わない)。
- **dirty は記録・表示するが、それだけでは失格にしない**。ただし dirty な記録はコミットからは再現できない。
- HEAD を持たない旧形式の `ratification.json`(schema 1)は `fde status` では従来どおり読める(批准済みのまま)が、
  `evidence red|green|pass|verify` と `report` は再批准を求めて 2 になる。現在の fingerprint と一致するが
  `ratified_head` を持たない記録(旧版 fde が書いたもの。旧形式 ledger 由来を含む)は epoch を推測せず、それが
  ledger にある限り `evidence red|green|pass|verify` と `report` は 2(人間が ledger の扱いを決める。例: target の
  state を消して `fde begin` からやり直す)。

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
5. **報告**: `fde evidence verify`(全要件 RED→GREEN のときだけ 0。PASS のみは完了に数えない)と `fde report`(批准済みかつ全要件 RED→GREEN のときだけ 0)の
   終了コード、`fde report` の unresolved(RED のみで GREEN 無し)と unmeasured(証跡なし)、やっていないことを逐語で報告する。

## gate(PreToolUse)

`hooks/hooks.json` で matcher `Bash|Edit|Write|MultiEdit|NotebookEdit` に `fde gate` を登録する。
`fde begin` 済みで未批准(または状態が判定不能)の target を「保護対象」とし、その範囲(登録された target
ディレクトリの root のみ)への編集を拒否する。target の repo の他の worktree は列挙も検査もしない
(`git worktree list` を走らせない。ユーザー裁定 2026-11-08: ゲートは他の worktree を見ない)。

- Edit 系: 対象パスを正規化し、保護範囲の中なら deny。
- Bash: 正規化した cwd が保護範囲の中、コマンド文字列が root のパスを文字列として含む、または
  コマンドの語を解決したパスが保護範囲の中なら deny。語は小さな保守的 lexer で切り出す: 引用符の外の空白(改行を
  含む)と `;&|()<>` で区切り(`>file` / `2>file` の `file` も 1 語)、シングルクォートは字義どおり、ダブルクォートは
  中身を保つ、バックスラッシュは次の 1 文字をエスケープする。先頭の `~` / `~/` は `$HOME` に展開し、`=` を含む語
  (`--opt=value`・`NAME=value`)は値の部分も 1 語として調べる。各語は payload の cwd(正規化後)に join して
  `canonical_lenient`(存在する最も近い祖先を正規化 = symlink を辿り、残りは字句的に適用)を通すので、
  `../proj/x`・`rm -rf proj`・target を指す symlink 経由の `link/x` も検出する。
  保護対象がある間、**解決できない語**があり、かつ保護範囲に触れると判明していなければ、その target について
  判定不能(deny)とし、理由に語そのものを含める。解決できない語: 引用符の外またはダブルクォート内の `$`・バッククォート、
  引用符の外の `*` `?` `[` `{`、末尾の孤立したバックスラッシュ、`~user` 形式、`HOME` が未設定・空のときの `~`。
  閉じていない引用符はコマンド全体を解決不能にする。語のパスの正規化が `NotFound` 以外のエラー(例: 通常ファイル
  `notes.txt` に対する `notes.txt/x`)で失敗した場合も判定不能(deny)。保護対象も消えた target も無い(未登録、
  または全部が存在して批准済み)ときは語を判定に使わない。
  例外(2026-11-03 のユーザー裁定): 状態が「未批准」の target に対しては、`fde status` / `fde report` /
  `fde begin` の**ちょうど 1 回の単独呼び出し**だけ allow する。判定は保守的な許可リストで、コマンド全体が
  英数字と `-_./=:,@+%` だけの語を空白/タブで区切ったもので、第 1 語がちょうど `fde`、第 2 語が
  `status` / `report` / `begin` のいずれか、以降の語が `ratify` を含まないときだけ通る(連結・リダイレクト・
  置換・引用符・環境変数の前置・パス指定の `fde`・ラッパーはすべて deny)。`fde ratify` は人間専用で、
  未批准の間は deny のまま。人間が承認したら、人間自身が Claude Code のプロンプトで `! fde ratify [TARGET]`
  または自分の端末で実行する。状態が判定不能な target、Edit 系ツールにはこの例外は適用されない。
- **`fde forget` は人間専用**(`fde ratify` と同じ)。登録 target が 1 つでもある間(消えた target を含む)、
  コマンドが `fde forget` を呼ぶ Bash は、どこで実行しても何を指しても deny する。判定は文字列ベースで保守的:
  空白(改行・タブを含む)と `;&|()<>` で語に分け、引用符とバックスラッシュを取り除いたうえで、`fde` または
  `/fde` で終わる語の後に語 `forget` があれば deny(引用符内の文字列にたまたま含まれるだけでも deny)。
  人間が自分で Claude Code のプロンプトで `! fde forget <TARGET>` または自分の端末で実行する。
- **消えた(gone)target**: 登録済み target の root が存在しない(`symlink_metadata` が `NotFound`)ことを
  観測したら、それは判定済みの事実として扱い、保護対象から外す(git の照会もしない)。無関係なパスの編集・全語を
  解決できる Bash は止めない。ただし元の場所は deny のまま: root の下を指す Edit 系パス(そのまま、または正規化後)、
  cwd が root の下にある Bash、root の文字列を含む Bash、解決した語(上記。cwd に字句的に join したもの、または
  `canonical_lenient` を通したもの)が root の下にある Bash は、`gone` と `fde forget <root>` を理由に含む deny に
  なる(上の `fde status|report|begin` の例外はここでも効く)。さらに、**解決できない語**(上記: `$`・バッククォート・
  引用符の外のグロブ・`{`・`~user`・`HOME` 未設定の `~`・孤立したバックスラッシュ・閉じていない引用符)を含む Bash、
  および語のパスの正規化が `NotFound` 以外のエラーで失敗した Bash は、元の場所を避けていると示せないので判定不能
  (deny)とし、理由に語と `! fde forget <root>` を含める(2026-11-04 のユーザー裁定)。人間が `fde forget <root>`
  を実行すると解除される。
  root の stat が `NotFound` 以外のエラー(権限など)で失敗したら判定不能として、何に触れる呼び出しでも deny する。
- 空・壊れた payload、読めない state、壊れた登録(`fde forget` で消すまで)、matcher 外のツール(登録 target が
  ある場合)、panic はすべて deny。
- 登録 target が 1 つも無ければ、すべて allow(`fde forget` も含む)。

## state の配置

state dir は環境変数 `FDE_STATE_DIR`(設定済みかつ空でない場合)、それ以外は `~/.fde`。解決できなければ判定不能。

```
<state_dir>/targets/<state_key>/
  registration.json   {schema: 1, root, spec}                             fde begin が書く
  ratification.json   {schema: 2, spec, fingerprint, head, dirty}         fde ratify が書く
  evidence.json       {schema: 2, records: [{fingerprint, req, kind, cmd, exit_code, head, dirty, ratified_head}, ...]}
                                                                          fde evidence red/green/pass が追記
```

- `head` / `ratified_head` は 40 桁小文字 hex のコミット ID、`dirty` は bool。
- 旧形式: `ratification.json` の schema 1(`{schema, spec, fingerprint}`)は読めるが evidence 系では判定不能
  (上記)。`evidence.json` の schema 1(記録に `head` / `dirty` なし)も読め、次の追記で schema 2 として書き直される
  (旧記録は `head` なしのまま)。schema 2 で `ratified_head` を持たない記録(この変更以前の fde が書いたもの)も読める
  (扱いは上記)。未知の schema、schema 2 で `head` / `dirty` の片方だけを持つ記録、`head` / `dirty` なしで
  `ratified_head` を持つ記録は判定不能。

- いずれも一時ファイル + rename で原子的に書く。`fde begin` は既存の `ratification.json` を先に消す。
- `fde forget` は `targets/<state_key>/` をまず `<state_dir>/.forgotten/<state_key>.<pid>.<nanos>` へ rename し
  (ここで失敗すれば何も変わっていない)、その後に削除する。`targets/` の下に中途半端に消えた登録は残らない。
- ledger の各記録は記録時の批准 fingerprint と批准時 HEAD(`ratified_head`)を持ち、両方が現在の批准と一致する
  ものだけが判定に数えられる(上記「HEAD の来歴」)。
- 要件の状態: RED の CMD に同じ CMD の後続 GREEN が無ければ unresolved、GREEN(RED の後)があれば complete、
  PASS だけなら PASS のみ(complete ではない。2026-11-04 のユーザー裁定)、どれも無ければ unmeasured。
- specforge の config・要件素材・下書き spec も state dir 側(例: `<state_dir>/specforge/<state_key>/`)に置く。

## 制限

- 対象プラットフォームは Unix 系(WSL 含む)。Windows は対象外(上記「対象プラットフォーム」。制限ではなく対象範囲)。
- **gate の Bash 判定は語の解決まで**(上記 gate 節)。シェルの完全な構文解析はしない。次は**検出しない(未対応)**:
  alias・シェル関数・`PATH` 解決による別バイナリの実行。コマンド内のディレクトリ移動(`cd x && ...`。相対語は
  すべて payload の cwd に対して解決する)。プログラムが実行時に組み立てるパス(パスを計算するスクリプトなど)。
  here-doc の本文はコマンドの他の部分と同じく語として切り出される(本文中の `$` などで deny になりうる)。
  また保護対象または消えた target がある間は `$`・バッククォート・グロブ・`{`・`~user` を含む Bash は無関係な
  作業でも deny される(判定不能は制限側)。
- **登録済み target を削除すると**、その target は「消えた(gone)」扱いになり、無関係な作業は止めなくなるが、
  元の root の下(Edit 系のパス・Bash の cwd・root を文字列で含む Bash・解決した語が root の下にある Bash)は
  deny され続け、`fde status <root>` は 1(`gone`)を返す。state dir を消すのは人間の `fde forget <root>` で、
  モデルには許されない。消えた target がある間は、解決できない語(`$`・グロブなど)を含む Bash と、語のパスの
  正規化が `NotFound` 以外のエラー(例: 通常ファイル `notes.txt` に対する `notes.txt/x`)で失敗した Bash は、元の
  場所を避けていると示せないので判定不能(deny。理由に語と `! fde forget <root>` を含める。2026-11-04 のユーザー
  裁定)。無関係な作業でも `$` 等を使うと止まるので、人間に `! fde forget <root>` を頼む。元の root を指す dangling な symlink 経由の語(`link/x`、`link -> <消えた root>`)は、symlink の先が
  存在しないので辿れず検出しない。
- **壊れた登録**(読めない・パースできない `registration.json` など)がある間は、gate はすべてを deny する。
  人間が `fde forget [TARGET]` で消すまで解除されない。
- **登録 root から `.git` を消して git repo でなくなった場合**も、人間が `fde forget <root>` で state を消せる
  (git が解決できないときは root の正規化パスで key を引く)。
- **HEAD は前進のみ許す**。批准後に HEAD を reset・別ブランチへ切り替え・履歴の書き換えをすると verify / report は 2。
  人間が現在の HEAD で再批准すると新しい epoch になり、以前の証跡はすべて無視される(記録し直しが要る)。前進した
  コミットでの再批准でも同じく以前の証跡は判定から外れる(2026-11-04 のユーザー裁定で現状維持)。`ratified_head`
  を持たない旧版の記録は epoch を推測しないため、再批准しても 2 のまま(ledger の扱いは人間が判断する)。
- **HEAD を動かす CMD の証跡は取れない**。CMD の実行中に HEAD が変わると(コミット・reset・checkout など、前進も
  含む)evidence は 2 で何も記録しない(2026-11-04 のユーザー裁定)。修正のコミットは CMD の外で行う。
  判定は CMD の**前と後の 2 点**の HEAD の比較だけである。CMD の途中で HEAD を動かして元に戻した場合
  (コミットしてから `reset --hard` で戻す、別ブランチへ checkout して戻る、など)は検出しない(未対応)。
- **dirty は失格条件ではない**。dirty な tree で取った証跡も数える(report に `dirty` と出る)が、その状態は
  コミットからは再現できない。
- SHA-256 object format の repo(64 桁のコミット ID)は HEAD を判定できないため、ratify・evidence は 2 になる。
- 同時に 2 つの記録が走ると片方の追記が失われうる(最後の rename が勝つ)。ledger が「より不完全」になる方向にしか
  起きない。
- `specforge draft` の再利用で target が変わらないことは stub agent で確認しただけで、実 LLM agent については未確認。
