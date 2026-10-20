//! Peak/off-peak billing schedules from the catalog.
//!
//! A provider — or one of its models — may declare the windows of the day it
//! bills at peak rates: times of day in a schedule timezone (default UTC),
//! plus the day classes (weekends, holidays) on which no window applies at
//! all. The catalog entry is static; a session needs the live phase and when
//! it ends, so [`PeakSchedule`] resolves the entry and
//! [`PeakSchedule::status`] finds the next change of phase.
//!
//! Times are handled with chrono because that is the clock selune's schedule
//! API speaks; the [`PeakStatus`] handed to callers carries plain
//! [`SystemTime`] instants, so a UI needs no date-time dependency of its own.

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Days, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use selune::{Peaks, Provider, Tz};

use crate::catalog::find_model;

/// How far ahead a phase change is looked for. Exempt days can make an
/// off-peak stretch (a holiday run plus the weekends around it) a week or
/// more long; three weeks covers that with margin. A schedule with no change
/// inside the horizon is treated as untimed.
const LOOKAHEAD_DAYS: u64 = 21;

/// How many minutes a DST gap may swallow a local wall-clock time. Real gaps
/// are 30 to 120 minutes; the search only has to find the first local minute
/// that exists again.
const DST_GAP_MINUTES: i64 = 180;

/// Which side of a peak/off-peak schedule an instant falls on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeakPhase {
    /// Inside a peak window: the provider bills its higher rate.
    Peak,
    /// Outside every peak window.
    OffPeak,
}

/// A model's peak/off-peak schedule, resolved from the catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct PeakSchedule {
    peaks: Peaks,
}

impl PeakSchedule {
    /// The schedule governing `model_id` under `provider`: the model's own
    /// windows override the provider-wide ones, and a schedule without
    /// windows (the explicit opt-out) resolves to `None`, as does a provider
    /// and model that declare no schedule at all.
    pub fn resolve(provider: &Provider, model_id: &str) -> Option<Self> {
        let model = find_model(provider, model_id)?;
        let peaks = provider.peaks_for(model)?;
        (!peaks.periods.is_empty()).then(|| Self {
            peaks: peaks.clone(),
        })
    }

    /// The IANA timezone the windows are stated in (e.g. `Asia/Shanghai`).
    pub fn timezone(&self) -> Tz {
        self.peaks.timezone
    }

    /// Where the schedule stands at `at`, with the instant the current phase
    /// ends. `None` when the phase never changes inside the lookahead
    /// horizon (a schedule covering the whole day with no exempt days).
    pub fn status(&self, at: SystemTime) -> Option<PeakStatus> {
        self.status_at(DateTime::<Utc>::from(at))
    }

    /// Where the schedule stands right now.
    pub fn status_now(&self) -> Option<PeakStatus> {
        self.status(SystemTime::now())
    }

    fn status_at(&self, at: DateTime<Utc>) -> Option<PeakStatus> {
        let phase = self.phase_at(at);
        let date = self.peaks.local(at).date_naive();
        let mut boundaries = Vec::with_capacity(8 * LOOKAHEAD_DAYS as usize);
        for offset in 0..LOOKAHEAD_DAYS {
            if let Some(date) = date.checked_add_days(Days::new(offset)) {
                self.day_boundaries(date, &mut boundaries);
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        // A phase can only change at one of the collected instants, so the
        // first one that lands on the other phase ends the current one.
        let ends_at = boundaries
            .into_iter()
            .find(|boundary| *boundary > at && self.phase_at(*boundary) != phase)?;
        Some(PeakStatus {
            phase,
            ends_at: ends_at.into(),
        })
    }

    /// Collect the instants at which the phase can change on a local `date`:
    /// midnight (a day-class exemption flips there) and every window edge,
    /// including the window that a period crossing midnight ends in.
    fn day_boundaries(&self, date: NaiveDate, out: &mut Vec<DateTime<Utc>>) {
        out.extend(self.instant(date, 0));
        for period in &self.peaks.periods {
            let start = i64::from(period.start().minutes_since_midnight());
            out.extend(self.instant(date, start));
            let end_date = if period.crosses_midnight() {
                date.checked_add_days(Days::new(1))
            } else {
                Some(date)
            };
            if let Some(end_date) = end_date {
                let end = i64::from(period.end().minutes_since_midnight());
                out.extend(self.instant(end_date, end));
            }
        }
    }

    fn phase_at(&self, at: DateTime<Utc>) -> PeakPhase {
        if self.peaks.is_peak(at) {
            PeakPhase::Peak
        } else {
            PeakPhase::OffPeak
        }
    }

    /// The UTC instant of `date` at `minutes` past local midnight.
    fn instant(&self, date: NaiveDate, minutes: i64) -> Option<DateTime<Utc>> {
        let naive = date.and_hms_opt(0, 0, 0)? + chrono::Duration::minutes(minutes);
        local_instant(self.peaks.timezone, naive)
    }
}

/// Where a schedule stands: the phase, and the instant it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeakStatus {
    phase: PeakPhase,
    ends_at: SystemTime,
}

impl PeakStatus {
    /// The phase the schedule is in.
    pub fn phase(&self) -> PeakPhase {
        self.phase
    }

    /// The instant the phase ends (the next phase change).
    pub fn ends_at(&self) -> SystemTime {
        self.ends_at
    }

    /// Wall-clock time left in the phase at `at`; zero once it has passed.
    pub fn remaining(&self, at: SystemTime) -> Duration {
        self.ends_at.duration_since(at).unwrap_or_default()
    }

    /// Wall-clock time left in the phase right now.
    pub fn remaining_now(&self) -> Duration {
        self.remaining(SystemTime::now())
    }
}

/// Resolve a local wall-clock time in `timezone` to a UTC instant. A time
/// skipped by a DST jump resolves to the first local minute that exists after
/// the jump, which is the transition itself within minute precision; a time
/// repeated by a jump resolves to its earlier occurrence, when the window
/// edge is reached first.
fn local_instant(timezone: Tz, naive: NaiveDateTime) -> Option<DateTime<Utc>> {
    match timezone.from_local_datetime(&naive) {
        LocalResult::Single(at) => Some(at.with_timezone(&Utc)),
        LocalResult::Ambiguous(earliest, latest) => Some(earliest.min(latest).with_timezone(&Utc)),
        LocalResult::None => (1..=DST_GAP_MINUTES).find_map(|minutes| {
            let after = naive + chrono::Duration::minutes(minutes);
            match timezone.from_local_datetime(&after) {
                LocalResult::Single(at) => Some(at.with_timezone(&Utc)),
                LocalResult::Ambiguous(earliest, latest) => {
                    Some(earliest.min(latest).with_timezone(&Utc))
                }
                LocalResult::None => None,
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selune::{
        InferenceProvider, Model, ModelCode, ModelCost, ModelLimit, PeakExemption, PeakPeriod,
        TimeOfDay,
    };

    fn time(hour: u8, minute: u8) -> TimeOfDay {
        TimeOfDay::new(hour, minute).unwrap()
    }

    fn period(start: (u8, u8), end: (u8, u8)) -> PeakPeriod {
        PeakPeriod::new(time(start.0, start.1), time(end.0, end.1)).unwrap()
    }

    fn peaks(timezone: Tz, periods: Vec<PeakPeriod>, exempt: Vec<PeakExemption>) -> Peaks {
        Peaks {
            timezone,
            periods,
            exempt,
        }
    }

    /// A one-model provider: `provider_peaks` is the provider-wide schedule,
    /// `model_peaks` the model's own override.
    fn provider(provider_peaks: Option<Peaks>, model_peaks: Option<Peaks>) -> Provider {
        Provider {
            name: "Test".to_string(),
            id: InferenceProvider("test".to_string()),
            api_key: None,
            auth: None,
            api_endpoint: None,
            r#type: None,
            dialect: None,
            doc: None,
            peaks: provider_peaks,
            default_large_model_id: None,
            default_small_model_id: None,
            models: vec![Model {
                id: "m".to_string(),
                model_code: ModelCode::new("test-org", "m", None).unwrap(),
                name: "M".to_string(),
                reasoning: false,
                reasoning_options: Vec::new(),
                attachment: false,
                limit: ModelLimit::default(),
                cost: ModelCost::default(),
                peaks: model_peaks,
                options: None,
            }],
            default_headers: None,
        }
    }

    /// A UTC instant, written the way the schedules' timezone reads it.
    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> SystemTime {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap()
            .into()
    }

    /// `2025-06-09` is a Monday, `2025-06-07` a Saturday.
    fn weekdays_only() -> Peaks {
        peaks(
            Tz::Asia__Shanghai,
            vec![period((9, 0), (12, 0))],
            vec![PeakExemption::Weekends],
        )
    }

    #[test]
    fn resolve_reads_the_provider_schedule() {
        let provider = provider(Some(weekdays_only()), None);
        let schedule = PeakSchedule::resolve(&provider, "m").expect("provider schedule");
        assert_eq!(schedule.timezone(), Tz::Asia__Shanghai);
    }

    #[test]
    fn resolve_prefers_the_model_schedule() {
        let provider = provider(
            Some(weekdays_only()),
            Some(peaks(Tz::UTC, vec![period((12, 0), (13, 0))], Vec::new())),
        );
        let schedule = PeakSchedule::resolve(&provider, "m").expect("model schedule");
        assert_eq!(schedule.timezone(), Tz::UTC);
        // 12:30 UTC is inside the model's window but outside the provider's.
        let status = schedule.status(at(2025, 6, 9, 12, 30)).unwrap();
        assert_eq!(status.phase(), PeakPhase::Peak);
        assert_eq!(status.ends_at(), at(2025, 6, 9, 13, 0));
        assert_eq!(
            schedule.status(at(2025, 6, 9, 10, 30)).unwrap().phase(),
            PeakPhase::OffPeak
        );
    }

    #[test]
    fn resolve_treats_an_empty_model_schedule_as_an_opt_out() {
        let provider = provider(
            Some(weekdays_only()),
            Some(peaks(Tz::UTC, Vec::new(), Vec::new())),
        );
        assert!(PeakSchedule::resolve(&provider, "m").is_none());
    }

    #[test]
    fn resolve_is_none_without_a_schedule_or_model() {
        assert!(PeakSchedule::resolve(&provider(None, None), "m").is_none());
        let provider = provider(Some(weekdays_only()), None);
        assert!(PeakSchedule::resolve(&provider, "unknown").is_none());
    }

    #[test]
    fn status_in_a_peak_window_ends_at_its_close() {
        let provider = provider(Some(weekdays_only()), None);
        let schedule = PeakSchedule::resolve(&provider, "m").unwrap();
        // 10:30 in Shanghai is inside the 09:00–12:00 window.
        let status = schedule.status(at(2025, 6, 9, 2, 30)).unwrap();
        assert_eq!(status.phase(), PeakPhase::Peak);
        assert_eq!(status.ends_at(), at(2025, 6, 9, 4, 0), "12:00 Shanghai");
        assert_eq!(
            status.remaining(at(2025, 6, 9, 2, 30)),
            Duration::from_secs(90 * 60)
        );
        assert_eq!(
            status.remaining(at(2025, 6, 9, 4, 0)),
            Duration::ZERO,
            "a countdown past the end stays at zero"
        );
    }

    #[test]
    fn status_outside_a_window_ends_at_the_next_open() {
        let provider = provider(Some(weekdays_only()), None);
        let schedule = PeakSchedule::resolve(&provider, "m").unwrap();
        // 13:00 in Shanghai is after the window.
        let status = schedule.status(at(2025, 6, 9, 5, 0)).unwrap();
        assert_eq!(status.phase(), PeakPhase::OffPeak);
        assert_eq!(
            status.ends_at(),
            at(2025, 6, 10, 1, 0),
            "09:00 Shanghai next day"
        );
    }

    #[test]
    fn status_skips_exempt_days() {
        let provider = provider(Some(weekdays_only()), None);
        let schedule = PeakSchedule::resolve(&provider, "m").unwrap();
        // Saturday 10:00 in Shanghai: no window all weekend, so the next peak
        // opens Monday 09:00.
        let status = schedule.status(at(2025, 6, 7, 2, 0)).unwrap();
        assert_eq!(status.phase(), PeakPhase::OffPeak);
        assert_eq!(
            status.ends_at(),
            at(2025, 6, 9, 1, 0),
            "Monday 09:00 Shanghai"
        );
    }

    #[test]
    fn status_handles_a_window_crossing_midnight() {
        let provider = provider(
            Some(peaks(
                Tz::Asia__Shanghai,
                vec![period((23, 0), (1, 0))],
                vec![PeakExemption::Weekends],
            )),
            None,
        );
        let schedule = PeakSchedule::resolve(&provider, "m").unwrap();
        // Friday 23:30 in Shanghai: the window runs into Saturday, but
        // Saturday is exempt, so the peak ends at the midnight boundary.
        let status = schedule.status(at(2025, 6, 6, 15, 30)).unwrap();
        assert_eq!(status.phase(), PeakPhase::Peak);
        assert_eq!(
            status.ends_at(),
            at(2025, 6, 6, 16, 0),
            "Saturday 00:00 Shanghai"
        );
        // Saturday 23:30 is exempt, so the next peak is Monday's window
        // opening at midnight.
        let status = schedule.status(at(2025, 6, 7, 15, 30)).unwrap();
        assert_eq!(status.phase(), PeakPhase::OffPeak);
        assert_eq!(
            status.ends_at(),
            at(2025, 6, 8, 16, 0),
            "Monday 00:00 Shanghai"
        );
    }

    #[test]
    fn status_is_off_peak_when_holidays_are_exempt() {
        let provider = provider(
            Some(peaks(
                Tz::Asia__Shanghai,
                vec![period((9, 0), (12, 0))],
                vec![PeakExemption::HolidaysCn],
            )),
            None,
        );
        let schedule = PeakSchedule::resolve(&provider, "m").unwrap();
        // 2025-10-01 is Chinese National Day: the window does not apply.
        let status = schedule.status(at(2025, 10, 1, 2, 0)).unwrap();
        assert_eq!(status.phase(), PeakPhase::OffPeak);
        assert!(
            status.ends_at() > at(2025, 10, 2, 0, 0),
            "the holiday runs past the next day"
        );
    }

    #[test]
    fn status_is_none_when_the_phase_never_changes() {
        let provider = provider(
            Some(peaks(
                Tz::UTC,
                vec![period((0, 0), (12, 0)), period((12, 0), (0, 0))],
                Vec::new(),
            )),
            None,
        );
        let schedule = PeakSchedule::resolve(&provider, "m").unwrap();
        assert!(schedule.status(at(2025, 6, 9, 3, 0)).is_none());
    }

    #[test]
    fn resolve_reads_the_embedded_catalog() {
        let providers = crate::catalog::providers();
        let deepseek = crate::catalog::find_provider(&providers, "deepseek")
            .expect("the embedded catalog has deepseek");
        let model = deepseek.default_large_model().expect("a default model");
        let schedule = PeakSchedule::resolve(deepseek, model).expect("deepseek bills peak hours");
        assert_eq!(schedule.timezone(), Tz::Asia__Shanghai);

        // Ollama Cloud has no provider-wide schedule; some of its models carry
        // their own windows, stated in UTC.
        let ollama = crate::catalog::find_provider(&providers, "ollama-cloud")
            .expect("the embedded catalog has ollama-cloud");
        let schedule =
            PeakSchedule::resolve(ollama, "deepseek-v4-pro").expect("a model-level schedule");
        assert_eq!(schedule.timezone(), Tz::UTC);
        assert!(PeakSchedule::resolve(ollama, "glm-5.3").is_none());
    }
}
