//! Temporary text captured only while copying across application scrolling.
use alacritty_terminal::selection::SelectionType;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub cells: Vec<String>,
    pub wrapped: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub rows: Vec<Row>,
    pub alternate: bool,
    pub scroll: ScrollTrace,
}
/// Bounded record of grid rotations emitted by the application, not inferred from text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScrollTrace {
    pub serial: u64,
    pub pending: std::collections::BTreeSet<usize>,
    events: std::collections::VecDeque<(u64, Option<(i64, std::ops::Range<usize>)>)>,
}
impl ScrollTrace {
    pub fn push(&mut self, event: Option<(i64, std::ops::Range<usize>)>) {
        self.serial += 1;
        self.events.push_back((self.serial, event));
        if self.events.len() > 64 {
            self.events.pop_front();
        }
    }
    fn since(&self, old: &Self) -> Result<Option<(i64, std::ops::Range<usize>)>, ()> {
        if self.serial == old.serial {
            return Ok(None);
        }
        let mut serial = old.serial;
        let mut movement: Option<(i64, std::ops::Range<usize>)> = None;
        for (id, event) in self.events.iter().filter(|(id, _)| *id > old.serial) {
            if *id != serial + 1 {
                return Err(());
            }
            serial = *id;
            let (delta, body) = event.as_ref().ok_or(())?;
            if let Some((sum, region)) = &mut movement {
                if region != body || (*sum > 0) != (*delta > 0) {
                    return Err(());
                }
                *sum += delta;
            } else {
                movement = Some((*delta, body.clone()));
            }
        }
        if serial != self.serial {
            return Err(());
        }
        Ok(movement)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Point {
    pub row: i64,
    pub col: usize,
}
pub(crate) struct TuiCopy {
    pub frame: Frame,
    rows: BTreeMap<i64, Row>,
    pub offset: i64,
    pub cursor: Point,
    anchor: Option<Point>,
    kind: SelectionType,
    body: Option<std::ops::Range<usize>>,
    gutter: usize,
    trusted_nvim: bool,
    limit: usize,
    pub stopped: Option<&'static str>,
}
impl TuiCopy {
    pub fn new(frame: Frame, row: usize, col: usize, limit: usize) -> Self {
        let rows = frame
            .rows
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, r)| (i as i64, r))
            .collect();
        Self {
            frame,
            rows,
            offset: 0,
            cursor: Point {
                row: row as i64,
                col,
            },
            anchor: None,
            kind: SelectionType::Simple,
            body: None,
            gutter: 0,
            trusted_nvim: false,
            limit,
            stopped: None,
        }
    }
    pub fn select(&mut self, kind: SelectionType) {
        if self.anchor.is_some() && self.kind == kind {
            self.anchor = None;
        } else {
            self.kind = kind;
            self.anchor = Some(self.cursor);
        }
    }
    pub fn trust_nvim(&mut self, trusted: bool) {
        self.trusted_nvim = trusted;
    }
    pub fn stop(&mut self, reason: &'static str) {
        self.stopped = Some(reason);
    }
    fn selected_body(&self) -> Option<std::ops::Range<usize>> {
        if let Some(body) = &self.body {
            return Some(body.clone());
        }
        let a = self.anchor.unwrap_or(self.cursor);
        let mut start = a.row.min(self.cursor.row).max(0) as usize;
        let mut end = (a.row.max(self.cursor.row).max(0) as usize + 1).min(self.frame.rows.len());
        // Matching context is separate from the selected text. A point or a
        // one-line selection still needs two retained rows after a scroll.
        if end - start < 3 {
            let max_end = if end < self.frame.rows.len() {
                self.frame.rows.len().saturating_sub(1)
            } else {
                self.frame.rows.len()
            };
            end = (start + 3).min(max_end.max(end));
            start = end.saturating_sub(3);
        }
        Some(start..end)
    }
    pub fn awaiting_paint(&self, frame: &Frame) -> bool {
        frame.scroll.serial != self.frame.scroll.serial && !frame.scroll.pending.is_empty()
    }
    pub fn display_unchanged(&self, frame: &Frame) -> bool {
        if frame.scroll.serial != self.frame.scroll.serial
            || frame.alternate != self.frame.alternate
            || frame.rows.len() != self.frame.rows.len()
        {
            return false;
        }
        self.selected_body().is_some_and(|b| {
            b.into_iter()
                .all(|i| rows_match(&self.frame.rows[i], &frame.rows[i], self.gutter))
        })
    }
    pub fn observe_scroll(&mut self, frame: Frame, down: bool) -> bool {
        if let Some((delta, _, _)) = scroll_match(
            &self.frame,
            &frame,
            self.selected_body().as_ref(),
            self.gutter,
            self.trusted_nvim,
        ) {
            if (delta > 0) != down {
                self.stop("要求と異なる画面移動のためコピー継続を停止しました");
                return false;
            }
        }
        self.observe(frame)
    }
    pub fn observe(&mut self, frame: Frame) -> bool {
        if self.stopped.is_some() {
            return false;
        }
        if self.frame.alternate != frame.alternate || self.frame.rows.len() != frame.rows.len() {
            self.stop("画面切替・サイズ変更でコピー継続を停止しました");
            return false;
        }
        if self.awaiting_paint(&frame) {
            self.stop("本文の描画を待てませんでした。保持済みの範囲はコピーできます");
            return false;
        }
        if self.frame == frame {
            return true;
        }
        if self.anchor.is_none() {
            let (row, col) = self.visible_cursor();
            let trusted_nvim = self.trusted_nvim;
            *self = Self::new(frame, row, col, self.limit);
            self.trusted_nvim = trusted_nvim;
            return true;
        }
        if self.display_unchanged(&frame) {
            self.frame = frame;
            return true;
        }
        let Some((delta, body, gutter)) = scroll_match(
            &self.frame,
            &frame,
            self.selected_body().as_ref(),
            self.gutter,
            self.trusted_nvim,
        ) else {
            self.stop("本文の連続性を確認できません。保持済みの範囲はコピーできます");
            return false;
        };
        if self.body.as_ref().is_some_and(|previous| previous != &body) {
            self.stop("本文領域が変更されたためコピー継続を停止しました");
            return false;
        }
        let offset = self.offset + delta;
        if self.body.is_none()
            && self
                .anchor
                .is_some_and(|p| p.row < body.start as i64 || p.row >= body.end as i64)
        {
            self.stop("本文外の選択からはコピーを継続できません");
            return false;
        }
        let mut rows = self.rows.clone();
        if self.body.is_none() {
            rows.retain(|i, _| *i >= body.start as i64 && *i < body.end as i64);
        }
        for i in body.clone() {
            let index = offset + i as i64;
            if rows
                .get(&index)
                .is_some_and(|r| !rows_match(r, &frame.rows[i], gutter))
            {
                self.stop("保持した本文が変更されたためコピー継続を停止しました");
                return false;
            }
            rows.insert(index, frame.rows[i].clone());
        }
        if rows.len() > self.limit {
            self.stop("コピー履歴の上限です。保持済みの範囲をコピーしてください");
            return false;
        }
        self.rows = rows;
        self.cursor.row = offset + self.visible_cursor().0.clamp(body.start, body.end - 1) as i64;
        self.offset = offset;
        self.body = Some(body);
        self.gutter = gutter;
        self.frame = frame;
        true
    }
    pub fn move_cursor(&mut self, row: usize, col: usize) {
        if self.stopped.is_some() {
            return;
        }
        let body = self.body.clone().unwrap_or(0..self.frame.rows.len());
        let row = row.clamp(body.start, body.end.saturating_sub(1));
        let cells = &self.frame.rows[row].cells;
        let mut col = col.min(cells.len().saturating_sub(1));
        while col > 0 && cells[col].is_empty() {
            col -= 1;
        }
        self.cursor = Point {
            row: self.offset + row as i64,
            col,
        };
    }
    pub fn bounds(&self) -> std::ops::Range<usize> {
        self.body.clone().unwrap_or(0..self.frame.rows.len())
    }
    pub fn visible_cursor(&self) -> (usize, usize) {
        (
            (self.cursor.row - self.offset).max(0) as usize,
            self.cursor.col,
        )
    }
    pub fn selection(&self) -> Option<(Point, Point, bool)> {
        let mut a = self.anchor?;
        let mut b = self.cursor;
        if self.kind == SelectionType::Lines {
            let (lo, hi) = if a.row <= b.row {
                (a.row, b.row)
            } else {
                (b.row, a.row)
            };
            a = Point { row: lo, col: 0 };
            b = Point {
                row: hi,
                col: self.rows.get(&hi)?.cells.len().saturating_sub(1),
            };
        }
        Some((a, b, self.kind == SelectionType::Block))
    }
    pub fn text(&self) -> Option<String> {
        let (a, b, block) = self.selection()?;
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let mut text = String::new();
        for i in start.row..=end.row {
            let r = self.rows.get(&i)?;
            let left = if block {
                a.col.min(b.col)
            } else if i == start.row {
                start.col
            } else {
                0
            }
            .max(self.gutter);
            let right = if block {
                a.col.max(b.col)
            } else if i == end.row {
                end.col
            } else {
                r.cells.len().saturating_sub(1)
            };
            let mut chunk = r
                .cells
                .iter()
                .skip(left)
                .take(right.saturating_sub(left) + 1)
                .cloned()
                .collect::<String>();
            if block || !r.wrapped {
                chunk = chunk.trim_end_matches(' ').to_owned();
            }
            text.push_str(&chunk);
            if i < end.row && (block || !r.wrapped) {
                text.push('\n');
            }
        }
        Some(text)
    }
}

/// Match a unique, contiguous displacement with at least two distinct nonblank rows.
/// Unmatched margins are headers/footers; never manufacture missing content.
fn rows_match(old: &Row, new: &Row, gutter: usize) -> bool {
    old.wrapped == new.wrapped
        && old.cells.len() == new.cells.len()
        && old.cells[gutter.min(old.cells.len())..] == new.cells[gutter.min(new.cells.len())..]
}

fn editor_ruler(frame: &Frame, region: &std::ops::Range<usize>) -> bool {
    let Some(row) = frame.rows.get(region.end) else {
        return false;
    };
    let line = row.cells.concat();
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    tokens.iter().any(|token| {
        token.split_once(',').is_some_and(|(line, col)| {
            !line.is_empty()
                && line.bytes().all(|b| b.is_ascii_digit())
                && !col.is_empty()
                && col.bytes().all(|b| b.is_ascii_digit())
        })
    }) && tokens.iter().any(|token| {
        matches!(*token, "Top" | "Bot" | "All")
            || (token.ends_with('%')
                && token.len() > 1
                && token[..token.len() - 1].bytes().all(|b| b.is_ascii_digit()))
    })
}

fn changing_number_gutter(
    old: &Frame,
    new: &Frame,
    delta: i64,
    region: &std::ops::Range<usize>,
    trusted_nvim: bool,
) -> Option<usize> {
    let start = region.start + (-delta).max(0) as usize;
    let end = region.end - delta.max(0) as usize;
    if end - start < 2
        || !(trusted_nvim || (editor_ruler(old, region) && editor_ruler(new, region)))
    {
        return None;
    }
    let relative_numbers = |frame: &Frame, gutter: usize| {
        let numbers = region.clone().try_fold(Vec::new(), |mut numbers, i| {
            let cells = &frame.rows[i].cells;
            if cells.get(gutter - 1)? != " " {
                return None;
            }
            let prefix = cells[..gutter - 1].concat();
            let prefix = prefix.trim();
            if prefix.is_empty() || (prefix == "~" && cells[gutter..].iter().all(|c| c == " ")) {
                return Some(numbers);
            }
            numbers.push(prefix.parse::<usize>().ok()?);
            Some(numbers)
        });
        numbers.is_some_and(|numbers| {
            numbers.len() >= 2
                && (0..numbers.len()).any(|cursor| {
                    numbers
                        .iter()
                        .enumerate()
                        .all(|(i, &n)| i == cursor || n == i.abs_diff(cursor))
                })
        })
    };
    let width = old.rows[region.start].cells.len().min(32);
    (2..=width).find(|&gutter| {
        relative_numbers(old, gutter)
            && relative_numbers(new, gutter)
            && (start..end).all(|j| {
                let a = &old.rows[(j as i64 + delta) as usize];
                let b = &new.rows[j];
                [a, b].into_iter().all(|r| {
                    r.cells.get(gutter - 1).is_some_and(|c| c == " ")
                        && (r.cells[..gutter - 1]
                            .iter()
                            .all(|cell| cell.chars().all(|c| c.is_ascii_digit() || c == ' '))
                            || (r.cells[..gutter - 1].concat().trim() == "~"
                                && r.cells[gutter..].iter().all(|c| c == " ")))
                }) && rows_match(a, b, gutter)
            })
    })
}

fn scroll_match(
    old: &Frame,
    new: &Frame,
    body: Option<&std::ops::Range<usize>>,
    gutter: usize,
    trusted_nvim: bool,
) -> Option<(i64, std::ops::Range<usize>, usize)> {
    // An explicit rotation gives both the complete body and the displacement.
    // Validate every retained row; repeated/blank lines no longer make it ambiguous.
    if let Some((delta, region)) = new.scroll.since(&old.scroll).ok()? {
        if region.end > old.rows.len()
            || region.end > new.rows.len()
            || delta == 0
            || delta.unsigned_abs() as usize >= region.len()
        {
            return None;
        }
        let matches = |range: &std::ops::Range<usize>| {
            let start = range.start + (-delta).max(0) as usize;
            let end = range.end - delta.max(0) as usize;
            (start..end)
                .all(|j| rows_match(&old.rows[(j as i64 + delta) as usize], &new.rows[j], gutter))
        };
        if matches(&region) {
            return Some((delta, region, gutter));
        }
        if gutter == 0 {
            if let Some(found) = changing_number_gutter(old, new, delta, &region, trusted_nvim) {
                return Some((delta, region, found));
            }
        }
        // less scrolls the whole screen, then repaints its prompt. Preserve the
        // already selected body when that fixed margin is the only mismatch.
        let body = body?;
        if body.start >= region.start
            && body.end <= region.end
            && body.len() > delta.unsigned_abs() as usize + 1
            && matches(body)
        {
            return Some((delta, body.clone(), gutter));
        }
        return None;
    }
    let body = body?;
    let h = body.len();
    let mut candidates = Vec::new();
    for delta in -(h as i64 - 1)..h as i64 {
        if delta == 0 {
            continue;
        }
        let new_start = body.start + (-delta).max(0) as usize;
        let new_end = body.end - delta.max(0) as usize;
        if new_end - new_start < 2 {
            continue;
        }
        let matched = (new_start..new_end)
            .all(|j| rows_match(&old.rows[(j as i64 + delta) as usize], &new.rows[j], gutter));
        if !matched {
            continue;
        }
        let overlap = &new.rows[new_start..new_end];
        let distinct: std::collections::BTreeSet<_> = overlap
            .iter()
            .map(|r| r.cells.concat())
            .filter(|s| !s.trim().is_empty())
            .collect();
        if distinct.len() < 2 {
            continue;
        }
        // A retained row duplicated at the newly exposed edge is also a common
        // partial repaint. Decline both interpretations instead of guessing.
        let exposed = if delta > 0 {
            new_end..body.end
        } else {
            body.start..new_start
        };
        if new.rows[exposed].iter().any(|r| overlap.contains(r)) {
            continue;
        }
        candidates.push((new_end - new_start, delta, body.clone()));
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.0));
    let best = candidates.first()?;
    if candidates.get(1).is_some_and(|c| c.0 == best.0) {
        return None;
    }
    Some((best.1, best.2.clone(), gutter))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(rows: &[&str]) -> Frame {
        Frame {
            rows: rows
                .iter()
                .map(|r| Row {
                    cells: r.chars().map(|c| c.to_string()).collect(),
                    wrapped: false,
                })
                .collect(),
            alternate: true,
            scroll: ScrollTrace::default(),
        }
    }
    #[test]
    fn explicit_scroll_from_one_line_preserves_repeated_code_and_reverse() {
        let old = frame(&["fn a() {", "}", "", "fn b() {", "}", "", "status"]);
        let mut c = TuiCopy::new(old.clone(), 0, 0, 100);
        c.select(SelectionType::Lines);
        let mut next = frame(&["fn b() {", "}", "", "fn c() {", "}", "", "status2"]);
        next.scroll.push(Some((3, 0..6)));
        assert!(c.observe_scroll(next.clone(), true), "{:?}", c.stopped);
        assert_eq!(c.text().as_deref(), Some("fn a() {\n}\n\nfn b() {"));
        let mut back = old;
        back.scroll = next.scroll;
        back.scroll.push(Some((-3, 0..6)));
        assert!(c.observe_scroll(back, false), "{:?}", c.stopped);
        assert_eq!(c.text().as_deref(), Some("fn a() {"));
    }
    #[test]
    fn explicit_scroll_moves_even_when_all_rows_are_equal() {
        let mut c = TuiCopy::new(frame(&["}", "}", "}", "}", "}", "}", ":"]), 0, 0, 100);
        c.select(SelectionType::Lines);
        let mut next = c.frame.clone();
        next.scroll.push(Some((3, 0..6)));
        assert!(!c.display_unchanged(&next));
        assert!(c.observe_scroll(next, true));
        assert_eq!(c.text().as_deref(), Some("}\n}\n}\n}"));
    }
    #[test]
    fn one_numeric_content_edit_does_not_look_like_a_line_number_gutter() {
        let mut c = TuiCopy::new(
            frame(&[
                "1 alpha",
                "2 beta ",
                "3 gamma",
                "4 delta",
                "5 echo ",
                "6 foxtrot",
                "7 golf ",
                "8 hotel",
                ":",
            ]),
            0,
            0,
            100,
        );
        c.select(SelectionType::Lines);
        let mut next = frame(&[
            "2 beta ",
            "9 gamma",
            "4 delta",
            "5 echo ",
            "6 foxtrot",
            "7 golf ",
            "8 hotel",
            "9 india",
            ":",
        ]);
        next.scroll.push(Some((1, 0..8)));
        assert!(!c.observe_scroll(next, true));
        assert_eq!(c.text().as_deref(), Some("1 alpha"));
    }
    #[test]
    fn bulk_numeric_content_edits_are_not_discarded_as_line_numbers() {
        let mut c = TuiCopy::new(
            frame(&[
                "1 alpha",
                "2 beta",
                "3 gamma",
                "4 delta",
                "5 echo",
                "6 foxtrot",
                "7 golf",
                "8 hotel",
                ":",
            ]),
            0,
            0,
            100,
        );
        c.select(SelectionType::Lines);
        let mut next = frame(&[
            "9 beta",
            "8 gamma",
            "7 delta",
            "6 echo",
            "5 foxtrot",
            "4 golf",
            "3 hotel",
            "2 india",
            ":",
        ]);
        next.scroll.push(Some((1, 0..8)));
        assert!(!c.observe_scroll(next, true));
        assert_eq!(c.text().as_deref(), Some("1 alpha"));
    }
    #[test]
    fn relative_looking_numeric_body_without_editor_status_is_not_a_gutter() {
        let mut c = TuiCopy::new(frame(&["1 a", "1 b", "2 c", "3 d", "4 e", ":"]), 0, 0, 100);
        c.select(SelectionType::Lines);
        let mut next = frame(&["1 b", "1 c", "2 d", "3 e", "4 f", ":"]);
        next.scroll.push(Some((1, 0..5)));
        assert!(!c.observe_scroll(next, true));
        assert_eq!(c.text().as_deref(), Some("1 a"));
    }
    #[test]
    fn missing_or_invalid_scroll_trace_never_falls_back_to_text_guessing() {
        for invalid in [true, false] {
            let mut c = TuiCopy::new(frame(&["A", "B", "C", "D", "E", ":"]), 0, 0, 100);
            c.select(SelectionType::Lines);
            let mut next = frame(&["B", "C", "D", "E", "F", ":"]);
            if invalid {
                next.scroll.push(None);
            } else {
                for _ in 0..65 {
                    next.scroll.push(Some((1, 0..5)));
                }
            }
            assert!(!c.observe_scroll(next, true));
            assert_eq!(c.text().as_deref(), Some("A"));
        }
    }
    #[test]
    fn one_line_selection_uses_context_and_can_extend_with_wheel() {
        let mut c = TuiCopy::new(frame(&["A", "B", "C", "D", "E", ":"]), 0, 0, 100);
        c.select(SelectionType::Lines);
        assert!(c.observe_scroll(frame(&["B", "C", "D", "E", "F", ":"]), true));
        assert_eq!(c.text().as_deref(), Some("A\nB"));
    }
    #[test]
    fn footer_change_is_not_a_body_scroll() {
        let mut c = TuiCopy::new(frame(&["A", "B", "C", "D", "E", "ruler1"]), 0, 0, 100);
        c.select(SelectionType::Lines);
        let next = frame(&["A", "B", "C", "D", "E", "ruler2"]);
        assert!(c.display_unchanged(&next));
        assert!(c.observe_scroll(next, true));
        assert_eq!(c.offset, 0);
        assert!(c.stopped.is_none());
    }
    #[test]
    fn partial_redraw_never_becomes_new_body_content() {
        let mut c = TuiCopy::new(
            frame(&["header", "A", "B", "C", "D", "E", "status"]),
            1,
            0,
            100,
        );
        c.select(SelectionType::Lines);
        c.move_cursor(4, 0);
        let text = c.text();
        assert!(!c.observe_scroll(frame(&["header", "B", "C", "D", "D", "E", "status"]), true));
        assert!(c.stopped.is_some());
        assert_eq!(c.text(), text);
    }
    #[test]
    fn wide_spacer_cursor_selects_complete_glyph() {
        let f = Frame {
            rows: vec![Row {
                cells: vec!["日".into(), "".into(), "本".into(), "".into()],
                wrapped: false,
            }],
            alternate: true,
            scroll: ScrollTrace::default(),
        };
        let mut c = TuiCopy::new(f, 0, 0, 10);
        c.move_cursor(0, 1);
        c.select(SelectionType::Simple);
        assert_eq!(c.text().as_deref(), Some("日"));
    }
    #[test]
    fn request_direction_cannot_be_reversed_by_unrelated_redraw() {
        let mut c = TuiCopy::new(frame(&["one", "two", "three", ":"]), 0, 0, 100);
        c.select(SelectionType::Lines);
        assert!(!c.observe_scroll(frame(&["two", "three", "four", ":"]), false));
        assert_eq!(c.offset, 0);
        assert_eq!(c.text().as_deref(), Some("one"));
    }
    #[test]
    fn forward_scroll_preserves_anchor_and_excludes_fixed_status() {
        let mut c = TuiCopy::new(
            frame(&["header", "one", "two", "three", "status"]),
            1,
            0,
            100,
        );
        c.select(SelectionType::Lines);
        c.cursor.row = 3;
        assert!(c.observe(frame(&["header", "two", "three", "four", "status2"])));
        assert_eq!(c.offset, 1);
        assert_eq!(c.text().as_deref(), Some("one\ntwo\nthree\nfour"));
    }
    #[test]
    fn reverse_scroll_reuses_rows_without_duplication() {
        let a = frame(&["one", "two", "three", ":"]);
        let b = frame(&["two", "three", "four", ":"]);
        let mut c = TuiCopy::new(a.clone(), 0, 0, 100);
        c.select(SelectionType::Lines);
        c.cursor.row = 2;
        assert!(c.observe(b));
        assert!(c.observe(a));
        assert_eq!(c.offset, 0);
        assert_eq!(c.text().as_deref(), Some("one\ntwo\nthree"));
    }
    #[test]
    fn repeated_rows_and_full_redraw_stop_without_losing_text() {
        for (old, new) in [
            (
                frame(&["same", "same", "same"]),
                frame(&["same", "different", "different"]),
            ),
            (
                frame(&["one", "two", "three"]),
                frame(&["four", "five", "six"]),
            ),
        ] {
            let mut c = TuiCopy::new(old, 0, 0, 100);
            c.select(SelectionType::Lines);
            c.cursor.row = 1;
            let text = c.text();
            assert!(!c.observe(new));
            assert!(c.stopped.is_some());
            assert_eq!(c.text(), text);
        }
    }
    #[test]
    fn unchanged_frame_is_safe_and_boundary_changes_stop() {
        let f = frame(&["one", "two", "three"]);
        let mut c = TuiCopy::new(f.clone(), 0, 0, 100);
        assert!(c.observe(f.clone()));
        assert_eq!(c.offset, 0);
        let mut changed = f;
        changed.alternate = false;
        assert!(!c.observe(changed));
        assert!(c.stopped.is_some());
    }
    #[test]
    fn history_limit_stops_before_selected_rows_are_lost() {
        let mut c = TuiCopy::new(frame(&["one", "two", "three", ":"]), 0, 0, 3);
        c.select(SelectionType::Lines);
        c.move_cursor(2, 0);
        assert!(!c.observe(frame(&["two", "three", "four", ":"])));
        assert!(c.stopped.is_some());
        assert_eq!(c.text().as_deref(), Some("one\ntwo\nthree"));
    }
    #[test]
    fn wide_cells_wrapping_and_block_selection() {
        let f = Frame {
            rows: vec![
                Row {
                    cells: vec!["日".into(), "".into(), "本".into(), "".into()],
                    wrapped: true,
                },
                Row {
                    cells: vec!["語".into(), "".into(), "!".into(), " ".into()],
                    wrapped: false,
                },
            ],
            alternate: true,
            scroll: ScrollTrace::default(),
        };
        let mut c = TuiCopy::new(f, 0, 0, 100);
        c.select(SelectionType::Simple);
        c.cursor = Point { row: 1, col: 2 };
        assert_eq!(c.text().as_deref(), Some("日本語!"));
        c.select(SelectionType::Block);
        c.cursor = Point { row: 0, col: 0 };
        assert_eq!(c.text().as_deref(), Some("日本\n語!"));
    }
}
