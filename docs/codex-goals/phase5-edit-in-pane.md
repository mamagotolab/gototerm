# Codex Goal: Phase 5 — プレビュー中のファイルをその場で編集

## ゴール

サイドバーでプレビュー中のファイルを、**ワンクリックでエディタ（nvim等）ペインとして開く**。
エディタは自作しない。ペインを分割して `$EDITOR <file>` を起動するだけ。

体験：Claudeの出力のパスをクリック → 中身を確認 → `[編集]` をクリック →
ペインが上下分割され、下ペインに nvim がそのファイルを開いた状態で現れる →
`:wq` で閉じるとペインも閉じる（既存の刈り取りがそのまま働く）。

## 前提（先に読むこと）

- `src/vt.rs` の `VtTerminal::new` … 起動コマンドは現在 config の shell 固定。ここを一般化する
- `src/window.rs` の `TerminalWindow::with_viewport` / `make_terminal`（multiplexer.rs）
- `src/multiplexer.rs` の `Node::split_focused` … 分割してペインを作る既存経路
- `src/sidebar.rs` の `on_click` / `RowAction` … 行クリックの既存機構
- `docs/codex-goals/phase3-live-preview.md` の「絶対制約」も適用（fmtは触ったファイルのみ等）

## 実装内容

### 1. ペインの起動コマンド指定（基盤・今後レイアウトプリセットでも使う）

- `VtTerminal::new` に「起動コマンド」を渡せるようにする：`Option<&[String]>`。
  `None` なら従来どおり config の shell。`Some` なら `cmd[0]` をプログラム、残りを引数に。
  env の整備（STRIP_ENV / TERM 等）は従来と完全に同一に通すこと。
- `TerminalWindow` に既存 `with_viewport` と同じ引数＋`command: Option<&[String]>` を取る
  コンストラクタを追加（既存呼び出し箇所は無変更で通ること）。

### 2. エディタの解決（純関数＋テスト）

```rust
/// 使うエディタを決める。優先順: config.editor（空でなければ）→ $EDITOR → "nvim"
pub(crate) fn resolve_editor(config_editor: &[String], env_editor: Option<&str>) -> Vec<String>
```

- `src/config.rs` に `pub editor: Vec<String>`（デフォルト空 = $EDITOR に従う）を追加。
  `config.example.toml` / `config.windows.example.toml` にコメント付きで追記
  （Windows 例は `["notepad"]`）。
- `$EDITOR` は空白区切りで分割してよい（`"nvim -u NONE"` のような値に対応）。
- テスト：config指定あり / configなし＋$EDITORあり / どちらもなし→nvim。

### 3. サイドバーに `[編集]` 行

- プレビュー対象があるとき、プレビューヘッダの直下に1行：
  ```
     [クリックで編集: nvim]
  ```
  （`nvim` 部分は resolve_editor の先頭要素。Color は Cyan）
- この行に `RowAction::EditFile(PathBuf)`（**絶対パス**）を割り当てる。
- `Sidebar::on_click` は現在サイドバー内部で処理を完結しているが、
  ペイン分割はサイドバーにはできないので、**戻り値でマネージャに依頼する**形に変える：
  ```rust
  pub enum SidebarRequest { EditFile(PathBuf) }
  pub fn on_click(&mut self, p: PhysicalPosition<f64>) -> Option<SidebarRequest>
  ```
  既存の PreviewFile / Unpin はこれまでどおりサイドバー内で処理し `None` を返す。

### 4. `multiplexer.rs`: エディタペインを開く

- サイドバークリック箇所で `Some(SidebarRequest::EditFile(path))` を受けたら：
  1. `resolve_editor(...)` でエディタコマンドを組み立て、末尾に `path` を足す
  2. フォーカス中の葉を**上下分割**（`Partition::Horizontal`・ratio 0.5）し、
     新ペインをそのコマンドで起動する。cwd は元ペインのシェルの現在地
     （`pane_cwd()`、取れなければ起動ディレクトリ）＝既存の分割と同じ規則
  3. フォーカスは新ペイン（エディタ）へ（既存 split_focused と同じ）
- `Node::split_focused` に `command: Option<&[String]>` を通す形の最小変更でよい
  （既存のキーボード分割は `None` を渡す）。
- エディタ終了（`:wq`）でペインが閉じるのは既存の update_and_prune がそのまま働くはず。
  動作確認だけすること。

## 制約

- 新規依存なし。エディタ本体・構文ハイライト等は一切作らない。
- 既存のキーボード分割・タブの挙動を変えない（`command: None` 経路の回帰に注意）。
- 既存コードの無関係な整形禁止。cargo fmt は自分が触ったファイルのみ。
- コメントは日本語で「なぜ」を書く。

## 完了条件（この順で検証）

1. `cargo test` 全パス（`resolve_editor` の新規テスト含む）。
2. `cargo build --release` 成功、新規警告なし。
3. 手動確認手順を `docs/codex-goals/phase5-verify.md` に出力：
   パスをクリックしてプレビュー → `[クリックで編集: nvim]` をクリック →
   上下分割で nvim が該当ファイルを開く → `:wq` でペインが閉じて元のレイアウトに戻る →
   既存の Ctrl+Shift+E/O 分割は従来どおりシェルが開く。
