# gototerm Workbench 要件定義 v2（実装アーキテクチャ版）

2026-07-02。ChatGPT版要件定義（v1）を、現行コードベース（v0.2.13・約5,600行）に即して再構成したもの。
v1からの主な変更：ターゲット修正／危険コマンド警告の格下げ／実装アーキテクチャとクレート選定の追加／Windows対応の作業表化。

## 1. コンセプト

**AI時代の軽量な作業フォルダビューア付きターミナル。**

Claude Code / Codex CLI で開発する人が、重いIDEを開かずに
「AIが何を作ったか・変えたか・実行したか」をターミナル内で見える化する。

- AI本体は作らない。既存AI CLIの**作業環境**に徹する。
- IDEにしない。ターミナル＋サイドペインの薄いレイヤーに留める。
- gototermの既存の強み（日本語IME・Sixel画像・軽さ）を土台にする。

## 2. ターゲット（v1から修正）

**メイン：CLIでAI開発する中級者。** ターミナルは使えるが、AIの変更を把握する視覚補助が欲しい人。VS Code / Cursor が重いと感じる人。

v1の「黒い画面が怖い非エンジニア」はメインから外す。その層は自作ターミナルを
インストールしない（claude.ai/code やデスクトップアプリへ流れる）。
日本語UI・日本語警告は「初心者向け」ではなく「日本語圏の開発者への品質」として残す。

## 3. 差別化の核（この2つ以外は捨てられる）

1. **Claude Code hooks / OSC 133 連携によるAI作業の見える化**（他ターミナル未実装）
2. **日本語ファースト**（IME品質・日本語UI。Warp / Wave Terminal の明確な弱点）

隠し玉：既存の`PositionedImage`（OpenGL画像クアッド）を流用すれば
**画像がインライン表示されるMarkdownプレビュー**を内蔵できる。Warp/Wave/WezTermのどれにもない。

## 4. アーキテクチャ原則（実装上の3判断）

### 判断1：第2の描画スタックを入れない
egui/icedは採用しない。サイドペインはすべて「セルグリッドに描くTUIウィジェット」として
既存の`TerminalView`（view.rs）で描画する。フォント・グリフキャッシュ・日本語・画像描画を共有し、軽さを守る。

### 判断2：サイドバーは分割ツリーの外に置く
`multiplexer.rs`の`Node`ツリー（葉=`TerminalWindow`固定）は改造リスクが高い。
既に`status_view`が「ツリー外の特別ペイン」として実装されている前例に倣い、
サイドバーも`Multiplexer`直下のフィールドとして持つ。
`content_viewport()`がタブバーの高さを引いているのと同じ要領で、サイドバー表示中は右側の幅を引く。

→ 将来4ペインレイアウトが本当に必要になったら、その時点で`Node`葉の抽象化
（`trait Pane`）を検討する。MVPでは右サイドバー1枚で足りる。

### 判断2.5：操作の二重化原則（2026-07-03追加・以後の全フェーズに適用）
**サイドバーの全ての操作は、マウス（クリック/ホイール）とキーボードの両方で行えること。**
片方しかない操作を作らない。実装上は「キー操作＝対応する行のクリックと同じ経路
（RowAction / SidebarRequest）に流す」ことで二重実装を避ける。
新しい操作を足すときは、この表に両方の欄が埋まることを確認する。

### 判断3：Gitはシェルアウト、AIログはhooksから取る
- Git：`git status --porcelain=v2 --branch` / `git diff --no-color` のパース。
  `gix`等のライブラリはWindowsビルドの変数を増やすので当面使わない。
- AIログ：PTY出力のスクレイピングはしない。Claude Code hooks（構造化JSON）と
  OSC 133（コマンド境界＋exit code）の2層で取る。

## 5. モジュール構成（目標形）

```
src/
  workspace.rs   // 作業フォルダ情報（cwd / git status / AI CLI 有無）
  sidebar.rs     // 右サイドバー（TerminalView にセルを書く表示専用ペイン）
  widgets/       // Phase 3 以降：file_tree / preview / diff / ai_log
  hooks.rs       // Phase 4：AIイベント受信サーバ（127.0.0.1 + トークン）
  shell_integration.rs // Phase 4：OSC 133 パースと rc スニペット
```

## 6. 機能設計の要点

### 6.1 ワークスペースサマリ（Phase 1）
サイドバーに cwd / git branch / modified・untracked数 / claude・codex・gemini の有無を表示。
`Ctrl+Shift+F`でトグル。表示専用（フォーカスなし）から始める。

### 6.2 ファイル監視（Phase 2）
`notify` v6 + `notify-debouncer-mini`（Linux=inotify / Windows=ReadDirectoryChangesW）。
除外は`ignore`クレート（.gitignore準拠）。イベントは mpsc →`EventLoopProxy`で再描画
（Sixelの非同期描画と同じパターン）。NEW / MOD / DEL をサイドバーに表示。

### 6.3 Markdown・テキストプレビュー（Phase 3）
`pulldown-cmark`でパースし、スタイル付きセルに畳み込む（見出し=太字+色、コード=背景色）。
画像は`image`クレートでデコードし`PositionedImage`経由で描画（差別化ポイント）。

### 6.4 AI作業ログ（Phase 4・記事化の目玉）
- **第1層 Claude Code hooks**：起動時に`tiny_http`で127.0.0.1のランダムポートを開き、
  PTY環境変数`GOTOTERM_EVENT_URL`＋トークンを注入。`gototerm init-hooks`が
  プロジェクトの`.claude/settings.local.json`にPostToolUse/Stopフック
  （`curl -s $GOTOTERM_EVENT_URL -d @-`）を書き込む。
- **第2層 OSC 133**：bash/zsh/fish/PowerShell用rcスニペットを同梱（`gototerm init-shell`）。
  vt.rsのOSC処理にA/B/C/Dハンドラを追加し、コマンド＋exit codeをログ化。
  hooksのないAIツール（Codex等）はこの層＋ファイル監視でカバー。

### 6.5 Git diff（Phase 5）
`git diff --no-color <file>`を行単位パースして+/-着色。ワード単位は`similar`で後日。

### 6.6 危険コマンド警告（v1から格下げ・最後尾）
端末層ではAIが実行するサブプロセスのコマンドは捕捉できない（プロンプト行を通らない）。
対象は**ユーザー手入力のみ**と明記し、OSC 133導入後（コマンド境界が取れてから）に実装する。
Claude Code側のpermissionと役割が重複する点も踏まえ、優先度最下位。

## 7. Windows対応の現状（ビルドスパイク＝Phase 0は完了済み）

v0.2.2〜v0.2.13で、Windows対応は**実機運用段階**まで進んでいる
（CI windows-latest 緑・release.yml でタグpush→exe自動配布・実機クラッシュ3件を crash.log 起点で根治済み）。
freetype の cmake 問題も回避策確立済み（CMAKE_GENERATOR=Ninja 等・README記載）。

v1要件の「Windows固有要件」で残っているのは以下だけ。

| 項目 | 状態 |
|---|---|
| ConPTY / ビルド / CI / exe自動配布 | ✅ 済 |
| 日本語IME（確定Enter二重入力もv0.2.3で修正済） | ✅ 済 |
| クリップボード / exeアイコン / crash.log | ✅ 済 |
| Explorer右クリック統合 | 未着手 → Phase 5。`gototerm --install-context-menu`（HKCU配下＝管理者権限不要） |
| WSLパス変換表示 | 未着手 → Phase 2 の workspace 情報に含める |
| ウィンドウ透過 | 保留（WGLは透過FB非対応・DWM対応が別途必要） |
| winget配布 | 未着手（現状は GitHub Release の zip） |

## 8. フェーズ計画（検証条件つき）

| Phase | 内容 | 完了条件 | 担当 |
|---|---|---|---|
| 0 | ~~Windowsビルドスパイク~~ **完了済み**（§7参照） | — | — |
| 1 | サイドバー基盤＋ワークスペースサマリ | Ctrl+Shift+Fでcwd/git/AIツールが見える。cargo test全パス | Codex（`docs/codex-goals/phase1-sidebar.md`） |
| 2 | ファイル監視＋NEW/MOD/DEL表示＋WSLパス表示 | AIがファイルを作るとサイドバーにNEWが出る | Codex |
| 3 | Markdown/テキストプレビュー（画像込み） | README.mdが画像込みで表示される | Codex＋Claude（画像描画の接続） |
| 4 | hooks＋OSC 133のAI作業ログ | Claude Codeの作業がリアルタイムにログに流れる | Claude（設計）→Codex（実装） |
| 5 | Git diff＋右クリック統合＋危険コマンド警告（手入力のみ） | 変更ファイルのdiffが色付きで見える | Codex |

各Phase完了ごとにWindows実機でも動作確認する（サイドバーはOS非依存の実装だが、レイアウト崩れ・最小化まわりはWindowsで踏みやすい）。

## 9. 除外事項（v1から継承）

自前AIチャット／AIの無確認コマンド実行／GUIエディタ／プラグイン機構／
GUIのGit操作／ノートアプリ機能／クラウド同期。
加えてv2で明示：**4ペイン固定レイアウトはMVPでは作らない**（右サイドバー1枚から）。

## 10. 位置づけ

収益プロダクトではなく、**開発ラボブランドの広告塔＋記事シリーズ資産**。
要件定義（本書）→実装→運用知見の各段階を記事化する。特にPhase 4
（hooks連携ターミナル）は他に前例がなく、Qiita/技術記事の目玉とする。
