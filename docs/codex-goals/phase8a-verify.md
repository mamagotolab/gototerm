# Phase 8a 手動確認手順

1. gototerm を起動し、サイドバーを表示する。

2. ローカルホスト名つき OSC 7 を送る。

   ```sh
   printf '\033]7;file://%s%s\033\\' "$(cat /proc/sys/kernel/hostname)" "$PWD"
   ```

   期待結果:
   - サイドバーは通常のローカル表示のまま。
   - `workspace` の cwd 表示が現在の `$PWD` に追従する。
   - files / changes / preview が従来どおり使える。

3. リモートホスト扱いの OSC 7 を送る。

   ```sh
   printf '\033]7;file://otherhost/home/naoto\033\\'
   ```

   期待結果:
   - サイドバーのヘッダが `remote otherhost:/home/naoto` 相当の表示に切り替わる。
   - 本文に「リモート接続中 (otherhost)」の案内が表示される。
   - ローカルの watcher / files ブラウザ / changes 表示は止まる。

4. ローカルホスト名つき OSC 7 をもう一度送る。

   ```sh
   printf '\033]7;file://%s%s\033\\' "$(cat /proc/sys/kernel/hostname)" "$PWD"
   ```

   期待結果:
   - サイドバーが通常のローカル表示に復帰する。
   - cwd が現在の `$PWD` に追従する。
   - files / changes / preview が再び使える。

5. 既存 VT / Sixel 回帰確認:
   - `printf '\033]0;phase8a-title\033\\'` を実行し、OSC 0 タイトル変更が画面に漏れないこと。
   - yazi など既存の Sixel 画像表示が従来どおり動くこと。
