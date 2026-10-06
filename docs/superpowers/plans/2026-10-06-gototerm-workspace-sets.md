# gototerm 作業セット実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 名前付き作業セットから、タブ、分割配置、作業フォルダを保存・再作成できるようにする。

**Architecture:** 新しい`src/workspace_sets.rs`が設定領域にJSON形式の小さな一覧を保存する。Multiplexerはタブ木を値へ変換し、復元時は保存したフォルダごとに新しいシェルを起動する。シリアライズ対象を`WorkspaceSet`に限定し、PTYや端末出力を永続化しない。

**Tech Stack:** Rust、既存のserde/serde_json、設定パス解決、既存のlauncher/list UI。

**Spec:** `docs/superpowers/specs/2026-10-06-productivity-shortcuts-design.md`（作業セット節）

## Global Constraints

- 初版は名前付きセットを明示的に保存・選択する。
- 実行中プロセス、端末出力、シェル履歴、メールやAI会話を保存しない。
- 保存ファイルはユーザーのgototerm設定領域に置き、外部送信しない。
- 壊れたデータや欠落フォルダでプロセスを終了させない。

## Review Focus

- 空名、同名、パス区切りを含む名前を安全に扱う。
- 不正・途中書き込みのJSONを読み込み時に無視してアプリを継続する。
- 復元時に存在しないフォルダを個別に示し、他のタブを復元する。
- 作業セット保存にライブセッション・端末画面を混入させない。
- 分割比率を有限範囲へ制限して復元する。

---

### Task 1: 永続化形式

**Files:** Create `src/workspace_sets.rs`; modify `src/config.rs`, `src/lib.rs`.

**Interfaces:** `WorkspaceSet { name: String, tabs: Vec<SavedTab> }`; `SavedTab { root: SavedNode }`; `SavedNode` is either a pane `{ cwd: PathBuf }` or split `{ partition, ratio, first, second }`. Provide `load_all`, `save`, `delete`, and name validation functions.

- [ ] Resolve a data file under the existing per-user config directory and store only versioned workspace-set JSON.
- [ ] Validate names, reject duplicate names, and write through a temporary sibling file before replacing the saved file.
- [ ] Make a missing file an empty set list and malformed input a non-fatal load error with a user-visible message.
- [ ] Verify serialization round trips pane and split trees, and validation rejects invalid names.

### Task 2: Capture and restore layout

**Files:** Modify `src/multiplexer.rs` and its tab/split helpers.

**Interfaces:** Add conversions from `Tab<Node>` / `SplitNode` to `SavedTab` / `SavedNode`, plus `restore_workspace_set(&WorkspaceSet)`.

- [ ] Capture pane cwd, split direction, child ordering, and split ratio; do not capture child process commands or terminal contents.
- [ ] Recreate every pane as a shell rooted at its saved cwd and restore the split tree and ratios.
- [ ] Skip missing directories with a per-tab notice while restoring valid tabs.
- [ ] Preserve the current application state if validation fails before restore starts.

### Task 3: Save/select interface

**Files:** Modify `src/launcher.rs`, `src/multiplexer.rs`, `README.md`.

- [ ] Add explicit save-current-set, open-set, and delete-set actions to the existing launcher flow.
- [ ] Prompt for a name through the existing text/IME input component and show saved sets beside recent projects.
- [ ] Update the README with keyboard actions, persistence location, and what restoration recreates.
- [ ] Verify `cargo build` and inspect the diff for any serialized field beyond the approved saved layout.

---
