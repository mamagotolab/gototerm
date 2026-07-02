# gt プロトコル — SSH越しワークベンチのためのエスケープシーケンス設計

## 背景と原理

サイドバーの機能（cwd追従・変更検知・プレビュー）はローカルFS前提で、ssh先では盲目になる。
一方、**エスケープシーケンスは ssh を素通りする**（画面バイト列の一部だから）。
そこで、リモート側から情報をエスケープシーケンスに包んで送り、gototerm が
alacritty に渡す前段（SixelSplitter と同じ位置）で抽出する。

- **一方向 push のみ**（リモート→ローカル）。要求応答はしない（プロトコルも実装も一気に複雑化するため）。
- **表示専用**。受け取った内容はディスクに書かない・実行しない（悪意あるリモート出力への安全策）。
- リモート側の送り手は POSIX sh スクリプト1枚（`gt`）＋シェル統合スニペット＋Claude Code hooks。

## メッセージ一覧

### 1. cwd 通知 — 標準 OSC 7（新規発明しない）

```
ESC ] 7 ; file://<host>/<percent-encoded-path> (BEL | ESC \)
```

- fish は対応端末で標準発行。bash/zsh はスニペットで対応。
- `<host>` が空 / "localhost" / ローカルのホスト名と一致 → ローカル扱い（パスをcwd追従に使う）。
  それ以外 → **リモート接続中**と判定し、サイドバーはリモート表示に切り替わる。
- 副産物：Windows ローカルでも（/proc が無くても）シェル統合さえ入れれば cwd 追従が動くようになる。

### 2. gt メッセージ — OSC 7717（私用番号）

```
ESC ] 7717 ; <type> ; <key>=<value> ; ... [; data=<base64>] (BEL | ESC \)
```

`<type>`:

| type | 意味 | キー |
|---|---|---|
| `event` | ファイル変更イベント | `kind=new\|mod\|del`、`path=<base64のパス>` |
| `file` | ファイル内容のチャンク | `path=<base64>`、`seq=<0始まり>`、`last=0\|1`、`data=<base64>` |

- パスは base64（`;` や非ASCIIを含み得るため）。内容チャンクは **生データ8KBまで**を base64 化。
- `seq=0` が新しい転送の開始（前の未完了転送は破棄）。`last=1` で完結し表示に反映。
- 内容は送り手側で**末尾64KBに切ってから**送る（ローカルプレビューと同じ上限）。
- 受信側の防御：1メッセージ最大1MBでそれ以上は破棄・不正な base64/欠落キーは黙って捨てる・
  `seq` 飛びは転送ごと破棄。

## リモート側の送り手（Phase 8b で実装）

`gt`（POSIX sh・依存は base64/dd 程度・/dev/tty へ書く）:

- `gt view <file>` … 末尾64KBを file メッセージで送出（手動プレビュー）
- `gt event <kind> <path>` … event メッセージ送出
- `gt hook` … Claude Code の PostToolUse フックから呼ぶ。stdin の JSON から
  file_path を取り、event＋file を送出（hooks の stdout は Claude Code に食われるので
  必ず /dev/tty へ書く）
- `gt init-hooks` … カレントの `.claude/settings.local.json` にフック設定を書き込む

シェル統合（OSC 7）スニペット: bash は PROMPT_COMMAND、zsh は precmd、fish は不要な場合が多い。

## gototerm 側の受信（Phase 8a で実装）

- vt.rs の SixelSplitter を一般化（Sixel DCS ＋ OSC 7/7717 を抽出、他は素通し）。
  チャンクまたぎ・BEL/ST 両終端・不正シーケンス破棄に耐えること。
- 抽出結果は VtTerminal の共有状態に積み、Multiplexer/AboutToWait 経由でサイドバーへ。
- リモート判定中のサイドバー：
  - ヘッダに `host:path` 表示
  - ローカル watcher / files ブラウザは停止（「リモート接続中」の案内表示）
  - changes 相当は event メッセージで、プレビューは file メッセージで供給（8b）
  - [編集]（ローカルエディタ起動）はリモート内容では非表示（編集はリモート側の nvim で）
