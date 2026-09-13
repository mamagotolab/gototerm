use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthChar;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::launcher::column_cells;
use crate::task_activity::{ActivityKind, PaneActivity, PaneId};
use crate::terminal::{Color, Line};
use crate::view::{TerminalView, Viewport};
use crate::Display;

#[derive(Clone, Debug)]
pub(crate) struct TaskRow {
    pub(crate) id: PaneId,
    pub(crate) tab_number: usize,
    pub(crate) pane_number: usize,
    pub(crate) location: Option<String>,
    pub(crate) activity: PaneActivity,
}

pub(crate) struct OverviewState {
    rows: Vec<TaskRow>,
    selected: Option<PaneId>,
    notice: Option<String>,
}

impl OverviewState {
    pub(crate) fn open(mut rows: Vec<TaskRow>, current: PaneId) -> Self {
        rows.sort_by_key(|row| row.activity.kind != ActivityKind::Blocked);
        let selected = rows
            .iter()
            .any(|row| row.id == current)
            .then_some(current)
            .or_else(|| rows.first().map(|row| row.id));
        Self {
            rows,
            selected,
            notice: None,
        }
    }

    pub(crate) fn reconcile(&mut self, rows: Vec<TaskRow>) {
        let old_index = self.selected_index().unwrap_or(0);
        let incoming_ids: HashSet<_> = rows.iter().map(|row| row.id).collect();
        let mut replacements: HashMap<_, _> =
            rows.iter().cloned().map(|row| (row.id, row)).collect();
        self.rows.retain(|row| incoming_ids.contains(&row.id));
        for row in &mut self.rows {
            if let Some(replacement) = replacements.remove(&row.id) {
                *row = replacement;
            }
        }
        for row in rows {
            if let Some(new_row) = replacements.remove(&row.id) {
                self.rows.push(new_row);
            }
        }

        if self.rows.is_empty() {
            self.selected = None;
        } else if !self
            .selected
            .is_some_and(|id| self.rows.iter().any(|row| row.id == id))
        {
            self.selected = Some(self.rows[old_index.min(self.rows.len() - 1)].id);
        }
    }

    pub(crate) fn selected_id(&self) -> Option<PaneId> {
        self.selected
    }

    fn set_notice(&mut self, notice: String) {
        self.notice = Some(notice);
    }

    pub(crate) fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.selected = None;
            return;
        }
        let current = self.selected_index().unwrap_or(0);
        let next = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current
                .saturating_add(delta as usize)
                .min(self.rows.len() - 1)
        };
        self.selected = Some(self.rows[next].id);
    }

    pub(crate) fn move_page(&mut self, page_rows: usize) {
        self.move_by(page_rows.max(1) as isize);
    }

    pub(crate) fn move_page_up(&mut self, page_rows: usize) {
        self.move_by(-(page_rows.max(1) as isize));
    }

    pub(crate) fn select_edge(&mut self, last: bool) {
        self.selected = if last {
            self.rows.last().map(|row| row.id)
        } else {
            self.rows.first().map(|row| row.id)
        };
    }

    fn selected_index(&self) -> Option<usize> {
        let id = self.selected?;
        self.rows.iter().position(|row| row.id == id)
    }

    pub(crate) fn first_visible(&self, visible_rows: usize) -> usize {
        if visible_rows == 0 {
            return 0;
        }
        self.selected_index()
            .map(|index| index.saturating_add(1).saturating_sub(visible_rows))
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn row_ids(&self) -> Vec<PaneId> {
        self.rows.iter().map(|row| row.id).collect()
    }
}

pub(crate) enum OverviewOutcome {
    None,
    Dismissed,
    Activate(PaneId),
}

pub(crate) struct TaskOverview {
    view: TerminalView,
    state: OverviewState,
}

impl TaskOverview {
    pub(crate) fn new(
        display: Display,
        viewport: Viewport,
        scale_factor: f64,
        rows: Vec<TaskRow>,
        current: PaneId,
    ) -> Self {
        let mut overview = Self {
            view: TerminalView::with_viewport(
                display,
                viewport,
                crate::TOYTERM_CONFIG.font_size,
                scale_factor,
                None,
            ),
            state: OverviewState::open(rows, current),
        };
        overview.rebuild(Instant::now());
        overview
    }

    pub(crate) fn update_rows(&mut self, rows: Vec<TaskRow>, now: Instant) {
        self.state.reconcile(rows);
        self.rebuild(now);
    }

    pub(crate) fn set_notice(&mut self, notice: impl Into<String>) {
        self.state.set_notice(notice.into());
        self.rebuild(Instant::now());
    }

    pub(crate) fn set_viewport(&mut self, viewport: Viewport) {
        self.view.set_viewport(viewport);
        self.rebuild(Instant::now());
    }

    pub(crate) fn set_scale_factor(&mut self, scale_factor: f64) -> bool {
        let changed = self.view.set_scale_factor(scale_factor);
        if changed {
            self.rebuild(Instant::now());
        }
        changed
    }

    pub(crate) fn change_font_size(&mut self, diff: i32) {
        if self.view.increase_font_size(diff) {
            self.rebuild(Instant::now());
        }
    }

    pub(crate) fn draw(&mut self, surface: &mut glium::Frame) {
        self.view.draw(surface);
    }

    pub(crate) fn needs_redraw(&self) -> bool {
        self.view.needs_redraw()
    }

    pub(crate) fn handle_key(&mut self, key: &KeyEvent) -> OverviewOutcome {
        if key.state != ElementState::Pressed || key.repeat {
            return OverviewOutcome::None;
        }
        let code = match key.physical_key {
            PhysicalKey::Code(code) => code,
            PhysicalKey::Unidentified(_) => return OverviewOutcome::None,
        };
        let page = row_slots((self.view.viewport().h / self.view.cell_size().h.max(1)) as usize);
        match code {
            KeyCode::Escape => return OverviewOutcome::Dismissed,
            KeyCode::Enter => {
                return self
                    .state
                    .selected_id()
                    .map_or(OverviewOutcome::None, OverviewOutcome::Activate)
            }
            KeyCode::ArrowUp => self.state.move_by(-1),
            KeyCode::ArrowDown => self.state.move_by(1),
            KeyCode::PageUp => self.state.move_page_up(page),
            KeyCode::PageDown => self.state.move_page(page),
            KeyCode::Home => self.state.select_edge(false),
            KeyCode::End => self.state.select_edge(true),
            _ => return OverviewOutcome::None,
        }
        self.rebuild(Instant::now());
        OverviewOutcome::None
    }

    fn rebuild(&mut self, now: Instant) {
        let cols = (self.view.viewport().w / self.view.cell_size().w.max(1)) as usize;
        let rows = (self.view.viewport().h / self.view.cell_size().h.max(1)) as usize;
        let lines = render_overview(&self.state, cols, rows, now);
        self.view.update_contents(|view| {
            view.bg_color = Color::Background;
            view.lines = lines;
            view.images = Vec::new();
            view.cursor = None;
            view.selection_range = None;
            view.scroll_bar = None;
            view.view_focused = true;
        });
    }
}

fn row_slots(rows: usize) -> usize {
    rows.saturating_sub(if rows >= 4 {
        2
    } else if rows >= 3 {
        1
    } else {
        0
    })
}

pub(crate) fn render_overview(
    state: &OverviewState,
    cols: usize,
    rows: usize,
    now: Instant,
) -> Vec<Line> {
    if cols == 0 || rows == 0 {
        return Vec::new();
    }
    let show_header = rows >= 3;
    let show_detail = rows >= 4;
    let list_rows = row_slots(rows);
    let mut lines = Vec::with_capacity(rows);
    if show_header {
        lines.push(line(
            "作業状態の一覧  —  最後に受け取った状態",
            Color::BrightWhite,
            Color::Background,
            cols,
        ));
    }

    let start = state.first_visible(list_rows);
    for row in state.rows.iter().skip(start).take(list_rows) {
        let selected = state.selected_id() == Some(row.id);
        let text = format_row(row, now, cols, selected);
        lines.push(line(
            &text,
            if selected { Color::Black } else { Color::White },
            if selected {
                Color::BrightBlue
            } else {
                Color::Background
            },
            cols,
        ));
    }
    while lines.len() < rows.saturating_sub(usize::from(show_detail)) {
        lines.push(line("", Color::White, Color::Background, cols));
    }

    if show_detail {
        let detail = state.notice.clone().unwrap_or_else(|| {
            state
                .selected_index()
                .and_then(|index| state.rows.get(index))
                .map(|row| {
                    let location = row.location.as_deref().unwrap_or("場所不明");
                    match row.activity.detail.as_deref() {
                        Some(detail) => format!("場所: {location}  補足: {detail}"),
                        None => format!("場所: {location}"),
                    }
                })
                .unwrap_or_else(|| "作業はありません".to_owned())
        });
        lines.push(line(&detail, Color::BrightBlack, Color::Background, cols));
    }
    lines.truncate(rows);
    lines
}

fn format_row(row: &TaskRow, now: Instant, cols: usize, selected: bool) -> String {
    let marker = if selected { ">" } else { " " };
    let location = short_location(row.location.as_deref()).unwrap_or_else(|| "場所不明".to_owned());
    let pane = format!("T{} / P{}", row.tab_number, row.pane_number);
    let state = activity_label(row.activity.kind);
    let agent = row.activity.agent.as_deref().unwrap_or("—");
    let elapsed = row
        .activity
        .received_at
        .map(|at| format_elapsed(now.saturating_duration_since(at)))
        .unwrap_or_else(|| "—".to_owned());
    let prefix = 2usize; // 選択マーカーと空白
    let state_w = cell_width(&state);
    let elapsed_w = cell_width(&elapsed);
    let pane_w = cell_width(&pane);
    let sep_w = 2usize;
    let mut remaining = cols.saturating_sub(prefix + state_w + sep_w);
    let show_elapsed = cols >= 60;
    if show_elapsed {
        remaining = remaining.saturating_sub(elapsed_w + sep_w);
    }
    let show_agent = cols >= 60;
    if show_agent {
        remaining = remaining.saturating_sub(sep_w + 8);
    }
    let pane_take = pane_w.min(remaining.saturating_sub(sep_w));
    remaining = remaining.saturating_sub(pane_take + sep_w);
    let location_take = remaining;
    let location = fit_cells(&location, location_take);
    let pane = fit_cells(&pane, pane_take);
    let agent = fit_cells(agent, 8);
    let mut parts = vec![location, pane];
    if show_agent { parts.push(agent); }
    parts.push(state.to_owned());
    if show_elapsed { parts.push(elapsed); }
    format!("{marker} {}", parts.join("  "))
}

fn cell_width(text: &str) -> usize {
    text.chars().map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0)).sum()
}

fn fit_cells(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w == 0 {
            out.push(ch);
            continue;
        }
        if used + w > width { break; }
        out.push(ch);
        used += w;
    }
    out
}

fn short_location(location: Option<&str>) -> Option<String> {
    let location = location?;
    let trimmed = location.trim_end_matches(['/', '\\']);
    let name = trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())?;
    if let Some((host, path)) = trimmed.split_once(":/") {
        if !host.is_empty() && !path.is_empty() {
            return Some(format!("{host}:{name}"));
        }
    }
    Some(name.to_owned())
}

fn activity_label(kind: ActivityKind) -> &'static str {
    match kind {
        ActivityKind::NoSignal => "状態通知なし",
        ActivityKind::SessionStarted => "セッション開始",
        ActivityKind::FileChanged => "ファイル更新",
        ActivityKind::Blocked => "入力・許可待ち",
        ActivityKind::ResponseEnded => "応答終了",
        ActivityKind::SessionEnded => "セッション終了",
    }
}

pub(crate) fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match seconds {
        0 => "いま".to_owned(),
        1..=59 => format!("{seconds}秒前"),
        60..=3599 => format!("{}分前", seconds / 60),
        _ => format!("{}時間前", seconds / 3600),
    }
}

fn line(text: &str, fg: Color, bg: Color, cols: usize) -> Line {
    Line::from_cells(column_cells(text, fg, bg, cols), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_activity::{ActivityKind, PaneActivity, PaneId};
    use std::time::{Duration, Instant};

    fn row(id: u64, kind: ActivityKind) -> TaskRow {
        TaskRow {
            id: PaneId(id),
            tab_number: id as usize,
            pane_number: id as usize,
            location: None,
            activity: PaneActivity {
                kind,
                ..PaneActivity::default()
            },
        }
    }

    #[test]
    fn open_prioritizes_blocked_but_selects_current_identity() {
        let list = OverviewState::open(
            vec![
                row(1, ActivityKind::NoSignal),
                row(2, ActivityKind::Blocked),
                row(3, ActivityKind::ResponseEnded),
                row(4, ActivityKind::Blocked),
            ],
            PaneId(3),
        );
        assert_eq!(
            list.row_ids(),
            vec![PaneId(2), PaneId(4), PaneId(1), PaneId(3)]
        );
        assert_eq!(list.selected_id(), Some(PaneId(3)));
    }

    #[test]
    fn updates_keep_selected_identity_and_open_order() {
        let mut list = OverviewState::open(
            vec![
                row(1, ActivityKind::NoSignal),
                row(2, ActivityKind::Blocked),
            ],
            PaneId(1),
        );
        assert_eq!(list.selected_id(), Some(PaneId(1)));
        list.reconcile(vec![
            row(1, ActivityKind::Blocked),
            row(2, ActivityKind::ResponseEnded),
        ]);
        list.select_edge(false);
        assert_eq!(list.selected_id(), Some(PaneId(2)));
    }

    #[test]
    fn reconcile_removes_missing_rows_and_appends_new_rows() {
        let mut list = OverviewState::open(
            vec![
                row(1, ActivityKind::NoSignal),
                row(2, ActivityKind::NoSignal),
            ],
            PaneId(1),
        );
        list.reconcile(vec![
            row(2, ActivityKind::Blocked),
            row(3, ActivityKind::Blocked),
        ]);
        assert_eq!(list.row_ids(), vec![PaneId(2), PaneId(3)]);
        assert_eq!(list.selected_id(), Some(PaneId(2)));
    }

    #[test]
    fn selection_clamps_at_edges_and_survives_empty_list() {
        let mut list = OverviewState::open(vec![], PaneId(1));
        list.move_by(4);
        list.select_edge(true);
        assert_eq!(list.selected_id(), None);

        list.reconcile(vec![
            row(7, ActivityKind::NoSignal),
            row(8, ActivityKind::NoSignal),
        ]);
        list.move_by(-5);
        assert_eq!(list.selected_id(), Some(PaneId(7)));
        list.move_by(9);
        assert_eq!(list.selected_id(), Some(PaneId(8)));
    }

    #[test]
    fn page_movement_keeps_selection_visible() {
        let mut list = OverviewState::open(
            (1..=10).map(|id| row(id, ActivityKind::NoSignal)).collect(),
            PaneId(1),
        );
        list.move_page(4);
        assert_eq!(list.selected_id(), Some(PaneId(5)));
        assert_eq!(list.first_visible(4), 1);
        list.select_edge(true);
        assert_eq!(list.first_visible(4), 6);
    }

    fn line_len(line: &Line) -> usize {
        let mut line = line.clone();
        line.cells_mut().len()
    }

    fn line_text(line: &Line) -> String {
        let mut line = line.clone();
        line.cells_mut()
            .iter()
            .filter(|cell| cell.width > 0)
            .map(|cell| cell.ch)
            .collect()
    }

    #[test]
    fn rendering_stays_within_every_small_viewport() {
        let now = Instant::now();
        let mut activity = PaneActivity::default();
        activity.kind = ActivityKind::Blocked;
        activity.agent = Some("a".repeat(100));
        activity.detail = Some("確認が必要です".repeat(80));
        activity.received_at = Some(now - Duration::from_secs(65));
        let state = OverviewState::open(
            vec![TaskRow {
                id: PaneId(1),
                tab_number: 1,
                pane_number: 1,
                location: Some("/長い/日本語/パス/プロジェクト".into()),
                activity,
            }],
            PaneId(1),
        );
        for cols in [0, 1, 20, 80] {
            for rows in [0, 1, 3, 24] {
                let lines = render_overview(&state, cols, rows, now);
                assert!(lines.len() <= rows);
                assert!(lines.iter().all(|line| line_len(line) <= cols));
            }
        }
    }

    #[test]
    fn rendering_labels_last_received_state_and_full_detail() {
        let now = Instant::now();
        let mut activity = PaneActivity::default();
        activity.kind = ActivityKind::ResponseEnded;
        activity.agent = Some("codex".into());
        activity.detail = Some("補足".into());
        activity.received_at = Some(now);
        let state = OverviewState::open(
            vec![TaskRow {
                id: PaneId(1),
                tab_number: 2,
                pane_number: 3,
                location: Some("/work/完全な場所".into()),
                activity,
            }],
            PaneId(1),
        );
        let text = render_overview(&state, 100, 6, now)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("最後に受け取った状態"));
        assert!(text.contains("応答終了"));
        assert!(text.contains("/work/完全な場所"));
        assert!(text.contains("補足"));
    }

    #[test]
    fn long_location_keeps_state_visible_at_forty_cells() {
        let state = OverviewState::open(
            vec![TaskRow {
                id: PaneId(1),
                tab_number: 1,
                pane_number: 1,
                location: Some("a".repeat(38)),
                activity: PaneActivity {
                    kind: ActivityKind::Blocked,
                    ..PaneActivity::default()
                },
            }],
            PaneId(1),
        );
        let rendered = line_text(&render_overview(&state, 40, 4, Instant::now())[1]);
        assert!(rendered.contains("入力・許可待ち"));
        let line = &render_overview(&state, 40, 4, Instant::now())[1];
        assert!(line_len(line) <= 40);
    }

    #[test]
    fn combining_name_and_following_state_are_both_rendered() {
        let name = "e\u{301}か\u{3099}";
        assert_eq!(fit_cells(name, 3), name);

        let state = OverviewState::open(
            vec![TaskRow {
                id: PaneId(1),
                tab_number: 1,
                pane_number: 1,
                location: Some(name.to_owned()),
                activity: PaneActivity {
                    kind: ActivityKind::Blocked,
                    ..PaneActivity::default()
                },
            }],
            PaneId(1),
        );
        let rendered = line_text(&render_overview(&state, 40, 4, Instant::now())[1]);
        assert!(rendered.contains("éが"));
        assert!(rendered.contains("入力・許可待ち"));
    }

    #[test]
    fn only_selected_row_has_a_text_marker() {
        let state = OverviewState::open(
            vec![
                row(1, ActivityKind::NoSignal),
                row(2, ActivityKind::NoSignal),
            ],
            PaneId(1),
        );
        let lines = render_overview(&state, 60, 4, Instant::now());
        assert!(line_text(&lines[1]).starts_with('>'));
        assert!(!line_text(&lines[2]).starts_with('>'));
    }

    #[test]
    fn remote_location_keeps_host_in_the_list_row() {
        let mut remote = row(1, ActivityKind::NoSignal);
        remote.location = Some("build-host:/work/project".into());
        let state = OverviewState::open(vec![remote], PaneId(1));
        let lines = render_overview(&state, 80, 4, Instant::now());
        assert!(line_text(&lines[1]).contains("build-host:project"));
    }

    #[test]
    fn elapsed_time_uses_stable_second_minute_and_hour_boundaries() {
        assert_eq!(format_elapsed(Duration::ZERO), "いま");
        assert_eq!(format_elapsed(Duration::from_secs(1)), "1秒前");
        assert_eq!(format_elapsed(Duration::from_secs(59)), "59秒前");
        assert_eq!(format_elapsed(Duration::from_secs(60)), "1分前");
        assert_eq!(format_elapsed(Duration::from_secs(3599)), "59分前");
        assert_eq!(format_elapsed(Duration::from_secs(3600)), "1時間前");
    }
}
