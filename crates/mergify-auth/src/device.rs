//! The OAuth 2.0 device authorization grant (RFC 8628), client side.
//!
//! Three unauthenticated endpoints on the Mergify API:
//!
//! | Step | Endpoint |
//! |---|---|
//! | Ask for a pair of codes | `POST /v1/oauth/device/code` |
//! | Poll until the owner approves | `POST /v1/oauth/token` |
//! | Give the credential back (RFC 7009) | `POST /v1/oauth/revoke` |
//!
//! The device grant rather than a browser redirect because the CLI
//! runs where there is no browser to redirect to and no port to
//! listen on: over SSH, in a container, on a build machine. The
//! user reads a code off the terminal and approves it wherever they
//! already have a Mergify session.
//!
//! Everything the flow needs comes from the server's own
//! authorization response — the verification URL included. A
//! self-hosted deployment answers with its own dashboard, so a
//! client that hardcoded `dashboard.mergify.com` would send those
//! users to a page that knows nothing about their grant.

use std::time::Duration;
use std::time::Instant;

use mergify_core::ApiFlavor;
use mergify_core::ApiOutcome;
use mergify_core::CliError;
use mergify_core::HttpClient;
use serde::Deserialize;
use url::Url;

/// The `client_id` this CLI presents. The server keeps a fixed
/// allowlist and answers an unregistered id with `invalid_client`,
/// which is the anti-phishing control of the whole design: a device
/// grant open to any client lets anyone put their own words on our
/// approval page.
pub const CLIENT_ID: &str = "mergify-cli";

const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

const DEVICE_CODE_PATH: &str = "/v1/oauth/device/code";
const TOKEN_PATH: &str = "/v1/oauth/token";
const REVOKE_PATH: &str = "/v1/oauth/revoke";

/// RFC 8628 §3.5: add five seconds to the interval on every
/// `slow_down`.
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

/// What to say when nothing ever approved the grant. Deliberately
/// about the browser rather than about the network: the approval
/// page is where a refusal is visible, and this client cannot tell a
/// refused approval from an ignored one.
const EXPIRED_MESSAGE: &str = "the login was not approved in time. If the approval page \
     reported a problem — for example that you already hold the maximum number of Mergify \
     tokens — fix it there, then run `mergify auth login` again.";

/// RFC 8628 §3.2 makes `interval` optional and defaults it to five
/// seconds. `expires_in` is required, but a client that fell over
/// because a deployment omitted it would be failing on the one field
/// it can pick a safe value for.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_EXPIRES_IN: Duration = Duration::from_secs(600);

/// Ceilings on what the server is allowed to talk this client into.
/// `--api-url` points the CLI at an arbitrary host, and both numbers
/// come from that host: without a cap, one that answered `interval:
/// 86400` would hang `auth login` for a day and look like a bug in
/// the CLI. Both are far above anything a real deployment sends —
/// ours sends 5 and 600.
const MAX_INTERVAL: Duration = Duration::from_secs(60);
/// …and a floor, which matters more. `interval: 0` from a server
/// turns the poll loop into an unthrottled request flood for as long
/// as the grant lives — RFC 8628 §3.2 defaults the interval to five
/// seconds precisely so a client never does that.
const MIN_INTERVAL: Duration = Duration::from_secs(1);
const MAX_EXPIRES_IN: Duration = Duration::from_secs(30 * 60);
/// `expires_in` needs a floor for the same reason. It is
/// `Option<u64>`, so `expires_in: 0` is not the missing value that
/// would get [`DEFAULT_EXPIRES_IN`] — it is a deadline already
/// behind us, which makes `poll` give up after one request and blame
/// the browser for a number the server chose.
const MIN_EXPIRES_IN: Duration = Duration::from_secs(60);

/// A pending grant, as the authorization endpoint describes it.
#[derive(Clone, Debug, Deserialize)]
pub struct Authorization {
    /// The secret this client polls with. Never displayed.
    pub device_code: String,
    /// What the user types on the verification page. Already
    /// dash-grouped by the server, so display it verbatim rather
    /// than re-grouping it.
    pub user_code: String,
    /// Where the user goes to approve. Derived from the
    /// deployment's own dashboard URL.
    pub verification_uri: String,
    /// The same page with the code pre-filled, when the server
    /// offers it (RFC 8628 §3.2 makes it optional).
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub interval: Option<u64>,
}

/// The credential the token endpoint mints once a grant is approved.
#[derive(Clone, Debug, Deserialize)]
pub struct Token {
    pub access_token: String,
    #[serde(default)]
    pub token_type: Option<String>,
    /// Seconds the token is good for. Advisory: it can be revoked
    /// from the dashboard long before it runs out.
    #[serde(default)]
    pub expires_in: Option<u64>,
    /// Declared because RFC 6749 puts it here and because the field
    /// must not make deserialization fail the day it appears. The
    /// Mergify API issues none today, and using one would need a
    /// refresh grant that does not exist server-side either, so
    /// nothing reads this yet.
    #[serde(default)]
    pub refresh_token: Option<String>,
}

/// How fast [`poll`] polls, and how long it keeps trying.
///
/// Built from the authorization response in production. Tests build
/// one directly with millisecond values, the way `RetryPolicy` lets
/// an HTTP test skip real backoff — a `slow_down` round trip would
/// otherwise cost five real seconds.
#[derive(Copy, Clone, Debug)]
pub struct PollSchedule {
    pub interval: Duration,
    pub slow_down_step: Duration,
    pub expires_in: Duration,
}

impl PollSchedule {
    /// The cadence the server asked for, clamped to something a
    /// person will sit through.
    #[must_use]
    pub fn from_authorization(authorization: &Authorization) -> Self {
        Self {
            interval: authorization
                .interval
                .map_or(DEFAULT_INTERVAL, Duration::from_secs)
                .clamp(MIN_INTERVAL, MAX_INTERVAL),
            slow_down_step: SLOW_DOWN_STEP,
            expires_in: authorization
                .expires_in
                .map_or(DEFAULT_EXPIRES_IN, Duration::from_secs)
                .clamp(MIN_EXPIRES_IN, MAX_EXPIRES_IN),
        }
    }
}

/// The RFC 6749 §5.2 error body all three endpoints answer with.
#[derive(Clone, Debug, Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

impl OAuthError {
    /// What to show the user. The server writes these descriptions
    /// and they say more than the code does — the `access_denied`
    /// you get at the token cap names the cap and the fix — so
    /// prefer them, and fall back to the code when one is missing.
    fn message(&self) -> String {
        // Through `displayable` for the same reason the verification
        // URL is: this text is written by whatever host `--api-url`
        // names and printed by a trusted binary.
        displayable(
            &self
                .error_description
                .clone()
                .unwrap_or_else(|| format!("the Mergify API answered {}", self.error)),
        )
    }
}

/// A client for the three device-grant endpoints.
///
/// They are unauthenticated by design, so this carries no bearer
/// token: `logout` revokes a credential by putting it in the form
/// body, and presenting the same secret as an `Authorization` header
/// as well would be sending it twice for no reason.
pub fn client(api_url: Url) -> Result<HttpClient, CliError> {
    HttpClient::new(api_url, "", ApiFlavor::Mergify)
}

/// Open a grant: ask the server for the code pair the user is about
/// to approve.
pub async fn authorize(client: &HttpClient) -> Result<Authorization, CliError> {
    match client
        .post_form::<Authorization, OAuthError>(DEVICE_CODE_PATH, &[("client_id", CLIENT_ID)])
        .await?
    {
        ApiOutcome::Ok(authorization) => checked(authorization),
        ApiOutcome::Error { body, .. } => Err(CliError::MergifyApi(format!(
            "could not start the login: {}",
            body.message(),
        ))),
    }
}

/// Everything the user is shown during a login comes out of the
/// authorization response, and `--api-url` — or `MERGIFY_API_URL`,
/// which a `.envrc` or a devcontainer can set without the user
/// typing anything — decides which host wrote it. The same two lines
/// above clamp `interval` and `expires_in` against that host; these
/// are the strings.
///
/// So the verification URL has to parse and has to be `http(s)`. A
/// `javascript:` or `file:` URL printed under "Open this URL to
/// authorize the Mergify CLI" is a phishing line in this binary's
/// voice, and a lookalike host is one the user can at least see.
/// And every string that reaches the terminal is stripped of control
/// characters, because an `ESC` in `user_code` can rewrite the URL
/// already on screen.
fn checked(mut authorization: Authorization) -> Result<Authorization, CliError> {
    authorization.user_code = displayable(&authorization.user_code);
    authorization.verification_uri = checked_uri(&authorization.verification_uri)?;
    authorization.verification_uri_complete = authorization
        .verification_uri_complete
        .as_deref()
        .and_then(|raw| match checked_uri(raw) {
            Ok(uri) => Some(uri),
            // RFC 8628 §3.2 makes this one optional, so dropping it
            // costs the user a pre-filled code while refusing would
            // cost them the login.
            Err(e) => {
                tracing::debug!(error = %e, "ignoring an unusable verification_uri_complete");
                None
            }
        });
    Ok(authorization)
}

fn checked_uri(raw: &str) -> Result<String, CliError> {
    let url = Url::parse(raw).map_err(|e| {
        CliError::MergifyApi(format!(
            "the deployment answered with a verification URL this client will not print \
             ({e}): {}",
            displayable(raw),
        ))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(CliError::MergifyApi(format!(
            "the deployment answered with a {} verification URL, and only http(s) is ever \
             printed: {}",
            url.scheme(),
            displayable(raw),
        )));
    }
    Ok(url.to_string())
}

/// Drop what a terminal would act on rather than print.
fn displayable(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_control()).collect()
}

/// Poll the token endpoint until the grant is approved, refused, or
/// out of time.
///
/// The three outcomes the RFC makes a client sit through — nobody
/// has answered yet, you are polling too fast, the code is gone —
/// are the ordinary path here, not failures. Anything else is
/// terminal on the first answer.
pub async fn poll(
    client: &HttpClient,
    device_code: &str,
    schedule: PollSchedule,
) -> Result<Token, CliError> {
    let form = [
        ("grant_type", GRANT_TYPE),
        ("device_code", device_code),
        ("client_id", CLIENT_ID),
    ];
    let deadline = Instant::now() + schedule.expires_in;
    let mut interval = schedule.interval;
    let mut unreachable: Option<CliError> = None;

    loop {
        // Before the first request, not after: the user has to read
        // a code and switch to a browser, so an immediate poll can
        // only ever be told nobody has approved yet.
        tokio::time::sleep(interval).await;

        match client
            .post_form::<Token, OAuthError>(TOKEN_PATH, &form)
            .await
        {
            Ok(ApiOutcome::Ok(token)) => return Ok(token),
            Ok(ApiOutcome::Error { body, .. }) => {
                // The endpoint answered, so an earlier transport
                // failure is history: a deadline reached from here on
                // is about the browser again, not about the network.
                if unreachable.is_some() {
                    unreachable = None;
                }
                match body.error.as_str() {
                    "authorization_pending" => {}
                    "slow_down" => {
                        interval = (interval + schedule.slow_down_step).min(MAX_INTERVAL);
                        tracing::debug!(?interval, "the server asked us to poll more slowly");
                    }
                    _ => return Err(CliError::MergifyApi(body.message())),
                }
            }
            // Not a refusal. RFC 8628 §3.5 makes only `access_denied`
            // and `expired_token` terminal, and a proxy's 502, a
            // dropped connection or an exhausted rate limit is
            // neither — while the grant the user may have approved
            // one second ago is still open. Giving up here abandons a
            // token the server mints and nothing ever stores or
            // revokes, which is one of the twenty a user gets. So
            // keep polling to the deadline, and if that is how it
            // ends, report *this* rather than blaming the browser.
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "the token endpoint did not answer; still polling"
                );
                unreachable = Some(e);
            }
        }

        // A backstop, not the expiry: the server drops the grant on
        // its own and starts answering `expired_token`, which is the
        // branch above. This one catches a deployment that would
        // keep saying `authorization_pending` forever.
        //
        // The message points at the browser because that is where the
        // answer is. An approval the server refuses — the twenty-token
        // cap is refused on the approval page itself — never reaches
        // this client as anything, so "nobody approved it" and "the
        // page said no" are indistinguishable from here.
        if Instant::now() >= deadline {
            return Err(
                unreachable.unwrap_or_else(|| CliError::MergifyApi(EXPIRED_MESSAGE.to_string()))
            );
        }
    }
}

/// Revoke a credential (RFC 7009).
///
/// The endpoint answers 200 for any string, including one that was
/// never a token: saying otherwise would make an unauthenticated
/// endpoint an oracle for which tokens exist. So a successful call
/// means "the server accepted the request", never "that token was
/// real" — a caller must not report the difference it cannot see.
pub async fn revoke(client: &HttpClient, token: &str) -> Result<(), CliError> {
    client
        .post_form_no_response(REVOKE_PATH, &[("token", token), ("client_id", CLIENT_ID)])
        .await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mergify_core::RetryPolicy;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::body_string_contains;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    /// A client with the real retry policy's shape but no wall-clock
    /// backoff, so a 5xx test does not sleep.
    fn test_client(server: &MockServer) -> HttpClient {
        HttpClient::with_retry_policy(
            Url::parse(&server.uri()).unwrap(),
            "",
            ApiFlavor::Mergify,
            RetryPolicy {
                max_attempts: 3,
                initial_backoff: Duration::from_millis(0),
            },
        )
        .unwrap()
    }

    fn fast_schedule() -> PollSchedule {
        PollSchedule {
            interval: Duration::from_millis(0),
            slow_down_step: Duration::from_millis(300),
            expires_in: Duration::from_secs(30),
        }
    }

    fn oauth_error(status: u16, code: &str, description: &str) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(serde_json::json!({
            "error": code,
            "error_description": description,
        }))
    }

    #[tokio::test]
    async fn authorize_sends_the_client_id_and_returns_both_codes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/device/code"))
            .and(body_string_contains("client_id=mergify-cli"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-secret",
                "user_code": "BCDF-GHJK",
                "verification_uri": "https://dashboard.mergify.com/device",
                "verification_uri_complete":
                    "https://dashboard.mergify.com/device?user_code=BCDF-GHJK",
                "expires_in": 600,
                "interval": 5,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let authorization = authorize(&test_client(&server)).await.unwrap();
        assert_eq!(authorization.device_code, "dev-secret");
        assert_eq!(authorization.user_code, "BCDF-GHJK");
        assert_eq!(
            authorization.verification_uri,
            "https://dashboard.mergify.com/device",
        );
        let schedule = PollSchedule::from_authorization(&authorization);
        assert_eq!(schedule.interval, Duration::from_secs(5));
        assert_eq!(schedule.expires_in, Duration::from_secs(600));
    }

    // A deployment that omits the two optional numbers still has to
    // produce a usable schedule, at the RFC's defaults.
    #[tokio::test]
    async fn authorize_tolerates_a_response_without_interval_or_expiry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-secret",
                "user_code": "BCDF-GHJK",
                "verification_uri": "https://dashboard.mergify.com/device",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let authorization = authorize(&test_client(&server)).await.unwrap();
        assert_eq!(authorization.verification_uri_complete, None);
        let schedule = PollSchedule::from_authorization(&authorization);
        assert_eq!(schedule.interval, DEFAULT_INTERVAL);
        assert_eq!(schedule.expires_in, DEFAULT_EXPIRES_IN);
    }

    // `--api-url` points at whatever host the caller names, and both
    // numbers come from that host.
    #[test]
    fn a_server_cannot_talk_the_client_into_waiting_forever() {
        let authorization = Authorization {
            device_code: "d".to_string(),
            user_code: "u".to_string(),
            verification_uri: "https://example.test/device".to_string(),
            verification_uri_complete: None,
            expires_in: Some(86_400),
            interval: Some(86_400),
        };
        // Literal bounds, not the constants that do the clamping:
        // asserting `== MAX_INTERVAL` passes for any value of
        // `MAX_INTERVAL`, including one that reintroduces the bug.
        let schedule = PollSchedule::from_authorization(&authorization);
        assert_eq!(schedule.interval, Duration::from_secs(60));
        assert_eq!(schedule.expires_in, Duration::from_secs(30 * 60));
    }

    // The other end of the same threat, and the worse one: a zero
    // interval is not a slow client, it is an unthrottled flood
    // aimed at whatever host `--api-url` names.
    #[test]
    fn a_server_cannot_talk_the_client_into_polling_flat_out() {
        let authorization = Authorization {
            device_code: "d".to_string(),
            user_code: "u".to_string(),
            verification_uri: "https://example.test/device".to_string(),
            verification_uri_complete: None,
            expires_in: Some(600),
            interval: Some(0),
        };
        assert_eq!(
            PollSchedule::from_authorization(&authorization).interval,
            Duration::from_secs(1),
        );
    }

    // And the same threat aimed the other way: `expires_in: 0` is a
    // deadline already behind us, so `poll` would make one request
    // and tell the user their browser was too slow.
    #[test]
    fn a_server_cannot_talk_the_client_into_giving_up_instantly() {
        let authorization = Authorization {
            device_code: "d".to_string(),
            user_code: "u".to_string(),
            verification_uri: "https://example.test/device".to_string(),
            verification_uri_complete: None,
            expires_in: Some(0),
            interval: Some(5),
        };
        assert_eq!(
            PollSchedule::from_authorization(&authorization).expires_in,
            Duration::from_secs(60),
        );
    }

    #[tokio::test]
    async fn a_verification_url_that_is_not_http_is_refused() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-secret",
                "user_code": "BCDF-GHJK",
                "verification_uri": "javascript:fetch('https://attacker.test')",
                "expires_in": 600,
                "interval": 5,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let err = authorize(&test_client(&server)).await.unwrap_err();
        assert!(err.to_string().contains("only http(s)"), "got {err}");
    }

    // A server-written string reaches the terminal inside a line this
    // binary signs its own name to. An `ESC` in it can rewrite the
    // URL printed above.
    #[tokio::test]
    async fn a_hostile_code_cannot_rewrite_what_is_already_on_screen() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-secret",
                "user_code": "BCDF\r\u{1b}[2Kgo to https://attacker.test",
                "verification_uri": "https://dashboard.mergify.com/device",
                "verification_uri_complete": "not a url at all",
                "expires_in": 600,
                "interval": 5,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let authorization = authorize(&test_client(&server)).await.unwrap();
        assert_eq!(
            authorization.user_code,
            "BCDF[2Kgo to https://attacker.test",
        );
        // The optional pre-filled URL is dropped rather than fatal:
        // the login still works off `verification_uri`.
        assert_eq!(authorization.verification_uri_complete, None);
    }

    // RFC 8628 §3.5 makes only `access_denied` and `expired_token`
    // terminal. A 502 from a proxy is neither, and the grant the user
    // may already have approved is still open — abandoning it leaks a
    // token that is minted, never stored and never revoked.
    #[tokio::test]
    async fn poll_survives_an_endpoint_that_stops_answering() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(502).set_body_string("<html>bad gateway</html>"))
            .up_to_n_times(3)
            .expect(3)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "mut_secret",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let token = poll(&test_client(&server), "dev-secret", fast_schedule())
            .await
            .unwrap();
        assert_eq!(token.access_token, "mut_secret");
    }

    // ...and when that is how the whole window ends, the error says
    // the endpoint never answered rather than blaming the browser.
    #[tokio::test]
    async fn poll_reports_the_transport_failure_it_died_on() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(502).set_body_string("bad gateway"))
            .mount(&server)
            .await;

        let schedule = PollSchedule {
            interval: Duration::from_millis(0),
            slow_down_step: Duration::from_millis(0),
            expires_in: Duration::from_millis(0),
        };
        let err = poll(&test_client(&server), "dev-secret", schedule)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("502"), "got {err}");
        assert!(
            !err.to_string().contains("not approved in time"),
            "the browser is not what failed here: got {err}",
        );
    }

    #[tokio::test]
    async fn authorize_surfaces_an_unregistered_client_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/device/code"))
            .respond_with(oauth_error(401, "invalid_client", "Unknown client_id."))
            .expect(1)
            .mount(&server)
            .await;

        let err = authorize(&test_client(&server)).await.unwrap_err();
        assert!(err.to_string().contains("Unknown client_id."), "got {err}");
        assert_eq!(err.exit_code(), mergify_core::ExitCode::MergifyApiError);
    }

    #[tokio::test]
    async fn poll_waits_out_authorization_pending() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .and(body_string_contains("device_code=dev-secret"))
            .respond_with(oauth_error(
                400,
                "authorization_pending",
                "Waiting for the user to approve.",
            ))
            .up_to_n_times(2)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "mut_secret",
                "token_type": "bearer",
                "expires_in": 31_536_000,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let token = poll(&test_client(&server), "dev-secret", fast_schedule())
            .await
            .unwrap();
        assert_eq!(token.access_token, "mut_secret");
        assert_eq!(token.expires_in, Some(31_536_000));
        assert_eq!(token.refresh_token, None);
    }

    // RFC 8628 §3.5: `slow_down` means "add five seconds", not "give
    // up". Measured rather than asserted on a field, because the
    // interval only exists as a sleep.
    #[tokio::test]
    async fn poll_backs_off_when_told_to_slow_down() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(oauth_error(400, "slow_down", "Polling too fast."))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "mut_secret",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let started = Instant::now();
        let token = poll(&test_client(&server), "dev-secret", fast_schedule())
            .await
            .unwrap();
        assert_eq!(token.access_token, "mut_secret");
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "the second poll must wait one slow_down step longer, waited {:?}",
            started.elapsed(),
        );
    }

    // A refusal comes back as a sentence the server wrote, and it
    // says more than the error code does. Showing `access_denied`
    // instead would drop the only part the user can act on.
    #[tokio::test]
    async fn poll_surfaces_the_server_description_of_a_refusal() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(oauth_error(
                400,
                "access_denied",
                "You already hold the maximum number of Mergify tokens. \
                 Revoke one from the dashboard and try again.",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let err = poll(&test_client(&server), "dev-secret", fast_schedule())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Revoke one from the dashboard"),
            "got {err}",
        );
    }

    #[tokio::test]
    async fn poll_stops_on_an_expired_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(oauth_error(
                400,
                "expired_token",
                "Unknown or expired device_code.",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let err = poll(&test_client(&server), "dev-secret", fast_schedule())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("expired"), "got {err}");
    }

    // The backstop: a deployment that answers `authorization_pending`
    // forever must not hang the command forever.
    #[tokio::test]
    async fn poll_gives_up_at_its_own_deadline() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(oauth_error(
                400,
                "authorization_pending",
                "Waiting for the user to approve.",
            ))
            .mount(&server)
            .await;

        let schedule = PollSchedule {
            interval: Duration::from_millis(0),
            slow_down_step: Duration::from_millis(0),
            expires_in: Duration::from_millis(0),
        };
        let err = poll(&test_client(&server), "dev-secret", schedule)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not approved in time"),
            "got {err}"
        );
        assert!(
            err.to_string().contains("approval page"),
            "the timeout has to point at the browser, where a refusal is visible: got {err}",
        );
    }

    #[tokio::test]
    async fn revoke_posts_the_token_and_the_client_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/revoke"))
            .and(body_string_contains("token=mut_secret"))
            .and(body_string_contains("client_id=mergify-cli"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        revoke(&test_client(&server), "mut_secret").await.unwrap();
    }

    // The device-grant client never presents a bearer token: these
    // endpoints are unauthenticated, and `revoke` already carries the
    // secret in the form body.
    #[tokio::test]
    async fn the_client_sends_no_authorization_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/revoke"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = client(Url::parse(&server.uri()).unwrap()).unwrap();
        revoke(&client, "mut_secret").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert!(
            requests[0].headers.get("authorization").is_none(),
            "got {:?}",
            requests[0].headers,
        );
    }
}
