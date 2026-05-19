//! `mergify freeze create` — schedule a new freeze.
//!
//! `POST /v1/repos/<repo>/scheduled_freeze`. Payload mirrors
//! Python's `CreateScheduledFreezePayload` exactly: `reason`,
//! `start`, `end`, `timezone` always present (with `null` for an
//! open-ended emergency freeze), `matching_conditions` and
//! `exclude_conditions` only included when non-empty. On success
//! the server echoes the freeze body, which we render through the
//! shared [`print_freeze`](crate::common::print_freeze) helper.

use std::io::Write;

use chrono::NaiveDateTime;
use mergify_core::CliError;
use mergify_core::CommandContext;
use mergify_core::Output;
use serde::Serialize;

use crate::common::NaiveDateTimeWire;
use crate::common::ScheduledFreeze;
use crate::common::detect_local_timezone;
use crate::common::write_freeze;

pub struct CreateOptions<'a> {
    pub repository: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
    pub reason: &'a str,
    /// IANA timezone. When `None`, defaults to the system timezone
    /// (errors out if undetectable).
    pub timezone: Option<&'a str>,
    pub start: Option<NaiveDateTime>,
    pub end: Option<NaiveDateTime>,
    pub matching_conditions: &'a [String],
    pub exclude_conditions: &'a [String],
}

#[derive(Serialize)]
struct CreatePayload<'a> {
    reason: &'a str,
    // `Option<String>` rather than `&str` because the API expects
    // `null` for missing values (matches Python's
    // `start.isoformat() if start is not None else None`), and
    // serde flattens `Option::None` to `null` cleanly. Keep these
    // owned to avoid juggling lifetimes on the formatted string.
    start: Option<String>,
    end: Option<String>,
    timezone: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    matching_conditions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_conditions: Option<Vec<String>>,
}

/// Run the `freeze create` command.
pub async fn run(opts: CreateOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let ctx = CommandContext::resolve(opts.repository, opts.token, opts.api_url)?;
    let timezone = match opts.timezone {
        Some(tz) => tz.to_string(),
        None => detect_local_timezone()?,
    };

    let payload = CreatePayload {
        reason: opts.reason,
        start: opts.start.as_ref().map(|dt| NaiveDateTimeWire(dt).iso()),
        end: opts.end.as_ref().map(|dt| NaiveDateTimeWire(dt).iso()),
        timezone,
        // Python passes the list when the user supplied any
        // matching conditions, and `None` (omitting the key) when
        // they didn't. Mirror that — the API may interpret a
        // present-but-empty list differently from an absent key.
        matching_conditions: if opts.matching_conditions.is_empty() {
            None
        } else {
            Some(opts.matching_conditions.to_vec())
        },
        exclude_conditions: if opts.exclude_conditions.is_empty() {
            None
        } else {
            Some(opts.exclude_conditions.to_vec())
        },
    };

    output.status(&format!(
        "Creating scheduled freeze for {repo}…",
        repo = ctx.repository,
    ))?;

    let client = ctx.mergify_client()?;
    let path = format!("/v1/repos/{}/scheduled_freeze", ctx.repository);
    let freeze: ScheduledFreeze = client.post(&path, &payload).await?;

    output.emit(&(), &mut |w: &mut dyn Write| {
        writeln!(w, "Freeze created successfully:")?;
        write_freeze(w, &freeze)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use mergify_test_support::Captured;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::body_partial_json;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;
    use crate::common::parse_naive_datetime;

    #[tokio::test]
    async fn run_posts_payload_with_optional_conditions_when_provided() {
        let server = MockServer::start().await;
        let start = parse_naive_datetime("2099-01-01T00:00:00").unwrap();
        let end = parse_naive_datetime("2099-01-02T00:00:00").unwrap();

        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/scheduled_freeze"))
            .and(header("Authorization", "Bearer t"))
            // Use `body_partial_json` so unrelated fields don't make
            // the matcher brittle — but include the conditions key
            // explicitly to assert it's serialized (the Python
            // `if matching_conditions is not None:` branch).
            .and(body_partial_json(json!({
                "reason": "emergency-fix",
                "start": "2099-01-01T00:00:00",
                "end": "2099-01-02T00:00:00",
                "timezone": "UTC",
                "matching_conditions": ["base=main"],
                "exclude_conditions": ["label=hotfix"],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "11111111-2222-3333-4444-555555555555",
                "reason": "emergency-fix",
                "start": "2099-01-01T00:00:00",
                "end": "2099-01-02T00:00:00",
                "timezone": "UTC",
                "matching_conditions": ["base=main"],
                "exclude_conditions": ["label=hotfix"],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        let matching = ["base=main".to_string()];
        let exclude = ["label=hotfix".to_string()];
        run(
            CreateOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                reason: "emergency-fix",
                timezone: Some("UTC"),
                start: Some(start),
                end: Some(end),
                matching_conditions: &matching,
                exclude_conditions: &exclude,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let stdout = cap.stdout();
        assert!(
            stdout.contains("Freeze created successfully"),
            "got: {stdout}"
        );
        assert!(
            stdout.contains("11111111-2222-3333-4444-555555555555"),
            "got: {stdout}"
        );
        assert!(stdout.contains("emergency-fix"), "got: {stdout}");
    }

    #[tokio::test]
    async fn run_omits_conditions_keys_when_empty() {
        // Python's API client omits `matching_conditions` /
        // `exclude_conditions` when the user passes no `-c` / `-e`
        // flags. The Mergify API may treat absent-vs-empty
        // differently, so the Rust port must keep the same wire
        // shape.
        let server = MockServer::start().await;

        // Match the request body, capture the raw bytes via a fixed
        // response, then read them back through `received_requests`.
        Mock::given(method("POST"))
            .and(path("/v1/repos/owner/repo/scheduled_freeze"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "abc",
                "reason": "no-conditions",
                "start": null,
                "end": null,
                "timezone": "UTC",
                "matching_conditions": [],
                "exclude_conditions": [],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        let empty: [String; 0] = [];
        run(
            CreateOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                reason: "no-conditions",
                timezone: Some("UTC"),
                start: None,
                end: None,
                matching_conditions: &empty,
                exclude_conditions: &empty,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let map = body.as_object().unwrap();
        assert!(
            !map.contains_key("matching_conditions"),
            "matching_conditions key must be omitted when empty, got body: {body}"
        );
        assert!(
            !map.contains_key("exclude_conditions"),
            "exclude_conditions key must be omitted when empty, got body: {body}"
        );
        // `start` / `end` are always present (with `null` for
        // open-ended freezes) — match Python's
        // `CreateScheduledFreezePayload` total fields.
        assert!(map.contains_key("start"));
        assert!(map.contains_key("end"));
        assert!(map["start"].is_null());
        assert!(map["end"].is_null());
    }
}
