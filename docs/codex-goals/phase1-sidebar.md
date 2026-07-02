# Codex Goal: Phase 1 — サイドバー基盤＋ワークスペースサマリ

## ゴール

gototerm に右サイドバー（表示専用）を追加し、`Ctrl+Shift+F` でトグルできるようにする。
サイドバーには現在の作業フォルダ・Git状態・AI CLIの有無を表示する。

## 前提（先に読むこと）

- `docs/workbench-v2.md` … 全体方針。特に「判断1・判断2」（第2描画スタック禁止／サイドバーは分割ツリーの外）。
- `src/multiplexer.rs` … `status_view` が「ツリー外の特別ペイン」の前例。`content_viewport()` がタブバー高さを引く要領で、サイドバー表示中は右側の幅を引く。
- `src/view.rs` … `TerminalView::with_viewport(display, viewport, font_size, ...)` と `update_contents()`。`Multiplexer::update_status_bar()` が `Cell`/`Line` を組み立てて `view.lines` に入れる実例。
- `src/terminal.rs` … `Cell::new_ascii` / `Line::from_cells` / `Color`。

## 実装内容

### 1. 新規 `src/workspace.rs`

```rust
pub struct GitSummary {
    pub branch: String,
    pub staged: usize,
    pub modified: usize,   // unstaged の変更（porcelain v2 の "1"/"2" 行で XY の Y が非 '.'）
    pub untracked: usize,  // "?" 行
    pub deleted: usize,    // worktree 側で D
}

pub struct WorkspaceInfo {
    pub cwd: std::path::PathBuf,
    pub git: Option<GitSummary>,        // git リポジトリでなければ None
    pub ai_tools: Vec<(&'static str, bool)>, // ("claude"|"codex"|"gemini", 存在するか)
}

pub fn collect(cwd: &std::path::Path) -> WorkspaceInfo
```

- Git 情報は `git status --porcelain=v2 --branch` を `std::process::Command` で実行して取得。
  - branch は `# branch.head <name>` 行から。
  - git コマンド自体が無い／リポジトリでない（exit≠0）→ `git: None`。エラーにしない。
- **パースは純関数 `fn parse_porcelain_v2(text: &str) -> GitSummary` に切り出し、ユニットテストを書く**
  （clean / modified / staged+unstaged 混在 / untracked / deleted / rename("2"行) の各ケース）。
- AI CLI の有無は PATH 検索。クレート `which = "6"` を追加してよい（それ以外の新規依存は不可）。

### 2. 新規 `src/sidebar.rs`

```rust
pub struct Sidebar {
    view: TerminalView,
    visible: bool,          // 初期値 false
    info: Option<WorkspaceInfo>,
    last_refresh: std::time::Instant,
}
```

- `toggle()` / `is_visible()` / `set_viewport()` / `draw()` / `needs_redraw()` / `refresh_if_stale()` を持つ。
- `refresh_if_stale()`：表示中かつ前回取得から5秒以上経過していたら `workspace::collect()` を呼び直し、内容を再構築。トグルONの瞬間も必ず取得。
- 表示内容（`update_status_bar()` と同じ要領で `Cell`/`Line` を組み立てる）：

```
 workspace
 ─────────────
 dir: <cwd 末尾を右ペイン幅に収まるよう省略表示>
 git: <branch>            ← リポジトリでなければ "not a git repo" を BrightBlack で
   staged:    N
   modified:  N
   untracked: N
   deleted:   N
 ai tools:
   claude  ✓              ← 存在=Green の ✓ / 無し=BrightBlack の ✗
   codex   ✓
   gemini  ✗
```

- 幅からはみ出す行は末尾を切り詰める（`unicode-width` は依存済み）。
- フォントサイズは `TOYTERM_CONFIG.font_size` を使う。
- フォーカス・キー入力・マウスは受けない（表示専用）。

### 3. `src/multiplexer.rs` の変更（最小限に）

- フィールド `sidebar: Sidebar` を追加。
- `Action::ToggleSidebar` を追加し、`parse_shortcut` の `(true, KeyCode::KeyF)`（Ctrl+Shift+F）に割り当て。既存ショートカット（T/W/Q/E/O/Tab/矢印）と衝突しないこと。
- レイアウト：サイドバー表示中は端末領域の右側を `config.sidebar_ratio` ぶん確保する。
  `split_viewport(Partition::Vertical, 1.0 - ratio, vp)` を流用してよい（GAP も既存定数）。
  純関数として切り出せる形なら切り出してテストを書く（既存の `split_viewport` テストのスタイルに合わせる）。
  - タブバーの高さ計算（`content_viewport`）との併用順序：タブバーを引いた後の領域を左右に分ける。
- `RedrawRequested`：`self.tabs[self.focus].draw(...)` の後にサイドバーを描画。
- `AboutToWait`：`refresh_if_stale()` を呼び、`needs_redraw()` を再描画判定に OR する。
- `Resized` / `ScaleFactorChanged`：`refresh_layout()` 内でサイドバーの viewport も更新。
- トグル時は `refresh_layout()` を呼び、既存ペインを広げ直す／縮め直す。

### 4. `src/config.rs` の変更

- `pub sidebar_ratio: f64` を追加。デフォルト `0.30`。`config.example.toml` と `config.windows.example.toml` にもコメント付きで追記。

## 制約

- **サイドバー非表示（デフォルト）のとき、既存の挙動を1ピクセルも変えないこと。**
- 新規依存は `which` のみ。egui / iced / ratatui 等の描画系クレートは禁止。
- コメントは既存コードのスタイル（日本語・「なぜ」を書く）に合わせる。
- 既存コードの無関係な整形・リファクタはしない。

## 完了条件（この順で検証）

1. `cargo test` 全パス（porcelain v2 パーサ＋レイアウトの新規テストを含む）。
2. `cargo build --release` 成功、警告の新規追加なし。
3. 手動確認手順を `docs/codex-goals/phase1-verify.md` に書き出す：
   起動 → Ctrl+Shift+F でサイドバー表示 → cwd/git/AIツールが見える →
   もう一度押すと消えて端末が全幅に戻る → タブ・分割・リサイズが従来どおり動く。
