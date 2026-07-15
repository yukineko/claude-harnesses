## north_star
backlog 97ec7512: .compass/charter.md の未コミット変更をcommitするか破棄するか決める

## definition_of_done
- git log --oneline -- .compass/charter.md の既存18件超の慣行(docs(compass): re-carve charter — north_star = ... 形式)に従い、現在の未コミット差分をcommitする
- backlog 97ec7512 を done にする

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
charter.mdは過去18回以上 re-carveのたびにcommitされてきた確立済み慣行があるため、今回も同様にcommitするのが一貫した扱い。破棄する理由(charterが誤っている等)は無い。

## next_action
git commit -m "docs(compass): re-carve charter — north_star = 97ec7512 charter diff commit-or-discard"

## parked
- backlog 942c7d0b: specguard.toml の area/invariant を AEGIS 全域に拡張する — harness plugin自体の実装ではなく別プロダクト(AEGIS)向けの設定作業のため、ユーザー指示によりスコープ外。backlogにはpendingのまま残す。

