//! One activity-log event: the raw API object plus the decoded
//! envelope every event type shares.

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;

/// An event from the activity log.
///
/// `raw` is the API's own object, untouched — the schema is
/// Mergify's contract, not this CLI's, so unknown fields must
/// survive the round-trip (`--json` republishes them, and a newer
/// engine cannot break an older CLI). The envelope accessors decode
/// only the fields every event type carries.
#[derive(Debug)]
pub struct Event {
    pub raw: serde_json::Value,
    envelope: Envelope,
}

/// The common envelope. Every field is optional: an event shape this
/// CLI has never seen must still render, not crash.
#[derive(Deserialize, Default, Debug)]
struct Envelope {
    #[serde(default, rename = "type")]
    event_type: Option<String>,
    #[serde(default)]
    received_at: Option<String>,
    #[serde(default)]
    trigger: Option<String>,
    #[serde(default)]
    pull_request: Option<u64>,
    #[serde(default)]
    outcome: Option<String>,
}

impl Event {
    /// Wrap a raw API event. Infallible by design: a payload whose
    /// envelope does not decode (wrong types, not an object) is still
    /// an event — its accessors read as absent and `raw` keeps
    /// everything.
    #[must_use]
    pub fn from_raw(raw: serde_json::Value) -> Self {
        let envelope = Envelope::deserialize(&raw).unwrap_or_default();
        Self { raw, envelope }
    }

    /// The event type (e.g. `action.queue.leave`), verbatim from the
    /// API — new engine types pass through unrecognized.
    #[must_use]
    pub fn event_type(&self) -> Option<&str> {
        non_empty(self.envelope.event_type.as_deref())
    }

    /// When the engine recorded the event, as the API sent it.
    #[must_use]
    pub fn received_at(&self) -> Option<&str> {
        non_empty(self.envelope.received_at.as_deref())
    }

    /// [`Self::received_at`] parsed, for sorting and rendering.
    /// `None` when absent or unparseable — degrading beats crashing
    /// on a timestamp format change.
    #[must_use]
    pub fn received_at_utc(&self) -> Option<DateTime<Utc>> {
        self.received_at()
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|ts| ts.with_timezone(&Utc))
    }

    /// What caused the event (e.g. `merge queue internal`, a command
    /// author).
    #[must_use]
    pub fn trigger(&self) -> Option<&str> {
        non_empty(self.envelope.trigger.as_deref())
    }

    /// The pull request the event belongs to, when it has one.
    #[must_use]
    pub const fn pull_request(&self) -> Option<u64> {
        self.envelope.pull_request
    }

    /// The API's derived outcome label (`success` / `failure` /
    /// `pending` / `neutral`).
    #[must_use]
    pub fn outcome(&self) -> Option<&str> {
        non_empty(self.envelope.outcome.as_deref())
    }

    /// The event's type-specific `metadata` object; `Null` when the
    /// payload has none. Callers decode the slice of it they
    /// understand.
    #[must_use]
    pub fn metadata(&self) -> &serde_json::Value {
        self.raw.get("metadata").unwrap_or(&serde_json::Value::Null)
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn from_raw_decodes_the_shared_envelope() {
        let event = Event::from_raw(json!({
            "id": 1,
            "type": "action.queue.leave",
            "received_at": "2026-07-20T23:25:11.263987Z",
            "trigger": "merge queue internal",
            "pull_request": 1700,
            "metadata": {"merged": false},
        }));
        assert_eq!(event.event_type(), Some("action.queue.leave"));
        assert_eq!(event.received_at(), Some("2026-07-20T23:25:11.263987Z"));
        assert_eq!(event.trigger(), Some("merge queue internal"));
        assert_eq!(event.pull_request(), Some(1700));
        assert_eq!(event.metadata()["merged"], json!(false));
    }

    #[test]
    fn from_raw_keeps_unknown_fields_verbatim() {
        let raw = json!({
            "type": "something.from.2027",
            "future_field": {"nested": true},
        });
        let event = Event::from_raw(raw.clone());
        assert_eq!(event.raw, raw);
        assert_eq!(event.event_type(), Some("something.from.2027"));
    }

    #[test]
    fn a_malformed_envelope_still_yields_an_event() {
        // Wrong types must not crash — accessors read as absent and
        // the raw payload survives for `--json`.
        let raw = json!({"type": 42, "received_at": ["not", "a", "date"]});
        let event = Event::from_raw(raw.clone());
        assert_eq!(event.event_type(), None);
        assert_eq!(event.received_at(), None);
        assert_eq!(event.raw, raw);
    }

    #[test]
    fn received_at_utc_parses_and_degrades() {
        let parsed = Event::from_raw(json!({"received_at": "2026-07-20T23:25:11Z"}));
        assert!(parsed.received_at_utc().is_some());
        let garbage = Event::from_raw(json!({"received_at": "not-a-date"}));
        assert!(garbage.received_at_utc().is_none());
    }

    #[test]
    fn metadata_is_null_when_absent() {
        let event = Event::from_raw(json!({"type": "action.label"}));
        assert!(event.metadata().is_null());
    }
}
