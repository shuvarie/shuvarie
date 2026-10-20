//! The `/peak-table` popup: the active model's peak/off-peak windows as a
//! table.
//!
//! The catalog states the windows as times of day in the schedule's own
//! timezone (say `09:00–12:00` in `Asia/Shanghai`), which is rarely where the
//! user sits — the zone is an internal billing detail. The table therefore
//! renders its time columns in one of three clocks and `t` cycles them: the
//! schedule's zone (what the catalog literally says), UTC, or the machine's
//! local time. Every switch re-renders the same windows, resolved to instants
//! once when the popup opens, so a DST shift can never make two rows disagree
//! about which day the window belongs to.
//!
//! The schedule is static, so the table is a snapshot: it does not follow the
//! wall clock while open (the sidebar's countdown does that).

use std::time::SystemTime;

use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use selune::{PeakExemption, Tz};
use shuvarie_core::peaks::{PeakSchedule, PeakWindow};
use termina::event::{KeyCode, KeyEvent};

use super::{popup, theme};
use crate::tui::utils::ctrl;
use crate::tui::utils::time::fmt_relative;

/// Columns the table gives each time field, before the span. The end column is
/// wide enough for the `+1` day marker (`01:00 +1`), so a window that runs
/// past local midnight stays in column.
const START_COLS: usize = 8;
const END_COLS: usize = 10;
/// The popup's width: 4 columns of block padding plus the widest row the
/// schedule can make, so the table never wraps.
const POPUP_COLS: u16 = 44;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeakTableMessage {
    /// Cycle the clock the time columns are rendered in.
    CycleZone,
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeakTableEffect {
    Close,
}

/// The clock the table's time columns are rendered against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeakZone {
    /// The timezone the catalog states the windows in.
    Schedule,
    /// UTC.
    Utc,
    /// The machine's local timezone.
    Local,
}

impl PeakZone {
    /// The next clock a `t` press switches to.
    fn next(self) -> Self {
        match self {
            Self::Schedule => Self::Utc,
            Self::Utc => Self::Local,
            Self::Local => Self::Schedule,
        }
    }
}

/// One rendered row: the window's opening and closing wall-clock times in the
/// selected zone, and how long it lasts. A `+1` suffix marks a close that
/// falls on the following day (a window that crosses local midnight).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    start: String,
    end: String,
    span: String,
}

impl Row {
    fn line(&self) -> Line<'static> {
        Line::from(vec![
            Span::raw(format!("{:<START_COLS$}", self.start)).fg(theme::text()),
            Span::raw(format!("{:<END_COLS$}", self.end)).fg(theme::text()),
            Span::raw(self.span.clone()).fg(theme::text_dim()),
        ])
    }
}

/// The `/peak-table` popup: one row per peak window of the day the popup
/// opened on, plus the exempt day classes the schedule skips.
pub struct PeakTablePopup {
    pub open: bool,
    schedule: Option<PeakSchedule>,
    /// The windows resolved when the popup opened, in declared order.
    windows: Vec<PeakWindow>,
    /// The clock the rows are rendered in; kept across opens, so reopening the
    /// table lands on the clock the user last picked.
    zone: PeakZone,
    /// `windows` rendered for `zone`.
    rows: Vec<Row>,
}

impl PeakTablePopup {
    pub fn new() -> Self {
        Self {
            open: false,
            schedule: None,
            windows: Vec::new(),
            zone: PeakZone::Schedule,
            rows: Vec::new(),
        }
    }

    /// Open on a snapshot of `schedule`: its windows for the current civil day
    /// in the schedule's timezone.
    pub fn open(&mut self, schedule: PeakSchedule) {
        self.open = true;
        self.windows = schedule.windows(SystemTime::now());
        self.schedule = Some(schedule);
        self.rebuild_rows();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.schedule = None;
        self.windows.clear();
        self.rows.clear();
    }

    /// The clock the table is currently rendered in.
    #[cfg(test)]
    pub fn zone(&self) -> PeakZone {
        self.zone
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<PeakTableMessage> {
        if ctrl(key) {
            return None;
        }
        match key.code {
            KeyCode::Escape | KeyCode::Char('q') => Some(PeakTableMessage::Close),
            KeyCode::Char('t') => Some(PeakTableMessage::CycleZone),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: PeakTableMessage) -> Option<PeakTableEffect> {
        if !self.open {
            return None;
        }
        match msg {
            PeakTableMessage::Close => {
                self.close();
                Some(PeakTableEffect::Close)
            }
            PeakTableMessage::CycleZone => {
                self.zone = self.zone.next();
                self.rebuild_rows();
                None
            }
        }
    }

    /// Re-render the cached rows for the selected clock. The windows are
    /// instants, so this only re-expresses them; it never re-reads the clock.
    fn rebuild_rows(&mut self) {
        let Some(schedule) = self.schedule.as_ref() else {
            self.rows.clear();
            return;
        };
        let zone = self.zone;
        let schedule_zone = schedule.timezone();
        self.rows = self
            .windows
            .iter()
            .map(|window| {
                let (start, start_day) = wall_clock(window.start(), zone, schedule_zone);
                let (end, end_day) = wall_clock(window.end(), zone, schedule_zone);
                let days = end_day - start_day;
                let mut end_text = fmt_time(end);
                if days > 0 {
                    end_text.push_str(&format!(" +{days}"));
                }
                Row {
                    start: fmt_time(start),
                    end: end_text,
                    span: fmt_relative(window.span()),
                }
            })
            .collect();
    }

    /// The label naming the clock the time columns are in.
    fn zone_label(&self) -> String {
        match self.zone {
            // Marked `schedule` so the state reads apart from `UTC` even when
            // the catalog happens to state a UTC schedule, and `t` looks like
            // it did nothing.
            PeakZone::Schedule => match self.schedule.as_ref() {
                Some(schedule) => format!("{} (schedule)", schedule.timezone().name()),
                None => "UTC (schedule)".to_string(),
            },
            PeakZone::Utc => "UTC".to_string(),
            PeakZone::Local => format!("local time ({})", local_offset_label()),
        }
    }

    /// The exempt day classes, as one readable list.
    fn exemptions_label(&self) -> Option<String> {
        let schedule = self.schedule.as_ref()?;
        let exempt: Vec<&str> = schedule
            .exemptions()
            .iter()
            .copied()
            .map(exemption_label)
            .collect();
        (!exempt.is_empty()).then(|| format!("No peak on {}", exempt.join(", ")))
    }

    /// The popup's rect: as tall as its content needs (title, zone line, table
    /// header, rows, the exemption note, the hint row) and clamped to `area`.
    fn rect(&self, area: Rect) -> Rect {
        let note = u16::from(self.exemptions_label().is_some());
        // Border ring 2 + zone 1 + blank 1 + table header 1 + rows + blank 1 +
        // note + hint 1.
        let height = 7 + self.rows.len() as u16 + note;
        let width = POPUP_COLS.min(area.width);
        let height = height.min(area.height);
        Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        )
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool) {
        if !self.open {
            return;
        }
        let rect = self.rect(area);
        popup::dialog(frame, rect, "Peak periods", dimmed, |inner, buf| {
            if inner.width == 0 || inner.height == 0 {
                return;
            }
            let note = u16::from(self.exemptions_label().is_some());
            let [
                zone_area,
                _gap,
                header_area,
                rows_area,
                _spacer,
                note_area,
                hint_area,
            ] = Layout::vertical([
                Length(1),
                Length(1),
                Length(1),
                Min(0),
                Length(1),
                Length(note),
                Length(1),
            ])
            .areas(inner);

            Paragraph::new(self.zone_label())
                .fg(theme::accent())
                .bold()
                .render(zone_area, buf);

            Paragraph::new(Line::from(vec![
                Span::raw(format!("{:<START_COLS$}", "Start")),
                Span::raw(format!("{:<END_COLS$}", "End")),
                Span::raw("Lasts"),
            ]))
            .fg(theme::text_muted())
            .render(header_area, buf);

            let rows: Vec<Line<'static>> = self
                .rows
                .iter()
                .take(rows_area.height as usize)
                .map(Row::line)
                .collect();
            Paragraph::new(rows).render(rows_area, buf);

            if let Some(note) = self.exemptions_label() {
                Paragraph::new(note)
                    .fg(theme::text_dim())
                    .render(note_area, buf);
            }

            let hint = theme::help_line(&[("t", "timezone"), ("Esc", "close")]);
            Paragraph::new(hint)
                .fg(theme::text_muted())
                .render(hint_area, buf);
        });
    }
}

impl Default for PeakTablePopup {
    fn default() -> Self {
        Self::new()
    }
}

/// A wall-clock time and the civil day it falls on, used to spot a close that
/// lands on the following day.
fn wall_clock(at: SystemTime, zone: PeakZone, schedule: Tz) -> (chrono::NaiveTime, i32) {
    let utc = DateTime::<Utc>::from(at);
    let local = match zone {
        PeakZone::Schedule => utc.with_timezone(&schedule).naive_local(),
        PeakZone::Utc => utc.naive_utc(),
        PeakZone::Local => utc.with_timezone(&Local).naive_local(),
    };
    (local.time(), local.ordinal() as i32)
}

/// `09:30`. Minute precision: a window edge is stated in whole minutes.
fn fmt_time(time: chrono::NaiveTime) -> String {
    format!("{:02}:{:02}", time.hour(), time.minute())
}

/// The machine's current UTC offset, e.g. `UTC+02:00`.
fn local_offset_label() -> String {
    let offset = Local::now().offset().local_minus_utc();
    let sign = if offset < 0 { '-' } else { '+' };
    let offset = offset.abs();
    format!("UTC{sign}{:02}:{:02}", offset / 3600, offset % 3600 / 60)
}

/// How an exempt day class reads in the schedule's summary line.
fn exemption_label(exempt: PeakExemption) -> &'static str {
    match exempt {
        PeakExemption::Weekends => "weekends",
        PeakExemption::HolidaysCn => "CN holidays",
        PeakExemption::HolidaysUs => "US holidays",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selune::{PeakPeriod, Peaks, TimeOfDay};
    use termina::event::Modifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::empty())
    }

    fn period(start: (u8, u8), end: (u8, u8)) -> PeakPeriod {
        PeakPeriod::new(
            TimeOfDay::new(start.0, start.1).unwrap(),
            TimeOfDay::new(end.0, end.1).unwrap(),
        )
        .unwrap()
    }

    /// A schedule stated in UTC, so the fixture's times and the `Schedule`
    /// clock agree without the machine's timezone entering into it.
    fn schedule(periods: Vec<PeakPeriod>, exempt: Vec<PeakExemption>) -> PeakSchedule {
        let provider: selune::Provider = serde_json::from_value(serde_json::json!({
            "name": "Test",
            "id": "test",
            "peaks": { "periods": periods.iter().map(|p| {
                serde_json::json!({
                    "start": format!("{:02}:{:02}", p.start().hour(), p.start().minute()),
                    "end": format!("{:02}:{:02}", p.end().hour(), p.end().minute()),
                })
            }).collect::<Vec<_>>(), "exempt": exempt },
            "models": [{
                "id": "m",
                "modelCode": "test-org/m",
                "name": "M",
                "reasoning": false,
                "attachment": false,
                "limit": {},
                "cost": {}
            }]
        }))
        .expect("fixture provider");
        PeakSchedule::resolve(&provider, "m").expect("fixture schedule")
    }

    fn utc_schedule() -> PeakSchedule {
        schedule(
            vec![period((9, 0), (12, 0)), period((14, 0), (18, 0))],
            vec![PeakExemption::Weekends],
        )
    }

    fn draw(popup: &PeakTablePopup, w: u16, h: u16) -> ratatui::buffer::Buffer {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| popup.view(frame, frame.area(), false))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area().width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    fn row_with(buf: &ratatui::buffer::Buffer, needle: &str) -> String {
        (0..buf.area().height)
            .map(|y| row_text(buf, y))
            .find(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("no row containing {needle:?}"))
    }

    #[test]
    fn open_snapshots_the_days_windows_and_closes_clean() {
        let mut popup = PeakTablePopup::new();
        popup.open(utc_schedule());
        assert!(popup.open);
        assert_eq!(popup.windows.len(), 2);
        assert_eq!(popup.rows.len(), 2);
        assert_eq!(popup.zone(), PeakZone::Schedule);
        assert_eq!(
            popup.update(PeakTableMessage::Close),
            Some(PeakTableEffect::Close)
        );
        assert!(!popup.open);
        assert!(popup.windows.is_empty(), "closing drops the snapshot");
        assert!(popup.rows.is_empty());
        assert_eq!(popup.update(PeakTableMessage::Close), None);
    }

    #[test]
    fn rows_render_the_windows_in_the_schedule_clock() {
        let popup = {
            let mut popup = PeakTablePopup::new();
            popup.open(utc_schedule());
            popup
        };
        assert_eq!(
            popup.rows,
            vec![
                Row {
                    start: "09:00".into(),
                    end: "12:00".into(),
                    span: "3h".into()
                },
                Row {
                    start: "14:00".into(),
                    end: "18:00".into(),
                    span: "4h".into()
                },
            ]
        );
    }

    #[test]
    fn the_zone_cycle_runs_schedule_utc_local_and_back() {
        let mut popup = PeakTablePopup::new();
        popup.open(utc_schedule());
        assert_eq!(popup.zone(), PeakZone::Schedule);
        popup.update(PeakTableMessage::CycleZone);
        assert_eq!(popup.zone(), PeakZone::Utc);
        popup.update(PeakTableMessage::CycleZone);
        assert_eq!(popup.zone(), PeakZone::Local);
        assert_eq!(
            popup.rows.len(),
            2,
            "every clock keeps the same windows: the rows only re-express them"
        );
        popup.update(PeakTableMessage::CycleZone);
        assert_eq!(popup.zone(), PeakZone::Schedule, "the cycle wraps");
        assert_eq!(popup.rows[0].start, "09:00");
    }

    /// The chosen clock survives a close: reopening the table lands where the
    /// user left it.
    #[test]
    fn the_zone_survives_reopening() {
        let mut popup = PeakTablePopup::new();
        popup.open(utc_schedule());
        popup.update(PeakTableMessage::CycleZone);
        popup.update(PeakTableMessage::Close);
        popup.open(utc_schedule());
        assert_eq!(popup.zone(), PeakZone::Utc);
        assert_eq!(popup.rows[0].start, "09:00");
    }

    #[test]
    fn the_view_names_the_zone_and_lists_the_windows() {
        let _guard = theme::lock_for_tests();
        let mut popup = PeakTablePopup::new();
        popup.open(utc_schedule());
        let buf = draw(&popup, 60, 20);
        row_with(&buf, "Peak periods");
        row_with(&buf, "UTC (schedule)");
        row_with(&buf, "Start");
        let first = row_with(&buf, "09:00");
        assert!(first.contains("12:00"), "row: {first:?}");
        assert!(first.contains("3h"), "row: {first:?}");
        row_with(&buf, "14:00");
        row_with(&buf, "No peak on weekends");
        row_with(&buf, "t timezone");
    }

    #[test]
    fn the_view_switches_the_clock_on_t() {
        let _guard = theme::lock_for_tests();
        let mut popup = PeakTablePopup::new();
        popup.open(utc_schedule());
        popup.update(PeakTableMessage::CycleZone);
        let buf = draw(&popup, 60, 20);
        assert_eq!(popup.zone(), PeakZone::Utc);
        let zone_row = row_with(&buf, "UTC");
        assert!(
            !zone_row.contains("schedule"),
            "the UTC clock is named plainly, apart from the schedule's own zone: {zone_row:?}"
        );
        popup.update(PeakTableMessage::CycleZone);
        let buf = draw(&popup, 60, 20);
        row_with(&buf, "local time (UTC");
    }

    #[test]
    fn a_window_closing_after_local_midnight_is_marked() {
        let mut popup = PeakTablePopup::new();
        popup.open(schedule(vec![period((23, 0), (1, 0))], Vec::new()));
        assert_eq!(popup.rows[0].start, "23:00");
        assert_eq!(popup.rows[0].end, "01:00 +1");
        assert_eq!(popup.rows[0].span, "2h");

        // The marker sits in the end column, so the span keeps its gap instead
        // of reading as `+12h`.
        let _guard = theme::lock_for_tests();
        let buf = draw(&popup, 60, 20);
        let row = row_with(&buf, "23:00");
        assert!(row.contains("01:00 +1  2h"), "row: {row:?}");
    }

    #[test]
    fn a_schedule_without_exemptions_skips_the_note() {
        let _guard = theme::lock_for_tests();
        let mut popup = PeakTablePopup::new();
        popup.open(schedule(vec![period((9, 0), (10, 0))], Vec::new()));
        assert!(popup.exemptions_label().is_none());
        let buf = draw(&popup, 60, 20);
        assert!(
            !(0..buf.area().height).any(|y| row_text(&buf, y).contains("No peak on")),
            "no note without exemptions"
        );
    }

    #[test]
    fn every_exemption_reads_as_a_day_class() {
        assert_eq!(exemption_label(PeakExemption::Weekends), "weekends");
        assert_eq!(exemption_label(PeakExemption::HolidaysCn), "CN holidays");
        assert_eq!(exemption_label(PeakExemption::HolidaysUs), "US holidays");
        let popup = {
            let mut popup = PeakTablePopup::new();
            popup.open(schedule(
                vec![period((9, 0), (10, 0))],
                vec![
                    PeakExemption::Weekends,
                    PeakExemption::HolidaysCn,
                    PeakExemption::HolidaysUs,
                ],
            ));
            popup
        };
        assert_eq!(
            popup.exemptions_label().as_deref(),
            Some("No peak on weekends, CN holidays, US holidays")
        );
    }

    #[test]
    fn map_event_routes_the_keys() {
        let popup = PeakTablePopup::new();
        assert_eq!(
            popup.map_event(&key(KeyCode::Char('t'))),
            Some(PeakTableMessage::CycleZone)
        );
        assert_eq!(
            popup.map_event(&key(KeyCode::Escape)),
            Some(PeakTableMessage::Close)
        );
        assert_eq!(
            popup.map_event(&key(KeyCode::Char('q'))),
            Some(PeakTableMessage::Close)
        );
        assert_eq!(popup.map_event(&key(KeyCode::Char('x'))), None);
    }

    #[test]
    fn a_closed_popup_ignores_messages() {
        let mut popup = PeakTablePopup::new();
        assert_eq!(popup.update(PeakTableMessage::CycleZone), None);
        assert_eq!(popup.update(PeakTableMessage::Close), None);
        assert_eq!(popup.zone(), PeakZone::Schedule);
    }

    /// `Peaks` is what `PeakSchedule` wraps, so the fixture's own window times
    /// are the schedule's: this pins the two together against a typo in the
    /// JSON fixture.
    #[test]
    fn the_fixture_schedule_matches_the_peaks_it_was_built_from() {
        let peaks: Peaks = serde_json::from_value(serde_json::json!({
            "periods": [{ "start": "09:00", "end": "12:00" }],
        }))
        .unwrap();
        assert_eq!(peaks.periods, vec![period((9, 0), (12, 0))]);
    }
}
