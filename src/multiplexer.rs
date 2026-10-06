//! タブと画面分割を司るマネージャ。
//!
//! 1つの OS ウィンドウの中に複数の端末セッション（[`TerminalWindow`]）を
//! 抱え、タブ（[`Vec<Node>`]）と二分木の分割（[`Node::Split`]）として配置する。
//! ウィンドウ全体のイベント（リサイズ・再描画・終了）はここで握り、
//! キー/マウス/IME は現在フォーカスしているペインへ振り分ける。

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent},
    event_loop::{ControlFlow, EventLoopWindowTarget},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::Window,
};

use crate::config::{resolve_editor, resolve_file_open, OpenMethod};
use crate::gt::{AgentSignal, GtFileAssembler, GtMessage};
use crate::keybindings::{self, ShortcutAction};
use crate::launcher::{Launcher, LauncherOutcome};
use crate::reader::{ReaderHeaderAction, ReaderKeyResult, ReaderPane, ReaderRequest};
use crate::recent::RecentProjects;
use crate::session_review::{SessionReview, SessionReviewOutcome, SessionSummary};
use crate::sidebar::{Sidebar, SidebarKeyResult, SidebarRequest};
use crate::task_activity::{sanitize_display_text, PaneId};
use crate::task_overview::{OverviewOutcome, TaskOverview, TaskRow};
use crate::terminal::{Color, Line};
use crate::view::{TerminalView, Viewport};
use crate::vt::ShellLocation;
use crate::window::TerminalWindow;
use crate::Display;

type Event = winit::event::Event<()>;

/// 分割の境界に空ける隙間（px）。
const GAP: u32 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DpiUpdate {
    surface_size: Option<PhysicalSize<u32>>,
    viewport_size: Option<PhysicalSize<u32>>,
    sync_layout_and_pty: bool,
}

#[derive(Clone, Copy, Debug)]
struct DpiTransitionState {
    scale_factor: f64,
    physical_size: PhysicalSize<u32>,
    pending_metrics_sync: bool,
}

impl DpiTransitionState {
    fn new(scale_factor: f64, physical_size: PhysicalSize<u32>) -> Self {
        Self {
            scale_factor,
            physical_size,
            pending_metrics_sync: false,
        }
    }

    fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    fn begin_scale_factor_change(&mut self, scale_factor: f64) -> Option<f64> {
        if self.scale_factor == scale_factor {
            return None;
        }
        self.scale_factor = scale_factor;
        Some(scale_factor)
    }

    fn mark_metrics_changed(&mut self, changed: bool) {
        self.pending_metrics_sync |= changed;
    }

    fn on_resized(&mut self, physical_size: PhysicalSize<u32>) -> DpiUpdate {
        let size_changed = self.physical_size != physical_size;
        self.physical_size = physical_size;

        if physical_size.width == 0 || physical_size.height == 0 {
            return DpiUpdate {
                viewport_size: size_changed.then_some(physical_size),
                ..DpiUpdate::default()
            };
        }

        let sync_layout_and_pty = size_changed || self.pending_metrics_sync;
        self.pending_metrics_sync = false;
        DpiUpdate {
            surface_size: size_changed.then_some(physical_size),
            viewport_size: size_changed.then_some(physical_size),
            sync_layout_and_pty,
        }
    }

    fn flush_pending(&mut self) -> DpiUpdate {
        if !self.pending_metrics_sync
            || self.physical_size.width == 0
            || self.physical_size.height == 0
        {
            return DpiUpdate::default();
        }
        self.pending_metrics_sync = false;
        DpiUpdate {
            sync_layout_and_pty: true,
            ..DpiUpdate::default()
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Partition {
    /// 縦の仕切り線で左右に分ける（幅を分割）。
    Vertical,
    /// 横の仕切り線で上下に分ける（高さを分割）。
    Horizontal,
}

#[derive(Clone, Copy)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// マネージャが横取りするキー操作。
#[derive(Clone, Copy)]
enum Action {
    NewTab,
    OpenLauncher,
    OpenTaskOverview,
    CloseFocused,
    NextTab,
    PrevTab,
    SelectTab(usize),
    SplitVertical,
    SplitHorizontal,
    ToggleSidebar,
    ToggleFollow,
    Focus(Dir),
    Resize(Dir),
    /// フォントサイズを差分だけ変える（+1 / -1）。全ペイン・全パネルへ一括で効かせる。
    ChangeFont(i32),
}

#[derive(Clone, Copy)]
enum OverviewFocusRegion {
    Terminal,
    Sidebar,
    Reader,
    Editor,
}

#[derive(Clone, Copy)]
struct OverviewReturnFocus {
    pane: PaneId,
    region: OverviewFocusRegion,
}

/// タブ内のペイン木。葉が端末、節が分割。
enum Node {
    Leaf(Box<TerminalWindow>),
    Split(SplitNode),
    /// `mem::replace` の一時退避にだけ使う番兵。通常は出現しない。
    Empty,
}

trait PaneTreeNode: Sized {
    fn leaf_id(&self) -> Option<PaneId>;
    fn children(&self) -> Option<(&Self, &Self)>;
    fn children_mut(&mut self) -> Option<(&mut bool, &mut Self, &mut Self)>;
}

fn tree_contains_pane<T: PaneTreeNode>(tree: &T, id: PaneId) -> bool {
    if tree.leaf_id() == Some(id) {
        return true;
    }
    tree.children().is_some_and(|(first, second)| {
        tree_contains_pane(first, id) || tree_contains_pane(second, id)
    })
}

fn focus_pane_in_tree<T: PaneTreeNode>(tree: &mut T, id: PaneId) -> bool {
    if tree.leaf_id() == Some(id) {
        return true;
    }
    let target_first = match tree.children() {
        Some((first, _)) if tree_contains_pane(first, id) => Some(true),
        Some((_, second)) if tree_contains_pane(second, id) => Some(false),
        _ => None,
    };
    let Some(target_first) = target_first else {
        return false;
    };
    let Some((focus_first, first, second)) = tree.children_mut() else {
        return false;
    };
    let found = if target_first {
        focus_pane_in_tree(first, id)
    } else {
        focus_pane_in_tree(second, id)
    };
    if found {
        *focus_first = target_first;
    }
    found
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GtRoute {
    apply_panel: bool,
    notify: bool,
    review: bool,
}

fn route_gt(
    source: PaneId,
    focused: PaneId,
    signal: Option<AgentSignal>,
    window_focused: bool,
    modal_open: bool,
) -> GtRoute {
    let source_focused = source == focused;
    let done = signal == Some(AgentSignal::Done);
    GtRoute {
        apply_panel: source_focused,
        notify: done && !window_focused,
        review: done && source_focused && window_focused && !modal_open,
    }
}

fn terminal_focus_allowed(task_overview_open: bool) -> bool {
    !task_overview_open
}

fn overview_key_is_consumed(
    pending: Option<KeyCode>,
    state: ElementState,
    physical: PhysicalKey,
) -> bool {
    state == ElementState::Pressed
        && matches!(physical, PhysicalKey::Code(code) if pending == Some(code))
}

struct Tab<T = Node> {
    root: T,
    workbench_visible: bool,
}

impl<T> Tab<T> {
    fn new(root: T) -> Self {
        Self {
            root,
            workbench_visible: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WorkbenchFocus {
    sidebar: bool,
    editor: bool,
    reader: bool,
}

impl WorkbenchFocus {
    fn terminal_should_focus(self) -> bool {
        !self.sidebar && !self.editor && !self.reader
    }
}

trait WorkbenchVisibility {
    fn set_visible(&mut self, location: &ShellLocation, visible: bool);
}

trait WorkbenchViewInitializer {
    fn ensure_initialized(&mut self);
}

fn ensure_focused_workbench_views<T>(
    tabs: &[Tab<T>],
    focus: usize,
    sidebar: &mut impl WorkbenchViewInitializer,
    preview: &mut impl WorkbenchViewInitializer,
) {
    if tabs[focus].workbench_visible {
        sidebar.ensure_initialized();
        preview.ensure_initialized();
    }
}

impl WorkbenchVisibility for Sidebar {
    fn set_visible(&mut self, location: &ShellLocation, visible: bool) {
        Sidebar::set_visible(self, location, visible);
    }
}

fn synchronize_focused_tab_workbench<T>(
    tabs: &[Tab<T>],
    focus: usize,
    location: &ShellLocation,
    workbench: &mut impl WorkbenchVisibility,
    current_focus: WorkbenchFocus,
) -> WorkbenchFocus {
    let visible = tabs[focus].workbench_visible;
    workbench.set_visible(location, visible);
    if visible {
        current_focus
    } else {
        WorkbenchFocus::default()
    }
}

fn adjacent_tab_index(focus: usize, tab_count: usize, next: bool) -> usize {
    if next {
        (focus + 1) % tab_count
    } else {
        (focus + tab_count - 1) % tab_count
    }
}

fn remove_tab<T>(tabs: &mut Vec<Tab<T>>, focus: &mut usize, index: usize) -> Tab<T> {
    let removed = tabs.remove(index);
    if !tabs.is_empty() {
        if index < *focus {
            *focus -= 1;
        } else if *focus >= tabs.len() {
            *focus = tabs.len() - 1;
        }
    }
    removed
}

struct SplitNode {
    partition: Partition,
    ratio: f64,
    /// フォーカスが first 側にあるか。
    focus_first: bool,
    first: Box<Node>,
    second: Box<Node>,
}

fn capture_workspace_node(node: &mut Node) -> Result<crate::workspace_sets::SavedNode, String> {
    use crate::workspace_sets::SavedNode;
    match node {
        Node::Leaf(pane) => capture_workspace_location(pane.observed_location()),
        Node::Split(split) => Ok(SavedNode::Split {
            vertical: matches!(split.partition, Partition::Vertical),
            ratio: split.ratio,
            first: Box::new(capture_workspace_node(&mut split.first)?),
            second: Box::new(capture_workspace_node(&mut split.second)?),
        }),
        Node::Empty => Err("空のペインは保存できません".into()),
    }
}

fn capture_workspace_location(
    observed: Option<ShellLocation>,
) -> Result<crate::workspace_sets::SavedNode, String> {
    use crate::workspace_sets::SavedNode;
    match observed {
            Some(ShellLocation::Local(cwd)) => Ok(SavedNode::Pane { cwd }),
            Some(ShellLocation::Remote { .. }) => Err(
                "リモート接続の配置は保存できません。ローカルの作業セットを保存してください".into(),
            ),
            None => Err("作業フォルダを取得できません。WindowsではOSC 7のシェル統合を設定してから保存してください".into()),
    }
}

/// 親ビューポートを比率で2分割する（GAP 分の隙間を空ける）。
fn split_viewport(partition: Partition, ratio: f64, vp: Viewport) -> (Viewport, Viewport) {
    match partition {
        Partition::Vertical => {
            let mid = (vp.w as f64 * ratio).round() as u32;
            let left = Viewport {
                x: vp.x,
                y: vp.y,
                w: mid.saturating_sub(GAP),
                h: vp.h,
            };
            let right = Viewport {
                x: vp.x + mid + GAP,
                y: vp.y,
                w: vp.w.saturating_sub(mid + GAP),
                h: vp.h,
            };
            (left, right)
        }
        Partition::Horizontal => {
            let mid = (vp.h as f64 * ratio).round() as u32;
            let top = Viewport {
                x: vp.x,
                y: vp.y,
                w: vp.w,
                h: mid.saturating_sub(GAP),
            };
            let bottom = Viewport {
                x: vp.x,
                y: vp.y + mid + GAP,
                w: vp.w,
                h: vp.h.saturating_sub(mid + GAP),
            };
            (top, bottom)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WorkbenchViewports {
    sidebar: Viewport,
    preview: Viewport,
    terminal: Viewport,
}

/// ワークベンチ表示中だけ、タブバー下を左一覧・右上プレビュー・右下端末に分ける。
/// フォント差分を、既定サイズ(base)を足した実サイズが 6px〜72px に収まる範囲へ丸める。
/// 極端な連打で行き過ぎたり、下限で「実サイズ0」になったりしないようにする。
fn clamp_font_diff(diff: i32, base: i32) -> i32 {
    const MIN: i32 = 6;
    const MAX: i32 = 72;
    diff.clamp(MIN - base, MAX - base)
}

fn workbench_viewports(vp: Viewport, sidebar_ratio: f64, preview_ratio: f64) -> WorkbenchViewports {
    let (sidebar, right) = split_viewport(Partition::Vertical, sidebar_ratio.clamp(0.0, 1.0), vp);
    let (preview, terminal) =
        split_viewport(Partition::Horizontal, preview_ratio.clamp(0.0, 1.0), right);
    WorkbenchViewports {
        sidebar,
        preview,
        terminal,
    }
}

#[cfg(test)]
fn focused_workbench_viewports<T>(
    tabs: &[Tab<T>],
    focus: usize,
    content: Viewport,
    sidebar_ratio: f64,
    preview_ratio: f64,
) -> Option<WorkbenchViewports> {
    tabs[focus]
        .workbench_visible
        .then(|| workbench_viewports(content, sidebar_ratio, preview_ratio))
}

fn layout_for_focused_tab<T>(
    tabs: &[Tab<T>],
    focus: usize,
    content: Viewport,
    sidebar_ratio: f64,
    preview_ratio: f64,
) -> (WorkbenchViewports, Viewport) {
    let panels = workbench_viewports(content, sidebar_ratio, preview_ratio);
    let terminal = if tabs[focus].workbench_visible {
        panels.terminal
    } else {
        content
    };
    (panels, terminal)
}

pub(crate) fn command_exists(command: &str) -> bool {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path.is_file();
    }

    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };

    #[cfg(windows)]
    let extensions: Vec<String> = std::env::var_os("PATHEXT")
        .map(|value| {
            std::env::split_paths(&value)
                .map(|path| path.to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_else(|| vec![".exe".to_owned(), ".cmd".to_owned(), ".bat".to_owned()]);

    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(command);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            for ext in &extensions {
                if dir.join(format!("{command}{ext}")).is_file() {
                    return true;
                }
            }
        }
    }
    false
}

/// エージェント起動コマンドを「実行後に対話シェルへ落ちる」形に包む。
/// 例: fish なら `fish -c 'codex; exec fish'`。抜けてもタブが残る。
/// `None`（＝そのまま作業＝シェル）はそのまま。Windows は従来どおり直接実行。
fn wrap_agent_command(command: Option<Vec<String>>) -> Option<Vec<String>> {
    let cmd = command?;
    if cmd.is_empty() {
        return Some(cmd);
    }
    let shell = crate::TOYTERM_CONFIG
        .shell
        .first()
        .cloned()
        .unwrap_or_else(default_shell);

    #[cfg(windows)]
    {
        // Windows でも「実行後にプロンプトを残す」形にする。素の `claude` は
        // .cmd シムのことが多く CreateProcess で解決できず落ちるため、必ずシェル
        // 経由で起動する（PATHEXT 解決＋抜けてもタブが残る）。
        // セッション名など空白を含む引数がここで分割されないよう括る。
        let joined = cmd
            .iter()
            .map(|arg| win_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let lower = shell.to_ascii_lowercase();
        if lower.contains("powershell") || lower.contains("pwsh") {
            return Some(vec![
                shell,
                "-NoExit".to_owned(),
                "-Command".to_owned(),
                joined,
            ]);
        }
        return Some(vec![shell, "/K".to_owned(), joined]);
    }

    #[cfg(not(windows))]
    {
        // 実行後に対話シェルへ落ちる（抜けてもタブが残る）。例: fish -c 'codex; exec fish'。
        let joined = cmd
            .iter()
            .map(|arg| sh_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        Some(vec![
            shell.clone(),
            "-c".to_owned(),
            format!("{joined}; exec {}", sh_quote(&shell)),
        ])
    }
}

fn default_shell() -> String {
    #[cfg(windows)]
    {
        "cmd.exe".to_owned()
    }
    #[cfg(not(windows))]
    {
        "/bin/sh".to_owned()
    }
}

/// POSIX/fish で安全な単一引用符クォート。
#[cfg_attr(windows, allow(dead_code))]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// cmd.exe / PowerShell 向け。空白を含む引数だけ二重引用符で括る
/// （素の引数まで括ると cmd 側で余計な解釈が入ることがあるため）。
#[cfg_attr(not(windows), allow(dead_code))]
fn win_quote(s: &str) -> String {
    if s.contains(' ') && !s.starts_with('"') {
        format!("\"{s}\"")
    } else {
        s.to_owned()
    }
}

impl Node {
    fn focused_leaf(&self) -> &TerminalWindow {
        match self {
            Node::Leaf(w) => w,
            Node::Split(s) => {
                if s.focus_first {
                    s.first.focused_leaf()
                } else {
                    s.second.focused_leaf()
                }
            }
            Node::Empty => unreachable!("Empty node"),
        }
    }

    fn focused_leaf_mut(&mut self) -> &mut TerminalWindow {
        match self {
            Node::Leaf(w) => w,
            Node::Split(s) => {
                if s.focus_first {
                    s.first.focused_leaf_mut()
                } else {
                    s.second.focused_leaf_mut()
                }
            }
            Node::Empty => unreachable!("Empty node"),
        }
    }

    fn take_clicked_file(&mut self) -> Option<std::path::PathBuf> {
        self.focused_leaf_mut().take_clicked_file()
    }

    /// フォーカス中ペインを含む、軸 `axis` に一致する最も内側の分割の境界を
    /// `delta` だけ動かす（絶対方向＝矢印の向きに境界が動く）。戻り値 true=動かした。
    fn resize_focused(&mut self, axis: Partition, delta: f64) -> bool {
        match self {
            Node::Leaf(_) | Node::Empty => false,
            Node::Split(s) => {
                // フォーカス側の子を先に試し、より内側の一致を優先する。
                let deeper = if s.focus_first {
                    s.first.resize_focused(axis, delta)
                } else {
                    s.second.resize_focused(axis, delta)
                };
                if deeper {
                    return true;
                }
                if s.partition == axis {
                    s.ratio = (s.ratio + delta).clamp(0.15, 0.85);
                    return true;
                }
                false
            }
        }
    }

    fn set_viewport(&mut self, vp: Viewport) {
        match self {
            Node::Leaf(w) => w.set_viewport(vp),
            Node::Split(s) => {
                let (a, b) = split_viewport(s.partition, s.ratio, vp);
                s.first.set_viewport(a);
                s.second.set_viewport(b);
            }
            Node::Empty => {}
        }
    }

    fn draw(&mut self, surface: &mut glium::Frame) {
        match self {
            Node::Leaf(w) => w.draw(surface),
            Node::Split(s) => {
                s.first.draw(surface);
                s.second.draw(surface);
            }
            Node::Empty => {}
        }
    }

    fn for_each_leaf(&mut self, f: &mut dyn FnMut(&mut TerminalWindow)) {
        match self {
            Node::Leaf(w) => f(w),
            Node::Split(s) => {
                s.first.for_each_leaf(f);
                s.second.for_each_leaf(f);
            }
            Node::Empty => {}
        }
    }

    fn for_each_leaf_ref(&self, f: &mut dyn FnMut(&TerminalWindow)) {
        match self {
            Node::Leaf(w) => f(w),
            Node::Split(s) => {
                s.first.for_each_leaf_ref(f);
                s.second.for_each_leaf_ref(f);
            }
            Node::Empty => {}
        }
    }

    fn take_gt_messages(&mut self, out: &mut Vec<(PaneId, GtMessage)>) {
        self.for_each_leaf(&mut |w| {
            let id = w.pane_id();
            out.extend(
                w.take_gt_messages()
                    .into_iter()
                    .map(|message| (id, message)),
            );
        });
    }

    fn contains_pane(&self, id: PaneId) -> bool {
        tree_contains_pane(self, id)
    }

    fn focus_pane(&mut self, id: PaneId) -> bool {
        focus_pane_in_tree(self, id)
    }

    fn pane(&self, id: PaneId) -> Option<&TerminalWindow> {
        match self {
            Node::Leaf(win) => (win.pane_id() == id).then_some(win),
            Node::Split(split) => split.first.pane(id).or_else(|| split.second.pane(id)),
            Node::Empty => None,
        }
    }

    fn needs_redraw(&self) -> bool {
        match self {
            Node::Leaf(w) => w.needs_redraw(),
            Node::Split(s) => s.first.needs_redraw() || s.second.needs_redraw(),
            Node::Empty => false,
        }
    }

    /// カーソル位置 `p` を含む葉へフォーカス経路を張り替える（true=見つかった）。
    /// 葉の focus_changed は呼び出し側でまとめて行う。
    fn focus_at(&mut self, p: PhysicalPosition<f64>) -> bool {
        match self {
            Node::Leaf(w) => w.viewport().contains(p),
            Node::Split(s) => {
                if s.first.focus_at(p) {
                    s.focus_first = true;
                    true
                } else if s.second.focus_at(p) {
                    s.focus_first = false;
                    true
                } else {
                    false
                }
            }
            Node::Empty => false,
        }
    }

    /// フォーカス中の葉を分割する。`window`/`display` は新ペイン生成に使う。
    fn split_focused(
        &mut self,
        partition: Partition,
        window: &Rc<Window>,
        display: &Display,
        command: Option<&[String]>,
        scale_factor: f64,
        font_diff: i32,
    ) {
        match self {
            Node::Leaf(_) => {
                let vp = self.focused_leaf_mut().viewport();

                let taken = std::mem::replace(self, Node::Empty);
                let old = match taken {
                    Node::Leaf(w) => w,
                    _ => unreachable!(),
                };

                // 新ペインの作業ディレクトリは元ペインのシェルの現在地を継承する
                // （取れない環境では gototerm の起動ディレクトリ）。
                let cwd = match old.pane_location() {
                    ShellLocation::Local(path) => Some(path),
                    ShellLocation::Remote { .. } => std::env::current_dir().ok(),
                };
                let mut new_win = Box::new(TerminalWindow::with_viewport_command(
                    window.clone(),
                    display.clone(),
                    vp,
                    scale_factor,
                    cwd.as_deref(),
                    command,
                ));
                // 元ペインがズームされていれば、新ペインも同じ差分に合わせる。
                if font_diff != 0 {
                    new_win.change_font_size(font_diff);
                }

                let mut first = Box::new(Node::Leaf(old));
                let mut second = Box::new(Node::Leaf(new_win));
                first.focused_leaf_mut().focus_changed(false);
                second.focused_leaf_mut().focus_changed(true);

                *self = Node::Split(SplitNode {
                    partition,
                    ratio: 0.5,
                    focus_first: false, // 新ペイン(second)にフォーカス
                    first,
                    second,
                });
                self.set_viewport(vp);
            }
            Node::Split(s) => {
                if s.focus_first {
                    s.first.split_focused(
                        partition,
                        window,
                        display,
                        command,
                        scale_factor,
                        font_diff,
                    );
                } else {
                    s.second.split_focused(
                        partition,
                        window,
                        display,
                        command,
                        scale_factor,
                        font_diff,
                    );
                }
            }
            Node::Empty => {}
        }
    }

    /// 方向キーでフォーカスを移す（true=この部分木内で消費した）。
    fn move_focus(&mut self, dir: Dir) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split(s) => {
                let deep = if s.focus_first {
                    s.first.move_focus(dir)
                } else {
                    s.second.move_focus(dir)
                };
                if deep {
                    return true;
                }

                let can = matches!(
                    (s.partition, dir, s.focus_first),
                    (Partition::Vertical, Dir::Right, true)
                        | (Partition::Vertical, Dir::Left, false)
                        | (Partition::Horizontal, Dir::Down, true)
                        | (Partition::Horizontal, Dir::Up, false)
                );

                if can {
                    s.focused_child_leaf().focus_changed(false);
                    s.focus_first = !s.focus_first;
                    s.focused_child_leaf().focus_changed(true);
                    true
                } else {
                    false
                }
            }
            Node::Empty => false,
        }
    }

    /// フォーカス中の葉を閉じる。戻り値 true = このノードが空になった
    /// （＝親（またはタブ）が自分を取り除くべき）。
    fn close_focused(&mut self) -> bool {
        match self {
            Node::Leaf(w) => {
                w.close_pty();
                true
            }
            Node::Split(s) => {
                let removed = if s.focus_first {
                    s.first.close_focused()
                } else {
                    s.second.close_focused()
                };
                if removed {
                    // 閉じた側を外し、残った側を自分の位置へ引き上げる。
                    let survivor = if s.focus_first {
                        std::mem::replace(&mut *s.second, Node::Empty)
                    } else {
                        std::mem::replace(&mut *s.first, Node::Empty)
                    };
                    *self = survivor;
                    self.focused_leaf_mut().focus_changed(true);
                }
                false
            }
            Node::Empty => false,
        }
    }

    /// 全葉の PTY を1ティック分汲み取り、終了した葉を刈り取る。
    /// 分割を畳んだら `collapsed` を true にする（呼び出し側が再レイアウトする）。
    /// 戻り値 true = このノードの全端末が終了した（タブを閉じてよい）。
    fn update_and_prune(&mut self, collapsed: &mut bool) -> bool {
        match self {
            Node::Leaf(w) => w.check_update(),
            Node::Split(s) => {
                let a_dead = s.first.update_and_prune(collapsed);
                let b_dead = s.second.update_and_prune(collapsed);
                if a_dead && b_dead {
                    return true;
                }
                if a_dead {
                    let focus_was_here = s.focus_first;
                    let survivor = std::mem::replace(&mut *s.second, Node::Empty);
                    *self = survivor;
                    *collapsed = true;
                    if focus_was_here {
                        self.focused_leaf_mut().focus_changed(true);
                    }
                } else if b_dead {
                    let focus_was_here = !s.focus_first;
                    let survivor = std::mem::replace(&mut *s.first, Node::Empty);
                    *self = survivor;
                    *collapsed = true;
                    if focus_was_here {
                        self.focused_leaf_mut().focus_changed(true);
                    }
                }
                false
            }
            Node::Empty => true,
        }
    }
}

impl PaneTreeNode for Node {
    fn leaf_id(&self) -> Option<PaneId> {
        match self {
            Node::Leaf(win) => Some(win.pane_id()),
            Node::Split(_) | Node::Empty => None,
        }
    }

    fn children(&self) -> Option<(&Self, &Self)> {
        match self {
            Node::Split(split) => Some((&split.first, &split.second)),
            Node::Leaf(_) | Node::Empty => None,
        }
    }

    fn children_mut(&mut self) -> Option<(&mut bool, &mut Self, &mut Self)> {
        match self {
            Node::Split(split) => {
                Some((&mut split.focus_first, &mut split.first, &mut split.second))
            }
            Node::Leaf(_) | Node::Empty => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_capture_requires_observed_location_and_keeps_distinct_cwds() {
        use crate::workspace_sets::SavedNode;
        assert!(super::capture_workspace_location(None).is_err());
        let first = std::env::temp_dir().join("project-a");
        let second = std::env::temp_dir().join("project-b");
        assert_eq!(
            super::capture_workspace_location(Some(crate::vt::ShellLocation::Local(first.clone())))
                .unwrap(),
            SavedNode::Pane { cwd: first }
        );
        assert_eq!(
            super::capture_workspace_location(Some(crate::vt::ShellLocation::Local(
                second.clone()
            )))
            .unwrap(),
            SavedNode::Pane { cwd: second }
        );
        assert!(
            super::capture_workspace_location(Some(crate::vt::ShellLocation::Remote {
                host: "server".into(),
                path: "/project".into()
            }))
            .is_err()
        );
    }
    use super::*;

    #[test]
    fn background_done_never_opens_focused_review() {
        let route = route_gt(PaneId(2), PaneId(1), Some(AgentSignal::Done), true, false);
        assert!(!route.apply_panel);
        assert!(!route.review);
        assert!(!route.notify);

        let away = route_gt(PaneId(2), PaneId(1), Some(AgentSignal::Done), false, false);
        assert!(away.notify);
        assert!(!away.review);
    }

    #[test]
    fn focused_messages_apply_only_when_modal_allows_review() {
        let event = route_gt(PaneId(4), PaneId(4), None, true, false);
        assert!(event.apply_panel);
        assert!(!event.notify);
        assert!(!event.review);

        let done = route_gt(PaneId(4), PaneId(4), Some(AgentSignal::Done), true, false);
        assert!(done.apply_panel);
        assert!(done.review);

        let modal = route_gt(PaneId(4), PaneId(4), Some(AgentSignal::Done), true, true);
        assert!(modal.apply_panel);
        assert!(!modal.review);
    }

    #[test]
    fn background_lifecycle_cannot_restore_terminal_focus_under_overview() {
        assert!(!terminal_focus_allowed(true));
        assert!(terminal_focus_allowed(false));
    }

    #[test]
    fn overview_release_guard_consumes_same_key_until_release() {
        assert!(overview_key_is_consumed(
            Some(KeyCode::Escape),
            ElementState::Pressed,
            PhysicalKey::Code(KeyCode::Escape)
        ));
        assert!(!overview_key_is_consumed(
            Some(KeyCode::Escape),
            ElementState::Released,
            PhysicalKey::Code(KeyCode::Escape)
        ));
        assert!(!overview_key_is_consumed(
            Some(KeyCode::Escape),
            ElementState::Pressed,
            PhysicalKey::Code(KeyCode::Enter)
        ));
    }

    enum TestPaneTree {
        Leaf(PaneId),
        Split {
            focus_first: bool,
            first: Box<TestPaneTree>,
            second: Box<TestPaneTree>,
        },
    }

    impl PaneTreeNode for TestPaneTree {
        fn leaf_id(&self) -> Option<PaneId> {
            match self {
                Self::Leaf(id) => Some(*id),
                Self::Split { .. } => None,
            }
        }

        fn children(&self) -> Option<(&Self, &Self)> {
            match self {
                Self::Split { first, second, .. } => Some((first, second)),
                Self::Leaf(_) => None,
            }
        }

        fn children_mut(&mut self) -> Option<(&mut bool, &mut Self, &mut Self)> {
            match self {
                Self::Split {
                    focus_first,
                    first,
                    second,
                } => Some((focus_first, first, second)),
                Self::Leaf(_) => None,
            }
        }
    }

    fn test_tree() -> TestPaneTree {
        TestPaneTree::Split {
            focus_first: true,
            first: Box::new(TestPaneTree::Leaf(PaneId(1))),
            second: Box::new(TestPaneTree::Split {
                focus_first: true,
                first: Box::new(TestPaneTree::Leaf(PaneId(2))),
                second: Box::new(TestPaneTree::Leaf(PaneId(3))),
            }),
        }
    }

    #[test]
    fn pane_focus_changes_only_after_target_is_found() {
        let mut tree = test_tree();
        assert!(focus_pane_in_tree(&mut tree, PaneId(3)));
        let TestPaneTree::Split {
            focus_first,
            second,
            ..
        } = &tree
        else {
            panic!("expected split");
        };
        assert!(!focus_first);
        let TestPaneTree::Split { focus_first, .. } = second.as_ref() else {
            panic!("expected nested split");
        };
        assert!(!focus_first);

        assert!(!focus_pane_in_tree(&mut tree, PaneId(99)));
        let TestPaneTree::Split {
            focus_first,
            second,
            ..
        } = &tree
        else {
            panic!("expected split");
        };
        assert!(!focus_first);
        let TestPaneTree::Split { focus_first, .. } = second.as_ref() else {
            panic!("expected nested split");
        };
        assert!(!focus_first);
    }

    #[test]
    fn pane_membership_survives_tree_collapse_and_reordering() {
        let mut tree = test_tree();
        assert!(tree_contains_pane(&tree, PaneId(2)));
        tree = TestPaneTree::Split {
            focus_first: false,
            first: Box::new(TestPaneTree::Leaf(PaneId(3))),
            second: Box::new(TestPaneTree::Leaf(PaneId(2))),
        };
        assert!(focus_pane_in_tree(&mut tree, PaneId(2)));
        assert!(!tree_contains_pane(&tree, PaneId(1)));
    }

    #[test]
    fn dpi_scale_then_resize_syncs_surface_and_pty_once() {
        let original = winit::dpi::PhysicalSize::new(800, 600);
        let resized = winit::dpi::PhysicalSize::new(1200, 900);
        let mut state = DpiTransitionState::new(1.0, original);

        assert_eq!(state.begin_scale_factor_change(1.5), Some(1.5));
        state.mark_metrics_changed(true);
        assert_eq!(
            state.on_resized(resized),
            DpiUpdate {
                surface_size: Some(resized),
                viewport_size: Some(resized),
                sync_layout_and_pty: true,
            }
        );
        assert_eq!(state.flush_pending(), DpiUpdate::default());
        assert_eq!(state.on_resized(resized), DpiUpdate::default());
    }

    #[test]
    fn dpi_scale_without_resize_flushes_new_metrics_at_existing_size() {
        let size = winit::dpi::PhysicalSize::new(800, 600);
        let mut state = DpiTransitionState::new(1.0, size);

        assert_eq!(state.begin_scale_factor_change(1.25), Some(1.25));
        state.mark_metrics_changed(true);
        assert_eq!(
            state.flush_pending(),
            DpiUpdate {
                sync_layout_and_pty: true,
                ..DpiUpdate::default()
            }
        );
        assert_eq!(state.flush_pending(), DpiUpdate::default());
    }

    #[test]
    fn dpi_repeated_events_and_unchanged_metrics_are_noops() {
        let size = winit::dpi::PhysicalSize::new(800, 600);
        let mut state = DpiTransitionState::new(1.0, size);

        assert_eq!(state.begin_scale_factor_change(1.0), None);
        assert_eq!(state.on_resized(size), DpiUpdate::default());
        assert_eq!(state.begin_scale_factor_change(1.01), Some(1.01));
        state.mark_metrics_changed(false);
        assert_eq!(state.flush_pending(), DpiUpdate::default());
    }

    #[derive(Default)]
    struct RecordedWorkbench {
        visibility_calls: Vec<(ShellLocation, bool)>,
    }

    #[derive(Default)]
    struct RecordedWorkbenchViews {
        initialized: bool,
        initializations: usize,
    }

    impl WorkbenchViewInitializer for RecordedWorkbenchViews {
        fn ensure_initialized(&mut self) {
            if !self.initialized {
                self.initialized = true;
                self.initializations += 1;
            }
        }
    }

    impl WorkbenchVisibility for RecordedWorkbench {
        fn set_visible(&mut self, location: &ShellLocation, visible: bool) {
            self.visibility_calls.push((location.clone(), visible));
        }
    }

    #[test]
    fn visible_tab_sync_preserves_workbench_focus_and_refreshes_location() {
        let mut tabs = vec![Tab::new(()), Tab::new(())];
        tabs[0].workbench_visible = true;
        tabs[1].workbench_visible = true;
        let location = ShellLocation::Local(PathBuf::from("/second-tab"));
        let focus = WorkbenchFocus {
            sidebar: true,
            editor: false,
            reader: false,
        };
        let mut workbench = RecordedWorkbench::default();
        let selected = adjacent_tab_index(0, tabs.len(), true);

        let synced =
            synchronize_focused_tab_workbench(&tabs, selected, &location, &mut workbench, focus);

        assert_eq!(synced, focus, "表示中のサイドバーフォーカスを保つ");
        assert_eq!(
            workbench.visibility_calls,
            vec![(location, true)],
            "表示中同士の切替でも新しい現在地を即時反映する"
        );
    }

    #[test]
    fn hidden_tab_sync_releases_all_workbench_focus() {
        let tabs = vec![Tab::new(())];
        let location = ShellLocation::Local(PathBuf::from("/hidden-tab"));
        let mut workbench = RecordedWorkbench::default();

        let synced = synchronize_focused_tab_workbench(
            &tabs,
            0,
            &location,
            &mut workbench,
            WorkbenchFocus {
                sidebar: true,
                editor: true,
                reader: true,
            },
        );

        assert_eq!(synced, WorkbenchFocus::default());
        assert!(synced.terminal_should_focus());
        assert_eq!(workbench.visibility_calls, vec![(location, false)]);
    }

    #[test]
    fn removing_background_tab_keeps_visible_focus_state_attached() {
        let mut tabs = vec![Tab::new("first"), Tab::new("focused"), Tab::new("last")];
        tabs[1].workbench_visible = true;
        let mut focus = 1;
        let location = ShellLocation::Local(PathBuf::from("/focused"));
        let workbench_focus = WorkbenchFocus {
            sidebar: true,
            editor: false,
            reader: false,
        };
        let mut workbench = RecordedWorkbench::default();

        let removed = remove_tab(&mut tabs, &mut focus, 0);
        let synced = synchronize_focused_tab_workbench(
            &tabs,
            focus,
            &location,
            &mut workbench,
            workbench_focus,
        );

        assert_eq!(removed.root, "first");
        assert_eq!(focus, 0);
        assert_eq!(tabs[focus].root, "focused");
        assert!(tabs[focus].workbench_visible);
        assert_eq!(
            synced, workbench_focus,
            "背景タブ削除でフォーカスを奪わない"
        );
        assert_eq!(workbench.visibility_calls, vec![(location, true)]);
    }

    #[test]
    fn startup_replacement_applies_selected_tabs_visibility() {
        let mut tabs = vec![Tab::new("startup"), Tab::new("selected")];
        tabs[0].workbench_visible = true;
        let mut focus = 1;
        let location = ShellLocation::Local(PathBuf::from("/selected"));
        let mut workbench = RecordedWorkbench::default();

        let removed = remove_tab(&mut tabs, &mut focus, 0);
        let synced = synchronize_focused_tab_workbench(
            &tabs,
            focus,
            &location,
            &mut workbench,
            WorkbenchFocus {
                sidebar: true,
                editor: false,
                reader: false,
            },
        );

        assert_eq!(removed.root, "startup");
        assert_eq!(focus, 0);
        assert_eq!(tabs[focus].root, "selected");
        assert!(!tabs[focus].workbench_visible);
        assert_eq!(synced, WorkbenchFocus::default());
        assert_eq!(workbench.visibility_calls, vec![(location, false)]);
    }

    #[test]
    fn focused_tab_flag_selects_terminal_viewport() {
        let mut tabs = vec![Tab::new(()), Tab::new(())];
        tabs[0].workbench_visible = true;
        let content = Viewport {
            x: 0,
            y: 20,
            w: 1000,
            h: 700,
        };

        let visible = focused_workbench_viewports(&tabs, 0, content, 0.25, 0.5);
        let hidden = focused_workbench_viewports(&tabs, 1, content, 0.25, 0.5);

        assert_eq!(
            visible.unwrap().terminal,
            workbench_viewports(content, 0.25, 0.5).terminal
        );
        assert_eq!(hidden, None);
    }

    #[test]
    fn hidden_workbench_keeps_lazy_panel_viewports_current() {
        let tabs = vec![Tab::new(())];
        let content = Viewport {
            x: 0,
            y: 20,
            w: 1000,
            h: 700,
        };

        let (panels, terminal) = layout_for_focused_tab(&tabs, 0, content, 0.25, 0.5);

        assert_eq!(
            panels,
            WorkbenchViewports {
                sidebar: Viewport {
                    x: 0,
                    y: 20,
                    w: 248,
                    h: 700
                },
                preview: Viewport {
                    x: 252,
                    y: 20,
                    w: 748,
                    h: 348
                },
                terminal: Viewport {
                    x: 252,
                    y: 372,
                    w: 748,
                    h: 348
                },
            }
        );
        assert_eq!(terminal, content);
    }

    #[test]
    fn switching_tabs_restores_each_workbench_visibility() {
        let mut tabs = [Tab::new(()), Tab::new(())];
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

    #[test]
    fn workbench_views_are_lazy() {
        let mut tabs = vec![Tab::new(())];
        let mut sidebar = RecordedWorkbenchViews::default();
        let mut reader = RecordedWorkbenchViews::default();

        ensure_focused_workbench_views(&tabs, 0, &mut sidebar, &mut reader);
        assert_eq!((sidebar.initializations, reader.initializations), (0, 0));

        tabs[0].workbench_visible = true;
        ensure_focused_workbench_views(&tabs, 0, &mut sidebar, &mut reader);
        assert_eq!((sidebar.initializations, reader.initializations), (1, 1));

        tabs[0].workbench_visible = false;
        ensure_focused_workbench_views(&tabs, 0, &mut sidebar, &mut reader);
        tabs[0].workbench_visible = true;
        ensure_focused_workbench_views(&tabs, 0, &mut sidebar, &mut reader);
        assert_eq!((sidebar.initializations, reader.initializations), (1, 1));
    }

    #[test]
    fn sh_quote_wraps_and_escapes_single_quotes() {
        assert_eq!(sh_quote("codex"), "'codex'");
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn font_diff_moves_freely_within_range() {
        // base=18: +3 も -5 もそのまま通る。
        assert_eq!(clamp_font_diff(3, 18), 3);
        assert_eq!(clamp_font_diff(-5, 18), -5);
    }

    #[test]
    fn font_diff_stops_at_the_floor_and_ceiling() {
        // base=18 → 実サイズ下限6px は diff -12、上限72px は diff +54。
        assert_eq!(clamp_font_diff(-100, 18), -12, "6px より小さくしない");
        assert_eq!(clamp_font_diff(100, 18), 54, "72px より大きくしない");
    }

    /// 下限に張り付いた状態からさらに「−」を押しても実サイズは動かない（applied=0）。
    #[test]
    fn font_diff_at_floor_does_not_shrink_further() {
        let base = 18;
        let at_floor = clamp_font_diff(-100, base); // -12
        let next = clamp_font_diff(at_floor - 1, base);
        assert_eq!(next, at_floor, "これ以上縮まない");
    }

    #[test]
    fn wrap_agent_command_keeps_shell_choice_as_none() {
        assert_eq!(wrap_agent_command(None), None);
    }

    #[test]
    #[cfg(windows)]
    fn wrap_agent_command_keeps_pane_open_on_windows() {
        let wrapped = wrap_agent_command(Some(vec!["claude".to_owned()])).unwrap();
        // シェル経由で起動し、実行後もプロンプトを残す（/K か -NoExit）。
        assert!(wrapped.len() >= 3);
        assert!(wrapped.iter().any(|s| s == "claude"));
        assert!(wrapped.iter().any(|s| s == "/K" || s == "-NoExit"));
    }

    #[test]
    #[cfg(not(windows))]
    fn wrap_agent_command_drops_to_shell_after_agent() {
        let wrapped = wrap_agent_command(Some(vec!["codex".to_owned()])).unwrap();
        // [shell, "-c", "<agent>; exec <shell>"]
        assert_eq!(wrapped.len(), 3);
        assert_eq!(wrapped[1], "-c");
        assert!(wrapped[2].contains("'codex'"));
        assert!(wrapped[2].contains("; exec "));
    }

    #[test]
    fn vertical_split_leaves_a_gap_and_fills_width() {
        let vp = Viewport {
            x: 10,
            y: 20,
            w: 100,
            h: 50,
        };
        let (left, right) = split_viewport(Partition::Vertical, 0.5, vp);

        // 左右は同じ高さ・同じ y、左端は親の左端
        assert_eq!(left.y, 20);
        assert_eq!(right.y, 20);
        assert_eq!(left.h, 50);
        assert_eq!(right.h, 50);
        assert_eq!(left.x, 10);

        // 仕切りで GAP 分の隙間が空く（mid=50）
        assert_eq!(left.w, 50 - GAP);
        assert_eq!(right.x, 10 + 50 + GAP);
        assert_eq!(right.w, 100 - 50 - GAP);
        // 隙間(GAP*2)を除いて親幅をちょうど覆う
        assert_eq!(left.w + right.w + GAP * 2, vp.w);
    }

    #[test]
    fn horizontal_split_leaves_a_gap_and_fills_height() {
        let vp = Viewport {
            x: 0,
            y: 0,
            w: 80,
            h: 200,
        };
        let (top, bottom) = split_viewport(Partition::Horizontal, 0.5, vp);

        assert_eq!(top.w, 80);
        assert_eq!(bottom.w, 80);
        assert_eq!(top.x, 0);
        assert_eq!(top.y, 0);
        assert_eq!(top.h, 100 - GAP);
        assert_eq!(bottom.y, 100 + GAP);
        assert_eq!(bottom.h, 200 - 100 - GAP);
        assert_eq!(top.h + bottom.h + GAP * 2, vp.h);
    }

    #[test]
    fn uneven_ratio_keeps_panes_within_parent() {
        let vp = Viewport {
            x: 0,
            y: 0,
            w: 120,
            h: 60,
        };
        let (left, right) = split_viewport(Partition::Vertical, 0.25, vp);
        // mid = 30
        assert_eq!(left.w, 30 - GAP);
        assert_eq!(right.x, 30 + GAP);
        assert_eq!(right.w, 120 - 30 - GAP);
    }

    #[test]
    fn hidden_sidebar_keeps_content_viewport_unchanged() {
        let vp = Viewport {
            x: 5,
            y: 7,
            w: 300,
            h: 200,
        };

        assert_eq!(vp.x, 5);
        assert_eq!(vp.y, 7);
        assert_eq!(vp.w, 300);
        assert_eq!(vp.h, 200);
    }

    #[test]
    fn workbench_layout_uses_left_sidebar_and_right_preview_terminal() {
        let vp = Viewport {
            x: 0,
            y: 20,
            w: 1000,
            h: 700,
        };
        let layout = workbench_viewports(vp, 0.25, 0.5);

        assert_eq!(layout.sidebar.x, 0);
        assert_eq!(layout.sidebar.y, 20);
        assert_eq!(layout.sidebar.w, 250 - GAP);
        assert_eq!(layout.sidebar.h, 700);

        assert_eq!(layout.preview.x, 250 + GAP);
        assert_eq!(layout.preview.y, 20);
        assert_eq!(layout.preview.w, 1000 - 250 - GAP);
        assert_eq!(layout.preview.h, 350 - GAP);

        assert_eq!(layout.terminal.x, 250 + GAP);
        assert_eq!(layout.terminal.y, 20 + 350 + GAP);
        assert_eq!(layout.terminal.w, 1000 - 250 - GAP);
        assert_eq!(layout.terminal.h, 700 - 350 - GAP);
    }

    #[test]
    fn workbench_layout_clamps_ratios() {
        let vp = Viewport {
            x: 0,
            y: 0,
            w: 100,
            h: 80,
        };
        let layout = workbench_viewports(vp, 2.0, -1.0);

        assert_eq!(layout.sidebar.w, 100 - GAP);
        assert_eq!(layout.sidebar.y, 0);
        assert_eq!(layout.preview.h, 0);
        assert_eq!(layout.terminal.h, 80 - GAP);
    }
}

impl SplitNode {
    fn focused_child_leaf(&mut self) -> &mut TerminalWindow {
        if self.focus_first {
            self.first.focused_leaf_mut()
        } else {
            self.second.focused_leaf_mut()
        }
    }
}

enum PreviewSlot {
    Reader(ReaderPane),
    Editor {
        win: Box<TerminalWindow>,
        saved: Box<ReaderPane>,
    },
    Empty,
}

impl PreviewSlot {
    fn reader_mut(&mut self) -> Option<&mut ReaderPane> {
        match self {
            PreviewSlot::Reader(reader) => Some(reader),
            PreviewSlot::Editor { saved, .. } => Some(saved),
            PreviewSlot::Empty => None,
        }
    }

    fn ensure_view_initialized(&mut self) {
        if let Some(reader) = self.reader_mut() {
            reader.ensure_view_initialized();
        }
    }

    fn visible_reader_mut(&mut self) -> Option<&mut ReaderPane> {
        match self {
            PreviewSlot::Reader(reader) => Some(reader),
            _ => None,
        }
    }

    fn editor_mut(&mut self) -> Option<&mut TerminalWindow> {
        match self {
            PreviewSlot::Editor { win, .. } => Some(win),
            _ => None,
        }
    }

    fn contains(&self, p: PhysicalPosition<f64>) -> bool {
        match self {
            PreviewSlot::Reader(reader) => reader.contains(p),
            PreviewSlot::Editor { win, .. } => win.viewport().contains(p),
            PreviewSlot::Empty => false,
        }
    }

    fn set_viewport(&mut self, viewport: Viewport) {
        match self {
            PreviewSlot::Reader(reader) => reader.set_viewport(viewport),
            PreviewSlot::Editor { win, saved } => {
                win.set_viewport(viewport);
                saved.set_viewport(viewport);
            }
            PreviewSlot::Empty => {}
        }
    }

    fn change_font_size(&mut self, size_diff: i32) {
        match self {
            PreviewSlot::Reader(reader) => reader.change_font_size(size_diff),
            // エディタを開いている間も、裏に控えるビューア(saved)ごと揃える。
            PreviewSlot::Editor { win, saved } => {
                win.change_font_size(size_diff);
                saved.change_font_size(size_diff);
            }
            PreviewSlot::Empty => {}
        }
    }

    fn set_scale_factor(&mut self, scale_factor: f64) -> bool {
        match self {
            PreviewSlot::Reader(reader) => reader.set_scale_factor(scale_factor),
            PreviewSlot::Editor { win, saved } => {
                win.set_scale_factor(scale_factor) | saved.set_scale_factor(scale_factor)
            }
            PreviewSlot::Empty => false,
        }
    }

    fn draw(&mut self, surface: &mut glium::Frame) {
        match self {
            PreviewSlot::Reader(reader) => reader.draw(surface),
            PreviewSlot::Editor { win, .. } => win.draw(surface),
            PreviewSlot::Empty => {}
        }
    }

    fn needs_redraw(&self) -> bool {
        match self {
            PreviewSlot::Reader(reader) => reader.needs_redraw(),
            PreviewSlot::Editor { win, .. } => win.needs_redraw(),
            PreviewSlot::Empty => false,
        }
    }

    fn check_update(&mut self) -> bool {
        let PreviewSlot::Editor { win, .. } = self else {
            return false;
        };
        win.check_update()
    }

    fn drain_gt_messages(&mut self) {
        if let PreviewSlot::Editor { win, .. } = self {
            let _ = win.take_gt_messages();
        }
    }
}

impl WorkbenchViewInitializer for Sidebar {
    fn ensure_initialized(&mut self) {
        self.ensure_view_initialized();
    }
}

impl WorkbenchViewInitializer for PreviewSlot {
    fn ensure_initialized(&mut self) {
        self.ensure_view_initialized();
    }
}

pub struct Multiplexer {
    window: Rc<Window>,
    display: Display,
    viewport: Viewport,
    status_view: TerminalView,
    status_signature: String,
    sidebar: Sidebar,
    sidebar_focused: bool,
    preview_slot: PreviewSlot,
    launcher: Option<Launcher>,
    /// Stop hook（AI 応答完了）受信時に出す変更点の要約ポップアップ。
    session_review: Option<SessionReview>,
    task_overview: Option<TaskOverview>,
    overview_return_focus: Option<OverviewReturnFocus>,
    overview_release_key: Option<winit::keyboard::KeyCode>,
    overview_last_refresh: Instant,
    recent: RecentProjects,
    /// 起動時に自動で開いたランチャーか（true の間に選ぶと最初の空タブを畳む）。
    startup_launcher: bool,
    editor_focused: bool,
    reader_focused: bool,
    gt_file_assembler: GtFileAssembler,
    observed_pane: PaneId,
    tabs: Vec<Tab>,
    focus: usize,
    modifiers: ModifiersState,
    cursor_pos: PhysicalPosition<f64>,
    exited: bool,
    /// ウィンドウが隠れているか。Wayland では隠れると frame callback が
    /// 止まり、vsync 付きの描画がブロックして無応答になるため、隠れている間は
    /// 描画しない。`WindowEvent::Occluded` で更新する。
    occluded: bool,
    /// ウィンドウがフォーカスされているか。AI の完了通知（Stop hook）は
    /// 画面を見ていないとき（非フォーカス）だけ出す判定に使う。
    window_focused: bool,
    /// カーソル点滅の起点と現在の表示フェーズ。
    blink_start: Instant,
    cursor_blink_on: bool,
    /// ワークベンチの左サイドバー幅・上プレビュー高さの比率（実行時に
    /// Ctrl+Shift+矢印 で変えられる。初期値は config）。
    sidebar_ratio: f64,
    preview_ratio: f64,
    /// 設定の既定サイズからのフォント差分。あとから開くペイン・タブ・ランチャーにも
    /// この差分を適用して、画面全体の文字サイズを常に揃える。
    font_diff: i32,
    dpi_transition: DpiTransitionState,
}

impl Multiplexer {
    pub fn new(window: Window, display: Display) -> Self {
        let window = Rc::new(window);

        let size = window.inner_size();
        let scale_factor = window.scale_factor();
        let dpi_transition = DpiTransitionState::new(scale_factor, size);
        let viewport = Viewport {
            x: 0,
            y: 0,
            w: size.width,
            h: size.height,
        };

        let status_view = TerminalView::with_viewport(
            display.clone(),
            viewport,
            crate::TOYTERM_CONFIG.status_bar_font_size,
            scale_factor,
            None,
        );
        let sidebar = Sidebar::new(display.clone(), viewport, scale_factor);
        let preview_slot =
            PreviewSlot::Reader(ReaderPane::new(display.clone(), viewport, scale_factor));

        let mut recent = RecentProjects::load();
        let initial_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        // 最初のタブ（1枚なのでタブバーは出ない＝全面が端末）。
        let first = Node::Leaf(Box::new(TerminalWindow::with_viewport(
            window.clone(),
            display.clone(),
            viewport,
            scale_factor,
            None,
        )));
        recent.record(&initial_cwd);

        let observed_pane = first.focused_leaf().pane_id();
        let mut mux = Multiplexer {
            window,
            display,
            viewport,
            status_view,
            status_signature: String::new(),
            sidebar,
            sidebar_focused: false,
            preview_slot,
            launcher: None,
            session_review: None,
            task_overview: None,
            overview_return_focus: None,
            overview_release_key: None,
            overview_last_refresh: Instant::now(),
            recent,
            startup_launcher: false,
            editor_focused: false,
            reader_focused: false,
            gt_file_assembler: GtFileAssembler::default(),
            observed_pane,
            tabs: vec![Tab::new(first)],
            focus: 0,
            modifiers: ModifiersState::empty(),
            cursor_pos: PhysicalPosition::default(),
            exited: false,
            occluded: false,
            window_focused: true,
            blink_start: Instant::now(),
            cursor_blink_on: true,
            sidebar_ratio: crate::TOYTERM_CONFIG.sidebar_ratio,
            preview_ratio: crate::TOYTERM_CONFIG.preview_ratio,
            font_diff: 0,
            dpi_transition,
        };
        mux.refresh_layout();
        // 設定で有効なら、起動直後にランチャーを重ねて出す。
        if crate::TOYTERM_CONFIG.show_launcher_on_start {
            mux.launcher = Some(Launcher::new(
                mux.display.clone(),
                mux.viewport,
                mux.dpi_transition.scale_factor(),
                mux.recent.entries(),
            ));
            mux.startup_launcher = true;
        }
        mux
    }

    fn focused_root(&mut self) -> &mut Node {
        &mut self.tabs[self.focus].root
    }

    fn focused_location(&mut self) -> ShellLocation {
        self.focused_root().focused_leaf_mut().pane_location()
    }

    fn collect_task_rows(&self) -> Vec<TaskRow> {
        let mut rows = Vec::new();
        for (tab_index, tab) in self.tabs.iter().enumerate() {
            let mut pane_number = 0;
            tab.root.for_each_leaf_ref(&mut |window| {
                pane_number += 1;
                let location = window.observed_location().map(|location| match location {
                    ShellLocation::Local(path) => {
                        sanitize_display_text(&path.to_string_lossy(), usize::MAX)
                    }
                    ShellLocation::Remote { host, path } => sanitize_display_text(
                        &format!("{}:{}", host, path.to_string_lossy()),
                        usize::MAX,
                    ),
                });
                rows.push(TaskRow {
                    id: window.pane_id(),
                    tab_number: tab_index + 1,
                    pane_number,
                    location,
                    activity: window.activity().clone(),
                });
            });
        }
        rows
    }

    fn activate_pane_by_id(&mut self, id: PaneId) -> bool {
        let Some(tab_index) = self.tabs.iter().position(|tab| tab.root.contains_pane(id)) else {
            return false;
        };

        self.focused_root().focused_leaf_mut().focus_changed(false);
        if self.sidebar_focused {
            self.sidebar.set_focused(false);
        }
        if self.reader_focused {
            self.unfocus_reader();
        }
        if self.editor_focused {
            if let Some(editor) = self.preview_slot.editor_mut() {
                editor.focus_changed(false);
            }
        }
        self.sidebar_focused = false;
        self.reader_focused = false;
        self.editor_focused = false;
        self.focus = tab_index;
        let found = self.tabs[tab_index].root.focus_pane(id);
        debug_assert!(found);
        self.apply_focused_tab_workbench();
        self.update_status_bar();
        self.window.set_ime_allowed(true);
        true
    }

    fn open_task_overview(&mut self) {
        if self.launcher.is_some() || self.session_review.is_some() || self.task_overview.is_some()
        {
            return;
        }
        let current = self.tabs[self.focus].root.focused_leaf().pane_id();
        let region = if self.sidebar_focused {
            OverviewFocusRegion::Sidebar
        } else if self.reader_focused {
            OverviewFocusRegion::Reader
        } else if self.editor_focused {
            OverviewFocusRegion::Editor
        } else {
            OverviewFocusRegion::Terminal
        };
        self.overview_return_focus = Some(OverviewReturnFocus {
            pane: current,
            region,
        });

        self.suspend_focus_for_overview();

        let rows = self.collect_task_rows();
        let mut overview = TaskOverview::new(
            self.display.clone(),
            self.viewport,
            self.dpi_transition.scale_factor(),
            rows,
            current,
        );
        if self.font_diff != 0 {
            overview.change_font_size(self.font_diff);
        }
        self.task_overview = Some(overview);
        self.overview_last_refresh = Instant::now();
        self.window.request_redraw();
    }

    fn suspend_focus_for_overview(&mut self) {
        self.focused_root().focused_leaf_mut().focus_changed(false);
        self.sidebar.set_focused(false);
        if let Some(reader) = self.preview_slot.reader_mut() {
            reader.set_focused(false);
        }
        if let Some(editor) = self.preview_slot.editor_mut() {
            editor.focus_changed(false);
        }
        self.window.set_ime_allowed(false);
    }

    fn dismiss_task_overview(&mut self) {
        self.task_overview = None;
        let restore = self.overview_return_focus.take();
        let restored_pane = restore.is_some_and(|restore| self.activate_pane_by_id(restore.pane));
        if !restored_pane {
            self.focused_root().focused_leaf_mut().focus_changed(true);
            self.window.set_ime_allowed(true);
        }
        if let Some(restore) = restore {
            match restore.region {
                OverviewFocusRegion::Terminal => {}
                OverviewFocusRegion::Sidebar if self.sidebar.is_visible() => self.focus_sidebar(),
                OverviewFocusRegion::Reader
                    if self.sidebar.is_visible()
                        && self.preview_slot.visible_reader_mut().is_some() =>
                {
                    self.focus_reader()
                }
                OverviewFocusRegion::Editor
                    if self.sidebar.is_visible() && self.preview_slot.editor_mut().is_some() =>
                {
                    self.focus_editor()
                }
                _ => {}
            }
        }
        self.window.request_redraw();
    }

    fn handle_overview_outcome(&mut self, outcome: OverviewOutcome, key: KeyCode) {
        match outcome {
            OverviewOutcome::None => {}
            OverviewOutcome::Dismissed => {
                self.overview_release_key = Some(key);
                self.dismiss_task_overview();
            }
            OverviewOutcome::Activate(id) => {
                if self.activate_pane_by_id(id) {
                    self.overview_release_key = Some(key);
                    self.task_overview = None;
                    self.overview_return_focus = None;
                    self.window.request_redraw();
                } else {
                    let rows = self.collect_task_rows();
                    if let Some(overview) = self.task_overview.as_mut() {
                        overview.update_rows(rows, Instant::now());
                        overview.set_notice("選択した作業は終了しました");
                    }
                }
            }
        }
    }

    fn apply_focused_tab_workbench(&mut self) {
        ensure_focused_workbench_views(
            &self.tabs,
            self.focus,
            &mut self.sidebar,
            &mut self.preview_slot,
        );
        let location = self.focused_location();
        let previous_focus = WorkbenchFocus {
            sidebar: self.sidebar_focused,
            editor: self.editor_focused,
            reader: self.reader_focused,
        };
        let synced_focus = synchronize_focused_tab_workbench(
            &self.tabs,
            self.focus,
            &location,
            &mut self.sidebar,
            previous_focus,
        );

        if previous_focus.sidebar && !synced_focus.sidebar {
            self.sidebar_focused = false;
            self.sidebar.set_focused(false);
        }
        if previous_focus.editor && !synced_focus.editor {
            if let Some(editor) = self.preview_slot.editor_mut() {
                editor.focus_changed(false);
            }
            self.editor_focused = false;
        }
        if previous_focus.reader && !synced_focus.reader {
            self.unfocus_reader();
        }

        self.refresh_layout();
        if synced_focus.terminal_should_focus() {
            self.focused_root().focused_leaf_mut().focus_changed(true);
        }
    }

    /// フォーカスを矢印方向へ動かす。ワークベンチ表示中は3領域
    /// （左=サイドバー／右上=プレビュー／右下=ターミナル）もまたぐ。
    /// ターミナル領域内では従来どおり分割ツリーを辿り、端に達したら隣の領域へ。
    fn move_focus_workbench(&mut self, dir: Dir) {
        if !self.sidebar.is_visible() {
            self.tabs[self.focus].root.move_focus(dir);
            return;
        }
        if self.sidebar_focused {
            // サイドバーから右 → ターミナルへ。
            if matches!(dir, Dir::Right) {
                self.release_sidebar_focus();
            }
            return;
        }
        if self.editor_focused {
            // エディタ（プレビュー枠）から左 → サイドバーへ。
            if matches!(dir, Dir::Left) {
                self.focus_sidebar();
            }
            return;
        }
        if self.reader_focused {
            // ビューアから下 → ターミナル、左 → サイドバーへ。
            match dir {
                Dir::Down => self.release_reader_focus(),
                Dir::Left => self.focus_sidebar(),
                _ => {}
            }
            return;
        }
        // ターミナル領域。まず分割ツリー内で移動し、端に達したら隣の領域へ。
        if self.tabs[self.focus].root.move_focus(dir) {
            return;
        }
        match dir {
            Dir::Left => self.focus_sidebar(),
            Dir::Up => self.focus_reader(),
            _ => {}
        }
    }

    /// ペイン境界を矢印方向へ動かす。ワークベンチ表示中はその3分割の境界
    /// （左右=サイドバー幅／上下=プレビュー高さ）を、そうでなければフォーカス中の
    /// 分割境界を動かす。1回あたり 3%。
    fn resize(&mut self, dir: Dir) {
        const STEP: f64 = 0.03;
        let (axis, sign) = match dir {
            Dir::Left => (Partition::Vertical, -1.0),
            Dir::Right => (Partition::Vertical, 1.0),
            Dir::Up => (Partition::Horizontal, -1.0),
            Dir::Down => (Partition::Horizontal, 1.0),
        };
        let delta = STEP * sign;

        if self.sidebar.is_visible() {
            match axis {
                Partition::Vertical => {
                    self.sidebar_ratio = (self.sidebar_ratio + delta).clamp(0.15, 0.6);
                }
                Partition::Horizontal => {
                    self.preview_ratio = (self.preview_ratio + delta).clamp(0.15, 0.85);
                }
            }
            self.refresh_layout();
        } else if self.tabs[self.focus].root.resize_focused(axis, delta) {
            self.refresh_layout();
        }
    }

    /// タブバーの高さ（px）。タブ1枚のときは 0（バー非表示）。
    fn status_bar_height(&self) -> u32 {
        if self.tabs.len() <= 1 {
            0
        } else {
            self.status_view.cell_size().h
        }
    }

    /// 端末群が使える領域（タブバーの下）。
    fn content_viewport(&self) -> Viewport {
        let h = self.status_bar_height();
        Viewport {
            x: 0,
            y: h,
            w: self.viewport.w,
            h: self.viewport.h.saturating_sub(h),
        }
    }

    /// 全タブのビューポートを再計算する（タブバーの有無が変わったときも）。
    fn refresh_layout(&mut self) {
        let bar = Viewport {
            x: 0,
            y: 0,
            w: self.viewport.w,
            h: self.status_view.cell_size().h,
        };
        self.status_view.set_viewport(bar);

        let content = self.content_viewport();
        let (viewports, cvp) = layout_for_focused_tab(
            &self.tabs,
            self.focus,
            content,
            self.sidebar_ratio,
            self.preview_ratio,
        );
        self.sidebar.set_viewport(viewports.sidebar);
        self.preview_slot.set_viewport(viewports.preview);
        for tab in &mut self.tabs {
            tab.root.set_viewport(cvp);
        }
    }

    fn update_status_bar(&mut self) {
        if self.tabs.len() <= 1 {
            return;
        }

        const BAR_BG: Color = Color::Background;
        let cols = (self.viewport.w / self.status_view.cell_size().w).max(1) as usize;
        let mut titles = Vec::new();
        for tab in &mut self.tabs {
            let path = match tab.root.focused_leaf_mut().pane_location() {
                ShellLocation::Local(path) | ShellLocation::Remote { path, .. } => path,
            };
            titles.push(
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "shell".into()),
            );
        }
        let signature = format!("{cols}:{}:{titles:?}", self.focus);
        if signature == self.status_signature {
            return;
        }
        self.status_signature = signature;
        let visible = (cols / 4).max(1).min(self.tabs.len());
        let first = if self.tabs.len() > visible {
            self.focus.saturating_sub(visible - 1)
        } else {
            0
        };
        let width = (cols / visible).clamp(1, 26);
        let mut cells = Vec::new();
        for (i, title) in titles.iter().enumerate().skip(first).take(visible) {
            let active = i == self.focus;
            let bg = if active { Color::BrightBlack } else { BAR_BG };
            let prefix = if width >= 4 {
                format!("{}{:02} ", if active { "›" } else { " " }, i + 1)
            } else {
                format!("{:02}", i + 1)
            };
            let prefix_width = width.min(if width >= 4 { 4 } else { 2 });
            let mut header = crate::launcher::column_cells(
                &prefix,
                if active { Color::Cyan } else { Color::White },
                bg,
                prefix_width,
            );
            if width > prefix_width {
                header.extend(crate::launcher::column_cells(
                    title,
                    Color::White,
                    bg,
                    width - prefix_width,
                ));
            }
            if !active {
                for cell in &mut header {
                    cell.attr.bold = -1;
                }
            }
            cells.extend(header);
        }
        cells.truncate(cols);
        cells.resize_with(cols, || {
            crate::launcher::column_cells(" ", Color::White, BAR_BG, 1)[0]
        });

        self.status_view.update_contents(|view| {
            view.bg_color = BAR_BG;
            view.lines = vec![Line::from_cells(cells, false)];
            view.images = Vec::new();
            view.cursor = None;
            view.selection_range = None;
        });
    }

    fn parse_shortcut(&self, key: &KeyEvent) -> Option<Action> {
        if key.state != ElementState::Pressed {
            return None;
        }
        let code = match key.physical_key {
            PhysicalKey::Code(c) => c,
            PhysicalKey::Unidentified(_) => return None,
        };

        match keybindings::lookup(self.modifiers, code)? {
            ShortcutAction::NewTab => Some(Action::NewTab),
            ShortcutAction::OpenLauncher => Some(Action::OpenLauncher),
            ShortcutAction::OpenTaskOverview => Some(Action::OpenTaskOverview),
            ShortcutAction::ClosePane => Some(Action::CloseFocused),
            ShortcutAction::NextTab => Some(Action::NextTab),
            ShortcutAction::PrevTab => Some(Action::PrevTab),
            ShortcutAction::SelectTab(index) => Some(Action::SelectTab(index)),
            ShortcutAction::SplitVertical => Some(Action::SplitVertical),
            ShortcutAction::SplitHorizontal => Some(Action::SplitHorizontal),
            ShortcutAction::ToggleSidebar => Some(Action::ToggleSidebar),
            ShortcutAction::ToggleFollow => Some(Action::ToggleFollow),
            ShortcutAction::FocusLeft => Some(Action::Focus(Dir::Left)),
            ShortcutAction::FocusDown => Some(Action::Focus(Dir::Down)),
            ShortcutAction::FocusUp => Some(Action::Focus(Dir::Up)),
            ShortcutAction::FocusRight => Some(Action::Focus(Dir::Right)),
            ShortcutAction::ResizeUp => Some(Action::Resize(Dir::Up)),
            ShortcutAction::ResizeDown => Some(Action::Resize(Dir::Down)),
            ShortcutAction::ResizeLeft => Some(Action::Resize(Dir::Left)),
            ShortcutAction::ResizeRight => Some(Action::Resize(Dir::Right)),
            ShortcutAction::IncreaseFont => Some(Action::ChangeFont(1)),
            ShortcutAction::DecreaseFont => Some(Action::ChangeFont(-1)),
            ShortcutAction::Copy
            | ShortcutAction::Paste
            | ShortcutAction::ClearHistory
            | ShortcutAction::CopyMode
            | ShortcutAction::LinkHints => None,
        }
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::NewTab => {
                self.open_tab_in(None, None);
            }

            Action::OpenLauncher => {
                let mut launcher = Launcher::new(
                    self.display.clone(),
                    self.viewport,
                    self.dpi_transition.scale_factor(),
                    self.recent.entries(),
                );
                if self.font_diff != 0 {
                    launcher.change_font_size(self.font_diff);
                }
                self.launcher = Some(launcher);
                self.window.request_redraw();
            }

            Action::OpenTaskOverview => self.open_task_overview(),

            Action::CloseFocused => {
                let tab_empty = self.tabs[self.focus].root.close_focused();
                if tab_empty {
                    let removed_index = self.focus;
                    remove_tab(&mut self.tabs, &mut self.focus, removed_index);
                    if self.tabs.is_empty() {
                        self.exited = true;
                        return;
                    }
                    self.apply_focused_tab_workbench();
                } else {
                    // 分割の畳み込みだけなら表示状態は変わらない。
                    self.refresh_layout();
                }
                self.update_status_bar();
            }

            Action::NextTab | Action::PrevTab | Action::SelectTab(_) => {
                let target = match action {
                    Action::SelectTab(index) => index,
                    _ => adjacent_tab_index(
                        self.focus,
                        self.tabs.len(),
                        matches!(action, Action::NextTab),
                    ),
                };
                if target >= self.tabs.len() || target == self.focus {
                    return;
                }
                self.focused_root().focused_leaf_mut().focus_changed(false);
                self.focus = target;
                self.apply_focused_tab_workbench();
                self.update_status_bar();
            }

            Action::SplitVertical | Action::SplitHorizontal => {
                let partition = match action {
                    Action::SplitVertical => Partition::Vertical,
                    _ => Partition::Horizontal,
                };
                let window = self.window.clone();
                let display = self.display.clone();
                let scale_factor = self.dpi_transition.scale_factor();
                let font_diff = self.font_diff;
                self.tabs[self.focus].root.split_focused(
                    partition,
                    &window,
                    &display,
                    None,
                    scale_factor,
                    font_diff,
                );
            }

            Action::Focus(dir) => {
                self.move_focus_workbench(dir);
            }

            Action::Resize(dir) => {
                self.resize(dir);
            }

            Action::ToggleSidebar => {
                self.tabs[self.focus].workbench_visible = !self.tabs[self.focus].workbench_visible;
                self.apply_focused_tab_workbench();
            }

            Action::ToggleFollow => {
                self.sidebar.toggle_follow();
                self.window.request_redraw();
            }

            Action::ChangeFont(size_diff) => self.change_font(size_diff),
        }
    }

    /// フォントサイズを差分だけ変える。全タブ・全ペインの端末に加え、サイドバー・
    /// プレビュー（ビューア／エディタ）・ランチャーにも同じ差分を配って、画面全体の
    /// 文字を一度に揃える。ピクセル上のパネル配置は変わらない（各ビューが自分の枠内で
    /// セルサイズだけ変えて描き直す）ので、レイアウト再計算は要らない。
    fn change_font(&mut self, size_diff: i32) {
        // 既定サイズを基準に範囲内へ収める。下限では「−」を押しても
        // それ以上縮まないよう、実際に効かせる差分(applied)を計算しておく。
        let base = crate::TOYTERM_CONFIG.font_size as i32;
        let new_diff = clamp_font_diff(self.font_diff + size_diff, base);
        let applied = new_diff - self.font_diff;
        self.font_diff = new_diff;
        if applied == 0 {
            return;
        }

        for tab in &mut self.tabs {
            tab.root.for_each_leaf(&mut |w| w.change_font_size(applied));
        }
        self.sidebar.change_font_size(applied);
        self.preview_slot.change_font_size(applied);
        if let Some(launcher) = self.launcher.as_mut() {
            launcher.change_font_size(applied);
        }
        if let Some(review) = self.session_review.as_mut() {
            review.change_font_size(applied);
        }
        if let Some(overview) = self.task_overview.as_mut() {
            overview.change_font_size(applied);
        }
        self.window.request_redraw();
    }

    /// 新しく作った端末を、いまのフォントサイズ（既定＋差分）に合わせる。
    /// ズームしたあとに開くタブ・分割・エディタが独りだけ既定サイズに戻らないように。
    fn apply_current_font(&self, win: &mut TerminalWindow) {
        if self.font_diff != 0 {
            win.change_font_size(self.font_diff);
        }
    }

    fn apply_scale_factor(&mut self, scale_factor: f64) -> bool {
        let mut metrics_changed = false;
        for tab in &mut self.tabs {
            tab.root.for_each_leaf(&mut |window| {
                metrics_changed |= window.set_scale_factor(scale_factor);
            });
        }
        metrics_changed |= self.status_view.set_scale_factor(scale_factor);
        metrics_changed |= self.sidebar.set_scale_factor(scale_factor);
        metrics_changed |= self.preview_slot.set_scale_factor(scale_factor);
        if let Some(launcher) = self.launcher.as_mut() {
            launcher.set_scale_factor(scale_factor);
        }
        if let Some(review) = self.session_review.as_mut() {
            review.set_scale_factor(scale_factor);
        }
        if let Some(overview) = self.task_overview.as_mut() {
            overview.set_scale_factor(scale_factor);
        }
        metrics_changed
    }

    fn apply_dpi_update(&mut self, update: DpiUpdate) {
        if let Some(size) = update.surface_size {
            // glium 0.34 の手書きサーフェスは自動リサイズされないため明示。
            self.display.resize((size.width, size.height));
        }
        if let Some(size) = update.viewport_size {
            self.viewport = Viewport {
                x: 0,
                y: 0,
                w: size.width,
                h: size.height,
            };
        }
        if update.sync_layout_and_pty {
            self.refresh_layout();
            if let Some(launcher) = self.launcher.as_mut() {
                launcher.set_viewport(self.viewport);
            }
            if let Some(review) = self.session_review.as_mut() {
                review.set_viewport(self.viewport);
            }
            if let Some(overview) = self.task_overview.as_mut() {
                overview.set_viewport(self.viewport);
            }
            self.update_status_bar();
        }
    }

    fn open_tab_in(&mut self, cwd: Option<&Path>, command: Option<&[String]>) {
        self.focused_root().focused_leaf_mut().focus_changed(false);

        // タブが増えるとバーが出て内容領域が縮むので、push 後に再レイアウト。
        let cvp = self.content_viewport();
        let mut win = Box::new(TerminalWindow::with_viewport_command(
            self.window.clone(),
            self.display.clone(),
            cvp,
            self.dpi_transition.scale_factor(),
            cwd,
            command,
        ));
        self.apply_current_font(&mut win);
        self.tabs.push(Tab::new(Node::Leaf(win)));
        self.focus = self.tabs.len() - 1;
        self.apply_focused_tab_workbench();
        self.update_status_bar();
        if let Some(cwd) = cwd {
            self.recent.record(cwd);
        }
    }

    fn handle_session_review_outcome(&mut self, outcome: SessionReviewOutcome) {
        if let SessionReviewOutcome::Dismissed = outcome {
            self.session_review = None;
        }
        self.window.request_redraw();
    }

    /// Stop hook（AI 応答完了）を受けたときに呼ぶ。ランチャー表示中や、
    /// このセッションで何も変わっていないときは出さない（ノイズを増やさない）。
    fn open_session_review(&mut self) {
        if self.launcher.is_some() {
            return;
        }
        // ワークベンチ（サイドバー）を一度も開いていなくても Stop は来るので、
        // sidebar.root()（サイドバーの watcher/git 情報由来）ではなく、
        // フォーカス中ペインの実際の cwd を直接見る。
        let ShellLocation::Local(root) = self.focused_location() else {
            return;
        };
        let files = self.sidebar.session_file_summary();
        if files.changed_files == 0 {
            return;
        }
        let diff = crate::workspace::diff_stat(&root);
        let summary = SessionSummary { files, diff };
        // ランチャーと同じくウィンドウ全体を使う（画面全体を覆う一時的なポップアップ）。
        self.session_review = Some(SessionReview::new(
            self.display.clone(),
            self.viewport,
            self.dpi_transition.scale_factor(),
            summary,
        ));
        self.window.request_redraw();
    }

    /// 変換候補ウィンドウを、ランチャーの入力欄の位置へ動かす（over-the-spot）。
    fn sync_launcher_ime_area(&self) {
        let Some(launcher) = self.launcher.as_ref() else {
            return;
        };
        if let Some((pos, size)) = launcher.ime_cursor_area() {
            self.window.set_ime_cursor_area(pos, size);
        }
    }

    fn handle_launcher_outcome(&mut self, outcome: LauncherOutcome) {
        match outcome {
            LauncherOutcome::SaveWorkspace(name) => {
                let captured = self
                    .tabs
                    .iter_mut()
                    .map(|tab| capture_workspace_node(&mut tab.root))
                    .collect::<Result<Vec<_>, _>>();
                let result = captured.and_then(|tabs| {
                    crate::workspace_sets::save(crate::workspace_sets::WorkspaceSet { name, tabs })
                });
                if let Some(launcher) = &mut self.launcher {
                    launcher.workspace_notice(match result {
                        Ok(()) => "配置を保存しました".into(),
                        Err(error) => error,
                    });
                }
                self.window.request_redraw();
            }
            LauncherOutcome::OpenWorkspace(name) => {
                let result = crate::workspace_sets::load_all().and_then(|sets| {
                    sets.into_iter()
                        .find(|set| set.name == name)
                        .ok_or_else(|| "作業セットが見つかりません".into())
                });
                let notice = match result {
                    Ok(set) => {
                        let mut missing = Vec::new();
                        let mut roots = Vec::new();
                        for node in &set.tabs {
                            if let Some(root) = self.restore_workspace_node(node, &mut missing) {
                                roots.push(root);
                            }
                        }
                        let count = roots.len();
                        if count > 0 {
                            self.focused_root().focused_leaf_mut().focus_changed(false);
                            self.tabs.extend(roots.into_iter().map(Tab::new));
                            self.focus = self.tabs.len() - count;
                            self.startup_launcher = false;
                            self.apply_focused_tab_workbench();
                            self.update_status_bar();
                        }
                        if missing.is_empty() {
                            format!("{count}タブを追加しました。Escで戻れます")
                        } else {
                            format!(
                                "{count}タブを追加。見つからないフォルダ: {}",
                                missing.join(", ")
                            )
                        }
                    }
                    Err(error) => error,
                };
                if let Some(launcher) = &mut self.launcher {
                    launcher.workspace_notice(notice);
                }
                self.window.request_redraw();
            }
            LauncherOutcome::OpenIn { dir, command } => {
                // エージェントは「抜けたらそのフォルダのシェルへ戻る」形で起動する
                // （直接 exec だと exit でタブごと閉じてしまう）。シェル選択は None のまま。
                let launch = wrap_agent_command(command);
                self.open_tab_from_launcher(&dir, launch.as_deref());
            }
            LauncherOutcome::OpenFile { file, dir } => {
                let env_editor = std::env::var("EDITOR").ok();
                match resolve_file_open(&crate::TOYTERM_CONFIG.editor, env_editor.as_deref()) {
                    // Windows の既定 notepad は GUI アプリ。タブに入れると空の cmd タブが
                    // 残るので、OS の既定アプリで開く（タブは作らない・ランチャーは畳む）。
                    OpenMethod::SystemDefault => {
                        self.startup_launcher = false;
                        self.launcher = None;
                        crate::window::open_url(&file.to_string_lossy());
                        self.window.request_redraw();
                    }
                    OpenMethod::InTerminal(mut editor) => {
                        if command_exists(&editor[0]) {
                            editor.push(file.to_string_lossy().into_owned());
                            // エディタを抜けたらそのフォルダのシェルに落ちる（エージェントと同じ流儀）。
                            let launch = wrap_agent_command(Some(editor));
                            self.open_tab_from_launcher(&dir, launch.as_deref());
                        } else {
                            // 設定されたエディタが見つからなければ OS の既定アプリに任せる。
                            self.startup_launcher = false;
                            self.launcher = None;
                            crate::window::open_url(&file.to_string_lossy());
                            self.window.request_redraw();
                        }
                    }
                }
            }
            LauncherOutcome::OpenExternal { file } => {
                // ランチャーは開いたまま（画像を続けて見る等）。
                crate::window::open_url(&file.to_string_lossy());
                self.window.request_redraw();
            }
            LauncherOutcome::Cancelled => {
                self.startup_launcher = false;
                self.launcher = None;
                self.window.request_redraw();
            }
            LauncherOutcome::None => {
                self.window.request_redraw();
            }
        }
    }

    /// ランチャーを畳んで、選んだ場所で新しいタブを開く共通処理。
    fn open_tab_from_launcher(&mut self, dir: &Path, command: Option<&[String]>) {
        let replace_startup = self.startup_launcher;
        self.startup_launcher = false;
        self.launcher = None;
        self.open_tab_in(Some(dir), command);
        // 起動時ランチャーで選んだ場合、自動で立った最初の空シェルタブを畳む。
        if replace_startup && self.tabs.len() > 1 {
            self.tabs[0].root.close_focused(); // 最初のタブは単一ペイン＝PTY を閉じる
            remove_tab(&mut self.tabs, &mut self.focus, 0);
            self.apply_focused_tab_workbench();
            self.update_status_bar();
        }
        self.window.request_redraw();
    }

    fn restore_workspace_node(
        &mut self,
        saved: &crate::workspace_sets::SavedNode,
        missing: &mut Vec<String>,
    ) -> Option<Node> {
        use crate::workspace_sets::SavedNode;
        match saved {
            SavedNode::Pane { cwd } => {
                if !cwd.is_dir() {
                    missing.push(cwd.display().to_string());
                    return None;
                }
                let mut pane = Box::new(TerminalWindow::with_viewport_command(
                    self.window.clone(),
                    self.display.clone(),
                    self.content_viewport(),
                    self.dpi_transition.scale_factor(),
                    Some(cwd),
                    None,
                ));
                self.apply_current_font(&mut pane);
                pane.focus_changed(false);
                Some(Node::Leaf(pane))
            }
            SavedNode::Split {
                vertical,
                ratio,
                first,
                second,
            } => {
                let first = self.restore_workspace_node(first, missing);
                let second = self.restore_workspace_node(second, missing);
                match (first, second) {
                    (Some(first), Some(second)) => Some(Node::Split(SplitNode {
                        partition: if *vertical {
                            Partition::Vertical
                        } else {
                            Partition::Horizontal
                        },
                        ratio: ratio.clamp(0.1, 0.9),
                        focus_first: true,
                        first: Box::new(first),
                        second: Box::new(second),
                    })),
                    (first, second) => first.or(second),
                }
            }
        }
    }

    fn focus_sidebar(&mut self) {
        if !self.sidebar.is_visible() || self.sidebar_focused {
            return;
        }
        if self.editor_focused {
            if let Some(editor) = self.preview_slot.editor_mut() {
                editor.focus_changed(false);
            }
            self.editor_focused = false;
        } else if self.reader_focused {
            self.unfocus_reader();
        } else {
            self.focused_root().focused_leaf_mut().focus_changed(false);
        }
        self.sidebar_focused = true;
        self.sidebar.set_focused(true);
    }

    fn release_sidebar_focus(&mut self) {
        if !self.sidebar_focused {
            self.sidebar.set_focused(false);
            return;
        }
        self.sidebar_focused = false;
        self.sidebar.set_focused(false);
        self.focused_root().focused_leaf_mut().focus_changed(true);
    }

    fn focus_editor(&mut self) {
        if !self.sidebar.is_visible() || self.editor_focused {
            return;
        }
        if self.sidebar_focused {
            self.sidebar_focused = false;
            self.sidebar.set_focused(false);
        } else if self.reader_focused {
            self.unfocus_reader();
        } else {
            self.focused_root().focused_leaf_mut().focus_changed(false);
        }
        if let Some(editor) = self.preview_slot.editor_mut() {
            editor.focus_changed(true);
            self.editor_focused = true;
        }
    }

    fn release_editor_focus(&mut self) {
        if !self.editor_focused {
            return;
        }
        if let Some(editor) = self.preview_slot.editor_mut() {
            editor.focus_changed(false);
        }
        self.editor_focused = false;
        self.focused_root().focused_leaf_mut().focus_changed(true);
    }

    /// ビューア（右上）へフォーカスを移す。エディタを開いている枠は対象外。
    fn focus_reader(&mut self) {
        if !self.sidebar.is_visible()
            || self.reader_focused
            || self.preview_slot.visible_reader_mut().is_none()
        {
            return;
        }
        if self.sidebar_focused {
            self.sidebar_focused = false;
            self.sidebar.set_focused(false);
        } else {
            self.focused_root().focused_leaf_mut().focus_changed(false);
        }
        self.reader_focused = true;
        if let Some(reader) = self.preview_slot.visible_reader_mut() {
            reader.set_focused(true);
        }
    }

    /// ビューアのフォーカス表示だけ落とす（次のフォーカス先は呼び出し側が決める）。
    fn unfocus_reader(&mut self) {
        self.reader_focused = false;
        if let Some(reader) = self.preview_slot.reader_mut() {
            reader.set_focused(false);
        }
    }

    fn release_reader_focus(&mut self) {
        if !self.reader_focused {
            return;
        }
        self.unfocus_reader();
        self.focused_root().focused_leaf_mut().focus_changed(true);
    }

    fn handle_reader_key_result(&mut self, result: ReaderKeyResult) {
        match result {
            ReaderKeyResult::Consumed => {}
            ReaderKeyResult::ReleaseFocus => self.release_reader_focus(),
            ReaderKeyResult::Request(request) => self.handle_reader_request(request),
        }
    }

    fn handle_sidebar_key_result(&mut self, result: SidebarKeyResult) {
        match result {
            SidebarKeyResult::Consumed => {}
            SidebarKeyResult::ReleaseFocus => self.release_sidebar_focus(),
            SidebarKeyResult::Request(request) => {
                self.handle_sidebar_request(request);
                if self.sidebar_focused {
                    self.focused_root().focused_leaf_mut().focus_changed(false);
                }
            }
        }
    }

    fn handle_clicked_file(&mut self) {
        let Some(path) = self.focused_root().take_clicked_file() else {
            return;
        };

        if !self.sidebar.is_visible() {
            self.tabs[self.focus].workbench_visible = true;
            self.apply_focused_tab_workbench();
        }
        let root = self.sidebar.root().map(Path::to_path_buf);
        if let Some(reader) = self.preview_slot.reader_mut() {
            reader.preview_pinned(&path, root.as_deref());
        }
        self.refresh_layout();
    }

    fn handle_sidebar_request(&mut self, request: SidebarRequest) {
        match request {
            SidebarRequest::PreviewFile(path) => {
                let root = self.sidebar.root().map(Path::to_path_buf);
                if let Some(reader) = self.preview_slot.reader_mut() {
                    reader.preview_pinned(&path, root.as_deref());
                }
            }
            SidebarRequest::ScrollPreview(delta) => {
                if let Some(reader) = self.preview_slot.reader_mut() {
                    reader.scroll_by(delta);
                }
            }
            SidebarRequest::EditPreview => {
                if let Some(request) = self
                    .preview_slot
                    .reader_mut()
                    .and_then(|reader| reader.on_header_key(ReaderHeaderAction::Edit))
                {
                    self.handle_reader_request(request);
                }
            }
            SidebarRequest::OpenPreview => {
                if let Some(request) = self
                    .preview_slot
                    .reader_mut()
                    .and_then(|reader| reader.on_header_key(ReaderHeaderAction::OpenWithSystem))
                {
                    self.handle_reader_request(request);
                }
            }
        }
    }

    fn handle_reader_request(&mut self, request: ReaderRequest) {
        match request {
            ReaderRequest::EditFile(path) => self.open_editor_in_preview(path),
            ReaderRequest::OpenWithSystem(path) => crate::window::open_url(&path.to_string_lossy()),
        }
    }

    fn open_editor_in_preview(&mut self, path: std::path::PathBuf) {
        let env_editor = std::env::var("EDITOR").ok();
        let mut command = resolve_editor(&crate::TOYTERM_CONFIG.editor, env_editor.as_deref());
        if !command_exists(&command[0]) {
            if let Some(reader) = self.preview_slot.reader_mut() {
                reader.show_missing_editor(&command[0]);
            }
            return;
        }
        command.push(path.to_string_lossy().into_owned());

        let viewport = match &self.preview_slot {
            PreviewSlot::Reader(reader) => reader.viewport(),
            PreviewSlot::Editor { win, .. } => win.viewport(),
            PreviewSlot::Empty => self.content_viewport(),
        };
        let cwd = path.parent().map(Path::to_path_buf);
        // 枠がエディタに変わるので、ビューアのフォーカス表示は畳んでおく
        // （エディタを閉じて戻したときにヒント行が残らないように）。
        self.unfocus_reader();
        let saved = match std::mem::replace(&mut self.preview_slot, PreviewSlot::Empty) {
            PreviewSlot::Reader(reader) => Box::new(reader),
            PreviewSlot::Editor { saved, .. } => saved,
            PreviewSlot::Empty => return,
        };
        let mut win = Box::new(TerminalWindow::with_viewport_command(
            self.window.clone(),
            self.display.clone(),
            viewport,
            self.dpi_transition.scale_factor(),
            cwd.as_deref(),
            Some(&command),
        ));
        self.apply_current_font(&mut win);
        win.focus_changed(true);
        self.focused_root().focused_leaf_mut().focus_changed(false);
        self.sidebar_focused = false;
        self.sidebar.set_focused(false);
        self.editor_focused = true;
        self.preview_slot = PreviewSlot::Editor { win, saved };
    }

    fn handle_gt_messages(&mut self) {
        // 以前はワークベンチ非表示中は汲まずに捨てていたが、State（Blocked/Done/
        // SessionStart/SessionEnd）はワークベンチを閉じていても・ウィンドウが
        // 非フォーカスでも拾えないと「離席中に完了通知」という主目的が果たせない。
        // 中身は Mutex::lock+mem::take（毎フレーム無条件で呼ぶ take_dirty と同等の
        // 軽さ）なので、常時ドレインしても分割数が多少あっても問題にならない。
        let mut messages = Vec::new();
        for tab in &mut self.tabs {
            tab.root.take_gt_messages(&mut messages);
        }
        self.preview_slot.drain_gt_messages();

        let focused = self.tabs[self.focus].root.focused_leaf().pane_id();
        if focused != self.observed_pane {
            self.observed_pane = focused;
            self.gt_file_assembler = GtFileAssembler::default();
            self.sidebar.reset_pane_observations();
            if let Some(reader) = self.preview_slot.reader_mut() {
                reader.clear_remote_content();
            }
        }

        if messages.is_empty() {
            return;
        }

        for (source, message) in messages {
            let signal = match &message {
                GtMessage::State { signal, .. } => Some(*signal),
                GtMessage::Event { .. } | GtMessage::FileChunk { .. } => None,
            };
            let modal_open = self.launcher.is_some()
                || self.session_review.is_some()
                || self.task_overview.is_some();
            let route = route_gt(source, focused, signal, self.window_focused, modal_open);
            if route.notify {
                crate::window::notify_completion();
            }
            if !route.apply_panel {
                continue;
            }
            match message {
                GtMessage::Event { kind, path, tool } => {
                    let root = self
                        .tabs
                        .iter()
                        .find_map(|tab| tab.root.pane(source))
                        .and_then(TerminalWindow::observed_location)
                        .and_then(|location| match location {
                            ShellLocation::Local(path) => Some(path),
                            ShellLocation::Remote { .. } => None,
                        });
                    self.sidebar
                        .apply_gt_event(root.as_deref(), kind, path, tool);
                }
                GtMessage::FileChunk {
                    path,
                    seq,
                    last,
                    data,
                } => {
                    if let Some((path, bytes)) = self.gt_file_assembler.push(path, seq, last, data)
                    {
                        if let Some(reader) = self.preview_slot.reader_mut() {
                            reader.show_remote_content(path, bytes);
                        }
                    }
                }
                GtMessage::State {
                    agent,
                    signal,
                    detail,
                } => {
                    self.sidebar.apply_gt_state(agent, signal, detail);
                    if route.review {
                        self.open_session_review();
                    }
                }
            }
        }
    }

    /// 今このフレームを描画してよいか。最小化中（またはサイズ0）は描かない。
    /// Windows では最小化中も毎フレーム描画→SwapBuffers を呼んでしまうと、
    /// 隠れたウィンドウには画面合成(vblank)が来ないため、投げた描画コマンドが
    /// ドライバ側に処理されず溜まり続け、メモリが膨張して最後は OOM で落ちる。
    /// タスクバーアイコンの連打（＝最小化⇔復元の高速な繰り返し）で顕著に出る。
    /// Occluded イベントは Windows では当てにならないので is_minimized で判定する。
    fn drawable(&self) -> bool {
        if self.viewport.w == 0 || self.viewport.h == 0 {
            return false;
        }
        !matches!(self.window.is_minimized(), Some(true))
    }

    pub fn on_event(&mut self, event: &Event, elwt: &EventLoopWindowTarget<()>) {
        if self.exited {
            elwt.exit();
            return;
        }

        match event {
            Event::WindowEvent { event: wev, .. } => match wev {
                WindowEvent::CloseRequested => {
                    elwt.exit();
                }

                &WindowEvent::Resized(new_size) => {
                    // ScaleFactorChanged のコールバック中は Window::inner_size() がまだ
                    // 旧値なので、確定した物理サイズはこのイベントで一度だけ適用する。
                    let update = self.dpi_transition.on_resized(new_size);
                    self.apply_dpi_update(update);
                    // リサイズが来た＝ウィンドウは見えている。モニター切替時に
                    // Occluded(true) を受けたまま解除イベントを取りこぼすと画面が
                    // 固まるため、ここで遮蔽フラグを下ろして即再描画する。
                    self.occluded = false;
                    self.window.request_redraw();
                }

                WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                    // winit 0.29 はこのコールバックの後で OS 提案サイズを適用する。
                    // ここではフォントだけ準備し、サーフェス・PTY は Resized（または
                    // 同じ物理サイズで Resized が来ない場合の AboutToWait）へ遅延する。
                    if let Some(scale_factor) =
                        self.dpi_transition.begin_scale_factor_change(*scale_factor)
                    {
                        let metrics_changed = self.apply_scale_factor(scale_factor);
                        self.dpi_transition.mark_metrics_changed(metrics_changed);
                    }
                    self.occluded = false;
                }

                &WindowEvent::Focused(focused) => {
                    // ワークスペース切替などで Occluded(false) を取りこぼしても、
                    // フォーカスが戻った＝必ず可視なので、遮蔽フラグを下ろして
                    // 描き直す。これが無いと別ワークスペースから戻ったとき画面が
                    // 固まったまま（待機）になる。
                    self.window_focused = focused;
                    if focused {
                        self.occluded = false;
                        self.window.request_redraw();
                    }
                    if self.task_overview.is_none() {
                        self.focused_root()
                            .focused_leaf_mut()
                            .process_window_event(wev);
                    } else {
                        self.window.set_ime_allowed(false);
                    }
                }

                &WindowEvent::Occluded(occluded) => {
                    // 遮蔽中の描画スキップは Wayland 限定の対策（frame callback 枯渇で
                    // swap がブロックし無応答になる問題）。Windows ではこの問題が無く、
                    // 遮蔽イベントの誤検出で描画が止まる恐れがあるので無視する。
                    #[cfg(not(windows))]
                    {
                        self.occluded = occluded;
                        // 再表示されたら最新内容を描き直す。
                        if !occluded {
                            self.window.request_redraw();
                        }
                    }
                    #[cfg(windows)]
                    let _ = occluded;
                }

                WindowEvent::RedrawRequested => {
                    // 隠れている間は描画しない。Wayland では frame callback が
                    // 止まり、ここでの swap がブロックして無応答になるため。
                    // Windows では最小化中の描画コマンドがドライバに溜まって
                    // メモリ膨張→OOM になるため drawable() でも弾く。
                    if self.occluded || !self.drawable() {
                        return;
                    }
                    let mut surface = self.display.draw();

                    // まずフレーム全体を不透明な黒でクリアする。分割の隙間（GAP）が
                    // そのまま黒い区切り線になる（透過パネル同士を分ける）。各ペインは
                    // 自分の矩形を上書きするので、ペイン内の透過には影響しない。
                    {
                        use glium::Surface as _;
                        surface.clear_color_srgb(0.0, 0.0, 0.0, 1.0);
                    }

                    if self.status_bar_height() > 0 {
                        self.status_view.draw(&mut surface);
                    }
                    self.tabs[self.focus].root.draw(&mut surface);
                    if self.sidebar.is_visible() {
                        self.preview_slot.draw(&mut surface);
                    }
                    self.sidebar.draw(&mut surface);
                    if let Some(launcher) = self.launcher.as_mut() {
                        launcher.draw(&mut surface);
                    }
                    if let Some(review) = self.session_review.as_mut() {
                        review.draw(&mut surface);
                    }
                    if let Some(overview) = self.task_overview.as_mut() {
                        overview.draw(&mut surface);
                    }

                    surface.finish().expect("finish");
                }

                WindowEvent::ModifiersChanged(m) => {
                    self.modifiers = m.state();
                    // 各ペインの内部修飾キー状態も合わせておく（フォーカス切替後も正しく）。
                    for tab in &mut self.tabs {
                        tab.root.for_each_leaf(&mut |w| w.process_window_event(wev));
                    }
                    // 非表示タブの古い座標でカーソルを上書きしない。
                    self.tabs[self.focus]
                        .root
                        .for_each_leaf(&mut |w| w.update_link_cursor());
                }

                WindowEvent::KeyboardInput {
                    event: key,
                    is_synthetic,
                    ..
                } => {
                    // winit はウィンドウがフォーカスを得た瞬間、その時点で押されて
                    // いるキーを「合成の押下イベント」として発行する。Win+3 でこの窓へ
                    // 切替えると合成 Pressed「3」が、Ctrl+Tab で切替えると合成 Pressed
                    // 「Tab」が届き、入力として打ち込まれてしまう。合成イベントは実入力
                    // でもショートカットでもないので無視する。
                    if *is_synthetic {
                        return;
                    }
                    if key.state == ElementState::Released {
                        if let PhysicalKey::Code(code) = key.physical_key {
                            if self.overview_release_key == Some(code) {
                                self.overview_release_key = None;
                                return;
                            }
                        }
                    }
                    // 一覧を閉じた直後の同一キーの自動リピートを、キー解放まで消費する。
                    // 起動キーを押し続けても一覧の開閉が繰り返されず、Enter/Esc の
                    // リピートも背後の端末へ流れない。
                    if overview_key_is_consumed(
                        self.overview_release_key,
                        key.state,
                        key.physical_key,
                    ) {
                        return;
                    }
                    // 入力中はカーソルを点いたままにする（押した瞬間に点滅で
                    // 消えていると打ちにくい）。点滅の起点をリセットする。
                    if key.state == ElementState::Pressed {
                        self.blink_start = Instant::now();
                        if !self.cursor_blink_on {
                            self.cursor_blink_on = true;
                            self.focused_root()
                                .focused_leaf_mut()
                                .set_cursor_blink(true);
                        }
                    }
                    // フォント変更はどのモードでも横取りして全体へ効かせる（ランチャー
                    // 表示中でも、フォーカスがどの領域にあっても同じ大きさで揃える）。
                    if let Some(action @ Action::ChangeFont(_)) = self.parse_shortcut(key) {
                        self.handle_action(action);
                        return;
                    }
                    if self.task_overview.is_some() {
                        let code = match key.physical_key {
                            PhysicalKey::Code(code) => code,
                            PhysicalKey::Unidentified(_) => return,
                        };
                        if matches!(self.parse_shortcut(key), Some(Action::OpenTaskOverview)) {
                            self.overview_release_key = Some(code);
                            self.dismiss_task_overview();
                            return;
                        }
                        let outcome = self
                            .task_overview
                            .as_mut()
                            .map(|overview| overview.handle_key(key))
                            .unwrap_or(OverviewOutcome::None);
                        self.handle_overview_outcome(outcome, code);
                        return;
                    }
                    if let Some(review) = self.session_review.as_mut() {
                        let outcome = review.handle_key(key);
                        self.handle_session_review_outcome(outcome);
                        return;
                    }
                    if let Some(launcher) = self.launcher.as_mut() {
                        let outcome = launcher.handle_key(key, self.modifiers);
                        self.handle_launcher_outcome(outcome);
                        // 入力欄に入った直後に変換候補の位置を合わせる
                        // （起動メニューを開いた時点で候補が正しい場所に出る）。
                        self.sync_launcher_ime_area();
                        return;
                    }
                    if !self.sidebar_focused
                        && !self.reader_focused
                        && !self.editor_focused
                        && self.focused_root().focused_leaf().local_input_mode()
                    {
                        self.focused_root()
                            .focused_leaf_mut()
                            .process_window_event(wev);
                        self.handle_clicked_file();
                        return;
                    }
                    if let Some(action) = self.parse_shortcut(key) {
                        if let (Action::OpenTaskOverview, PhysicalKey::Code(code)) =
                            (action, key.physical_key)
                        {
                            self.overview_release_key = Some(code);
                            self.handle_action(Action::OpenTaskOverview);
                            return;
                        }
                        self.handle_action(action);
                        if self.sidebar_focused && !matches!(action, Action::ToggleSidebar) {
                            self.focused_root().focused_leaf_mut().focus_changed(false);
                        }
                    } else if self.sidebar_focused {
                        let result = self.sidebar.on_key(key);
                        self.handle_sidebar_key_result(result);
                    } else if self.reader_focused {
                        if let Some(reader) = self.preview_slot.visible_reader_mut() {
                            let result = reader.on_key(key);
                            self.handle_reader_key_result(result);
                        }
                    } else if self.editor_focused {
                        if let Some(editor) = self.preview_slot.editor_mut() {
                            editor.process_window_event(wev);
                        }
                    } else {
                        self.focused_root()
                            .focused_leaf_mut()
                            .process_window_event(wev);
                    }
                }

                WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                    if self.task_overview.is_some() => {}

                WindowEvent::CursorMoved { position, .. } => {
                    self.cursor_pos = *position;
                    // どのペインでドラッグ選択しても効くよう、全ペインへ座標を配る。
                    let focus_tab = self.focus;
                    self.tabs[focus_tab]
                        .root
                        .for_each_leaf(&mut |w| w.process_window_event(wev));
                }

                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                } => {
                    if self.sidebar.contains(self.cursor_pos) {
                        self.focus_sidebar();
                        if let Some(request) = self.sidebar.on_click(self.cursor_pos) {
                            self.handle_sidebar_request(request);
                        }
                        return;
                    }
                    if self.sidebar.is_visible() && self.preview_slot.contains(self.cursor_pos) {
                        if self.preview_slot.editor_mut().is_some() {
                            self.focus_editor();
                            if let Some(editor) = self.preview_slot.editor_mut() {
                                editor.process_window_event(wev);
                            }
                        } else if self.preview_slot.visible_reader_mut().is_some() {
                            self.focus_reader();
                            let clicked = self
                                .preview_slot
                                .visible_reader_mut()
                                .and_then(|reader| reader.on_click(self.cursor_pos));
                            if let Some(request) = clicked {
                                self.handle_reader_request(request);
                            }
                        }
                        return;
                    }
                    // クリックしたペインへフォーカスを移してから入力を渡す。
                    let p = self.cursor_pos;
                    if self.sidebar_focused {
                        self.release_sidebar_focus();
                    } else if self.editor_focused {
                        self.release_editor_focus();
                    } else if self.reader_focused {
                        self.unfocus_reader();
                    } else {
                        self.focused_root().focused_leaf_mut().focus_changed(false);
                    }
                    self.tabs[self.focus].root.focus_at(p);
                    self.focused_root().focused_leaf_mut().focus_changed(true);
                    self.focused_root()
                        .focused_leaf_mut()
                        .process_window_event(wev);
                    self.handle_clicked_file();
                }

                WindowEvent::MouseWheel { delta, .. } => {
                    if self.sidebar.contains(self.cursor_pos) {
                        let rows = match delta {
                            MouseScrollDelta::LineDelta(_, y) => (*y * 1.5).trunc() as i32,
                            MouseScrollDelta::PixelDelta(pos) => {
                                let cell_h = self.sidebar.cell_height();
                                (pos.y / cell_h.max(1) as f64).trunc() as i32
                            }
                        };
                        self.sidebar.on_scroll(rows);
                    } else if self.sidebar.is_visible()
                        && self.preview_slot.contains(self.cursor_pos)
                    {
                        if let Some(reader) = self.preview_slot.visible_reader_mut() {
                            let rows = match delta {
                                MouseScrollDelta::LineDelta(_, y) => (*y * 1.5).trunc() as i32,
                                MouseScrollDelta::PixelDelta(pos) => {
                                    let cell_h = reader.cell_height();
                                    (pos.y / cell_h.max(1) as f64).trunc() as i32
                                }
                            };
                            reader.on_scroll(rows);
                        } else if let Some(editor) = self.preview_slot.editor_mut() {
                            editor.process_window_event(wev);
                        }
                    } else {
                        self.focused_root()
                            .focused_leaf_mut()
                            .process_window_event(wev);
                    }
                }

                WindowEvent::MouseInput { .. }
                    if self.sidebar.contains(self.cursor_pos)
                        || (self.sidebar.is_visible()
                            && self.preview_slot.contains(self.cursor_pos)) => {}

                // ランチャー表示中の日本語入力はランチャーの入力欄へ。
                // ここで受けないと、確定した文字が裏のシェルに打ち込まれる。
                WindowEvent::Ime(_)
                    if self.task_overview.is_some() || self.overview_release_key.is_some() => {}

                WindowEvent::Ime(ime) if self.launcher.is_some() => {
                    if let Some(launcher) = self.launcher.as_mut() {
                        launcher.handle_ime(ime);
                    }
                    self.sync_launcher_ime_area();
                }

                WindowEvent::Ime(_) if self.sidebar_focused || self.reader_focused => {}

                WindowEvent::Ime(_) if self.editor_focused => {
                    if let Some(editor) = self.preview_slot.editor_mut() {
                        editor.process_window_event(wev);
                    }
                }

                // フォーカス・IME・マウス離し等はフォーカス中ペインへ。
                _ => {
                    if self.editor_focused {
                        if let Some(editor) = self.preview_slot.editor_mut() {
                            editor.process_window_event(wev);
                        }
                    } else {
                        self.focused_root()
                            .focused_leaf_mut()
                            .process_window_event(wev);
                        self.handle_clicked_file();
                    }
                }
            },

            Event::AboutToWait => {
                // DPI変更後に物理サイズが変わらず Resized が来ない場合も、新しい
                // セル寸法をPTYとIMEへこのイベントバッチ中に一度だけ同期する。
                let dpi_update = self.dpi_transition.flush_pending();
                self.apply_dpi_update(dpi_update);

                // 全タブの PTY を汲み取り、終了したペイン/タブを取り除く。
                let mut changed = false;
                let mut tab_removed = false;
                let mut i = 0;
                while i < self.tabs.len() {
                    let mut collapsed = false;
                    let empty = self.tabs[i].root.update_and_prune(&mut collapsed);
                    if collapsed {
                        changed = true;
                    }
                    if empty {
                        remove_tab(&mut self.tabs, &mut self.focus, i);
                        changed = true;
                        tab_removed = true;
                        if self.tabs.is_empty() {
                            // exited を立てないと、終了確定前に届く次のイベント
                            // （AboutToWait やフォーカス/クリック）が空の tabs を
                            // 触って panic する（index out of bounds: len 0）。
                            // Ctrl+Shift+W の経路と同じくフラグを立てる。
                            self.exited = true;
                            elwt.exit();
                            return;
                        }
                        // remove(i) で詰めたので i はそのまま次のタブを指す。
                    } else {
                        i += 1;
                    }
                }
                // ペインやタブが減ったら、残ったペインを領域いっぱいに広げ直す。
                if tab_removed {
                    self.apply_focused_tab_workbench();
                    self.update_status_bar();
                } else if changed {
                    self.refresh_layout();
                    self.update_status_bar();
                }
                if (tab_removed || changed) && !terminal_focus_allowed(self.task_overview.is_some())
                {
                    self.suspend_focus_for_overview();
                }

                self.handle_gt_messages();
                self.update_status_bar();

                if self.task_overview.is_some()
                    && self.overview_last_refresh.elapsed() >= Duration::from_secs(1)
                {
                    let now = Instant::now();
                    let rows = self.collect_task_rows();
                    if let Some(overview) = self.task_overview.as_mut() {
                        overview.update_rows(rows, now);
                    }
                    self.overview_last_refresh = now;
                }

                if self.sidebar.is_visible() && self.preview_slot.check_update() {
                    if let PreviewSlot::Editor { mut saved, .. } =
                        std::mem::replace(&mut self.preview_slot, PreviewSlot::Empty)
                    {
                        saved.refresh_current();
                        self.preview_slot = PreviewSlot::Reader(*saved);
                        self.editor_focused = false;
                        if terminal_focus_allowed(self.task_overview.is_some()) {
                            self.focused_root().focused_leaf_mut().focus_changed(true);
                        }
                        self.refresh_layout();
                    }
                }

                // カーソル点滅：530ms ごとに表示/非表示を切り替える。フェーズが
                // 変わったフレームだけ set_cursor_blink で再描画を促す（毎フレーム
                // 描かないので CPU を無駄に回さない）。
                if crate::TOYTERM_CONFIG.cursor_blink {
                    const BLINK_MS: u128 = 530;
                    let on = (self.blink_start.elapsed().as_millis() / BLINK_MS) % 2 == 0;
                    if on != self.cursor_blink_on {
                        self.cursor_blink_on = on;
                        self.focused_root().focused_leaf_mut().set_cursor_blink(on);
                    }
                }

                // フォーカス中ペインの cd に追従する（非表示中は /proc も読まない）。
                if self.sidebar.is_visible() {
                    let location = self.focused_location();
                    self.sidebar.refresh_if_stale(&location);
                    let root = self.sidebar.root().map(Path::to_path_buf);
                    if let Some(path) = self.sidebar.take_follow_target() {
                        if let Some(reader) = self.preview_slot.reader_mut() {
                            if reader.is_following() {
                                reader.follow_target(path, root.as_deref());
                            } else if reader.target_abs() == Some(path.as_path()) {
                                reader.refresh_current();
                            }
                        }
                    }
                    if let Some(reader) = self.preview_slot.reader_mut() {
                        reader.poll();
                    }
                }

                // 隠れている間は再描画を要求しない（swap ブロック＝無応答を防ぐ）。
                // 内容更新自体は上で汲み取り済みなので、再表示時にまとめて描ける。
                let need = self.tabs[self.focus].root.needs_redraw()
                    || (self.status_bar_height() > 0 && self.status_view.needs_redraw())
                    || self.sidebar.needs_redraw()
                    || (self.sidebar.is_visible() && self.preview_slot.needs_redraw())
                    || self
                        .launcher
                        .as_ref()
                        .is_some_and(|launcher| launcher.needs_redraw())
                    || self
                        .session_review
                        .as_ref()
                        .is_some_and(|review| review.needs_redraw())
                    || self
                        .task_overview
                        .as_ref()
                        .is_some_and(|overview| overview.needs_redraw());
                if need && !self.occluded && self.drawable() {
                    self.window.request_redraw();
                }

                // 約16ms(=60fps)ごとにポーリングして PTY 出力を拾う。
                elwt.set_control_flow(ControlFlow::WaitUntil(
                    Instant::now() + Duration::from_millis(16),
                ));
            }

            _ => {}
        }
    }
}
