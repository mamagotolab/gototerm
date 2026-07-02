# Codex Goal: Phase 4 — ファイルパスのクリックでプレビュー

## ゴール

**画面に見えているファイルパスをクリックしたら、そのファイルがすぐ見える。**

1. ターミナル出力中のファイルパス（Claude Code の「Created: airticles/articles/xxx.md」等）を
   クリック → サイドバーのプレビューに表示（サイドバーが閉じていれば自動で開く）
2. サイドバーの changed files の行をクリック → そのファイルをプレビュー
3. クリックで表示したファイルは**ピン留め**され、AI が別ファイルを触っても勝手に切り替わらない

## 前提（先に読むこと）

- `src/window.rs` の `url_at()` … トークン抽出（全角対応・末尾句読点除去）の既存実装。
  ファイルパス検出はこれを拡張する
- `src/sidebar.rs` / `src/preview.rs` … Phase 3 のライブプレビュー（追従・バックグラウンド読込）
- `src/multiplexer.rs` … サイドバー内クリックは現在 `contains()` で握り潰している
- `docs/codex-goals/phase3-live-preview.md` の「絶対制約」4項目はこの Phase にも適用

## 実装内容

### 1. `window.rs`: パス検出（`url_at` の一般化）

```rust
enum LinkTarget {
    Url(String),
    File(PathBuf),   // 絶対パスに解決済み
}
fn link_at(&self, row: usize, col: usize) -> Option<LinkTarget>
```

- トークン抽出は `url_at` と同じ（全角・句読点処理も同じ）。
- `http://` / `https://` → 従来どおり `Url`。
- それ以外は**パス候補**として解決を試みる：
  - `/` 始まり → そのまま絶対パス
  - `~/` 始まり → `$HOME` で展開
  - それ以外（`./foo`、`airticles/xxx.md`、`README.md` など）→ **ペインのシェルの現在地**
    （`self.terminal.cwd()`、取れなければ起動ディレクトリ）に対して解決
- 解決後 `Path::is_file()` が true のものだけ `File` を返す（存在しないトークンは無視＝誤検出は自然に落ちる）。
- **ホバー時のカーソルアイコン判定（`CursorMoved` 経路）では stat しない。**
  マウス移動のたびにディスクを触るとネットワークマウントで固まる。
  アイコン判定は構文チェックのみ（http 接頭辞、または `/` を含む・`./`・`~/` 始まり）。
  stat はクリック時の1回だけ。
- スペースを含むパスは対象外（トークンが空白で切れるため）。既知の制限としてコメントに書く。

### 2. `window.rs` → `multiplexer.rs` の受け渡し

- URL と同じクリック操作で、`LinkTarget::File` の場合は `clicked_file: Option<PathBuf>` に保存
  （`open_url` は呼ばない）。
- `pub fn take_clicked_file(&mut self) -> Option<PathBuf>` を追加。
- `multiplexer.rs`：マウス入力をペインへ渡した直後に `take_clicked_file()` を確認し、
  `Some(path)` なら：
  1. サイドバーが非表示なら表示に切り替え（`toggle` 相当＋`refresh_layout`）
  2. `sidebar.preview_pinned(&path)` を呼ぶ

### 3. `sidebar.rs` / `preview.rs`: ピン留め

- `preview_pinned(abs_path: &Path)`：
  - watcher root 配下なら相対表記、外なら絶対表記でヘッダに表示
  - プレビュー対象に設定し、`pinned = true`
- **ピン中の動作**：
  - 変更イベントが来ても対象を切り替えない
  - ただし**ピン中のファイル自身**の変更イベントは内容を再読込する
    （記事を Claude が書き続けている間、その記事が追記されていく＝本命の体験）
  - ピン中のファイルが watcher root の外にある場合、変更イベントは来ないので
    5秒ごとの定期 refresh のタイミングで再読込する
- **ピン解除**：プレビューのヘッダ行（`▶ <file> 📌`）をクリック → 追従モードに戻り最新変更ファイルへ。
  ヘッダにピン中は `📌 (クリックで追従に戻る)`、追従中は `follow` と表示。

### 4. サイドバー内クリック

- `multiplexer.rs` の「サイドバー内クリックを握り潰す」箇所を `sidebar.on_click(pos)` に変える
  （握り潰す挙動は維持しつつ、サイドバーに委譲）。
- `Sidebar::on_click(&mut self, p: PhysicalPosition<f64>)`：
  - ピクセル座標 → 行番号（`viewport` と `cell_size` から計算）
  - `rebuild()` 時に「行番号 → アクション」の対応表（`Vec<Option<RowAction>>`）を作っておき、それを引く
  ```rust
  enum RowAction {
      PreviewFile(PathBuf),  // changed files の行
      Unpin,                 // プレビューヘッダ行（ピン中のみ）
  }
  ```
  - changed files の行クリック → そのファイルをピン留めプレビュー
  - 対応するアクションのない行は何もしない
- ホイール等はこれまでどおり無視でよい（スクロールは今回のスコープ外）。

## 制約

- 新規依存なし。
- ファイル読込は Phase 3 の `FilePreview` の経路（バックグラウンド＋末尾64KB＋デバウンス）を必ず通す。
  クリック起点でも UI スレッドで read しない。
- クリック時の stat（`is_file`）は1回のみ許容。ホバーでは stat 禁止（上記）。
- 既存コードの無関係な整形禁止。cargo fmt は自分が触ったファイルのみ。
- コメントは日本語で「なぜ」を書く。

## 完了条件（この順で検証）

1. `cargo test` 全パス。パス解決（絶対 / `~` / 相対 / 存在しないトークン）と
   行→アクション対応の純関数部分に新規テストを書くこと。
2. `cargo build --release` 成功、新規警告なし。
3. 手動確認手順を `docs/codex-goals/phase4-verify.md` に出力：
   `echo airticles/test.md` のような相対パスを画面に出す → クリック → サイドバーが開いて中身が見える →
   ピン留め中に別ファイルを変更してもプレビューが切り替わらない →
   ピン中のファイル自身への追記は反映される → ヘッダクリックで追従に戻る →
   changed files の行クリックでそのファイルに切り替わる → URL クリックは従来どおりブラウザが開く。
