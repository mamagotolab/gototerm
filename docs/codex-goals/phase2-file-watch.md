# Codex Goal: Phase 2 — ファイル監視＋変更ファイル表示（changed files）

## ゴール

サイドバー表示中、作業フォルダ配下のファイル変更を監視し、
「changed files」セクションに NEW / MOD / DEL を新しい順に表示する。
AI（Claude Code / Codex）がファイルを作る・変える・消すのがリアルタイムに見えるようにする。

## 前提（先に読むこと）

- `docs/workbench-v2.md` … 全体方針（判断1: 第2描画スタック禁止）
- `src/sidebar.rs` … Phase 1 のサイドバー。changed files はこの中に1セクション追加する
- `src/multiplexer.rs` … `AboutToWait` で16msごとに `sidebar.refresh_if_stale(&cwd)` が呼ばれている（表示中のみ）
- `src/workspace.rs` … Phase 1 の情報収集

## 実装内容

### 1. 依存追加（これ以外は不可）

```toml
notify = "6"
```

デバウンサクレートは使わない。イベントの合成は自前の純関数で行う（テスト可能にするため）。

### 2. 新規 `src/watcher.rs`

```rust
pub enum ChangeKind { New, Modified, Deleted }

pub struct FileChange {
    pub path: std::path::PathBuf,  // root からの相対パス
    pub kind: ChangeKind,
}

pub struct WorkspaceWatcher { /* notify::RecommendedWatcher + mpsc::Receiver + root */ }
```

- `WorkspaceWatcher::new(root: &Path) -> Result<Self, notify::Error>`：root を再帰監視。
- `set_root(&mut self, root: &Path)`：cd 追従で監視先を張り替える（unwatch → watch）。root が変わったら変更履歴もクリア。
- `drain(&mut self) -> Vec<FileChange>`：溜まったイベントを非ブロッキングで回収（`try_iter`）。
  notify の `EventKind::Create/Modify/Remove` を `ChangeKind` に写像する。

**イベント合成は純関数に切り出してテストを書く：**

```rust
/// 同一パスに複数イベントが来たときの合成規則。
/// New→Modified は New のまま（「新規作成されて編集中」）。
/// なんであれ最後に Deleted が来たら Deleted。
/// Deleted の後に Create が来たら Modified（上書き保存のパターン）。
fn merge_kind(prev: Option<ChangeKind>, next: ChangeKind) -> ChangeKind
```

**無視パターンも純関数＋テスト：**

```rust
/// パスのどこかの構成要素が patterns に一致したら無視。
fn is_ignored(rel_path: &Path, patterns: &[String]) -> bool
```

- 無視パターンは config から取る（下記 4）。デフォルト: `.git`, `node_modules`, `target`, `dist`, `__pycache__`。
- 監視開始が失敗した場合（inotify 上限など）はパニックせず、サイドバーに
  `watch: 監視を開始できません` を BrightBlack で1行表示して他の機能は生かす。

### 3. `src/sidebar.rs` の変更

- `Sidebar` に変更履歴（`Vec<FileChange>` 相当、**新しい順・同一パスは最新に合成・最大100件**）と
  `Option<WorkspaceWatcher>` を持たせる。
- **監視のライフサイクル**：サイドバー表示ONで watcher 生成、OFFで drop（非表示中の監視コストをゼロにする）。
  cwd が変わったら `set_root`（`refresh_if_stale` が cwd 変化を検知する既存経路に載せる）。
- 表示：`git:` セクションと `ai tools:` の間に追加。

```
 changed files
   NEW  docs/plan.md
   MOD  src/main.rs
   DEL  old_notes.md
```

- バッジ色：NEW=Green / MOD=Yellow / DEL=Red、パスは White。
  1行に2色必要なので、`cells_for_line` を「色付きセグメントの列」を受ける形の
  ヘルパー（例 `cells_for_segments(&[(&str, Color)], cols)`）に拡張してよい。
  既存の `push_line` 呼び出しの見た目は変えないこと。
- パスは root からの相対表記。幅からはみ出すときは既存の `abbreviate_start` で先頭省略。
- 表示件数は残り行数に応じて切る（keys セクションを押し出さない。changed files は最大10行、
  超過分は `   … ほか N 件` を BrightBlack で1行）。
- 変更が1件もないときは `   (変更なし)` を BrightBlack で表示。

### 4. `src/config.rs` の変更

```rust
pub watch_ignore: Vec<String>,  // デフォルト [".git", "node_modules", "target", "dist", "__pycache__"]
```

`config.example.toml` / `config.windows.example.toml` にコメント付きで追記。

### 5. `src/multiplexer.rs` の変更

原則変更なしの想定（既存の `refresh_if_stale` 経路に乗せる）。
どうしても必要な場合も差分は最小に。

## 制約

- 新規依存は `notify` のみ。
- サイドバー非表示時は watcher を持たない（メモリ・fd・CPUコストゼロ）。
- イベント処理でメインスレッドをブロックしない（`drain` は非ブロッキング必須）。
- 既存コードの無関係な整形・リファクタはしない。コメントは日本語で「なぜ」を書く。

## 完了条件（この順で検証）

1. `cargo test` 全パス（`merge_kind` / `is_ignored` / 相対パス表示の新規テストを含む）。
2. `cargo build --release` 成功、新規警告なし。
3. 手動確認手順を `docs/codex-goals/phase2-verify.md` に書き出す：
   サイドバー表示 → 別ペインで `touch a.md` → NEW が出る →
   `echo x >> a.md` → NEW のまま（New→Modified合成）→ `rm a.md` → DEL に変わる →
   `.git` 配下の変更は出ない → `cd` すると履歴がクリアされ新しい root を監視する。
