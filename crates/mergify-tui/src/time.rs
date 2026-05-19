//! Coarse relative-time formatter.
//!
//! Modeled on the Python CLI's `_relative_time` helper: an
//! ISO-8601 / RFC-3339 timestamp becomes a short delta like
//! `5m ago` or `~2h`. The granularity is intentionally coarse —
//! seconds / minutes / hours / days, single component — because
//! these numbers show up in dense table layouts where exact
//! fidelity adds noise without information.
//!
//! Behavior on unparseable input is the one **intentional**
//! divergence from Python: the Python helper returned the raw
//! input verbatim, which leaked an ugly ISO timestamp into the
//! column. The Rust port returns an empty string instead, and
//! callers treat that as "skip this column" so a single
//! malformed timestamp doesn't abort the whole render.

use chrono::DateTime;
use chrono::Utc;

/// Format an ISO-8601 / RFC-3339 timestamp as a coarse delta from
/// `now`.
///
/// - Past timestamps render as `"<value> ago"`.
/// - Future timestamps render as `"~<value>"` when `future = true`
///   (callers use this for ETAs to distinguish them visually from
///   "happened" times).
/// - Granularity collapses to the largest non-zero unit (`Ns`,
///   `Nm`, `Nh`, or `Nd`).
/// - Returns `""` when `iso` is not a valid RFC-3339 timestamp.
#[must_use]
pub fn relative_time(iso: &str, now: DateTime<Utc>, future: bool) -> String {
    let Ok(parsed) = DateTime::parse_from_rfc3339(iso) else {
        return String::new();
    };
    let parsed = parsed.with_timezone(&Utc);
    let delta = (now - parsed).num_seconds().abs();
    let value = if delta < 60 {
        format!("{delta}s")
    } else if delta < 3600 {
        format!("{}m", delta / 60)
    } else if delta < 86400 {
        format!("{}h", delta / 3600)
    } else {
        format!("{}d", delta / 86400)
    };
    if future {
        format!("~{value}")
    } else {
        format!("{value} ago")
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
    fn seconds() {
        assert_eq!(
            relative_time("2026-01-01T00:00:30Z", at("2026-01-01T00:01:00Z"), false),
            "30s ago",
        );
    }

    #[test]
    fn minutes() {
        assert_eq!(
            relative_time("2026-01-01T00:55:00Z", at("2026-01-01T01:00:00Z"), false),
            "5m ago",
        );
    }

    #[test]
    fn hours() {
        assert_eq!(
            relative_time("2026-01-01T00:00:00Z", at("2026-01-01T05:00:00Z"), false),
            "5h ago",
        );
    }

    #[test]
    fn days() {
        assert_eq!(
            relative_time("2026-01-01T00:00:00Z", at("2026-01-08T00:00:00Z"), false),
            "7d ago",
        );
    }

    #[test]
    fn future_prefix() {
        assert_eq!(
            relative_time("2026-01-01T00:30:00Z", at("2026-01-01T00:00:00Z"), true),
            "~30m",
        );
    }

    #[test]
    fn unparseable_returns_empty() {
        // Mirrors the Python CLI: skip the column rather than
        // abort the render on bad input.
        assert_eq!(
            relative_time("not-a-date", at("2026-01-01T00:00:00Z"), false),
            "",
        );
    }
}
