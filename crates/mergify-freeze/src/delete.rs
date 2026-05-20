//! `mergify freeze delete` — remove a scheduled freeze.
//!
//! `POST /v1/repos/<repo>/scheduled_freeze/<id>/delete`. The
//! endpoint is `POST … /delete` (not a `DELETE` verb) because
//! deleting an *active* freeze requires an audit reason — the
//! request body carries `{"delete_reason": "<text>"}`. We mirror
//! Python's payload shape: include `delete_reason` only when the
//! user provided one, otherwise send an empty `{}` (no key).

use std::io::Write;

use mergify_core::CliError;
use mergify_core::CommandContext;
use mergify_core::Output;
use serde::Serialize;

pub struct DeleteOptions<'a> {
    pub repository: Option<&'a str>,
    pub token: Option<&'a str>,
    pub api_url: Option<&'a str>,
    pub freeze_id: &'a str,
    /// Required by the API when the target freeze is active; the
    /// CLI doesn't validate ahead of time and lets the server
    /// reject a missing reason for an active freeze with its own
    /// 4xx + message.
    pub delete_reason: Option<&'a str>,
}

#[derive(Serialize, Default)]
struct DeletePayload<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    delete_reason: Option<&'a str>,
}

/// Run the `freeze delete` command.
pub async fn run(opts: DeleteOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let ctx = CommandContext::resolve(opts.repository, opts.token, opts.api_url)?;

    output.status(&format!(
        "Deleting scheduled freeze {id} on {repo}…",
        id = opts.freeze_id,
        repo = ctx.repository,
    ))?;

    let payload = DeletePayload {
        delete_reason: opts.delete_reason,
    };

    let client = ctx.mergify_client()?;
    let path = format!(
        "/v1/repos/{repo}/scheduled_freeze/{id}/delete",
        repo = ctx.repository,
        id = opts.freeze_id,
    );
    client.post_no_response(&path, &payload).await?;

    output.emit(&(), &mut |w: &mut dyn Write| {
        writeln!(w, "Freeze deleted successfully.")
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use mergify_test_support::Captured;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    #[tokio::test]
    async fn run_posts_empty_body_when_no_reason_provided() {
        let server = MockServer::start().await;
        let freeze_id = "abc-123";
        Mock::given(method("POST"))
            .and(path(format!(
                "/v1/repos/owner/repo/scheduled_freeze/{freeze_id}/delete"
            )))
            .and(header("Authorization", "Bearer t"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            DeleteOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                freeze_id,
                delete_reason: None,
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let map = body.as_object().unwrap();
        // Active vs inactive: an empty body is the right shape when
        // the freeze isn't active. The server decides whether to
        // require the key.
        assert!(map.is_empty(), "expected `{{}}` body, got {body}");

        let stdout = cap.stdout();
        assert!(
            stdout.contains("Freeze deleted successfully"),
            "got: {stdout}"
        );
    }

    #[tokio::test]
    async fn run_includes_delete_reason_when_provided() {
        let server = MockServer::start().await;
        let freeze_id = "abc-123";
        Mock::given(method("POST"))
            .and(path(format!(
                "/v1/repos/owner/repo/scheduled_freeze/{freeze_id}/delete"
            )))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let mut cap = Captured::human();
        let api_url = server.uri();
        run(
            DeleteOptions {
                repository: Some("owner/repo"),
                token: Some("t"),
                api_url: Some(&api_url),
                freeze_id,
                delete_reason: Some("audit-trail"),
            },
            &mut cap.output,
        )
        .await
        .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            body.get("delete_reason").and_then(|v| v.as_str()),
            Some("audit-trail"),
            "got body: {body}"
        );
    }
}
