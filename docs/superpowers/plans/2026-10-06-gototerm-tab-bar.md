# gototerm タブバー見直し実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 番号だけだったタブバーを、作業場所を見分けやすい落ち着いたタブ表示へ変更する。

**Architecture:** `Multiplexer::update_status_bar`が各タブの番号とフォーカス中ペインのcwd basenameをセル列へ描画する。既存の一行`TerminalView`を使い、文字セルの色と下線相当のアクセントを設定する。タブ幅計算はセル幅ベースで行い、レイアウトや高さは維持する。

**Tech Stack:** Rust、既存の`Cell`/`Line`、`TerminalView`、Vt `ShellLocation`。

**Spec:** `docs/superpowers/specs/2026-10-06-productivity-shortcuts-design.md`（タブバーの見直し節）

## Global Constraints

- タブが1つのときはバーを隠し、既存のバー高さを維持する。
- 新しいフォント・アイコン・テーマ依存を追加しない。
- 既存のタブ切り替え操作を維持する。
- 狭い画面で番号とタブ境界を消さない。

## Review Focus

- 端末幅がタブ数より狭い場合に文字列を越境させない。
- Windowsのパス・root cwd・リモート接続でも空でない安定ラベルを出す。
- 東アジア幅2セルの作業フォルダ名をセル単位で安全に省略する。
- 選択色が配色設定とコントラストを保つ。
- 起動・終了・フォーカス変更・画面リサイズでタブ名を更新する。

---

### Task 1: タブ見出しモデル

**Files:** Modify `src/multiplexer.rs` and a focused helper module if cell formatting cannot remain local.

**Interfaces:** A helper formats one `TabHeader { index, title, focused }` into at most a caller-provided number of terminal cells.

- [ ] Derive title from the focused pane's local cwd basename or remote location basename; use `shell` when cwd is unavailable/root.
- [ ] Format each tab as a two-digit index, separator, and clipped title with an explicit focused flag; prefix the active tab with an accent marker.
- [ ] Clip by Unicode display width, reserve room for tab boundaries, and omit title when width is too narrow.
- [ ] Add a subtle active background and colored index/marker, and use a muted inactive style with existing configured colors.
- [ ] Regenerate the status bar when tab focus/order, pane cwd, window width, or tab count changes.
- [ ] Update README with the new tab display and retain the existing keyboard controls; run `cargo build`.

---
