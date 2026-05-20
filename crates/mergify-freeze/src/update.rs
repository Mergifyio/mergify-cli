//! `mergify freeze update` — modify an existing scheduled freeze.
//!
//! `PATCH /v1/repos/<repo>/scheduled_freeze/<id>`. The payload
//! includes only the fields the user actually changed (Python's
//! `UpdateScheduledFreezePayload` is a `TypedDict` whose entries
//! are conditionally inserted) — we replicate that with
//! `skip_serializing_if = "Option::is_none"` so unspecified fields
//! stay absent on the wire rather than being sent as `null`.

use std::io::Write;

use chrono::NaiveDateTime;
use mergify_core::ApiFlavor;
use mergify_core::CliError;
use mergify_core::HttpClient;
use mergify_core::Output;
use mergify_core::auth;
use serde::Serialize;

use crate::common::NaiveDateTimeWire;
use crate::common::ScheduledFreeze;
use crate::common::write_freeze;

pub struct UpdateOptions<'a> {
    pub repository: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
    pub freeze_id: &'a str,
    pub reason: Option<&'a str>,
    pub timezone: Option<&'a str>,
    pub start: Option<NaiveDateTime>,
    pub end: Option<NaiveDateTime>,
    /// `Some(empty slice)` is treated the same as `None` — matches
    /// the Python behavior where `matching_conditions` is only
    /// included when the user passed `-c` at least once.
    pub matching_conditions: Option<&'a [String]>,
    pub exclude_conditions: Option<&'a [String]>,
}

#[derive(Serialize, Default)]
struct UpdatePayload<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timezone: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    matching_conditions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_conditions: Option<Vec<String>>,
}

/// Run the `freeze update` command.
pub async fn run(opts: UpdateOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let repository = auth::resolve_repository(opts.repository)?;
    let token = auth::resolve_token(opts.token)?;
    let api_url = auth::resolve_api_url(opts.api_url)?;

    let payload = UpdatePayload {
        reason: opts.reason,
        timezone: opts.timezone,
        start: opts.start.as_ref().map(|dt| NaiveDateTimeWire(dt).iso()),
        end: opts.end.as_ref().map(|dt| NaiveDateTimeWire(dt).iso()),
        matching_conditions: opts.matching_conditions.map(<[String]>::to_vec),
        exclude_conditions: opts.exclude_conditions.map(<[String]>::to_vec),
    };

    output.status(&format!(
        "Updating scheduled freeze {id} on {repository}…",
        id = opts.freeze_id,
    ))?;

    let client = HttpClient::new(api_url, token, ApiFlavor::Mergify)?;
    let path = format!(
        "/v1/repos/{repository}/scheduled_freeze/{id}",
        id = opts.freeze_id,
    );
    let freeze: ScheduledFreeze = client.patch(&path, &payload).await?;

    output.emit(&(), &mut |w: &mut dyn Write| {
        writeln!(w, "Freeze updated successfully:")?;
        write_freeze(w, &freeze)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use mergify_core::OutputMode;
    use mergify_core::StdioOutput;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    type SharedBytes = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

    struct Captured {
        output: StdioOutput,
        stdout: SharedBytes,
    }

    fn make_output() -> Captured {
        let stdout: SharedBytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stderr: SharedBytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = StdioOutput::with_sinks(
            OutputMode::Human,
            SharedWriter(std::sync::Arc::clone(&stdout)),
            SharedWriter(std::sync::Arc::clone(&stderr)),
        );
        Captured { output, stdout }
    }

    struct SharedWriter(SharedBytes);
    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn run_patches_only_user_supplied_fields() {
        // The whole point of PATCH semantics: only the fields the
        // user changed go on the wire. Mirrors Python's "if
        // <field> is not None: payload[<field>] = ..." chain in
        // `update_freeze`.
        let server = MockServer::start().await;
        let freeze_id = "11111111-2222-3333-4444-555555555555";
        Mock::given(method("PATCH"))
            .and(path(format!(
                "/v1/repos/owner/repo/scheduled_freeze/{freeze_id}"
            )))
            .and(header("Authorization", "Bearer t"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": freeze_id,
                "reason": "new-reason",
                "start": "2099-01-01T00:00:00",
                "end": null,
                "timezone": "UTC",
                "matching_conditions": [],
                "exclude_conditions": [],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = make_output();
        let api_url = server.uri();
        run(
            UpdateOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                freeze_id,
                reason: Some("new-reason"),
                timezone: None,
                start: None,
                end: None,
                matching_conditions: None,
                exclude_conditions: None,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let map = body.as_object().unwrap();
        assert_eq!(
            map.get("reason").and_then(|v| v.as_str()),
            Some("new-reason")
        );
        // Only `reason` was set — every other field must be absent
        // from the request body so the server's PATCH semantics
        // leave them untouched.
        for absent in [
            "timezone",
            "start",
            "end",
            "matching_conditions",
            "exclude_conditions",
        ] {
            assert!(
                !map.contains_key(absent),
                "{absent} must be omitted, got body: {body}"
            );
        }

        let stdout = String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap();
        assert!(
            stdout.contains("Freeze updated successfully"),
            "got: {stdout}"
        );
        assert!(stdout.contains("new-reason"), "got: {stdout}");
    }

    #[tokio::test]
    async fn run_sends_empty_conditions_list_when_provided() {
        // `Some(&[])` (explicit "clear the conditions list") is
        // wire-different from `None` (don't touch). The Mergify API
        // distinguishes them, and Python sends the empty list when
        // the user passes the flag. Mirror that.
        let server = MockServer::start().await;
        let freeze_id = "abc";
        Mock::given(method("PATCH"))
            .and(path(format!(
                "/v1/repos/owner/repo/scheduled_freeze/{freeze_id}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": freeze_id,
                "reason": "r",
                "start": null,
                "end": null,
                "timezone": "UTC",
                "matching_conditions": [],
                "exclude_conditions": [],
            })))
            .mount(&server)
            .await;

        let mut cap = make_output();
        let api_url = server.uri();
        let empty: [String; 0] = [];
        run(
            UpdateOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                freeze_id,
                reason: None,
                timezone: None,
                start: None,
                end: None,
                matching_conditions: Some(&empty),
                exclude_conditions: None,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let map = body.as_object().unwrap();
        assert!(map.contains_key("matching_conditions"));
        assert_eq!(map["matching_conditions"], json!([]));
        assert!(!map.contains_key("exclude_conditions"));
    }
}
