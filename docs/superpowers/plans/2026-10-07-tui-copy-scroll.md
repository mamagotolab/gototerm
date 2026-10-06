# 全画面アプリの連続コピー Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 対象5アプリのスクロール中、確定できる本文を保持して選択・コピーする。

**Architecture:** ペインのウィンドウが一時コピー履歴を所有し、既存のVTグリッドをスナップショットして照合する。スクロール入力は1行ずつ直列化して送り、描画が落ち着いた時点で確定する。画面の連続性を確定できない場合は保持済みの選択を残して停止する。

**Tech Stack:** Rust、alacritty_terminal、winit、既存PTY/クリップボード。

**Spec:** `docs/superpowers/specs/2026-10-07-tui-copy-scroll-design.md`

## Global Constraints

- 対象はless、mutt、Claude Code、Codex、Vim。個人のメール・会話を検証に使用しない。
- 画面外へ出た本文をメモリに保持し、ディスクへ保存しない。
- 通常の履歴コピーを維持し、コピー操作の生キーをアプリへ送らない。
- 判定不能な更新、上限、リサイズ、画面切り替えで無言の誤連結をしない。

## Review Focus

- 繰り返し行、動的な再描画、固定フッターを本文と誤認しない。
- スクロール要求とPTY読み取りの分割をページ境界と誤認しない。
- 逆方向移動や矩形選択で始点を変えない。
- 終了キーをアプリへ誤送信しない。
- 実アプリで未検証の動作を模擬テストだけで保証しない。

### Task 1: 画面照合とコピー履歴

**Files:** Create `src/tui_copy.rs`; modify `src/lib.rs`。

**Interfaces:** `Frame`は文字セルと折り返し情報、`TuiCopy`はフレーム・論理行座標・選択・停止理由を保持。`observe(frame)`で確定した移動を反映し、`text()`で保持済みの選択を返す。

- [ ] 前後方向の移動、固定フッター、繰り返し、全面更新、上限、日本語、矩形選択の失敗テストを書く。
- [ ] REDを確認し、確実な重なりだけを連結する最小実装を書く。
- [ ] 関連テストをGREENにしてコミットする。

### Task 2: ペインの入力と表示へ統合

**Files:** Modify `src/vt.rs`, `src/window.rs`。

**Interfaces:** `VtTerminal::copy_frame()`でライブグリッドを文字セルへ変換。`TerminalWindow`は`TuiCopy`とスクロール待機を保持する。

- [ ] コピー中のスクロール経路、停止、境界、選択表示に関する失敗テストを追加する。
- [ ] コピー専用モードと通常マウス選択に一時履歴を接続する。アプリへの要求を直列化し、描画確定後に次の行を要求する。
- [ ] フレームの論理座標を可視選択へ変換し、コピー結果を既存のクリップボードへ渡す。
- [ ] 関連テストをGREENにしてコミットする。

### Task 3: 実アプリ・レビュー・配布

**Files:** README、検証記録、必要な回帰テスト、Cargoバージョン。

- [ ] 個人データを使わず実PTY出力で各アプリの確認を行い、実行できない対象・描画方式の限界を記録する。
- [ ] 関連テストと全体テスト、差分チェックを実行する。
- [ ] 全体の独立レビューを受け、必要な回帰テストと修正を行う。
- [ ] GitHubへpushし、Windows/Linux CIを確認して統合・リリースする。
- [ ] この環境のバイナリをバックアップして差し替え、配布物との一致を確認する。
