# gototerm スクロールバックコピー実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 長い端末出力をスクロールし、Vim風のキー操作で選択してコピーできるようにする。

**Architecture:** `src/copy_mode.rs`がモード状態、絶対グリッドカーソル、選択種別を管理する。`VtTerminal`が絶対行列上の移動と選択をalacrittyグリッドへ適用し、端末ビューが独立カーソルを描画する。`TerminalWindow`はモード中だけ入力を捕捉し、既存クリップボード書き込みを使う。

**Tech Stack:** Rust、alacritty_terminal、既存の選択・クリップボード処理。

**Spec:** `docs/superpowers/specs/2026-10-06-productivity-shortcuts-design.md`（スクロールバックのコピー節）

## Global Constraints

- `Ctrl+Shift+Space`だけが通常入力からコピー専用モードへ入る。
- `h/j/k/l`, 矢印, `Ctrl+U/D`, `g/G`, `v/V/Ctrl+V`, `y`, `Esc`を仕様どおり扱う。
- モード中のキーを子PTYへ送らず、通常モードへ戻ったら端末を入力可能にする。
- 選択は可視行ではなくスクロールバックの絶対行列位置で保持する。

## Review Focus

- マウス報告中のTUIでもモード操作が端末へ漏れない。
- 履歴の上端・下端、出力追加、スクロール境界でカーソル・選択が範囲外にならない。
- wide cell、soft-wrap、全行選択、矩形選択でコピー結果とハイライトが一致する。
- Escape終了後に選択を誤って解除せず、ライブ画面・PTY入力が復帰する。
- Windows/Linuxのクリップボード実装を既存の経路から再利用する。

---

### Task 1: 絶対グリッドコピーカーソル

**Files:** Create `src/copy_mode.rs`; modify `src/lib.rs`, `src/vt.rs`.

**Interfaces:** `CopyModeState { cursor: Point, selection: Option<CopySelectionMode> }`; `CopySelectionMode::{Cell, Line, Block}`; `CopyMotion`; `VtTerminal::copy_mode_move(Point)` and `copy_mode_select(...)` operate on absolute scrollback points.

- [ ] Add viewport-to-absolute cursor conversion and clamp movements between `topmost_line`, `bottommost_line`, and valid columns.
- [ ] Keep an absolute anchor point and update `Term.selection` as the cursor moves; map Cell, Line, and Block selection modes to the corresponding alacritty selection type.
- [ ] Expose the visible cursor cell by subtracting `display_offset`; return none while it is outside the viewport.
- [ ] Add a copied-text read operation for the active selection that shares existing line-wrap handling.
- [ ] Verify the module builds with existing platform gates and no PTY writes are needed by motion functions.

### Task 2: Keyboard mode and copy action

**Files:** Modify `src/window.rs`, `src/keybindings.rs`, `src/view.rs`.

**Interfaces:** `TerminalWindow` owns `Option<CopyModeState>` and maps the accepted key set to `CopyMotion` and mode outcomes.

- [ ] Bind `Ctrl+Shift+Space` as `CopyMode` through the configurable shortcut system.
- [ ] Route hjkl/arrows, Ctrl+U/D, g/G, v/V/Ctrl+V, y, and Escape only while mode is active.
- [ ] Copy `y` selection through the existing OS clipboard abstraction and exit copy mode; preserve selection while entering/leaving safely.
- [ ] Render the copy-mode cursor and selection without changing the PTY grid contents.
- [ ] Document the key table in `README.md` and run `cargo build`.

---
