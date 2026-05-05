//! HTTP client wrapper used by all ported commands.
//!
//! Wraps [`reqwest::Client`] with:
//!
//! - Bearer-token auth (injected if the token is non-empty).
//! - Tenacity-style retry on 5xx and transient network errors
//!   (3 attempts, exponential backoff: 1s, 2s).
//! - Typed error mapping to [`CliError::GitHubApi`] or
//!   [`CliError::MergifyApi`] depending on the configured
//!   [`ApiFlavor`].
//! - Per-request timeout (30s default).
//!
//! Command crates must never import [`reqwest`] directly — they go
//! through [`Client::get`], [`Client::post`], or
//! [`Client::post_no_response`] (for endpoints that return an empty
//! body on success).

use std::time::Duration;

use reqwest::StatusCode;
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::error::CliError;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Cap on how many bytes of an error response body we surface in
/// `CliError`. A misbehaving server can return arbitrarily large
/// payloads; truncating keeps the CLI output sane and bounds memory
/// use.
const MAX_ERROR_BODY_BYTES: usize = 4 * 1024;

/// Which backend the client talks to. Determines whether HTTP
/// failures are mapped to [`CliError::GitHubApi`] or
/// [`CliError::MergifyApi`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ApiFlavor {
    GitHub,
    Mergify,
}

/// Retry policy for transient failures. Only 5xx responses and
/// connect/timeout errors are retried; 4xx responses are never
/// retried — those are caller errors and retrying would hide bugs.
#[derive(Copy, Clone, Debug)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
        }
    }
}

pub struct Client {
    inner: reqwest::Client,
    base_url: Url,
    flavor: ApiFlavor,
    token: Option<String>,
    retry: RetryPolicy,
}

impl Client {
    /// Build a client with the default retry policy.
    pub fn new(
        base_url: Url,
        token: impl Into<String>,
        flavor: ApiFlavor,
    ) -> Result<Self, CliError> {
        Self::with_retry_policy(base_url, token, flavor, RetryPolicy::default())
    }

    /// Build a client with a custom retry policy. Used by tests to
    /// skip the real-wall-clock backoff delay.
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Generic`] when `retry.max_attempts` is
    /// `0` — a zero-attempt policy would cause every request to
    /// short-circuit with a misleading "failed without response"
    /// message.
    pub fn with_retry_policy(
        base_url: Url,
        token: impl Into<String>,
        flavor: ApiFlavor,
        retry: RetryPolicy,
    ) -> Result<Self, CliError> {
        if retry.max_attempts == 0 {
            return Err(CliError::Generic(
                "RetryPolicy::max_attempts must be at least 1".to_string(),
            ));
        }
        let token_str = token.into();
        let token_opt = (!token_str.is_empty()).then_some(token_str);
        let inner = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| CliError::Generic(format!("build HTTP client: {e}")))?;
        Ok(Self {
            inner,
            base_url,
            flavor,
            token: token_opt,
            retry,
        })
    }

    /// GET `path` and deserialize the JSON body as `T`.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, CliError> {
        let url = self.join(path)?;
        let resp = self.execute_request(self.inner.get(url)).await?;
        self.decode_json(resp).await
    }

    /// POST `body` as JSON to `path` and deserialize the JSON
    /// response as `T`.
    pub async fn post<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, CliError> {
        let url = self.join(path)?;
        let resp = self
            .execute_request(self.inner.post(url).json(body))
            .await?;
        self.decode_json(resp).await
    }

    /// POST `body` as JSON to `path` and discard the response body.
    /// Use when the endpoint returns an empty body (or any body the
    /// caller does not care about) on success — `post::<Value>` would
    /// fail to deserialize an empty response.
    pub async fn post_no_response<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), CliError> {
        let url = self.join(path)?;
        self.execute_request(self.inner.post(url).json(body))
            .await
            .map(drop)
    }

    fn join(&self, path: &str) -> Result<Url, CliError> {
        // `Url::join` accepts absolute URLs and protocol-relative
        // paths (`//host/...`), which would let a caller-supplied
        // `path` swap out `base_url`'s authority and leak the bearer
        // token to an arbitrary host. Reject both up front.
        if path.starts_with("//") || Url::parse(path).is_ok() {
            return Err(self.api_error(format!(
                "invalid path {path:?}: absolute URLs are not allowed"
            )));
        }
        self.base_url
            .join(path)
            .map_err(|e| self.api_error(format!("invalid path {path:?}: {e}")))
    }

    async fn execute_request(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, CliError> {
        let mut backoff = self.retry.initial_backoff;
        let mut last_message = String::from("HTTP request failed without response");

        for attempt in 0..self.retry.max_attempts {
            let Some(cloned) = builder.try_clone() else {
                return Err(self.api_error(
                    "request body is not cloneable (streaming?) — cannot retry".into(),
                ));
            };
            let req = match &self.token {
                Some(token) => cloned.bearer_auth(token),
                None => cloned,
            };

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(resp);
                    }
                    last_message = error_message(status, resp).await;
                    if status.is_server_error() && attempt + 1 < self.retry.max_attempts {
                        tokio::time::sleep(backoff).await;
                        backoff *= 2;
                        continue;
                    }
                    return Err(self.api_error(last_message));
                }
                Err(e) if is_transient(&e) && attempt + 1 < self.retry.max_attempts => {
                    last_message = format!("network error: {e}");
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                }
                Err(e) => {
                    let msg = if e.is_timeout() {
                        format!(
                            "{} did not respond in time. The request was aborted — please retry.",
                            self.service_name()
                        )
                    } else if e.is_connect() {
                        format!("could not reach {}: {e}", self.service_name())
                    } else {
                        format!("request failed: {e}")
                    };
                    return Err(self.api_error(msg));
                }
            }
        }
        Err(self.api_error(last_message))
    }

    async fn decode_json<T: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, CliError> {
        resp.json::<T>()
            .await
            .map_err(|e| self.api_error(format!("parse response JSON: {e}")))
    }

    fn api_error(&self, message: String) -> CliError {
        match self.flavor {
            ApiFlavor::GitHub => CliError::GitHubApi(message),
            ApiFlavor::Mergify => CliError::MergifyApi(message),
        }
    }

    fn service_name(&self) -> &'static str {
        match self.flavor {
            ApiFlavor::GitHub => "GitHub",
            ApiFlavor::Mergify => "Mergify",
        }
    }
}

fn is_transient(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect()
}

async fn error_message(status: StatusCode, mut resp: reqwest::Response) -> String {
    // Stream chunks until we've buffered at most `MAX_ERROR_BODY_BYTES`,
    // then drop the rest. `Response::text()` would slurp the entire
    // body into memory regardless of size.
    let mut body: Vec<u8> = Vec::new();
    let mut truncated = false;
    while let Ok(Some(chunk)) = resp.chunk().await {
        if body.len() + chunk.len() > MAX_ERROR_BODY_BYTES {
            let remaining = MAX_ERROR_BODY_BYTES - body.len();
            body.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }
    let mut text = String::from_utf8_lossy(&body).into_owned();
    if truncated {
        text.push_str("…[truncated]");
    }
    if text.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {text}")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;
    use std::sync::atomic::Ordering;

    use serde::Deserialize;
    use serde::Serialize;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::Request;
    use wiremock::Respond;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::body_json;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Foo {
        bar: u32,
    }

    fn fast_client(server: &MockServer, flavor: ApiFlavor) -> Client {
        Client::with_retry_policy(
            Url::parse(&server.uri()).unwrap(),
            "test-token",
            flavor,
            RetryPolicy {
                max_attempts: 3,
                initial_backoff: Duration::from_millis(0),
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn get_deserializes_json_and_injects_bearer_auth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/foo"))
            .and(header("Authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(Foo { bar: 42 }))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let got: Foo = client.get("/foo").await.unwrap();
        assert_eq!(got, Foo { bar: 42 });
    }

    #[tokio::test]
    async fn empty_token_skips_auth_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/foo"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"bar": 1})))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::with_retry_policy(
            Url::parse(&server.uri()).unwrap(),
            "",
            ApiFlavor::GitHub,
            RetryPolicy::default(),
        )
        .unwrap();

        let _: Foo = client.get("/foo").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            !requests[0].headers.contains_key("authorization"),
            "expected no Authorization header for empty token"
        );
    }

    #[tokio::test]
    async fn post_no_response_succeeds_on_empty_2xx_body() {
        // Mergify endpoints like POST /scopes return an empty body
        // on success — `post::<Value>` would fail to deserialize.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/empty"))
            .and(body_json(Foo { bar: 1 }))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        client
            .post_no_response("/empty", &Foo { bar: 1 })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn post_no_response_propagates_4xx() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/empty"))
            .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let err = client
            .post_no_response("/empty", &Foo { bar: 1 })
            .await
            .unwrap_err();
        assert!(matches!(err, CliError::MergifyApi(_)));
        assert!(err.to_string().contains("404"));
    }

    #[tokio::test]
    async fn post_sends_json_body_and_returns_deserialized_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/simulate"))
            .and(body_json(Foo { bar: 7 }))
            .respond_with(ResponseTemplate::new(200).set_body_json(Foo { bar: 14 }))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let got: Foo = client.post("/simulate", &Foo { bar: 7 }).await.unwrap();
        assert_eq!(got, Foo { bar: 14 });
    }

    struct Flaky {
        attempts: Arc<AtomicU32>,
        fail_first: u32,
    }

    impl Respond for Flaky {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if attempt < self.fail_first {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200).set_body_json(Foo { bar: 99 })
            }
        }
    }

    #[tokio::test]
    async fn retries_5xx_then_succeeds() {
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicU32::new(0));
        Mock::given(method("GET"))
            .and(path("/foo"))
            .respond_with(Flaky {
                attempts: Arc::clone(&attempts),
                fail_first: 2,
            })
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let got: Foo = client.get("/foo").await.unwrap();
        assert_eq!(got, Foo { bar: 99 });
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn exhausted_retries_on_5xx_yield_mergify_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/foo"))
            .respond_with(ResponseTemplate::new(503).set_body_string("service down"))
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let err = client.get::<Foo>("/foo").await.unwrap_err();
        assert!(matches!(err, CliError::MergifyApi(_)));
        let msg = err.to_string();
        assert!(msg.contains("503"), "expected status in message, got {msg}");
    }

    #[tokio::test]
    async fn four_xx_is_not_retried_and_maps_to_github_api_error() {
        let server = MockServer::start().await;
        // `expect(1)` makes wiremock fail the test if a retry is
        // attempted — that's the "not retried" assertion.
        Mock::given(method("GET"))
            .and(path("/foo"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::GitHub);
        let err = client.get::<Foo>("/foo").await.unwrap_err();
        assert!(matches!(err, CliError::GitHubApi(_)));
        let msg = err.to_string();
        assert!(msg.contains("404"), "expected status in message, got {msg}");
    }

    #[tokio::test]
    async fn join_rejects_absolute_url() {
        let server = MockServer::start().await;
        let client = fast_client(&server, ApiFlavor::GitHub);
        let err = client
            .get::<Foo>("https://evil.example/foo")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("absolute URLs are not allowed"));
    }

    #[tokio::test]
    async fn join_rejects_protocol_relative_path() {
        let server = MockServer::start().await;
        let client = fast_client(&server, ApiFlavor::GitHub);
        let err = client.get::<Foo>("//evil.example/foo").await.unwrap_err();
        assert!(err.to_string().contains("absolute URLs are not allowed"));
    }

    #[test]
    fn with_retry_policy_rejects_zero_attempts() {
        let url = Url::parse("https://api.example/").unwrap();
        let result = Client::with_retry_policy(
            url,
            "t",
            ApiFlavor::Mergify,
            RetryPolicy {
                max_attempts: 0,
                initial_backoff: Duration::from_millis(0),
            },
        );
        let Err(err) = result else {
            panic!("expected Err for max_attempts=0");
        };
        assert!(err.to_string().contains("max_attempts"));
    }

    #[tokio::test]
    async fn timeout_yields_did_not_respond_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/foo"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;

        // Custom client with a tight request timeout so the test
        // provokes a real reqwest timeout in milliseconds rather than
        // the production-default 30s.
        let inner = reqwest::Client::builder()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let client = Client {
            inner,
            base_url: Url::parse(&server.uri()).unwrap(),
            flavor: ApiFlavor::GitHub,
            token: Some("test-token".to_string()),
            retry: RetryPolicy {
                max_attempts: 1,
                initial_backoff: Duration::from_millis(0),
            },
        };

        let err = client.get::<Foo>("/foo").await.unwrap_err();
        assert!(matches!(err, CliError::GitHubApi(_)));
        let msg = err.to_string();
        assert!(
            msg.contains("GitHub did not respond in time. The request was aborted — please retry."),
            "expected friendly timeout message, got: {msg}"
        );
    }

    #[tokio::test]
    async fn connect_failure_yields_could_not_reach_message() {
        let inner = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        // Bind, capture port, drop the listener — the port is then
        // guaranteed-closed for the duration of the test, so connect
        // fails fast with ECONNREFUSED. Avoids hard-coding a port like
        // `1` that could happen to be bound on some CI images.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let client = Client {
            inner,
            base_url: Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap(),
            flavor: ApiFlavor::Mergify,
            token: Some("t".to_string()),
            retry: RetryPolicy {
                max_attempts: 1,
                initial_backoff: Duration::from_millis(0),
            },
        };

        let err = client.get::<Foo>("/foo").await.unwrap_err();
        assert!(matches!(err, CliError::MergifyApi(_)));
        let msg = err.to_string();
        assert!(
            msg.contains("could not reach Mergify"),
            "expected connect message, got: {msg}"
        );
    }

    #[tokio::test]
    async fn error_message_truncates_oversized_body() {
        let server = MockServer::start().await;
        // Body just past the cap so we exercise the truncation path
        // without keeping a giant string in test memory.
        let huge = "x".repeat(MAX_ERROR_BODY_BYTES + 1024);
        Mock::given(method("GET"))
            .and(path("/foo"))
            .respond_with(ResponseTemplate::new(404).set_body_string(huge))
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::GitHub);
        let err = client.get::<Foo>("/foo").await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("[truncated]"),
            "expected truncation marker, got len={}",
            msg.len()
        );
        // The message embeds at most MAX_ERROR_BODY_BYTES of body
        // plus a small prefix/suffix; allow some slack for both.
        assert!(
            msg.len() < MAX_ERROR_BODY_BYTES + 256,
            "error message not bounded: len={}",
            msg.len()
        );
    }
}
