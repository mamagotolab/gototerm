//! タブと画面分割を司るマネージャ。
//!
//! 1つの OS ウィンドウの中に複数の端末セッション（[`TerminalWindow`]）を
//! 抱え、タブ（[`Vec<Node>`]）と二分木の分割（[`Node::Split`]）として配置する。
//! ウィンドウ全体のイベント（リサイズ・再描画・終了）はここで握り、
//! キー/マウス/IME は現在フォーカスしているペインへ振り分ける。

use std::rc::Rc;
use std::time::{Duration, Instant};

use winit::{
    dpi::PhysicalPosition,
    event::{ElementState, KeyEvent, WindowEvent},
    event_loop::{ControlFlow, EventLoopWindowTarget},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::Window,
};

use crate::terminal::{Cell, Color, Line};
use crate::view::{TerminalView, Viewport};
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
enum Action {
    NewTab,
    CloseFocused,
    NextTab,
    PrevTab,
    SplitVertical,
    SplitHorizontal,
    Focus(Dir),
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
    fn split_focused(&mut self, partition: Partition, window: &Rc<Window>, display: &Display) {
        match self {
            Node::Leaf(_) => {
                let vp = self.focused_leaf_mut().viewport();

                let taken = std::mem::replace(self, Node::Empty);
                let old = match taken {
                    Node::Leaf(w) => w,
                    _ => unreachable!(),
                };

                // 新ペインの作業ディレクトリは gototerm の起動ディレクトリ。
                let cwd = std::env::current_dir().ok();
                let new_win = Box::new(TerminalWindow::with_viewport(
                    window.clone(),
                    display.clone(),
                    vp,
                    cwd.as_deref(),
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
                    s.first.split_focused(partition, window, display);
                } else {
                    s.second.split_focused(partition, window, display);
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
        let vp = Viewport { x: 10, y: 20, w: 100, h: 50 };
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
        let vp = Viewport { x: 0, y: 0, w: 80, h: 200 };
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
        let vp = Viewport { x: 0, y: 0, w: 120, h: 60 };
        let (left, right) = split_viewport(Partition::Vertical, 0.25, vp);
        // mid = 30
        assert_eq!(left.w, 30 - GAP);
        assert_eq!(right.x, 30 + GAP);
        assert_eq!(right.w, 120 - 30 - GAP);
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

pub struct Multiplexer {
    window: Rc<Window>,
    display: Display,
    viewport: Viewport,
    status_view: TerminalView,
    tabs: Vec<Node>,
    focus: usize,
    modifiers: ModifiersState,
    cursor_pos: PhysicalPosition<f64>,
    exited: bool,
    /// ウィンドウが隠れているか。Wayland では隠れると frame callback が
    /// 止まり、vsync 付きの描画がブロックして無応答になるため、隠れている間は
    /// 描画しない。`WindowEvent::Occluded` で更新する。
    occluded: bool,
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
            tabs: vec![first],
            focus: 0,
            modifiers: ModifiersState::empty(),
            cursor_pos: PhysicalPosition::default(),
            exited: false,
            occluded: false,
        };
        mux.refresh_layout();
        mux
    }

    fn focused_root(&mut self) -> &mut Node {
        &mut self.tabs[self.focus]
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

        let cvp = self.content_viewport();
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
            // フォーカス移動
            (true, KeyCode::ArrowUp) => Action::Focus(Dir::Up),
            (true, KeyCode::ArrowDown) => Action::Focus(Dir::Down),
            (true, KeyCode::ArrowLeft) => Action::Focus(Dir::Left),
            (true, KeyCode::ArrowRight) => Action::Focus(Dir::Right),
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
                self.tabs[self.focus].split_focused(partition, &window, &display);
            }

            Action::Focus(dir) => {
                self.tabs[self.focus].move_focus(dir);
            }
        }
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
                }

                &WindowEvent::Occluded(occluded) => {
                    self.occluded = occluded;
                    // 再表示されたら最新内容を描き直す。
                    if !occluded {
                        self.window.request_redraw();
                    }
                }

                WindowEvent::RedrawRequested => {
                    // 隠れている間は描画しない。Wayland では frame callback が
                    // 止まり、ここでの swap がブロックして無応答になるため。
                    if self.occluded {
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

                    surface.finish().expect("finish");
                }

                WindowEvent::ModifiersChanged(m) => {
                    self.modifiers = m.state();
                    // 各ペインの内部修飾キー状態も合わせておく（フォーカス切替後も正しく）。
                    for tab in &mut self.tabs {
                        tab.for_each_leaf(&mut |w| w.process_window_event(wev));
                    }
                }

                WindowEvent::KeyboardInput { event: key, .. } => {
                    if let Some(action) = self.parse_shortcut(key) {
                        self.handle_action(action);
                    } else {
                        self.focused_root().focused_leaf_mut().process_window_event(wev);
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
                    // クリックしたペインへフォーカスを移してから入力を渡す。
                    let p = self.cursor_pos;
                    self.focused_root().focused_leaf_mut().focus_changed(false);
                    self.tabs[self.focus].focus_at(p);
                    self.focused_root().focused_leaf_mut().focus_changed(true);
                    self.focused_root().focused_leaf_mut().process_window_event(wev);
                }

                // フォーカス・IME・ホイール・マウス離し等はフォーカス中ペインへ。
                _ => {
                    self.focused_root().focused_leaf_mut().process_window_event(wev);
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

                // 隠れている間は再描画を要求しない（swap ブロック＝無応答を防ぐ）。
                // 内容更新自体は上で汲み取り済みなので、再表示時にまとめて描ける。
                let need = self.tabs[self.focus].needs_redraw()
                    || (self.status_bar_height() > 0 && self.status_view.needs_redraw());
                if need && !self.occluded {
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
