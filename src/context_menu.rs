//! 右クリックメニュー。項目・区切り線・ショートカット表示と、ピクセル単位の配置を持つ。
//! 文字の升目から切り離して置くので、行間を広く取れ、角も丸くできる。
//! 描画は window.rs が view の PopupLayer（端末の一番上に重ねる層）へ渡す。

use crate::keybindings::ShortcutAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuItem {
    Copy,
    Paste,
    Search,
    ClearHistory,
    Reset,
    SplitRight,
    SplitDown,
    NewTab,
    ClosePane,
    /// 「gototask ▸」：選ぶと横にサブメニューが開く。
    Gototask,
    TaskOpen,
    TaskToday,
    TaskList,
    TaskAdd,
    TaskDone,
    TaskHelp,
}

impl MenuItem {
    pub(crate) fn label(self) -> &'static str {
        match self {
            MenuItem::Copy => "コピー",
            MenuItem::Paste => "貼り付け",
            MenuItem::Search => "履歴を検索",
            MenuItem::ClearHistory => "履歴を消去",
            MenuItem::Reset => "端末をリセット",
            MenuItem::SplitRight => "右に分割",
            MenuItem::SplitDown => "下に分割",
            MenuItem::NewTab => "新しいタブ",
            MenuItem::ClosePane => "閉じる",
            MenuItem::Gototask => "gototask",
            MenuItem::TaskOpen => "タスク画面を開く",
            MenuItem::TaskToday => "今日のタスク",
            MenuItem::TaskList => "未完了の一覧",
            MenuItem::TaskAdd => "タスクを追加",
            MenuItem::TaskDone => "済にする",
            MenuItem::TaskHelp => "コマンドの一覧",
        }
    }

    /// 同じ操作のキー割り当て（右側に薄く表示する）。
    pub(crate) fn shortcut(self) -> Option<ShortcutAction> {
        match self {
            MenuItem::Copy => Some(ShortcutAction::Copy),
            MenuItem::Paste => Some(ShortcutAction::Paste),
            MenuItem::Search => None,
            MenuItem::ClearHistory => Some(ShortcutAction::ClearHistory),
            MenuItem::Reset => None,
            MenuItem::SplitRight => Some(ShortcutAction::SplitVertical),
            MenuItem::SplitDown => Some(ShortcutAction::SplitHorizontal),
            MenuItem::NewTab => Some(ShortcutAction::NewTab),
            MenuItem::ClosePane => Some(ShortcutAction::ClosePane),
            MenuItem::TaskOpen => Some(ShortcutAction::OpenTasks),
            _ => None,
        }
    }

    /// gototask の項目が打ち込むコマンド（右側に出して、コマンドを思い出せるようにする）。
    pub(crate) fn command(self) -> Option<&'static str> {
        match self {
            MenuItem::TaskToday => Some("gtask today"),
            MenuItem::TaskList => Some("gtask ls"),
            MenuItem::TaskAdd => Some("gtask add"),
            MenuItem::TaskDone => Some("gtask done"),
            MenuItem::TaskHelp => Some("gtask -h"),
            _ => None,
        }
    }

    /// 選ぶと横にサブメニューを開く項目か。
    pub(crate) fn has_submenu(self) -> bool {
        self == MenuItem::Gototask
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Entry {
    Item {
        item: MenuItem,
        enabled: bool,
        shortcut: String,
    },
    Separator,
}

/// 寸法（ピクセル）。セルの大きさから決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Metrics {
    pub(crate) cell_w: u32,
    pub(crate) cell_h: u32,
    /// 項目1行の高さ（文字の行より広い）。
    pub(crate) row_h: u32,
    /// 区切り線の行の高さ。
    pub(crate) sep_h: u32,
    /// 枠の上下の余白。
    pub(crate) pad_y: u32,
    /// 枠の左右から文字までの余白。
    pub(crate) pad_x: u32,
    /// 項目名とショートカットの間の最小の空き。
    pub(crate) gap: u32,
    /// 角の丸み。
    pub(crate) radius: u32,
}

impl Metrics {
    pub(crate) fn new(cell_w: u32, cell_h: u32) -> Self {
        let cell_w = cell_w.max(1);
        let cell_h = cell_h.max(1);
        Metrics {
            cell_w,
            cell_h,
            row_h: (cell_h * 17 / 10).max(cell_h + 8),
            sep_h: (cell_h * 6 / 10).max(9),
            pad_y: (cell_h / 3).max(5),
            pad_x: cell_w + cell_w / 2 + 4,
            gap: cell_w * 4,
            radius: (cell_h / 2).clamp(6, 12),
        }
    }
}

/// 項目1行の配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowBox {
    pub(crate) index: usize,
    pub(crate) top: i32,
    pub(crate) height: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct ContextMenu {
    pub(crate) entries: Vec<Entry>,
    pub(crate) metrics: Metrics,
    /// 枠の左上（ビュー内のピクセル）と大きさ。
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) w: u32,
    pub(crate) h: u32,
    /// ホバー中の項目（entries の番号）。
    pub(crate) hovered: Option<usize>,
    /// 開いているサブメニューと、それを開いた項目の番号。
    pub(crate) sub: Option<(usize, Box<ContextMenu>)>,
}

/// 画面の端からの最小の空き。
const EDGE: i32 = 4;

impl ContextMenu {
    /// クリックした点 (px, py) の右下に開く。下に入らなければ上へ、右に入らなければ左へ開く。
    pub(crate) fn new(
        entries: Vec<Entry>,
        px: i32,
        py: i32,
        view_w: u32,
        view_h: u32,
        metrics: Metrics,
    ) -> Self {
        let text_cols = entries
            .iter()
            .map(|entry| match entry {
                Entry::Item { item, shortcut, .. } => {
                    let label =
                        crate::launcher::display_width(item.label()) as u32 * metrics.cell_w;
                    let keys = crate::launcher::display_width(shortcut) as u32 * metrics.cell_w;
                    label + if keys > 0 { metrics.gap + keys } else { 0 }
                }
                Entry::Separator => 0,
            })
            .max()
            .unwrap_or(0);
        let w = text_cols + metrics.pad_x * 2;
        let h = metrics.pad_y * 2
            + entries
                .iter()
                .map(|entry| match entry {
                    Entry::Item { .. } => metrics.row_h,
                    Entry::Separator => metrics.sep_h,
                })
                .sum::<u32>();
        let place = |click: i32, size: u32, limit: u32| -> i32 {
            let (size, limit) = (size as i32, limit as i32);
            let start = if click + size + EDGE <= limit {
                click
            } else if click - size >= EDGE {
                click - size
            } else {
                limit - size - EDGE
            };
            start.max(EDGE.min(limit - size).max(0))
        };
        ContextMenu {
            x: place(px, w, view_w),
            y: place(py, h, view_h),
            entries,
            metrics,
            w,
            h,
            hovered: None,
            sub: None,
        }
    }

    /// 点がこの枠の中か。
    pub(crate) fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w as i32 && py >= self.y && py < self.y + self.h as i32
    }

    /// 項目 `index` の横にサブメニューを開く。右に入らなければ左へ。
    pub(crate) fn open_sub(&mut self, index: usize, entries: Vec<Entry>, view_w: u32, view_h: u32) {
        let Some(row) = self.rows().into_iter().find(|row| row.index == index) else {
            return;
        };
        let mut sub = ContextMenu::new(entries, 0, 0, view_w, view_h, self.metrics);
        // 親の枠に少し重ね、1行目が開いた項目の高さにそろうように置く。
        let overlap = 4;
        let right = self.x + self.w as i32 - overlap;
        sub.x = if right + sub.w as i32 + EDGE <= view_w as i32 {
            right
        } else {
            (self.x + overlap - sub.w as i32).max(EDGE.min(view_w as i32 - sub.w as i32).max(0))
        };
        let top = row.top - self.metrics.pad_y as i32;
        sub.y = top
            .min(view_h as i32 - sub.h as i32 - EDGE)
            .max(EDGE.min(view_h as i32 - sub.h as i32).max(0));
        self.sub = Some((index, Box::new(sub)));
    }

    /// 各行（項目と区切り）の位置。
    pub(crate) fn rows(&self) -> Vec<RowBox> {
        let mut top = self.y + self.metrics.pad_y as i32;
        let mut out = Vec::with_capacity(self.entries.len());
        for (index, entry) in self.entries.iter().enumerate() {
            let height = match entry {
                Entry::Item { .. } => self.metrics.row_h,
                Entry::Separator => self.metrics.sep_h,
            };
            out.push(RowBox { index, top, height });
            top += height as i32;
        }
        out
    }

    /// 点 (px, py) にある、押せる項目の番号。区切り・無効・枠外は None。
    pub(crate) fn item_at(&self, px: i32, py: i32) -> Option<usize> {
        if px < self.x || px >= self.x + self.w as i32 {
            return None;
        }
        self.rows()
            .into_iter()
            .find(|row| py >= row.top && py < row.top + row.height as i32)
            .and_then(|row| self.item(row.index).map(|_| row.index))
    }

    /// 押せる項目なら、その中身。
    pub(crate) fn item(&self, index: usize) -> Option<MenuItem> {
        match self.entries.get(index)? {
            Entry::Item {
                item,
                enabled: true,
                ..
            } => Some(*item),
            _ => None,
        }
    }

    /// キーボードでホバーを上下に動かす（区切り・無効は飛ばす）。
    pub(crate) fn move_hover(&mut self, down: bool) {
        let enabled: Vec<usize> = (0..self.entries.len())
            .filter(|&i| self.item(i).is_some())
            .collect();
        let Some((&first, &last)) = enabled.first().zip(enabled.last()) else {
            return;
        };
        self.hovered = Some(match self.hovered {
            None if down => first,
            None => last,
            Some(now) if down => enabled.iter().copied().find(|&i| i > now).unwrap_or(now),
            Some(now) => enabled
                .iter()
                .rev()
                .copied()
                .find(|&i| i < now)
                .unwrap_or(now),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(item: MenuItem, enabled: bool, shortcut: &str) -> Entry {
        Entry::Item {
            item,
            enabled,
            shortcut: shortcut.into(),
        }
    }

    fn sample() -> Vec<Entry> {
        vec![
            item(MenuItem::Copy, false, "Ctrl+Shift+C"),
            item(MenuItem::Paste, true, "Ctrl+Shift+V"),
            Entry::Separator,
            item(MenuItem::Search, true, ""),
        ]
    }

    fn metrics() -> Metrics {
        Metrics::new(10, 20)
    }

    #[test]
    fn menu_size_follows_text_and_metrics() {
        let m = metrics();
        let menu = ContextMenu::new(sample(), 100, 50, 1000, 800, m);
        assert_eq!((menu.x, menu.y), (100, 50));
        // 「貼り付け」8列 + 空き + 「Ctrl+Shift+V」12列 + 左右の余白
        assert_eq!(menu.w, 80 + m.gap + 120 + m.pad_x * 2);
        assert_eq!(menu.h, m.pad_y * 2 + m.row_h * 3 + m.sep_h);
        assert!(m.row_h > m.cell_h, "行間は文字の行より広い");
    }

    #[test]
    fn menu_opens_up_and_left_near_the_edge() {
        let menu = ContextMenu::new(sample(), 990, 790, 1000, 800, metrics());
        assert_eq!(menu.x, 990 - menu.w as i32);
        assert_eq!(menu.y, 790 - menu.h as i32);
        // 画面より大きいときも左上の内側に収める
        let tiny = ContextMenu::new(sample(), 5, 5, 60, 40, metrics());
        assert_eq!((tiny.x, tiny.y), (0, 0));
    }

    #[test]
    fn only_enabled_items_can_be_picked() {
        let m = metrics();
        let menu = ContextMenu::new(sample(), 0, 0, 1000, 800, m);
        let (x, top) = (menu.x + 10, menu.y);
        let y = |row: i32| top + (m.pad_y + m.row_h / 2) as i32 + row * m.row_h as i32;
        assert_eq!(menu.item_at(x, top + 1), None); // 上の余白
        assert_eq!(menu.item_at(x, y(0)), None); // 無効なコピー
        assert_eq!(menu.item_at(x, y(1)), Some(1));
        let sep = top + (m.pad_y + m.row_h * 2 + m.sep_h / 2) as i32;
        assert_eq!(menu.item_at(x, sep), None); // 区切り
        let search = top + (m.pad_y + m.row_h * 2 + m.sep_h + m.row_h / 2) as i32;
        assert_eq!(menu.item_at(x, search), Some(3));
        assert_eq!(menu.item_at(menu.x + menu.w as i32, y(1)), None); // 右の外
    }

    #[test]
    fn keyboard_hover_skips_separators_and_disabled_items() {
        let mut menu = ContextMenu::new(sample(), 0, 0, 1000, 800, metrics());
        menu.move_hover(true);
        assert_eq!(menu.hovered, Some(1));
        menu.move_hover(true);
        assert_eq!(menu.hovered, Some(3));
        menu.move_hover(true);
        assert_eq!(menu.hovered, Some(3));
        menu.move_hover(false);
        assert_eq!(menu.hovered, Some(1));
        menu.move_hover(false);
        assert_eq!(menu.hovered, Some(1));
    }

    #[test]
    fn submenu_opens_beside_its_row_and_flips_left_at_the_edge() {
        let m = metrics();
        let mut menu = ContextMenu::new(sample(), 100, 50, 1000, 800, m);
        menu.open_sub(3, sample(), 1000, 800);
        let (at, sub) = menu.sub.as_ref().unwrap();
        assert_eq!(*at, 3);
        assert_eq!(sub.x, menu.x + menu.w as i32 - 4, "親の右に少し重ねて開く");
        let row = menu.rows()[3].top;
        assert_eq!(sub.rows()[0].top, row, "1行目が開いた項目の高さにそろう");
        // 右に入らなければ左へ
        let mut edge = ContextMenu::new(sample(), 1000, 50, 1000, 800, m);
        edge.open_sub(1, sample(), 1000, 800);
        let (_, sub) = edge.sub.as_ref().unwrap();
        assert!(sub.x + sub.w as i32 <= edge.x + 4);
    }
}
