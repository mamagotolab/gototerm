# gototerm リンクヒント実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** マウスなしで、画面に見えるURLや既存ファイルを選んで開けるようにする。

**Architecture:** `src/link_hints.rs`に候補、短いラベル、入力状態を持つ状態機械を置く。端末グリッドから候補を作り、既存の`url_at`とファイルプレビュー要求を呼ぶ。描画は既存の`TerminalView`に短い文字ラベルの一時オーバーレイを追加し、端末本文を変更しない。

**Tech Stack:** Rust、winit、既存の`TerminalView`、alacritty_terminalのグリッド。

**Spec:** `docs/superpowers/specs/2026-10-06-productivity-shortcuts-design.md`（リンクヒント節）

## Global Constraints

- 候補は表示中のグリッドだけから作る。
- URLと既存ファイルだけを対象にし、ファイルは現在の作業フォルダから解決する。
- 本文、選択、出力履歴、IME文字列を書き換えない。
- 既存の通常クリックとCtrl＋クリックの動作を維持する。

## Review Focus

- 折り返し行で同じURLを重複候補にしない。
- 全角セル、空白、括弧でラベルと候補位置がずれない。
- 無効なラベル、Escape、画面更新後に古い候補を実行しない。
- マウス報告アプリを閉じた後、キー入力が端末へ戻る。
- IME変換中にラベルを誤選択しない。

---

### Task 1: 候補と選択状態

**Files:** Create `src/link_hints.rs`; modify `src/lib.rs`, `src/vt.rs`, `src/window.rs`.

**Interfaces:** `LinkHintState::open(Vec<LinkTarget>)`, `input(char) -> LinkHintOutcome`, `cancel()`, `targets()`. `LinkTarget` contains visible cell coordinates, displayed label, and either a URL string or a resolved `PathBuf`.

- [ ] Build candidate extraction from visible rows, deduplicating URLs across soft wraps and resolving local paths with existing `resolve_existing_file_token`.
- [ ] Assign stable one or two character labels in row-major order; accept only the matching next character and return an outcome only for a complete label.
- [ ] Wire Escape, no-candidate, invalid-label, and successful selection outcomes into `TerminalWindow` without sending consumed keystrokes to the child process.
- [ ] Rebuild or cancel the state when grid dimensions, scroll position, or visible contents change.
- [ ] Document `Ctrl+Shift+R`, label selection, and Escape in `README.md`.

### Task 2: Label overlay and focus handoff

**Files:** Modify `src/view.rs`, `src/window.rs`, `src/multiplexer.rs`.

**Interfaces:** Add a view overlay input containing label text and grid cell position; draw it after terminal cells without changing `ViewContents.lines`.

- [ ] Render labels above their target cells using the existing font and cell metrics.
- [ ] While hints are open, route label input to gototerm even when the application enabled terminal mouse reporting.
- [ ] On selection, call the existing URL opener or emit the existing file-preview request; restore normal key routing after completion or cancellation.
- [ ] Refresh labels after font scale or viewport changes, and cancel them when the owning pane loses focus.
- [ ] Verify `cargo build` and inspect the diff for changes to terminal output, selection, or unrelated rendering.

---
