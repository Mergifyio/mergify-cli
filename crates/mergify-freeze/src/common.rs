//! Shared helpers across the freeze subcommands.
//!
//! Three responsibilities:
//!
//! - [`ScheduledFreeze`] — the wire format shared by every endpoint.
//! - [`print_freeze`] — the per-freeze human block that
//!   `create` and `update` emit on success. Format matches Python's
//!   `_print_freeze` so the smoke tests parse the same way against
//!   either implementation.
//! - [`parse_naive_datetime`] — accept the ISO-8601 datetime
//!   strings users give to `--start` / `--end`, rejecting anything
//!   Python's `datetime.fromisoformat` would reject.
//! - [`detect_local_timezone`] — wrapper around `iana-time-zone`
//!   that produces the user-facing error Python raises when
//!   `tzlocal.get_localzone_name()` returns nothing.

use std::io::Write;

use chrono::NaiveDateTime;
use mergify_core::CliError;
use serde::Deserialize;

/// Mergify's `/scheduled_freeze` resource — used as the response
/// shape on every freeze endpoint. Every field is optional so the
/// CLI can render whatever the server actually returned without
/// aborting on a missing key.
#[derive(Deserialize, Debug, Clone)]
pub struct ScheduledFreeze {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub matching_conditions: Vec<String>,
    #[serde(default)]
    pub exclude_conditions: Vec<String>,
}

/// `--start` / `--end` payload encoder: the Python CLI feeds an
/// `isoformat()` string to the API, which is exactly the input
/// string we parsed (no offset). [`Self::iso`] reproduces that
/// shape — seconds precision, no offset — so the round-trip
/// matches what the Mergify server has historically accepted.
#[derive(Debug, Clone, Copy)]
pub struct NaiveDateTimeWire<'a>(pub &'a NaiveDateTime);

impl NaiveDateTimeWire<'_> {
    #[must_use]
    pub fn iso(self) -> String {
        // `%Y-%m-%dT%H:%M:%S` matches Python's `datetime.isoformat()`
        // for a naive datetime with no microseconds.
        self.0.format("%Y-%m-%dT%H:%M:%S").to_string()
    }
}

/// Parse a user-supplied `--start` / `--end` value as a naive
/// datetime. Accepts the same handful of ISO-8601 shapes
/// `datetime.fromisoformat` accepts: with seconds, with optional
/// fractional seconds, and (best-effort) with a `Z` / offset suffix.
/// Returns a [`CliError::Configuration`] on parse failure so the
/// binary exits with the right code (8) and an obvious message.
pub fn parse_naive_datetime(value: &str) -> Result<NaiveDateTime, CliError> {
    // The order matters: try the most specific patterns first so a
    // string with fractional seconds isn't lossily matched by the
    // seconds-only parser.
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(value, fmt) {
            return Ok(dt);
        }
    }
    // RFC-3339 / Z-terminated strings: parse with offset and drop it.
    // The Mergify API treats `start` / `end` as naive in the freeze's
    // own timezone, so an offset on the input is misleading — but
    // accepting the shape and ignoring the offset matches Python's
    // `datetime.fromisoformat` (which also strips the offset into a
    // naive value before we serialize it back out).
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(value) {
        return Ok(dt.naive_local());
    }
    Err(CliError::Configuration(format!(
        "invalid datetime format: {value:?}. \
         Use ISO 8601 format (e.g. 2024-06-19T08:00:00)"
    )))
}

/// Detect the system's IANA timezone (e.g. `Europe/Paris`). Used
/// as the default for `--timezone` on `freeze create` when the
/// user doesn't pass one — matches Python's
/// `tzlocal.get_localzone_name()` call. Returns a
/// [`CliError::Configuration`] when detection fails so the user
/// gets a clear "pass `--timezone` explicitly" message instead of
/// a panic.
pub fn detect_local_timezone() -> Result<String, CliError> {
    iana_time_zone::get_timezone().map_err(|_| {
        CliError::Configuration(
            "Could not detect system timezone. Please specify --timezone explicitly.".to_string(),
        )
    })
}

/// Emit the per-freeze human block. Format mirrors Python's
/// `_print_freeze`: each label padded to the same column, missing
/// values rendered as `-`. Writes to the caller's
/// [`std::io::Write`] sink so `create` and `update` can stitch the
/// "Freeze … successfully:" banner and the body into a single
/// [`Output::emit`] call.
pub fn write_freeze(w: &mut dyn Write, freeze: &ScheduledFreeze) -> std::io::Result<()> {
    let timezone = freeze.timezone.as_deref().unwrap_or("");
    writeln!(w, "  ID:         {}", freeze.id.as_deref().unwrap_or("-"))?;
    writeln!(
        w,
        "  Reason:     {}",
        freeze.reason.as_deref().unwrap_or("-"),
    )?;
    writeln!(
        w,
        "  Start:      {}",
        format_datetime(freeze.start.as_deref(), timezone),
    )?;
    writeln!(
        w,
        "  End:        {}",
        format_datetime(freeze.end.as_deref(), timezone),
    )?;
    let conditions = freeze.matching_conditions.join(", ");
    writeln!(w, "  Conditions: {conditions}")?;
    if !freeze.exclude_conditions.is_empty() {
        let excludes = freeze.exclude_conditions.join(", ");
        writeln!(w, "  Exclude:    {excludes}")?;
    }
    Ok(())
}

/// Render an ISO timestamp + IANA timezone tuple the way Python
/// formats them in the `_print_freeze` output: `"<value> (<tz>)"`
/// when both are present, `"-"` when the value is missing.
#[must_use]
pub fn format_datetime(value: Option<&str>, timezone: &str) -> String {
    match value.filter(|s| !s.is_empty()) {
        None => "-".to_string(),
        Some(v) => format!("{v} ({timezone})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_naive_datetime_basic_iso() {
        let dt = parse_naive_datetime("2026-05-19T10:30:00").unwrap();
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-05-19 10:30:00"
        );
    }

    #[test]
    fn parse_naive_datetime_with_fractional_seconds() {
        let dt = parse_naive_datetime("2026-05-19T10:30:00.123456").unwrap();
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-05-19 10:30:00"
        );
    }

    #[test]
    fn parse_naive_datetime_space_separator() {
        // Python's `fromisoformat` accepts a space between date and
        // time as a relaxed alternative to `T`. Mirror that.
        let dt = parse_naive_datetime("2026-05-19 10:30:00").unwrap();
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-05-19 10:30:00"
        );
    }

    #[test]
    fn parse_naive_datetime_rfc3339_drops_offset() {
        // The Mergify API treats `start` as naive in the freeze's
        // own timezone — accepting an offset is for Python parity,
        // but the offset is dropped so the round-trip keeps the
        // wall-clock value the user typed.
        let dt = parse_naive_datetime("2026-05-19T10:30:00Z").unwrap();
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-05-19 10:30:00"
        );
    }

    #[test]
    fn parse_naive_datetime_rejects_garbage() {
        let err = parse_naive_datetime("not a date").unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)));
        assert!(err.to_string().contains("invalid datetime format"));
    }

    #[test]
    fn naive_wire_round_trip_drops_microseconds() {
        // Match Python's `isoformat()` on a microsecond-bearing dt:
        // we still emit seconds-only because the API expects that
        // shape.
        let dt = parse_naive_datetime("2026-05-19T10:30:00.123456").unwrap();
        assert_eq!(NaiveDateTimeWire(&dt).iso(), "2026-05-19T10:30:00");
    }

    #[test]
    fn format_datetime_missing_renders_dash() {
        assert_eq!(format_datetime(None, "UTC"), "-");
        assert_eq!(format_datetime(Some(""), "UTC"), "-");
    }

    #[test]
    fn format_datetime_appends_timezone() {
        assert_eq!(
            format_datetime(Some("2026-01-01T10:00:00"), "Europe/Paris"),
            "2026-01-01T10:00:00 (Europe/Paris)",
        );
    }

    #[test]
    fn detect_local_timezone_returns_a_value() {
        // We can't assert a specific timezone (varies by environment),
        // but we can assert that detection doesn't fail outright in
        // a normal dev / CI environment. If this fires in a sandbox
        // that masks `TZ`, the `iana-time-zone` crate falls back to
        // `/etc/localtime` on Unix — both routes work in CI.
        let tz = detect_local_timezone().expect("local timezone detectable");
        assert!(!tz.is_empty());
    }
}
