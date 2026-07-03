//! タブと画面分割を司るマネージャ。
//!
//! 1つの OS ウィンドウの中に複数の端末セッション（[`TerminalWindow`]）を
//! 抱え、タブ（[`Vec<Node>`]）と二分木の分割（[`Node::Split`]）として配置する。
//! ウィンドウ全体のイベント（リサイズ・再描画・終了）はここで握り、
//! キー/マウス/IME は現在フォーカスしているペインへ振り分ける。

use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use winit::{
    dpi::PhysicalPosition,
    event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent},
    event_loop::{ControlFlow, EventLoopWindowTarget},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::Window,
};

use crate::config::resolve_editor;
use crate::gt::{GtFileAssembler, GtMessage};
use crate::reader::{ReaderHeaderAction, ReaderPane, ReaderRequest};
use crate::sidebar::{Sidebar, SidebarKeyResult, SidebarRequest};
use crate::terminal::{Cell, Color, Line};
use crate::view::{TerminalView, Viewport};
use crate::vt::ShellLocation;
use crate::window::TerminalWindow;
use crate::Display;

type Event = winit::event::Event<()>;

/// 分割の境界に空ける隙間（px）。
const GAP: u32 = 2;

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
    CloseFocused,
    NextTab,
    PrevTab,
    SplitVertical,
    SplitHorizontal,
    ToggleSidebar,
    Focus(Dir),
    Resize(Dir),
}

/// タブ内のペイン木。葉が端末、節が分割。
enum Node {
    Leaf(Box<TerminalWindow>),
    Split(SplitNode),
    /// `mem::replace` の一時退避にだけ使う番兵。通常は出現しない。
    Empty,
}

struct SplitNode {
    partition: Partition,
    ratio: f64,
    /// フォーカスが first 側にあるか。
    focus_first: bool,
    first: Box<Node>,
    second: Box<Node>,
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

fn command_exists(command: &str) -> bool {
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

impl Node {
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

    fn take_gt_messages(&mut self, out: &mut Vec<GtMessage>) {
        self.for_each_leaf(&mut |w| out.extend(w.take_gt_messages()));
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
                let new_win = Box::new(TerminalWindow::with_viewport_command(
                    window.clone(),
                    display.clone(),
                    vp,
                    cwd.as_deref(),
                    command,
                ));

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
                    s.first.split_focused(partition, window, display, command);
                } else {
                    s.second.split_focused(partition, window, display, command);
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

#[cfg(test)]
mod tests {
    use super::*;

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

    fn take_gt_messages(&mut self, out: &mut Vec<GtMessage>) {
        if let PreviewSlot::Editor { win, .. } = self {
            out.extend(win.take_gt_messages());
        }
    }
}

pub struct Multiplexer {
    window: Rc<Window>,
    display: Display,
    viewport: Viewport,
    status_view: TerminalView,
    sidebar: Sidebar,
    sidebar_focused: bool,
    preview_slot: PreviewSlot,
    editor_focused: bool,
    gt_file_assembler: GtFileAssembler,
    tabs: Vec<Node>,
    focus: usize,
    modifiers: ModifiersState,
    cursor_pos: PhysicalPosition<f64>,
    exited: bool,
    /// ウィンドウが隠れているか。Wayland では隠れると frame callback が
    /// 止まり、vsync 付きの描画がブロックして無応答になるため、隠れている間は
    /// 描画しない。`WindowEvent::Occluded` で更新する。
    occluded: bool,
    /// カーソル点滅の起点と現在の表示フェーズ。
    blink_start: Instant,
    cursor_blink_on: bool,
    /// ワークベンチの左サイドバー幅・上プレビュー高さの比率（実行時に
    /// Ctrl+Shift+矢印 で変えられる。初期値は config）。
    sidebar_ratio: f64,
    preview_ratio: f64,
}

impl Multiplexer {
    pub fn new(window: Window, display: Display) -> Self {
        let window = Rc::new(window);

        let size = window.inner_size();
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
            None,
        );
        let sidebar = Sidebar::new(display.clone(), viewport);
        let preview_slot = PreviewSlot::Reader(ReaderPane::new(display.clone(), viewport));

        // 最初のタブ（1枚なのでタブバーは出ない＝全面が端末）。
        let first = Node::Leaf(Box::new(TerminalWindow::with_viewport(
            window.clone(),
            display.clone(),
            viewport,
            None,
        )));

        let mut mux = Multiplexer {
            window,
            display,
            viewport,
            status_view,
            sidebar,
            sidebar_focused: false,
            preview_slot,
            editor_focused: false,
            gt_file_assembler: GtFileAssembler::default(),
            tabs: vec![first],
            focus: 0,
            modifiers: ModifiersState::empty(),
            cursor_pos: PhysicalPosition::default(),
            exited: false,
            occluded: false,
            blink_start: Instant::now(),
            cursor_blink_on: true,
            sidebar_ratio: crate::TOYTERM_CONFIG.sidebar_ratio,
            preview_ratio: crate::TOYTERM_CONFIG.preview_ratio,
        };
        mux.refresh_layout();
        mux
    }

    fn focused_root(&mut self) -> &mut Node {
        &mut self.tabs[self.focus]
    }

    fn focused_location(&mut self) -> ShellLocation {
        self.focused_root().focused_leaf_mut().pane_location()
    }

    /// フォーカスを矢印方向へ動かす。ワークベンチ表示中は3領域
    /// （左=サイドバー／右上=プレビュー／右下=ターミナル）もまたぐ。
    /// ターミナル領域内では従来どおり分割ツリーを辿り、端に達したら隣の領域へ。
    fn move_focus_workbench(&mut self, dir: Dir) {
        if !self.sidebar.is_visible() {
            self.tabs[self.focus].move_focus(dir);
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
        // ターミナル領域。まず分割ツリー内で移動し、端に達したら隣の領域へ。
        if self.tabs[self.focus].move_focus(dir) {
            return;
        }
        if matches!(dir, Dir::Left) {
            self.focus_sidebar();
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
        } else if self.tabs[self.focus].resize_focused(axis, delta) {
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

        let cvp = if self.sidebar.is_visible() {
            let viewports = workbench_viewports(
                self.content_viewport(),
                self.sidebar_ratio,
                self.preview_ratio,
            );
            self.sidebar.set_viewport(viewports.sidebar);
            self.preview_slot.set_viewport(viewports.preview);
            viewports.terminal
        } else {
            self.content_viewport()
        };
        for tab in &mut self.tabs {
            tab.set_viewport(cvp);
        }
    }

    fn update_status_bar(&mut self) {
        if self.tabs.len() <= 1 {
            return;
        }

        const BAR_BG: Color = Color::BrightBlack;

        let blank = {
            let mut c = Cell::new_ascii(' ');
            c.attr.fg = Color::White;
            c.attr.bg = BAR_BG;
            c
        };

        let cols = (self.viewport.w / self.status_view.cell_size().w).max(1) as usize;
        let mut cells: Vec<Cell> = Vec::new();

        for i in 0..self.tabs.len() {
            let focused = i == self.focus;
            let label = format!(" {} ", i + 1);
            for ch in label.chars() {
                let mut c = Cell::new_ascii(ch);
                if focused {
                    c.attr.fg = Color::Black;
                    c.attr.bg = Color::White;
                } else {
                    c.attr.fg = Color::White;
                    c.attr.bg = BAR_BG;
                }
                cells.push(c);
            }
        }

        cells.truncate(cols);
        cells.resize(cols, blank);

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

        if !self.modifiers.control_key() {
            return None;
        }
        let shift = self.modifiers.shift_key();

        let action = match (shift, code) {
            // タブ
            (false, KeyCode::Tab) => Action::NextTab,
            (true, KeyCode::Tab) => Action::PrevTab,
            (true, KeyCode::KeyT) => Action::NewTab,
            (true, KeyCode::KeyW) | (true, KeyCode::KeyQ) => Action::CloseFocused,
            // 分割
            (true, KeyCode::KeyE) => Action::SplitVertical,
            (true, KeyCode::KeyO) => Action::SplitHorizontal,
            (true, KeyCode::KeyF) => Action::ToggleSidebar,
            // フォーカス移動は vim 流 H/J/K/L。Alt+矢印 は Hyprland/IME に
            // 横取りされて届かなかったため、確実に届く Ctrl+Shift+英字にした。
            (true, KeyCode::KeyH) => Action::Focus(Dir::Left),
            (true, KeyCode::KeyJ) => Action::Focus(Dir::Down),
            (true, KeyCode::KeyK) => Action::Focus(Dir::Up),
            (true, KeyCode::KeyL) => Action::Focus(Dir::Right),
            // ペインのリサイズ（境界を矢印方向へ動かす）
            (true, KeyCode::ArrowUp) => Action::Resize(Dir::Up),
            (true, KeyCode::ArrowDown) => Action::Resize(Dir::Down),
            (true, KeyCode::ArrowLeft) => Action::Resize(Dir::Left),
            (true, KeyCode::ArrowRight) => Action::Resize(Dir::Right),
            _ => return None,
        };
        Some(action)
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::NewTab => {
                self.focused_root().focused_leaf_mut().focus_changed(false);

                // タブが増えるとバーが出て内容領域が縮むので、push 後に再レイアウト。
                let cvp = self.content_viewport();
                let pane = Node::Leaf(Box::new(TerminalWindow::with_viewport(
                    self.window.clone(),
                    self.display.clone(),
                    cvp,
                    None,
                )));
                self.tabs.push(pane);
                self.focus = self.tabs.len() - 1;
                self.refresh_layout();
                self.focused_root().focused_leaf_mut().focus_changed(true);
                self.update_status_bar();
            }

            Action::CloseFocused => {
                let tab_empty = self.tabs[self.focus].close_focused();
                if tab_empty {
                    self.tabs.remove(self.focus);
                    if self.tabs.is_empty() {
                        self.exited = true;
                        return;
                    }
                    if self.focus >= self.tabs.len() {
                        self.focus = self.tabs.len() - 1;
                    }
                    self.focused_root().focused_leaf_mut().focus_changed(true);
                }
                // タブ削除でも分割の畳み込みでも、残ったペインを広げ直す。
                self.refresh_layout();
                self.update_status_bar();
            }

            Action::NextTab | Action::PrevTab => {
                if self.tabs.len() <= 1 {
                    return;
                }
                self.focused_root().focused_leaf_mut().focus_changed(false);
                let n = self.tabs.len();
                self.focus = match action {
                    Action::NextTab => (self.focus + 1) % n,
                    _ => (self.focus + n - 1) % n,
                };
                self.focused_root().focused_leaf_mut().focus_changed(true);
                self.update_status_bar();
            }

            Action::SplitVertical | Action::SplitHorizontal => {
                let partition = match action {
                    Action::SplitVertical => Partition::Vertical,
                    _ => Partition::Horizontal,
                };
                let window = self.window.clone();
                let display = self.display.clone();
                self.tabs[self.focus].split_focused(partition, &window, &display, None);
            }

            Action::Focus(dir) => {
                self.move_focus_workbench(dir);
            }

            Action::Resize(dir) => {
                self.resize(dir);
            }

            // 1キーで3状態を回す：非表示→開いてフォーカス／表示中(端末フォーカス)→
            // サイドバーへフォーカス／サイドバーフォーカス中→閉じて端末へ。
            // 旧 Ctrl+Shift+B（フォーカスのみ）はこのサイクルに統合した。
            Action::ToggleSidebar => {
                if !self.sidebar.is_visible() {
                    let location = self.focused_location();
                    self.sidebar.toggle(&location);
                    self.refresh_layout();
                    self.focus_sidebar();
                } else if !self.sidebar_focused {
                    self.focus_sidebar();
                } else {
                    let location = self.focused_location();
                    self.sidebar.toggle(&location);
                    self.release_sidebar_focus();
                    self.release_editor_focus();
                    self.refresh_layout();
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
            let location = self.focused_location();
            self.sidebar.toggle(&location);
            self.refresh_layout();
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
        let saved = match std::mem::replace(&mut self.preview_slot, PreviewSlot::Empty) {
            PreviewSlot::Reader(reader) => Box::new(reader),
            PreviewSlot::Editor { saved, .. } => saved,
            PreviewSlot::Empty => return,
        };
        let mut win = Box::new(TerminalWindow::with_viewport_command(
            self.window.clone(),
            self.display.clone(),
            viewport,
            cwd.as_deref(),
            Some(&command),
        ));
        win.focus_changed(true);
        self.focused_root().focused_leaf_mut().focus_changed(false);
        self.sidebar_focused = false;
        self.sidebar.set_focused(false);
        self.editor_focused = true;
        self.preview_slot = PreviewSlot::Editor { win, saved };
    }

    fn handle_gt_messages(&mut self) {
        let mut messages = Vec::new();
        for tab in &mut self.tabs {
            tab.take_gt_messages(&mut messages);
        }
        self.preview_slot.take_gt_messages(&mut messages);

        if messages.is_empty() || !self.sidebar.is_visible() {
            return;
        }

        for message in messages {
            match message {
                GtMessage::Event { kind, path, tool } => {
                    let root = self.sidebar.root().map(Path::to_path_buf);
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
                    // 最小化すると Windows は Resized(0,0) を送ってくる。0 サイズで
                    // glium をリサイズすると落ちるうえ、描いても無意味なので、
                    // ビューポートだけ 0 にして（drawable() が false になる）戻る。
                    // 復元時は非0の Resized が再度届き、下の通常経路で描き直す。
                    if new_size.width == 0 || new_size.height == 0 {
                        self.viewport = Viewport {
                            x: 0,
                            y: 0,
                            w: 0,
                            h: 0,
                        };
                        return;
                    }
                    // glium 0.34 の手書きサーフェスは自動リサイズされないため明示。
                    self.display.resize((new_size.width, new_size.height));
                    self.viewport = Viewport {
                        x: 0,
                        y: 0,
                        w: new_size.width,
                        h: new_size.height,
                    };
                    self.refresh_layout();
                    self.update_status_bar();
                    // リサイズが来た＝ウィンドウは見えている。モニター切替時に
                    // Occluded(true) を受けたまま解除イベントを取りこぼすと画面が
                    // 固まるため、ここで遮蔽フラグを下ろして即再描画する。
                    self.occluded = false;
                    self.window.request_redraw();
                }

                WindowEvent::ScaleFactorChanged { .. } => {
                    // モニター間移動やマルチ→シングル切替で DPI(スケール)が変わると、
                    // ピクセルサイズが同じでもサーフェスが古いまま残ることがある。
                    // 実サイズで再同期し、遮蔽フラグも下ろして描き直す。直後に
                    // Resized が続く場合もあるが、来ないケースの取りこぼしを防ぐ。
                    let new = self.window.inner_size();
                    self.display.resize((new.width, new.height));
                    self.viewport = Viewport {
                        x: 0,
                        y: 0,
                        w: new.width,
                        h: new.height,
                    };
                    self.refresh_layout();
                    self.update_status_bar();
                    self.occluded = false;
                    self.window.request_redraw();
                }

                &WindowEvent::Focused(focused) => {
                    // ワークスペース切替などで Occluded(false) を取りこぼしても、
                    // フォーカスが戻った＝必ず可視なので、遮蔽フラグを下ろして
                    // 描き直す。これが無いと別ワークスペースから戻ったとき画面が
                    // 固まったまま（待機）になる。
                    if focused {
                        self.occluded = false;
                        self.window.request_redraw();
                    }
                    self.focused_root()
                        .focused_leaf_mut()
                        .process_window_event(wev);
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

                    // まずフレーム全体を背景色でクリアする。分割の隙間や、
                    // バー左右の余白を埋める。各ペインは自分の矩形を上書きする。
                    {
                        use glium::Surface as _;
                        let bg = crate::TOYTERM_CONFIG.color_background;
                        let r = ((bg >> 24) & 0xff) as f32 / 255.0;
                        let g = ((bg >> 16) & 0xff) as f32 / 255.0;
                        let b = ((bg >> 8) & 0xff) as f32 / 255.0;
                        let a = (bg & 0xff) as f32 / 255.0;
                        surface.clear_color_srgb(r, g, b, a);
                    }

                    if self.status_bar_height() > 0 {
                        self.status_view.draw(&mut surface);
                    }
                    self.tabs[self.focus].draw(&mut surface);
                    if self.sidebar.is_visible() {
                        self.preview_slot.draw(&mut surface);
                    }
                    self.sidebar.draw(&mut surface);

                    surface.finish().expect("finish");
                }

                WindowEvent::ModifiersChanged(m) => {
                    self.modifiers = m.state();
                    // 各ペインの内部修飾キー状態も合わせておく（フォーカス切替後も正しく）。
                    for tab in &mut self.tabs {
                        tab.for_each_leaf(&mut |w| w.process_window_event(wev));
                    }
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
                    if let Some(action) = self.parse_shortcut(key) {
                        self.handle_action(action);
                        if self.sidebar_focused && !matches!(action, Action::ToggleSidebar) {
                            self.focused_root().focused_leaf_mut().focus_changed(false);
                        }
                    } else if self.sidebar_focused {
                        let result = self.sidebar.on_key(key);
                        self.handle_sidebar_key_result(result);
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

                WindowEvent::CursorMoved { position, .. } => {
                    self.cursor_pos = *position;
                    // どのペインでドラッグ選択しても効くよう、全ペインへ座標を配る。
                    let focus_tab = self.focus;
                    self.tabs[focus_tab].for_each_leaf(&mut |w| w.process_window_event(wev));
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
                        } else if let Some(reader) = self.preview_slot.visible_reader_mut() {
                            if let Some(request) = reader.on_click(self.cursor_pos) {
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
                    } else {
                        self.focused_root().focused_leaf_mut().focus_changed(false);
                    }
                    self.tabs[self.focus].focus_at(p);
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

                WindowEvent::Ime(_) if self.sidebar_focused => {}

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
                // 全タブの PTY を汲み取り、終了したペイン/タブを取り除く。
                let mut changed = false;
                let mut i = 0;
                while i < self.tabs.len() {
                    let mut collapsed = false;
                    let empty = self.tabs[i].update_and_prune(&mut collapsed);
                    if collapsed {
                        changed = true;
                    }
                    if empty {
                        self.tabs.remove(i);
                        changed = true;
                        if self.tabs.is_empty() {
                            // exited を立てないと、終了確定前に届く次のイベント
                            // （AboutToWait やフォーカス/クリック）が空の tabs を
                            // 触って panic する（index out of bounds: len 0）。
                            // Ctrl+Shift+W の経路と同じくフラグを立てる。
                            self.exited = true;
                            elwt.exit();
                            return;
                        }
                        // フォーカス index を取り除いた位置に合わせて補正する。
                        if i < self.focus {
                            self.focus -= 1;
                        } else if i == self.focus {
                            if self.focus >= self.tabs.len() {
                                self.focus = self.tabs.len() - 1;
                            }
                            // フォーカスしていたタブが消えたので、新しいタブの葉へ。
                            self.focused_root().focused_leaf_mut().focus_changed(true);
                        }
                        // remove(i) で詰めたので i はそのまま次のタブを指す。
                    } else {
                        i += 1;
                    }
                }
                // ペインやタブが減ったら、残ったペインを領域いっぱいに広げ直す。
                if changed {
                    self.refresh_layout();
                    self.update_status_bar();
                }

                self.handle_gt_messages();

                if self.sidebar.is_visible() && self.preview_slot.check_update() {
                    if let PreviewSlot::Editor { mut saved, .. } =
                        std::mem::replace(&mut self.preview_slot, PreviewSlot::Empty)
                    {
                        saved.refresh_current();
                        self.preview_slot = PreviewSlot::Reader(*saved);
                        self.editor_focused = false;
                        self.focused_root().focused_leaf_mut().focus_changed(true);
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
                let need = self.tabs[self.focus].needs_redraw()
                    || (self.status_bar_height() > 0 && self.status_view.needs_redraw())
                    || self.sidebar.needs_redraw()
                    || (self.sidebar.is_visible() && self.preview_slot.needs_redraw());
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
