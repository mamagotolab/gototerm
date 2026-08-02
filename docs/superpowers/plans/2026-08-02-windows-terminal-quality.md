# gototerm Windows Terminal Quality Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Windows版gototermのタブ独立性、SSH先TUIのキー互換性、選択スクロール、DPI描画品質、起動時メモリ使用を、回帰テスト付きで改善する。

**Architecture:** 端末入力と選択座標を純粋な変換関数へ分離し、タブ固有状態は端末ペイン木と同じ`Tab`へまとめる。描画側は論理フォントサイズとDPI倍率を分け、サイドバー／プレビューの`TerminalView`だけを初回ワークベンチ表示まで遅延生成する。メール文字コードは変換せず、既存VTパーサーのUTF-8ストリーム処理をテストで固定し、内容を保存しない診断だけを追加する。

**Tech Stack:** Rust 2021、winit 0.29、glium 0.34、FreeType、alacritty_terminal 0.26、portable-pty 0.8。

## Global Constraints

- 子どもおよび開発者本人の実名をコード、コメント、ログ、文書へ追加しない。
- 効果値はWindows実機の実測だけを記録し、架空のメモリ削減量を記載しない。
- Windows実機で未確認の結果を確認済みとして報告しない。
- mutt本文のISO-2022-JP、Shift-JIS、EUC-JP変換は実装しない。
- サイドバー幅、プレビュー高さ、フォント倍率はウィンドウ共通のままにする。
- LinuxのDPI倍率1.0とlight hintingの既存挙動を維持する。
- 既存の無関係なClippy警告26件を一括修正しない。
- 各本体変更の前に失敗するテストを追加し、REDを確認してから最小実装を行う。
- ユーザーの既存変更`.serena/project.yml`と`docs/codex-goals/phase12-demo-auto-record.md`を変更、ステージ、コミットしない。

---

## File Structure

- `src/input.rs`: カーソルキーのモード別エンコードだけを担当する新規純粋モジュール。
- `src/vt.rs`: VTモード問い合わせ、絶対グリッド範囲のコピー、UTF-8境界テスト、安全な診断観測を担当する。
- `src/window.rs`: マウス座標を絶対グリッド座標へ変換し、描画用選択とコピー用選択を分離する。
- `src/multiplexer.rs`: `Tab`、タブ別ワークベンチ表示、DPI倍率の全ビュー配布を担当する。
- `src/view.rs`: 論理フォントサイズ、物理フォントサイズ、DPI変更時の必要最小限の再構築を担当する。
- `src/font.rs`: OS別FreeTypeヒンティングフラグを担当する。
- `src/sidebar.rs`: 状態モデルを維持したまま、描画用`TerminalView`を遅延生成する。
- `src/reader.rs`: プレビュー状態を維持したまま、描画用`TerminalView`を遅延生成する。
- `src/lib.rs`: 新規`input`モジュールを登録する。
- `README.md`: Windows実機でのDPI、mutt、メモリ確認手順と診断の有効化方法を記載する。

---

### Task 1: Application Cursor Mode対応のキーエンコーダー

**Files:**
- Create: `src/input.rs`
- Modify: `src/lib.rs`
- Modify: `src/vt.rs:797-833`
- Modify: `src/window.rs:836-906,927-1080`

**Interfaces:**
- Produces: `pub(crate) enum CursorKey { Up, Down, Right, Left }`
- Produces: `pub(crate) fn cursor_key_sequence(key: CursorKey, application: bool) -> &'static [u8]`
- Produces: `VtTerminal::application_cursor_mode(&self) -> bool`
- Consumes: `alacritty_terminal::term::TermMode::APP_CURSOR`

- [ ] **Step 1: Write failing encoder tests**

```rust
#[cfg(test)]
mod tests {
    use super::{cursor_key_sequence, CursorKey};

    #[test]
    fn cursor_keys_use_csi_in_normal_mode() {
        assert_eq!(cursor_key_sequence(CursorKey::Up, false), b"\x1b[A");
        assert_eq!(cursor_key_sequence(CursorKey::Down, false), b"\x1b[B");
        assert_eq!(cursor_key_sequence(CursorKey::Right, false), b"\x1b[C");
        assert_eq!(cursor_key_sequence(CursorKey::Left, false), b"\x1b[D");
    }

    #[test]
    fn cursor_keys_use_ss3_in_application_mode() {
        assert_eq!(cursor_key_sequence(CursorKey::Up, true), b"\x1bOA");
        assert_eq!(cursor_key_sequence(CursorKey::Down, true), b"\x1bOB");
        assert_eq!(cursor_key_sequence(CursorKey::Right, true), b"\x1bOC");
        assert_eq!(cursor_key_sequence(CursorKey::Left, true), b"\x1bOD");
    }
}
```

- [ ] **Step 2: Run the new test and verify RED**

Run: `cargo test input::tests --lib`

Expected: compilation fails because `src/input.rs` and the exported symbols do not exist.

- [ ] **Step 3: Add the minimal encoder and module registration**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorKey {
    Up,
    Down,
    Right,
    Left,
}

pub(crate) fn cursor_key_sequence(key: CursorKey, application: bool) -> &'static [u8] {
    match (application, key) {
        (false, CursorKey::Up) => b"\x1b[A",
        (false, CursorKey::Down) => b"\x1b[B",
        (false, CursorKey::Right) => b"\x1b[C",
        (false, CursorKey::Left) => b"\x1b[D",
        (true, CursorKey::Up) => b"\x1bOA",
        (true, CursorKey::Down) => b"\x1bOB",
        (true, CursorKey::Right) => b"\x1bOC",
        (true, CursorKey::Left) => b"\x1bOD",
    }
}
```

Add `mod input;` to `src/lib.rs`.

- [ ] **Step 4: Add and test the VT mode query**

Add a `vt.rs` test that feeds `b"\x1b[?1h"` and `b"\x1b[?1l"` through a `Processor`, asserting `TermMode::APP_CURSOR` becomes true then false. Add:

```rust
pub fn application_cursor_mode(&self) -> bool {
    use alacritty_terminal::term::TermMode;
    self.term.lock().unwrap().mode().contains(TermMode::APP_CURSOR)
}
```

Run: `cargo test vt::tests::app_cursor_mode_tracks_decset --lib`

Expected before the method/test helper exists: FAIL. Expected after implementation: PASS.

- [ ] **Step 5: Route keyboard and alternate-screen wheel arrows through the encoder**

In `TerminalWindow::on_key_press`, map the four `KeyCode::Arrow*` values to `CursorKey`, read `self.terminal.application_cursor_mode()`, and write `cursor_key_sequence(...)`. In the `alt_screen()` mouse-wheel branch, use the same function for vertical and horizontal repeats. Do not change PageUp, PageDown, Function keys, IME guards, or shortcut lookup.

- [ ] **Step 6: Run focused and full tests**

Run: `cargo test input::tests --lib`

Run: `cargo test vt::tests::app_cursor_mode_tracks_decset --lib`

Run: `cargo test --all-targets`

Expected: all existing tests plus the new tests pass.

- [ ] **Step 7: Commit Task 1**

```bash
git add src/input.rs src/lib.rs src/vt.rs src/window.rs
git commit -m "fix: honor application cursor mode"
```

---

### Task 2: ワークベンチ表示状態のタブ単位化

**Files:**
- Modify: `src/multiplexer.rs:285-525,808-1280,1988-2092`
- Modify: `src/sidebar.rs:100-131`

**Interfaces:**
- Produces: `struct Tab { root: Node, workbench_visible: bool }`
- Produces: `Sidebar::set_visible(&mut self, location: &ShellLocation, visible: bool)`
- Produces: `Multiplexer::apply_focused_tab_workbench(&mut self)`
- Consumes: existing `Node` pane-tree methods and `Sidebar::refresh_location`

- [ ] **Step 1: Write failing tab-state tests without constructing OpenGL objects**

Make `Tab` generic with `Node` as its default root type so the real production state container can be tested with `()` and no OpenGL objects:

```rust
#[test]
fn switching_tabs_restores_each_workbench_visibility() {
    let mut tabs = vec![Tab::new(()), Tab::new(())];
    tabs[0].workbench_visible = true;
    assert!(tabs[0].workbench_visible);
    assert!(!tabs[1].workbench_visible);
}

#[test]
fn removing_a_tab_keeps_visibility_attached_to_the_remaining_tab() {
    let mut tabs = vec![Tab::new(()), Tab::new(()), Tab::new(())];
    tabs[0].workbench_visible = true;
    tabs[2].workbench_visible = true;
    tabs.remove(1);
    assert!(tabs[0].workbench_visible);
    assert!(tabs[1].workbench_visible);
}
```

- [ ] **Step 2: Run the tests and verify RED**

Run: `cargo test multiplexer::tests::switching_tabs_restores_each_workbench_visibility --lib`

Run: `cargo test multiplexer::tests::removing_a_tab_keeps_visibility_attached_to_the_remaining_tab --lib`

Expected: FAIL because the helper and tab-level state do not exist.

- [ ] **Step 3: Introduce `Tab` and migrate all `Vec<Node>` access**

```rust
struct Tab<T = Node> {
    root: T,
    workbench_visible: bool,
}

impl<T> Tab<T> {
    fn new(root: T) -> Self {
        Self { root, workbench_visible: false }
    }
}
```

Change `tabs: Vec<Node>` to `tabs: Vec<Tab>`. Every existing pane-tree operation must explicitly use `.root`; examples include `focused_root`, split, draw, update/prune, viewport, focus, font iteration, GT message collection, and tab removal. New tabs always start with `workbench_visible = false`.

- [ ] **Step 4: Replace toggle-only sidebar API with idempotent visibility**

```rust
pub fn set_visible(&mut self, location: &ShellLocation, visible: bool) {
    if self.visible == visible {
        return;
    }
    self.visible = visible;
    if visible {
        self.refresh_location(location);
    } else {
        self.focused = false;
        self.clear_live_state();
        self.remote_location = None;
    }
}
```

Keep `toggle` only if another caller still needs it; implement it by calling `set_visible(location, !self.visible)` so there is one transition path.

- [ ] **Step 5: Apply focused tab state during toggle, switch, close, and process exit**

`Action::ToggleSidebar` changes only `self.tabs[self.focus].workbench_visible`. `NextTab`/`PrevTab`, tab removal, and automatic exited-pane removal call `apply_focused_tab_workbench`, which:

1. reads the focused tab flag;
2. obtains the focused terminal location;
3. calls `sidebar.set_visible(&location, visible)`;
4. releases sidebar, reader, and editor focus when `visible == false`;
5. calls `refresh_layout` and restores terminal focus.

Use the focused tab flag, not `sidebar.is_visible()`, to decide the content viewport.

- [ ] **Step 6: Run tab-state and full tests**

Run: `cargo test multiplexer::tests --lib`

Run: `cargo test --all-targets`

Expected: all tests pass; tab removal and startup-launcher replacement do not desynchronize state.

- [ ] **Step 7: Commit Task 2**

```bash
git add src/multiplexer.rs src/sidebar.rs
git commit -m "fix: keep workbench visibility per tab"
```

---

### Task 3: スクロールバック絶対座標による選択とコピー

**Files:**
- Modify: `src/vt.rs:842-874,900-1035`
- Modify: `src/window.rs:221-485,760-815,1085-1145`
- Modify: `src/view.rs:360-405`

**Interfaces:**
- Produces: `pub(crate) struct GridSelection { start: Point, end: Point, block: bool }`
- Produces: `visible_selection(selection: GridSelection, display_offset: usize, rows: usize, cols: usize) -> Option<crate::view::Selection>`
- Produces: `VtTerminal::selection_text(&self, selection: GridSelection) -> String`
- Consumes: `alacritty_terminal::index::{Column, Line, Point}` and `Term::bounds_to_string`

- [ ] **Step 1: Write failing clipping tests**

```rust
#[test]
fn selection_fully_above_viewport_is_not_painted() {
    let s = linear_grid_selection((-8, 1), (-6, 4));
    assert_eq!(visible_selection(s, 3, 4, 10), None);
}

#[test]
fn selection_crossing_viewport_is_clipped_to_visible_rows() {
    let s = linear_grid_selection((-5, 2), (1, 4));
    assert_eq!(
        visible_selection(s, 3, 4, 10),
        Some(Selection::Linear { left: 0, right: 39 })
    );
}
```

Use grid line `screen_row - display_offset`; with offset 3 the visible grid lines are `-3..=0`.

- [ ] **Step 2: Run clipping tests and verify RED**

Run: `cargo test window::tests::selection_fully_above_viewport_is_not_painted --lib`

Run: `cargo test window::tests::selection_crossing_viewport_is_clipped_to_visible_rows --lib`

Expected: FAIL because `GridSelection` and `visible_selection` do not exist.

- [ ] **Step 3: Implement absolute selection endpoints and visible intersection**

On mouse press and release, convert pixel coordinates to `Point<Line, Column>` using the current display offset. Preserve these points in `MouseState`; do not rewrite them when the user scrolls. Derive `view.selection_range` on refresh with `visible_selection`. A fully offscreen range yields `None` for paint only; it does not clear `MouseState`.

For double-click and triple-click, perform the existing word/line expansion against the visible snapshot first, then convert the expanded endpoints to grid lines. Preserve block selection columns without linearizing them.

- [ ] **Step 4: Write a failing copy-across-scroll test**

Build a `VtTerminal` test fixture with multiple known lines, create a `GridSelection`, capture `selection_text`, scroll the display, and assert the result remains unchanged:

```rust
let before = vt.selection_text(selection);
vt.scroll(3);
let after = vt.selection_text(selection);
assert_eq!(before, after);
```

Run: `cargo test vt::tests::selection_text_does_not_depend_on_display_offset --lib`

Expected: FAIL because `selection_text` does not exist.

- [ ] **Step 5: Implement selection copying from the VT grid**

For linear ranges, normalize start/end and call `term.bounds_to_string(start, end)`. For block ranges, iterate inclusive grid lines and call `bounds_to_string` once per same-line column range, trimming trailing spaces per row and joining with `\n`. Apply existing `dedent_common_indent` only to linear selection text in `window.rs`. Replace copy traversal over `view.lines` with `terminal.selection_text`.

- [ ] **Step 6: Run selection tests and full suite**

Run: `cargo test window::tests --lib`

Run: `cargo test vt::tests::selection_text_does_not_depend_on_display_offset --lib`

Run: `cargo test --all-targets`

Expected: all tests pass; screen-edge clamping no longer paints offscreen selection at the first or last row.

- [ ] **Step 7: Commit Task 3**

```bash
git add src/vt.rs src/window.rs src/view.rs
git commit -m "fix: keep text selection anchored while scrolling"
```

---

### Task 4: UTF-8境界保証と内容非保存の診断

**Files:**
- Modify: `src/vt.rs:130-560,670-720,1125-1420`
- Modify: `README.md:520-540`

**Interfaces:**
- Produces: `Utf8Diagnostic::new(enabled: bool) -> Self`
- Produces: `Utf8Diagnostic::observe(&mut self, bytes: &[u8], modes: DiagnosticModes) -> Vec<Utf8Issue>`
- Produces: environment switch `GOTOTERM_UTF8_DIAGNOSTICS=1`
- Consumes: only `Seg::Pass` bytes before `Processor::advance`

- [ ] **Step 1: Add a failing VT parser split-boundary test**

Feed `"件名：再利用メール".as_bytes()` to one `Processor` at every possible two-part split and compare the rendered snapshot with an unsplit feed. Repeat with OSC 7 followed immediately by Japanese text and with a Sixel segment followed by Japanese text.

Run: `cargo test vt::tests::utf8_render_is_independent_of_read_boundary --lib`

Expected: the test either passes, proving the existing `Processor` already owns UTF-8 carry state, or fails at a specific splitter boundary. If it passes, do not add a second decoder to the production text path.

- [ ] **Step 2: Add failing diagnostic privacy and boundary tests**

```rust
#[test]
fn utf8_diagnostic_carries_incomplete_prefix_without_reporting_content() {
    let mut d = Utf8Diagnostic::new(true);
    assert!(d.observe(&[0xe6, 0x97], modes()).is_empty());
    assert!(d.observe(&[0xa5], modes()).is_empty());
}

#[test]
fn utf8_issue_contains_position_and_modes_but_no_bytes() {
    let mut d = Utf8Diagnostic::new(true);
    let issues = d.observe(&[0xff], modes());
    assert_eq!(issues[0].offset, 0);
    assert_eq!(issues[0].modes, modes());
    assert!(!format!("{issues:?}").contains("ff"));
}
```

- [ ] **Step 3: Run diagnostic tests and verify RED**

Run: `cargo test vt::tests::utf8_diagnostic --lib`

Expected: FAIL because the diagnostic types do not exist.

- [ ] **Step 4: Implement the observer without payload retention**

Store only `pending_len: u8`, up to three pending prefix bytes needed for validation, `stream_offset: u64`, and counters. `Utf8Issue` contains only `offset: u64` and three booleans for application cursor, alternate screen, and mouse mode. Do not derive or implement formatting that exposes the pending byte array. Emit `log::warn!` with numeric position and booleans only when the environment variable equals `1`.

Create the observer once in the PTY reader thread. Feed only `Seg::Pass` chunks and query modes while the term mutex is already held. Do not inspect keyboard input, OSC payloads, Sixel payloads, paths, hostnames, or clipboard data.

- [ ] **Step 5: Document opt-in diagnostics and limitations**

Add a README troubleshooting subsection with the exact PowerShell invocation:

```powershell
$env:GOTOTERM_UTF8_DIAGNOSTICS = "1"
./gototerm-windows-x64.exe
```

State that logs contain counts, offsets, and terminal modes but no mail body; unset the variable after diagnosis. State that gototerm does not decode mail encodings.

- [ ] **Step 6: Run focused and full tests**

Run: `cargo test vt::tests --lib`

Run: `cargo test --all-targets`

Expected: all tests pass and no diagnostic output is emitted without the environment variable.

- [ ] **Step 7: Commit Task 4**

```bash
git add src/vt.rs README.md
git commit -m "test: harden UTF-8 PTY stream handling"
```

---

### Task 5: DPIを考慮した論理フォントサイズとWindowsヒンティング

**Files:**
- Modify: `src/font.rs:1-145`
- Modify: `src/view.rs:52-245,738-825`
- Modify: `src/window.rs:250-335,516-640`
- Modify: `src/multiplexer.rs:849-930,1225-1265,1680-1745`
- Modify: `src/sidebar.rs:575-590`
- Modify: `src/reader.rs:30-90`
- Modify: `src/launcher.rs:55-90`
- Modify: `src/session_review.rs:35-65`

**Interfaces:**
- Produces: `physical_font_size(logical: u32, scale_factor: f64) -> u32`
- Produces: `scale_change_requires_rebuild(old_scale: f64, new_scale: f64, logical: u32) -> bool`
- Produces: `TerminalView::set_scale_factor(&mut self, scale_factor: f64) -> bool`
- Produces: `TerminalWindow::set_scale_factor(&mut self, scale_factor: f64)`
- Produces: `font_render_flags(is_windows: bool) -> LoadFlag`
- Consumes: `Window::scale_factor()` and `WindowEvent::ScaleFactorChanged`

- [ ] **Step 1: Write failing physical-font-size tests**

```rust
#[test]
fn physical_font_size_preserves_one_x_and_rounds_scaled_sizes() {
    assert_eq!(physical_font_size(18, 1.0), 18);
    assert_eq!(physical_font_size(18, 1.25), 23);
    assert_eq!(physical_font_size(18, 1.5), 27);
}

#[test]
fn repeated_scale_factor_does_not_request_rebuild() {
    assert!(!scale_change_requires_rebuild(1.0, 1.0, 18));
    assert!(scale_change_requires_rebuild(1.0, 1.5, 18));
}
```

- [ ] **Step 2: Run sizing tests and verify RED**

Run: `cargo test view::tests::physical_font_size --lib`

Expected: FAIL because the functions do not exist.

- [ ] **Step 3: Separate logical and physical sizes in `TerminalView`**

Store `logical_font_size: u32` and `scale_factor: f64`. Build `FontSet` with `physical_font_size(logical_font_size, scale_factor)`. `increase_font_size` modifies the logical size, then rebuilds only if the computed physical size changed. `set_scale_factor` clamps non-finite or non-positive input to `1.0`, compares the old and new physical sizes, and returns whether it rebuilt the font, cell metrics, and glyph cache.

- [ ] **Step 4: Propagate scale factor consistently**

Initialize `Multiplexer::scale_factor` from `window.scale_factor()`. Pass it into every `TerminalView` and `TerminalWindow` constructor. On `ScaleFactorChanged`, update the framebuffer first, then propagate the new factor to all terminal leaves, status bar, visible or stored workbench views, launcher, and session review. Every terminal whose cell size changes calls `resize_buffer`, which updates PTY rows, columns, pixel cell size, and IME area.

- [ ] **Step 5: Add failing OS hinting selection tests**

```rust
#[test]
fn windows_uses_standard_hinting_and_unix_keeps_light_hinting() {
    assert!(!font_render_flags(true).contains(LoadFlag::TARGET_LIGHT));
    assert!(font_render_flags(false).contains(LoadFlag::TARGET_LIGHT));
}
```

Run: `cargo test font::tests::windows_uses_standard_hinting_and_unix_keeps_light_hinting --lib`

Expected: FAIL before `font_render_flags` exists.

- [ ] **Step 6: Implement platform-selected FreeType flags**

Use `LoadFlag::RENDER` for Windows and `LoadFlag::RENDER | LoadFlag::TARGET_LIGHT` elsewhere. Select the boolean with `cfg!(windows)` in `Font::render`; do not add a user configuration option.

- [ ] **Step 7: Run DPI, font, and full tests**

Run: `cargo test view::tests --lib`

Run: `cargo test font::tests --lib`

Run: `cargo test --all-targets`

Expected: all tests pass; scale 1.0 retains existing Linux cell sizing.

- [ ] **Step 8: Commit Task 5**

```bash
git add src/font.rs src/view.rs src/window.rs src/multiplexer.rs src/sidebar.rs src/reader.rs src/launcher.rs src/session_review.rs
git commit -m "fix: rebuild fonts for monitor DPI changes"
```

---

### Task 6: サイドバーとプレビュー描画資源の遅延生成

**Files:**
- Modify: `src/sidebar.rs:20-95,100-180,570-610,890-935`
- Modify: `src/reader.rs:20-95,470-550`
- Modify: `src/multiplexer.rs:849-925,1188-1220`

**Interfaces:**
- Produces: `LazySlot<T>::new() -> Self` for one-time storage without constructing `T`
- Produces: `LazySlot<T>::ensure_with(&mut self, factory: impl FnOnce() -> T) -> &mut T`
- Produces: `LazySlot<T>::is_initialized(&self) -> bool`
- Produces: private `LazyTerminalView` wrapper that owns the latest view specification and a `LazySlot<TerminalView>`
- Consumes: existing `Sidebar` and `ReaderPane` state without delaying GT/timeline models

- [ ] **Step 1: Write failing generic lazy-view tests with an injected factory**

Keep OpenGL out of unit tests by extracting a generic factory-backed holder in `view.rs` or private testable helper:

```rust
#[test]
fn lazy_resource_constructs_once_on_first_access() {
    let calls = Cell::new(0);
    let mut lazy = LazySlot::new();
    assert!(!lazy.is_initialized());
    assert_eq!(*lazy.ensure_with(|| {
        calls.set(calls.get() + 1);
        42
    }), 42);
    assert_eq!(*lazy.ensure_with(|| {
        calls.set(calls.get() + 1);
        99
    }), 42);
    assert_eq!(calls.get(), 1);
}
```

- [ ] **Step 2: Run the lazy-resource test and verify RED**

Run: `cargo test view::tests::lazy_resource_constructs_once_on_first_access --lib`

Expected: FAIL because the lazy holder does not exist.

- [ ] **Step 3: Implement lazy `TerminalView` ownership without delaying models**

`Sidebar` continues to exist at startup so GT state, timeline, session summaries, and non-render models are not lost. Replace only `view: TerminalView` with a holder that stores `Display`, latest `Viewport`, logical font size, scale factor, current font delta, and `Option<TerminalView>`. Do the same in `ReaderPane`; keep `FilePreview`, pin state, reader lines, notices, and row actions eager.

Methods that only update state must not call `ensure`. `draw`, `contains`, `cell_height`, visible `rebuild`, and the transition to visible may call it. `set_viewport`, font changes, and scale changes update stored parameters and forward to the view only when initialized.

- [ ] **Step 4: Ensure first workbench display initializes both views once**

When the focused tab transitions from hidden to visible, initialize sidebar and reader views before calculating workbench cell-dependent layout. Closing keeps both allocations for reuse. Opening a second tab's workbench reuses the same shared sidebar/reader views because only visibility is tab-specific.

- [ ] **Step 5: Add state-level tests for hidden and reopened workbench**

Assert that manager construction leaves both lazy holders uninitialized, the first visible transition initializes them, a close does not drop them, and reopening does not increment the factory count.

Run: `cargo test multiplexer::tests::workbench_views_are_lazy --lib`

Run: `cargo test view::tests::lazy_resource_constructs_once_on_first_access --lib`

Expected before integration: FAIL. Expected after integration: PASS.

- [ ] **Step 6: Run full tests and inspect initialization logs**

Run: `cargo test --all-targets`

Run a local debug build with `RUST_LOG=gototerm=debug cargo run`, close the launcher without opening the workbench, and confirm no sidebar/reader glyph-cache construction log appears. Open the workbench twice and confirm each view constructs once.

- [ ] **Step 7: Commit Task 6**

```bash
git add src/view.rs src/sidebar.rs src/reader.rs src/multiplexer.rs
git commit -m "perf: lazily create workbench render resources"
```

---

### Task 7: クロスプラットフォーム検証とWindows実機引き継ぎ

**Files:**
- Modify: `README.md:34-90,520-540`
- Modify: `docs/superpowers/specs/2026-08-02-windows-terminal-quality-design.md` only if implementation revealed a factual mismatch; do not rewrite approved requirements.

**Interfaces:**
- Consumes: all interfaces from Tasks 1-6
- Produces: reproducible verification commands and an explicit Windows manual checklist

- [ ] **Step 1: Run formatting and the complete Linux test baseline**

Run: `cargo fmt --all -- --check`

Run: `cargo test --all-targets`

Expected: formatting succeeds; all tests pass with the existing one ignored performance test.

- [ ] **Step 2: Run Clippy without claiming the pre-existing warning baseline is clean**

Run: `cargo clippy --all-targets`

Compare the output with the recorded baseline. Fix only warnings introduced on lines changed by this implementation. Do not modify unrelated files solely to eliminate existing warnings.

- [ ] **Step 3: Check Windows target availability and compile when available**

Run: `rustup target list --installed | rg '^x86_64-pc-windows-msvc$'`

If present, run: `cargo check --target x86_64-pc-windows-msvc`

If absent, record exactly `x86_64-pc-windows-msvc target not installed` in the handoff; do not install it without user approval.

- [ ] **Step 4: Update README manual verification checklist**

Document these exact Windows checks without invented results:

1. no-config embedded font at 100% scale;
2. move between each attached monitor and inspect size, sharpness, cell alignment, and IME candidate position;
3. open workbench in tab A, leave tab B normal, switch both directions;
4. SSH to the existing host and move through mutt with arrow keys;
5. reuse a previously sent Japanese mail and note whether corruption remains;
6. record Task Manager memory at startup, after first workbench open, and after adding a tab;
7. record WezTerm under the same window/tab conditions.

- [ ] **Step 5: Run final diff and repository checks**

Run: `git diff --check`

Run: `git status --short`

Run: `git diff --stat fbc92a0..HEAD`

Confirm `.serena/project.yml` and `docs/codex-goals/phase12-demo-auto-record.md` remain unstaged and unchanged by these tasks.

- [ ] **Step 6: Commit documentation changes**

```bash
git add README.md docs/superpowers/specs/2026-08-02-windows-terminal-quality-design.md
git commit -m "docs: add Windows terminal verification checklist"
```

If the spec did not require a factual correction, stage and commit only `README.md`.

- [ ] **Step 7: Invoke verification-before-completion before reporting success**

Re-run `cargo fmt --all -- --check` and `cargo test --all-targets` immediately before the final report. Report Windows-only checks as pending until the user runs the produced executable on the Windows machine. If mutt corruption remains, use the opt-in numeric diagnostic before proposing any encoding change.
