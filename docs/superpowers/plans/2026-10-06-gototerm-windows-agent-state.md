# gototerm Windows AI状態通知 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** WindowsネイティブのClaude CodeとCodexのフック通知を既存のペイン状態一覧へ表示する。

**Architecture:** PTYごとの一意なローカルWindows名前付きパイプを親gototermが所有する。子プロセス環境へそのパイプ名を渡し、Windows用`gototerm-hook.exe`はフックstdinから必要な状態だけを読み、パイプにイベントを一件送る。親は受信イベントを既存の`PaneActivity`と同じ遷移関数へ渡す。

**Tech Stack:** Rust、serde_json、Windows named pipe API、既存の`PaneActivity`、GitHub Actions Windowsビルド。

**Spec:** `docs/superpowers/specs/2026-10-06-productivity-shortcuts-design.md`（WindowsネイティブAI状態通知節）

## Global Constraints

- 状態名とペイン識別情報だけを通知し、会話・コマンド本文を保存・送信しない。
- `PermissionRequest`通知は許可も拒否も返さず、受け取り後に通常フローを継続する。
- フック設定の追加は明示操作にし、既存設定ファイルを自動上書きしない。
- pipe受信処理をUIスレッド上でブロックしない。

## Review Focus

- 二つのペインが同じ作業フォルダを使っても通知先を混同しない。
- 不正・過大・未知のJSONイベントを破棄し、helperは機密入力をログに出さない。
- 承認待ちを通知してもCodex/Claudeの判定や標準出力を変更しない。
- ペイン終了時にpipe受信を停止・解放する。
- 既存hooks JSON/TOML設定にユーザー定義があっても保持する。

---

### Task 1: 通知イベントとペイン状態遷移

**Files:** Modify `src/task_activity.rs`, `src/gt.rs`, `src/multiplexer.rs`.

**Interfaces:** Define a small `AgentStateEvent { state: AgentState, detail: Option<String> }` parser and one pane activity transition function reused by GT messages and Windows events.

- [ ] Add input validation for `session_start`, `blocked`, `done`, and `session_end` events.
- [ ] Route accepted events by owning pane ID and preserve the existing activity list retention/display semantics.
- [ ] Reject unknown pane IDs and invalid state values without changing any pane.
- [ ] Verify the shared transition function produces matching activity rows for existing GT and new native events.

### Task 2: Windows pipe transport and helper

**Files:** Modify `Cargo.toml`, `src/vt.rs`, `src/multiplexer.rs`; create `src/bin/gototerm-hook.rs`.

**Interfaces:** Parent creates one local pipe per PTY and injects `GOTOTERM_STATE_PIPE`; helper reads one JSON object from stdin and writes one validated event to that pipe.

- [ ] Start one non-blocking reader per PTY and send decoded events to the multiplexer through its event proxy.
- [ ] Build the helper only for Windows and fail quietly if the pipe endpoint is absent or gototerm has exited.
- [ ] Bound input size, parse only the required fields, and avoid logging stdin or event detail.
- [ ] Add the helper executable to the Windows CI and release artifacts.
- [ ] Verify `cargo check` on Linux and Windows-target CI build.

### Task 3: Opt-in hook setup

**Files:** Modify `assets/bin/gt`, `docs/gt-protocol.md`, and Windows helper setup code; update `README.md`.

- [ ] Provide an explicit setup command that merges the Claude PowerShell hooks and Codex Windows hooks while preserving existing user-defined entries.
- [ ] Convert Claude notification events and Codex `SessionStart`, `PermissionRequest`, and `Stop` to the shared event format.
- [ ] Do not return allow/deny decisions to either host agent.
- [ ] Make setup idempotent and report a clear conflict when the same hook cannot be merged safely.
- [ ] Document install, removal, supported state labels, and the absence of message-body collection.

---
