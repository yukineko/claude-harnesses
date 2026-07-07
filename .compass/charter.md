## north_star
code-RAG epic (backlog 32739700) slice-3 = interpreter に注入される決定論 code index を stale-blind から auto-refresh にする。slice-2 が明示 park した follow-on。現状の `build-if-absent` は一度建てたら二度と更新されず、worker が編集した後の Phase 5/6 では既に古い file:line を配る。決定論の cheap fingerprint (git ls-files の .rs 集合の path+size+mtime の hash・外部 API/embedding/content 読取り無し・std DefaultHasher の固定キー) を index の sidecar meta (.fugu/code-index.meta.json) に記録し、`code-index build --if-stale` が fingerprint 一致なら no-op・不一致なら再 build + meta 更新する。condukt SKILL Phase-1 の code_context 取得を build-if-absent → build --if-stale に差替え、全 consumer が新鮮な index を読む土台にする。純加算・後方互換 (引数無しの plain build は従来どおり常に再 build・fugu 不在/検索空は no-op・JSONL は Symbol-per-line 不変で load_index 無改変)。worker/verifier context への注入拡張は後続 slice へ parked。

## definition_of_done
- harness-core code_index.rs に純関数の決定論 fingerprint helper と fail-soft な meta sidecar I/O を追加する (既存 extract_symbols/write_index/load_index と同居・同族)。fingerprint は (rel path, size, mtime) の tuple 集合を内部で sort してから std の DefaultHasher (固定キー=決定論) で畳んだ hex 文字列で、file 内容は読まない (cheap check)。meta の read/write は missing/corrupt を fail-soft (panic せず None/no-op)。unit test で: 同一 stat 集合→同一 fingerprint / path または size または mtime のいずれか変化→異なる fingerprint / 入力順に依存しない (順不同で同一) / pathological 入力で never panic を assert。cargo test -p harness-core green
- fugu-router `code-index build` に `--if-stale` フラグを追加する。git ls-files の .rs 集合を stat して現在の fingerprint を計算し、sidecar meta が在り stored fingerprint と一致するなら再 build を skip して no-op を報告する (JSON に rebuilt:false)、不一致 or meta 不在なら従来どおり index を wholesale 再 build した上で sidecar meta を更新する (rebuilt:true)。引数無しの plain `build` は従来どおり常に再 build + meta 書込み (後方互換)。build の git ls-files→.rs 列挙は helper に factor out して両経路で共有する。観測: 無変更 tree で `build --if-stale` を2回叩くと2回目が rebuilt:false かつ index の bytes 不変、.rs を1つ touch すると `--if-stale` が rebuilt:true になる。cargo test -p fugu-router green (temp git repo を駆動する build/--if-stale の integration test 追加)
- condukt SKILL Phase-1 の code_context 注入ブロックを file-existence gate から auto-refresh に差替える: `if [ ! -f .fugu/code-index.jsonl ]; then fugu-router code-index build; fi` を `fugu-router code-index build --if-stale >/dev/null 2>&1 || true` に置換し (fail-soft 維持)、索引が source の変化に自動追従するようにする。境界隔離 untrusted 注入・no-override caveat・fugu 不在/検索空 [] の no-op・既存 Phase-1 出力形は不変。grep で SKILL が file-existence gate ではなく `build --if-stale` を呼ぶことを確認できる
- 純加算・後方互換: fugu-router lockstep bump 0.1.7→0.1.8 と condukt lockstep bump 0.7.32→0.7.33 (各 Cargo.toml + .claude-plugin/plugin.json + marketplace.json + Cargo.lock を同時)。sidecar meta (.fugu/code-index.meta.json) も .fugu/ 配下ゆえ既存 .gitignore で ignore 済み (build 後 git status --short に .fugu/ が出ないこと)。既存 slice-1 索引コア (Symbol/extract/search/JSONL 形式)・slice-2 注入様式・lessons/KNOWLEDGE/deepwiki 注入は untouched。check-plugin-versions.py + check-version-bumped.py green、fmt + clippy(-D warnings) clean、cargo test -p harness-core / -p fugu-router / -p condukt green
- 検証: 別モデルで独立に、無変更 tree で `--if-stale` の2回目が no-op (rebuilt:false・index bytes 不変) になること、.rs 変更後に再 build が走ること、SKILL の差替えが grep で確認できること、全 version/fmt/clippy/test ゲートが green であることを再確認する (build ≠ validate)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(condukt に注入される決定論 code index が source 変化に auto-refresh し、interpreter だけでなく全 consumer が新鮮な file:line を読む) − 現状 の最大差分: slice-2 で interpreter への注入配線は landed + rolled out したが、索引は build-if-absent の stale-blind — 一度建てたら二度と更新されず、worker が編集した後の Phase 5/6 では古い snapshot を配る。差分を埋める最小 slice(size m) = harness-core に決定論 fingerprint(git ls-files の .rs 集合の path+size+mtime hash・content 非読取り) + meta sidecar helper、fugu-router code-index build に --if-stale(fingerprint 一致=no-op / 不一致=再build+meta更新)、condukt SKILL Phase-1 を build-if-absent → build --if-stale に差替え + fugu 0.1.7→0.1.8 / condukt 0.7.32→0.7.33 lockstep bump。retrieval/fingerprint は決定論コード、query 文面のみ LLM。worker/verifier context への注入拡張は後続 slice へ parked。

## next_action

## parked

