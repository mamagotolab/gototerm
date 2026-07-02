// 描画で使うデータ構造（Cell/Line/Color/Cursor 等）を保持するモジュール。
// 一部のメソッド・定数は現状未使用だが、将来用・対称性のため残すので警告を抑える。
#![allow(dead_code)]

use std::cmp::min;
use std::ops::{Range, RangeBounds};

#[derive(Debug, Clone)]
pub struct PositionedImage {
    pub row: isize,
    pub col: isize,
    pub height: u64,
    pub width: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalSize {
    pub rows: usize,
    pub cols: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellSize {
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Cell {
    pub ch: char,
    pub width: u16,
    backlink: u16,
    pub attr: GraphicAttribute,
}

impl Cell {
    const VOID: Self = Cell {
        ch: '#',
        width: 0,
        backlink: u16::MAX,
        attr: GraphicAttribute::default(),
    };

    const SPACE: Self = Cell {
        ch: ' ',
        width: 1,
        backlink: 0,
        attr: GraphicAttribute::default(),
    };

    // A marker representing a termination of line
    const TERM: Self = Cell {
        ch: '\n',
        width: 1,
        backlink: 0,
        attr: GraphicAttribute::default(),
    };

    #[allow(unused)]
    pub fn new_ascii(ch: char) -> Cell {
        let mut cell = Self::SPACE;
        cell.ch = ch;
        cell
    }

    /// 描画用に外部（VTアダプタ）からセルを組むためのコンストラクタ。
    pub fn head(ch: char, width: u16, attr: GraphicAttribute) -> Cell {
        Cell {
            ch,
            width,
            backlink: 0,
            attr,
        }
    }

    /// 全角文字の右側など、幅0のスペーサセル。
    pub fn spacer(backlink: u16) -> Cell {
        Cell {
            ch: ' ',
            width: 0,
            backlink,
            attr: GraphicAttribute::default(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Color {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    Rgb { rgba: u32 },
    Special,
    Foreground,
    Background,
    Selection,
}

#[derive(Debug, Clone, Copy)]
pub struct GraphicAttribute {
    pub fg: Color,
    pub bg: Color,
    pub bold: i8,
    pub inversed: bool,
    pub blinking: u8,
    pub concealed: bool,
}

impl GraphicAttribute {
    pub const fn default() -> Self {
        GraphicAttribute {
            fg: Color::Foreground,
            bg: Color::Background,
            bold: 0,
            inversed: false,
            blinking: 0,
            concealed: false,
        }
    }
}

/// A single line of terminal buffer
///
/// A `Line` consists of multiple `Cell`s, which may have different width.
/// The number of cells is the same as terminal columns.
/// If there are multi-width cells, the following invariants must be met.
///
/// ## Invariants
/// - If a cell has multiple width (let's call this "head" cell),
///   each of following cells that are covered by the "head" must be 0-width.
/// - Every cell must have a `backlink` field, which represents a distance from the "head" cell.
///
/// ## Example
/// If we have a cell
/// `Cell { ch: '\t', width: 4, backlink: 0 }`, then it should be followed by
/// `Cell { ch: '#',  width: 0, backlink: 1 }`,
/// `Cell { ch: '#',  width: 0, backlink: 2 }`, and
/// `Cell { ch: '#',  width: 0, backlink: 3 }`.
///
#[derive(Clone)]
pub struct Line {
    cells: Vec<Cell>,
    linewrap: bool,
}

impl std::iter::FromIterator<Cell> for Line {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = Cell>,
    {
        Line {
            cells: iter.into_iter().collect(),
            linewrap: false,
        }
    }
}

impl Line {
    fn new(len: usize) -> Self {
        Line {
            cells: vec![Cell::TERM; len],
            linewrap: false,
        }
    }

    /// 描画用に外部（VTアダプタ）から行を組むためのコンストラクタ。
    pub fn from_cells(cells: Vec<Cell>, linewrap: bool) -> Self {
        Line { cells, linewrap }
    }

    pub fn cells_mut(&mut self) -> &mut [Cell] {
        &mut self.cells
    }

    pub fn copy_from(&mut self, src: &Self) {
        if self.cells.len() == src.cells.len() {
            self.cells.copy_from_slice(&src.cells);
        } else {
            self.cells.clear();
            self.cells.extend_from_slice(&src.cells);
        }
        self.linewrap = src.linewrap;
    }

    fn saturating_range<R: RangeBounds<usize>>(&self, range: R) -> Range<usize> {
        let len = self.cells.len();

        use std::ops::Bound;
        let start = match range.start_bound() {
            Bound::Included(&p) => p,
            Bound::Excluded(&p) => p + 1,
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&p) => p + 1,
            Bound::Excluded(&p) => p,
            Bound::Unbounded => len,
        };

        let start = min(start, len);
        let end = min(end, len);
        debug_assert!(start <= len && end <= len && start <= end);

        Range { start, end }
    }

    fn copy_within<R: RangeBounds<usize> + Clone>(&mut self, src: R, dst: usize) {
        let src = self.saturating_range(src);
        let count = min(src.len(), self.cells.len() - dst);
        if count == 0 {
            return;
        }

        self.cells.copy_within(src.start..src.start + count, dst);

        let (dst_start, dst_end) = (dst, dst + count);

        // Correct boundaries because the above `copy_within` may violates the invariant.
        {
            // correct ..dst_start)
            if dst_start > 0 {
                let head = self.get_head_pos(dst_start - 1);
                if head + self.cells[head].width as usize > dst_start {
                    self.cells[head..dst_start].fill(Cell::SPACE);
                }
            }

            // correct [dst_start..
            let mut i = dst_start;
            while i < dst_end && self.cells[i].width == 0 {
                self.cells[i] = Cell::SPACE;
                i += 1;
            }

            // correct ..dst_end)
            let head = self.get_head_pos(dst_end - 1);
            if head + self.cells[head].width as usize > dst_end {
                self.cells[head..dst_end].fill(Cell::SPACE);
            }

            // correct [dst_end..
            let mut i = dst + count;
            while i < self.cells.len() && self.cells[i].width == 0 {
                self.cells[i] = Cell::SPACE;
                i += 1;
            }
        }
    }

    fn erase<R: RangeBounds<usize>>(&mut self, range: R) {
        for i in self.saturating_range(range) {
            self.erase_at(i);
        }
    }

    fn erase_all(&mut self) {
        self.cells.fill(Cell::TERM);
        self.linewrap = false;
    }

    fn erase_at(&mut self, at: usize) {
        let head = self.get_head_pos(at);
        let width = self.cells[head].width as usize;
        let end = min(head + width, self.cells.len());

        #[cfg(debug_assertions)]
        for i in head + 1..end {
            debug_assert_eq!(self.cells[i].width, 0);
            debug_assert_eq!(self.cells[i].backlink as usize, i - head);
        }

        self.cells[head..end].fill(Cell::SPACE);
    }

    fn get_head_pos(&self, at: usize) -> usize {
        at - self.cells[at].backlink as usize
    }

    fn resize(&mut self, new_len: usize) {
        self.cells.resize(new_len, Cell::TERM);

        let head = self.get_head_pos(new_len - 1);
        let width = self.cells[head].width as usize;
        if head + width > self.cells.len() {
            self.erase_at(head);
        }
    }

    pub fn columns(&self) -> usize {
        self.cells.len()
    }

    fn put(&mut self, at: usize, cell: Cell) {
        let width = cell.width as usize;

        debug_assert!(at + width <= self.cells.len());

        self.erase(at..at + width);
        self.cells[at] = cell;
        for d in 1..width {
            let mut cell = Cell::VOID;
            cell.backlink = d as u16;
            self.cells[at + d] = cell;
        }
    }

    pub fn get(&self, at: usize) -> Option<Cell> {
        if at < self.cells.len() {
            let head = self.get_head_pos(at);
            Some(self.cells[head])
        } else {
            None
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = Cell> + '_ {
        self.cells.iter().copied()
    }

    pub fn linewrap(&self) -> bool {
        self.linewrap
    }
}

impl std::fmt::Debug for Line {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "[")?;
        for c in self.cells.iter() {
            writeln!(
                f,
                "\tch: {:?}, width: {}, backlink: {}",
                c.ch, c.width, c.backlink
            )?;
        }
        writeln!(f, "]")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Cursor {
    sz: TerminalSize,
    pub row: usize,
    pub col: usize,
    end: bool,
    pub style: CursorStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorStyle {
    #[default]
    Block,
    Underline,
    Bar,
}

impl Cursor {
    /// 描画用に row/col/style だけ指定して作る（VTアダプタ用）。
    pub fn at(row: usize, col: usize, style: CursorStyle) -> Cursor {
        Cursor {
            row,
            col,
            style,
            ..Cursor::default()
        }
    }

    fn pos(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    fn right_space(&self) -> usize {
        if self.end {
            0
        } else {
            self.sz.cols - self.col
        }
    }

    fn exact(mut self, row: usize, col: usize) -> Self {
        self.row = min(row, self.sz.rows - 1);
        self.col = min(col, self.sz.cols - 1);
        self.end = false;
        self
    }

    fn first_col(mut self) -> Self {
        self.end = false;
        self.col = 0;
        self
    }

    fn next_col(mut self) -> Self {
        if self.col + 1 < self.sz.cols {
            self.col += 1;
        } else {
            self.end = true;
        }
        self
    }

    fn prev_col(mut self) -> Self {
        if self.end {
            debug_assert_eq!(self.col, self.sz.cols - 1);
            self.end = false;
        } else if 0 < self.col {
            self.col -= 1;
        }
        self
    }

    fn next_row(mut self) -> Self {
        self.end = false;
        if self.row + 1 < self.sz.rows {
            self.row += 1;
        }
        self
    }

    fn prev_row(mut self) -> Self {
        self.end = false;
        if self.row > 0 {
            self.row -= 1;
        }
        self
    }
}
