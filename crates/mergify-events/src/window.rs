//! Explicit time range for an activity-log query.
//!
//! `/logs` has two window traps and this type is where both die:
//! the silent `received_to - 1 day` default (a [`Window`] always
//! carries both bounds, so callers cannot forget to send them) and
//! the 422 on a span over 93 days (construction refuses it, so the
//! raw HTTP error is unreachable).

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;

/// How many days of events the API retains. A query reaching further
/// back returns nothing extra — [`Window::retained`] is the widest
/// window worth asking for.
pub const RETENTION_DAYS: i64 = 90;

/// The widest `received_from`/`received_to` span the API accepts;
/// anything wider is rejected with a 422 (`'received_from' and
/// 'received_to' cannot span more than 93 days`).
pub const MAX_SPAN_DAYS: i64 = 93;

/// An explicit `received_from`/`received_to` range, guaranteed to be
/// positive and within the API's span cap.
#[derive(Copy, Clone, Debug)]
pub struct Window {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
}

/// Why a [`Window`] could not be constructed. Surfacing this at
/// construction is the whole point: the caller gets a typed,
/// explainable refusal instead of the API's raw 422.
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum WindowError {
    #[error(
        "a {requested_days}-day window is wider than the API accepts \
         (over {max} days); the activity log retains {retention} days \
         — use {retention} days or less",
        max = MAX_SPAN_DAYS,
        retention = RETENTION_DAYS
    )]
    SpanTooWide { requested_days: i64 },

    #[error("the window must cover a positive length of time")]
    NotPositive,
}

impl Window {
    /// The whole retained history, ending at `now`. This is
    /// "everything the API can still answer for", spelled without the
    /// caller knowing the retention number.
    #[must_use]
    pub fn retained(now: DateTime<Utc>) -> Self {
        Self {
            from: now - TimeDelta::days(RETENTION_DAYS),
            to: now,
        }
    }

    /// The last `span` of time, ending at `now`.
    ///
    /// # Errors
    ///
    /// [`WindowError::SpanTooWide`] when `span` exceeds
    /// [`MAX_SPAN_DAYS`] (the API would 422);
    /// [`WindowError::NotPositive`] when it is zero or negative.
    pub fn last(span: TimeDelta, now: DateTime<Utc>) -> Result<Self, WindowError> {
        if span <= TimeDelta::zero() {
            return Err(WindowError::NotPositive);
        }
        if span > TimeDelta::days(MAX_SPAN_DAYS) {
            return Err(WindowError::SpanTooWide {
                requested_days: ceil_days(span),
            });
        }
        Ok(Self {
            from: now - span,
            to: now,
        })
    }

    #[must_use]
    pub const fn from(&self) -> DateTime<Utc> {
        self.from
    }

    #[must_use]
    pub const fn to(&self) -> DateTime<Utc> {
        self.to
    }
}

/// `span` in days, rounded **up**. Truncating (`num_days`) makes a
/// 93-day-and-one-hour ask report itself as "a 93-day window is wider
/// than the API accepts (over 93 days)" — a refusal that contradicts
/// its own reason. A partial day is a day the caller asked for.
fn ceil_days(span: TimeDelta) -> i64 {
    let whole = span.num_days();
    if span > TimeDelta::days(whole) {
        whole + 1
    } else {
        whole
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(iso: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(iso)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn retained_covers_the_whole_retention() {
        let now = at("2026-07-30T00:00:00Z");
        let window = Window::retained(now);
        assert_eq!(window.to(), now);
        assert_eq!(window.from(), at("2026-05-01T00:00:00Z"));
    }

    #[test]
    fn last_builds_an_explicit_range_ending_now() {
        let now = at("2026-07-30T12:00:00Z");
        let window = Window::last(TimeDelta::hours(24), now).unwrap();
        assert_eq!(window.from(), at("2026-07-29T12:00:00Z"));
        assert_eq!(window.to(), now);
    }

    #[test]
    fn last_refuses_a_span_the_api_would_422_on() {
        // The typed replacement for the raw `422: 'received_from' and
        // 'received_to' cannot span more than 93 days`.
        let now = at("2026-07-30T00:00:00Z");
        let err = Window::last(TimeDelta::days(94), now).unwrap_err();
        assert_eq!(err, WindowError::SpanTooWide { requested_days: 94 });
        // The message must hand the user the fix, not just the refusal.
        assert!(err.to_string().contains("retains 90 days"), "got: {err}");
    }

    #[test]
    fn a_part_day_over_the_cap_rounds_the_message_up() {
        // `--since 2233h` is 93 days and one hour. Reported with a
        // truncating day count it reads "a 93-day window is wider than
        // the API accepts (over 93 days)".
        let now = at("2026-07-30T00:00:00Z");
        let span = TimeDelta::days(MAX_SPAN_DAYS) + TimeDelta::hours(1);
        let err = Window::last(span, now).unwrap_err();
        assert_eq!(err, WindowError::SpanTooWide { requested_days: 94 });
        // Sub-second precision counts too: a whole day plus a nanosecond
        // is still more than the caller may have.
        let sliver = TimeDelta::days(MAX_SPAN_DAYS) + TimeDelta::nanoseconds(1);
        assert_eq!(
            Window::last(sliver, now).unwrap_err(),
            WindowError::SpanTooWide { requested_days: 94 },
        );
    }

    #[test]
    fn last_accepts_the_span_cap_itself() {
        let now = at("2026-07-30T00:00:00Z");
        assert!(Window::last(TimeDelta::days(MAX_SPAN_DAYS), now).is_ok());
    }

    #[test]
    fn last_refuses_an_empty_or_negative_span() {
        let now = at("2026-07-30T00:00:00Z");
        assert_eq!(
            Window::last(TimeDelta::zero(), now).unwrap_err(),
            WindowError::NotPositive,
        );
        assert_eq!(
            Window::last(TimeDelta::hours(-1), now).unwrap_err(),
            WindowError::NotPositive,
        );
    }
}
