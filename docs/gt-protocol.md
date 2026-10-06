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

### Windowsネイティブの状態フック

WindowsのClaude Code/Codexには`gototerm-hook.exe`を使う。
`gototerm-hook.exe init-hooks <project> all`は、Claudeの`.claude/settings.local.json`と
Codexの`.codex/hooks.json`に定義を追加する。既存のJSONキー・フックと
`config.toml`は保持し、変更前のJSONを`.json.gototerm-backup`へ一度保存する。
不正なJSONや安全に結合できない形式なら書き込み前にエラーにする。
Codexは追加後に`/hooks`で定義を確認・信頼する必要がある。
`remove-hooks <project> all`は同じhelperへの定義だけを取り除く。

gototermはPTYごとにローカルの名前付きパイプを作り、子プロセスへ
`GOTOTERM_STATE_PIPE`を渡す。helperはフックのstdinを256KBまで読み、
イベント名だけから次のJSONを送る。会話、ツール入力、コマンド本文、理由は送らない。

```json
{"agent":"codex","state":"blocked"}
```

agentは`claude`/`codex`、stateは`session_start`/`blocked`/`done`/`session_end`だけ。
親は128バイトを超えるメッセージや未知のキーを破棄し、対応するPTYの
既存`GtMessage::State`に変換する。UIスレッドは受信待ちをしない。
PTY破棄時に受信を終了し、パイプを閉じる。
PermissionRequestでは許可・拒否・継続停止の判断を返さない。
Codexへの標準出力は空のJSONオブジェクトだけとし、通常の承認フローを維持する。
パイプが無いgototerm外では通知を行わず終了する。
Claude Notificationは承認・入力待ちだけを対象にし、認証成功等を待機状態にしない。

フック定義は[Claude CodeのPowerShellフック](https://code.claude.com/docs/en/hooks#windows-powershell-tool)と
[Codexのフック仕様](https://learn.chatgpt.com/docs/hooks)に従う。

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
| `event` | ファイル変更イベント | `kind=new\|mod\|del`、`path=<base64のパス>`、任意で `tool=<base64のツール名>` |
| `file` | ファイル内容のチャンク | `path=<base64>`、`seq=<0始まり>`、`last=0\|1`、`data=<base64>` |
| `state` | エージェントの意味的な状態信号 | `agent=<base64のエージェント名>`、`state=blocked\|done\|session_start\|session_end`、任意で `detail=<base64のメッセージ>` |

`state` は Claude Code hooks の Notification/Stop/SessionStart/SessionEnd から送る（Phase 11・2026-07-19）。
`event`（ファイル変更）と違い、画面の見た目を判定しない——hooks が「今どういう状態か」を
直接教えてくれるので、許可待ち（blocked）を screen scraping なしで検知できる。
`blocked` の `detail` はNotification hookの`message`本文（許可待ちの内容）。他は`detail`省略可。

- パスと `tool` は base64（`;` や非ASCIIを含み得るため）。内容チャンクは **生データ8KBまで**を base64 化。
- `seq=0` が新しい転送の開始（前の未完了転送は破棄）。`last=1` で完結し表示に反映。
- 内容は送り手側で**末尾64KBに切ってから**送る（ローカルプレビューと同じ上限）。
- 受信側の防御：1メッセージ最大1MBでそれ以上は破棄・不正な base64/欠落キーは黙って捨てる・
  `seq` 飛びは転送ごと破棄。

## リモート側の送り手（Phase 8b で実装）

`gt`（POSIX sh・依存は base64/dd 程度・/dev/tty へ書く）:

- `gt view <file>` … 末尾64KBを file メッセージで送出（手動プレビュー）
- `gt event <kind> <path> [tool]` … event メッセージ送出
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
