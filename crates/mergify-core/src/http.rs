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
//!
//! [`Client::post_form`] is the odd one out: it exists for
//! endpoints whose *error* body is part of the protocol rather
//! than a diagnostic — the OAuth device grant answers `400
//! {"error": "authorization_pending"}` on every poll before its
//! owner approves, and a client that treated that as a failure
//! could never complete the flow.

use std::time::Duration;

use reqwest::StatusCode;
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::error::CliError;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-connection TCP connect timeout, independent of the overall
/// request budget — a black-holed host shouldn't burn the full 30s
/// before the first byte.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound on a rate-limit-dictated wait. GitHub's reset window
/// can be an hour out; the client honours `Retry-After` / reset only
/// up to this cap, then fails fast rather than blocking the CLI on an
/// interactive command.
const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// User-Agent header sent on every request. GitHub's REST API
/// rejects requests without one (`403 Request forbidden by
/// administrative rules`), so this is non-negotiable. Cargo's
/// package version is fine here — calver vs semver doesn't
/// matter to GitHub, only that the header is present and
/// identifies the client.
const USER_AGENT: &str = concat!("mergify-cli/", env!("CARGO_PKG_VERSION"));
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

/// Outcome of [`Client::delete_if_exists`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DeleteOutcome {
    /// 2xx: the resource was deleted.
    Deleted,
    /// 404: the resource didn't exist (or was already gone).
    NotFound,
}

/// Outcome of a request whose non-2xx bodies are part of the
/// endpoint's protocol — see [`Client::post_form`].
///
/// Only *terminal* rejections reach [`Self::Error`]: 5xx and
/// rate-limit responses are retried first, and a body that does not
/// deserialize as `E` is a plain [`CliError`] rather than a
/// protocol answer.
#[derive(Debug, Eq, PartialEq)]
pub enum ApiOutcome<T, E> {
    Ok(T),
    Error { status: u16, body: E },
}

/// One page of a cursor-paginated Mergify list endpoint.
///
/// `next_cursor` is the opaque cursor of the following page,
/// extracted from the response's RFC 5988 `Link` header
/// (`rel="next"`); `None` on the last page. The caller re-issues its
/// own query with `("cursor", …)` appended — only the cursor is
/// taken from the header, never the rest of the echoed URL, so a
/// caller's query cannot be silently rewritten by the server.
pub struct Page<T> {
    pub body: T,
    pub next_cursor: Option<String>,
}

/// What the retry driver does with a **terminal** non-2xx response —
/// one it has already decided not to retry.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum OnTerminalError {
    /// Render it into a [`CliError`]. The default.
    Fail,
    /// 404 short-circuits to `Ok(None)`; everything else fails.
    NotFoundIsNone,
    /// Hand the response back unread so the caller can decode a
    /// protocol-defined error body.
    ReturnResponse,
}

/// Caller hook to remap a terminal non-2xx HTTP status to a domain
/// error before the default flavor mapping. Receives the status as a
/// `u16` (command crates never import `reqwest`) and the rendered
/// error message; returning `Some` overrides the error. `config
/// simulate` uses it to map the simulator's 422 to a config error.
type ErrorClassifier<'a> = &'a (dyn Fn(u16, &str) -> Option<CliError> + Send + Sync);

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
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(USER_AGENT)
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

    /// GET `path`, returning `None` on 404. Other 4xx/5xx responses
    /// surface as the normal `CliError` API failure. Mirrors
    /// [`Self::delete_if_exists`] but for read-only endpoints where
    /// "not found" is a meaningful caller branch (e.g. `queue show`
    /// must distinguish "PR not in queue" from a genuine API
    /// failure).
    pub async fn get_if_exists<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<Option<T>, CliError> {
        let url = self.join(path)?;
        match self.execute_request_optional(self.inner.get(url)).await? {
            Some(resp) => self.decode_json(resp).await.map(Some),
            None => Ok(None),
        }
    }

    /// GET `path` with query-string pairs appended in caller order.
    ///
    /// Repeating the same key is supported (each entry produces its
    /// own `key=value`), and values are percent-encoded so callers can
    /// pass arbitrary strings (`*`, `&`, `?`, spaces, unicode).
    /// An empty `query` slice produces no `?`.
    pub async fn get_with_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, CliError> {
        let mut url = self.join(path)?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let resp = self.execute_request(self.inner.get(url)).await?;
        self.decode_json(resp).await
    }

    /// GET one page of a cursor-paginated endpoint: like
    /// [`Self::get_with_query`], but also return the next page's
    /// cursor from the response's `Link` header (see [`Page`]).
    pub async fn get_page<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<Page<T>, CliError> {
        let mut url = self.join(path)?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let resp = self.execute_request(self.inner.get(url)).await?;
        let next_cursor = resp
            .headers()
            .get(reqwest::header::LINK)
            .and_then(|value| value.to_str().ok())
            .and_then(next_cursor_from_link);
        let body = self.decode_json(resp).await?;
        Ok(Page { body, next_cursor })
    }

    /// GET `path`, decoding the JSON body as `T` on success and as
    /// `E` on a terminal rejection — the [`ApiOutcome`] counterpart
    /// of [`Self::post_form`], for a caller that has to branch on
    /// *which* rejection it got.
    ///
    /// `auth status` is the first caller: a refused credential (403)
    /// and a deployment too old to serve the route (404) are
    /// different sentences to the user, and both are different from
    /// a network failure. Reporting any of the three as another
    /// would be a lie.
    pub async fn get_outcome<T: DeserializeOwned, E: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<ApiOutcome<T, E>, CliError> {
        let url = self.join(path)?;
        let resp = self
            // `OnTerminalError::ReturnResponse` never returns `None`;
            // `Option::expect` documents that invariant.
            .execute_with_retry(self.inner.get(url), OnTerminalError::ReturnResponse, None)
            .await?
            .expect("execute_with_retry returned None despite OnTerminalError::ReturnResponse");
        self.decode_outcome(resp).await
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

    /// POST `body` as JSON to `path` and deserialize the JSON response
    /// as `T`, like [`Self::post`], but consult `classify` on a
    /// terminal non-2xx response before the default flavor mapping:
    /// returning `Some(err)` overrides the error. `config simulate`
    /// uses this to surface the simulator's 422 — an unprocessable
    /// local config — as a [`CliError::Configuration`] (exit 8)
    /// rather than a Mergify API failure (exit 6). The classifier
    /// receives the HTTP status as a `u16` (command crates never
    /// import `reqwest`) and the rendered error message.
    pub async fn post_classifying<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        classify: impl Fn(u16, &str) -> Option<CliError> + Send + Sync,
    ) -> Result<T, CliError> {
        let url = self.join(path)?;
        let resp = self
            // `OnTerminalError::Fail` means the driver never returns
            // `None`; `Option::expect` documents that invariant.
            .execute_with_retry(
                self.inner.post(url).json(body),
                OnTerminalError::Fail,
                Some(&classify),
            )
            .await?
            .expect("execute_with_retry returned None despite OnTerminalError::Fail");
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

    /// POST `form` as `application/x-www-form-urlencoded`, decoding
    /// the JSON body as `T` on success and as `E` on a terminal
    /// rejection.
    ///
    /// For endpoints whose error body is part of the protocol rather
    /// than a diagnostic. The OAuth device grant is the first caller:
    /// it answers `400 {"error": "authorization_pending"}` on every
    /// poll until its owner approves, so a client that could only see
    /// "the request failed" could never complete the flow.
    ///
    /// Retries are unchanged — 5xx and rate limits are retried before
    /// anything is handed back — and a rejection whose body does not
    /// deserialize as `E` is a plain [`CliError`], so a proxy's HTML
    /// 502 page stays as diagnosable as it is on every other verb.
    pub async fn post_form<T: DeserializeOwned, E: DeserializeOwned>(
        &self,
        path: &str,
        form: &[(&str, &str)],
    ) -> Result<ApiOutcome<T, E>, CliError> {
        let url = self.join(path)?;
        let resp = self
            // `OnTerminalError::ReturnResponse` never returns `None`;
            // `Option::expect` documents that invariant.
            .execute_with_retry(
                self.inner.post(url).form(form),
                OnTerminalError::ReturnResponse,
                None,
            )
            .await?
            .expect("execute_with_retry returned None despite OnTerminalError::ReturnResponse");
        self.decode_outcome(resp).await
    }

    /// Split a response handed back by
    /// [`OnTerminalError::ReturnResponse`] into the two halves of an
    /// [`ApiOutcome`]. A rejection whose body does not deserialize as
    /// `E` is not the protocol speaking, so it stays a [`CliError`]
    /// carrying the same message every other verb would have printed.
    async fn decode_outcome<T: DeserializeOwned, E: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<ApiOutcome<T, E>, CliError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(ApiOutcome::Ok(self.decode_json(resp).await?));
        }
        let rejection = error_response(status, resp).await;
        match serde_json::from_slice::<E>(&rejection.body) {
            Ok(body) => Ok(ApiOutcome::Error {
                status: status.as_u16(),
                body,
            }),
            Err(_) => Err(self.api_error(rejection.message)),
        }
    }

    /// POST `form` as `application/x-www-form-urlencoded` and discard
    /// the response body. The form counterpart of
    /// [`Self::post_no_response`].
    pub async fn post_form_no_response(
        &self,
        path: &str,
        form: &[(&str, &str)],
    ) -> Result<(), CliError> {
        let url = self.join(path)?;
        self.execute_request(self.inner.post(url).form(form))
            .await
            .map(drop)
    }

    /// POST to `path` with **no** request body, treating 404 as
    /// success and discarding the response body.
    ///
    /// For "make sure this is off" endpoints that take no body and
    /// answer with an empty 2xx — GitHub's `POST
    /// /repos/{o}/{r}/stacks/{n}/unstack` is the first caller. A 404
    /// means the resource is already gone, which is exactly the
    /// postcondition the caller wanted, so it is not an error (same
    /// reasoning as [`Self::delete_if_exists`]).
    ///
    /// Distinct from [`Self::post_no_response`], which serializes a
    /// JSON body and treats 404 as a failure: an endpoint documented
    /// as taking no body should be sent none, not a JSON `null`.
    pub async fn post_empty_if_exists(&self, path: &str) -> Result<(), CliError> {
        let url = self.join(path)?;
        self.execute_with_retry(self.inner.post(url), OnTerminalError::NotFoundIsNone, None)
            .await
            .map(drop)
    }

    /// PUT `body` as JSON to `path` and deserialize the JSON
    /// response as `T`.
    pub async fn put<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, CliError> {
        let url = self.join(path)?;
        let resp = self.execute_request(self.inner.put(url).json(body)).await?;
        self.decode_json(resp).await
    }

    /// PUT `body` as JSON to `path`, discard the response body, and
    /// report whether the endpoint exists: `Ok(false)` on 404.
    ///
    /// For probing a route a Mergify deployment older than the CLI
    /// does not serve yet, so the caller can fall back to one it does
    /// — `ci scopes-send` is the first caller. 404 is terminal (never
    /// retried), so the probe costs exactly one request.
    ///
    /// Distinct from [`Self::put`], which treats 404 as a failure like
    /// any other 4xx, and from [`Self::post_no_response`], which does
    /// the same for POST.
    pub async fn put_no_response_if_exists<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<bool, CliError> {
        let url = self.join(path)?;
        Ok(self
            .execute_request_optional(self.inner.put(url).json(body))
            .await?
            .is_some())
    }

    /// PATCH `body` as JSON to `path` and deserialize the JSON
    /// response as `T`. Mirrors [`Self::put`] but for endpoints that
    /// use the more permissive PATCH semantics (partial update) —
    /// `freeze update` is the first caller.
    pub async fn patch<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, CliError> {
        let url = self.join(path)?;
        let resp = self
            .execute_request(self.inner.patch(url).json(body))
            .await?;
        self.decode_json(resp).await
    }

    /// DELETE `path`, returning whether the resource existed.
    ///
    /// Returns `Ok(DeleteOutcome::Deleted)` on 2xx responses and
    /// `Ok(DeleteOutcome::NotFound)` on 404 — useful for idempotent
    /// "turn this thing off if it's on" operations where 404 means
    /// "nothing to do". 4xx-other and 5xx map to the normal API
    /// errors.
    pub async fn delete_if_exists(&self, path: &str) -> Result<DeleteOutcome, CliError> {
        let url = self.join(path)?;
        self.execute_status(self.inner.delete(url)).await
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

    /// Single retry/auth/error driver behind every public verb.
    ///
    /// `terminal` says what happens to a non-2xx the driver has
    /// decided not to retry — see [`OnTerminalError`].
    ///
    /// Success (2xx) always returns `Ok(Some(response))` — the
    /// caller decides whether to decode the body, drop it, or
    /// map it to a domain type.
    ///
    /// 5xx is retried with exponential backoff (`self.retry`);
    /// transient send errors (timeout / connect) are retried with
    /// the same backoff. Other terminal errors and non-5xx 4xx
    /// fail immediately.
    async fn execute_with_retry(
        &self,
        builder: reqwest::RequestBuilder,
        terminal: OnTerminalError,
        classify_error: Option<ErrorClassifier<'_>>,
    ) -> Result<Option<reqwest::Response>, CliError> {
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

            tracing::debug!(
                service = self.service_name(),
                attempt = attempt + 1,
                max_attempts = self.retry.max_attempts,
                "sending HTTP request"
            );
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(Some(resp));
                    }
                    if terminal == OnTerminalError::NotFoundIsNone
                        && status == StatusCode::NOT_FOUND
                    {
                        return Ok(None);
                    }
                    // Inspect rate-limit headers before the body is
                    // read. GitHub signals secondary/abuse limits with
                    // 429, or 403 carrying `Retry-After` / an exhausted
                    // `X-RateLimit-Remaining`. A bare 403 (auth /
                    // permission denied) must NOT be retried.
                    let rate_limit = rate_limit_wait(&resp);
                    let rate_limited = status == StatusCode::TOO_MANY_REQUESTS
                        || (status == StatusCode::FORBIDDEN && rate_limit.is_some());
                    let retryable = (status.is_server_error() || rate_limited)
                        && attempt + 1 < self.retry.max_attempts;
                    // Terminal, and the caller wants the body: hand the
                    // response over unread. Reading it here to render a
                    // message would consume the very bytes the caller
                    // needs, so this has to come before `error_message`
                    // and after the retry decision.
                    //
                    // Never for a 5xx, even one that has run out of
                    // retries. A server error is the server failing,
                    // not the protocol speaking, and its body can
                    // still parse as the protocol's error type —
                    // `{"detail": …}` deserializes into anything whose
                    // fields are optional. Letting it through would
                    // trade "HTTP 500 …\nurl: …" for whatever the
                    // caller's error type made of it.
                    if !retryable
                        && !status.is_server_error()
                        && terminal == OnTerminalError::ReturnResponse
                    {
                        return Ok(Some(resp));
                    }
                    last_message = error_message(status, resp).await;
                    if retryable {
                        // A rate-limit response dictates the wait, capped
                        // so the CLI never blocks on a far-off reset;
                        // everything else uses exponential backoff. A
                        // reset beyond the cap fails fast.
                        let delay = match rate_limit {
                            Some(wait) if wait > MAX_RATE_LIMIT_WAIT => {
                                return Err(self.api_error(last_message));
                            }
                            Some(wait) => wait,
                            None => backoff,
                        };
                        tracing::warn!(
                            service = self.service_name(),
                            %status,
                            ?delay,
                            "retrying after error response"
                        );
                        tokio::time::sleep(delay).await;
                        backoff *= 2;
                        continue;
                    }
                    // Terminal non-2xx: let the caller remap specific
                    // statuses (e.g. simulate's 422 → config error)
                    // before the default flavor mapping.
                    if let Some(classify) = classify_error
                        && let Some(mapped) = classify(status.as_u16(), &last_message)
                    {
                        return Err(mapped);
                    }
                    return Err(self.api_error(last_message));
                }
                Err(e) if is_transient(&e) && attempt + 1 < self.retry.max_attempts => {
                    tracing::debug!(
                        service = self.service_name(),
                        error = %e,
                        ?backoff,
                        "retrying after transient network error"
                    );
                    last_message = format!("network error: {e}");
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                }
                Err(e) => {
                    return Err(self.api_error(self.terminal_send_error_message(&e)));
                }
            }
        }
        Err(self.api_error(last_message))
    }

    /// Send a request that must return a response. 404 is treated
    /// like any other 4xx (caller error → [`CliError`]).
    async fn execute_request(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, CliError> {
        // `OnTerminalError::Fail` means the driver never returns
        // `None`; `Option::expect` documents that invariant.
        Ok(self
            .execute_with_retry(builder, OnTerminalError::Fail, None)
            .await?
            .expect("execute_with_retry returned None despite OnTerminalError::Fail"))
    }

    /// Send a request where 404 is a routine caller branch
    /// rather than a server failure. Used by [`Self::get_if_exists`].
    async fn execute_request_optional(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<Option<reqwest::Response>, CliError> {
        self.execute_with_retry(builder, OnTerminalError::NotFoundIsNone, None)
            .await
    }

    /// Send a request that cares only about the HTTP status.
    /// Used by [`Self::delete_if_exists`] — the response body
    /// (if any) is discarded.
    async fn execute_status(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<DeleteOutcome, CliError> {
        match self
            .execute_with_retry(builder, OnTerminalError::NotFoundIsNone, None)
            .await?
        {
            Some(_) => Ok(DeleteOutcome::Deleted),
            None => Ok(DeleteOutcome::NotFound),
        }
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

    /// Render a non-retried `reqwest` send error as the message
    /// body for `CliError`. Shared between the GET/POST/PUT path
    /// (`execute_request`) and the DELETE-style status-only path
    /// (`execute_status`) so verbs don't drift on user-facing
    /// diagnostics — timeouts and connect failures must read the
    /// same regardless of HTTP method.
    fn terminal_send_error_message(&self, e: &reqwest::Error) -> String {
        let svc = self.service_name();
        let base = if e.is_timeout() {
            format!("{svc} did not respond in time. The request was aborted — please retry.")
        } else if e.is_connect() {
            format!("could not reach {svc}: {e}")
        } else {
            format!("request failed: {e}")
        };
        // Surface the contacted URL so a hung or mis-configured
        // endpoint (e.g. a wrong --api-url) is diagnosable — Python
        // printed it in the same network-error message.
        match e.url() {
            Some(url) => format!("{base} (url: {url})"),
            None => base,
        }
    }
}

fn is_transient(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect()
}

/// Extract the next page's `cursor` from an RFC 5988 `Link` header
/// value (`<url>; rel="next", <url>; rel="last", …`).
///
/// Only the `cursor` query parameter of the `rel="next"` target is
/// returned — the pagination contract is "same query, new cursor",
/// so the caller keeps building its own request rather than blindly
/// following a server-echoed URL. `None` when there is no `next`
/// link or its URL carries no cursor (both mean "last page").
fn next_cursor_from_link(header: &str) -> Option<String> {
    for part in header.split(',') {
        let mut segments = part.split(';');
        let target = segments.next()?.trim();
        let is_next = segments.any(|param| {
            let param = param.trim();
            param
                .strip_prefix("rel=")
                .is_some_and(|rel| rel.trim_matches('"') == "next")
        });
        if !is_next {
            continue;
        }
        let target = target.strip_prefix('<')?.strip_suffix('>')?;
        let url = Url::parse(target).ok()?;
        return url
            .query_pairs()
            .find(|(key, _)| key == "cursor")
            .map(|(_, value)| value.into_owned());
    }
    None
}

/// The wait hinted by a rejection's rate-limit headers, if any.
/// Prefers `Retry-After` (delta-seconds, as GitHub sends); falls back
/// to `X-RateLimit-Reset` (epoch seconds) when `X-RateLimit-Remaining`
/// is `0`. Returns `None` when there is no rate-limit signal — the
/// caller uses that to tell a rate-limited 403 from a permission 403.
fn rate_limit_wait(resp: &reqwest::Response) -> Option<Duration> {
    let headers = resp.headers();
    if let Some(secs) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
    {
        return Some(Duration::from_secs(secs));
    }
    let exhausted = headers
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "0");
    if exhausted {
        let reset = headers
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        return Some(Duration::from_secs(reset.saturating_sub(now)));
    }
    None
}

/// A rejection's rendered message plus the (capped) bytes it was
/// rendered from, so [`Client::post_form`] can try the body as a
/// protocol answer without reading the response twice.
struct ErrorResponse {
    message: String,
    body: Vec<u8>,
}

async fn error_message(status: StatusCode, resp: reqwest::Response) -> String {
    error_response(status, resp).await.message
}

async fn error_response(status: StatusCode, mut resp: reqwest::Response) -> ErrorResponse {
    // Capture the URL before the body stream consumes `resp` — a
    // failing endpoint (e.g. a mis-resolved --api-url) is surfaced on
    // a trailing `url:` line, matching Python's `check_for_status`.
    let url = resp.url().clone();

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

    // Prefer the JSON `detail` field the Mergify API returns so the
    // user sees a clean sentence instead of the raw `{"detail": …}`
    // envelope. Only when the (untruncated) body parses as an object
    // carrying a string `detail`; otherwise fall back to the body text.
    let detail = if truncated {
        None
    } else {
        serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v.get("detail")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
    };

    let head = match detail {
        Some(detail) => format!("HTTP {status}: {detail}"),
        None if text.is_empty() => format!("HTTP {status}"),
        None => format!("HTTP {status}: {text}"),
    };
    ErrorResponse {
        message: format!("{head}\nurl: {url}"),
        body,
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
    use wiremock::matchers::body_string;
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

    #[derive(Deserialize, Debug, PartialEq)]
    struct OAuthError {
        error: String,
    }

    #[tokio::test]
    async fn get_outcome_hands_back_the_status_of_a_terminal_rejection() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/user"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_json(serde_json::json!({"detail": "not allowed"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let outcome: ApiOutcome<Foo, serde_json::Value> =
            client.get_outcome("/v1/user").await.unwrap();
        match outcome {
            ApiOutcome::Error { status, .. } => assert_eq!(status, 403),
            ApiOutcome::Ok(body) => panic!("expected a rejection, got {body:?}"),
        }
    }

    #[tokio::test]
    async fn get_outcome_decodes_a_successful_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(Foo { bar: 3 }))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let outcome: ApiOutcome<Foo, serde_json::Value> =
            client.get_outcome("/v1/user").await.unwrap();
        assert_eq!(outcome, ApiOutcome::Ok(Foo { bar: 3 }));
    }

    #[tokio::test]
    async fn post_form_sends_urlencoded_and_decodes_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(header("Content-Type", "application/x-www-form-urlencoded"))
            .and(body_string("grant_type=device&device_code=abc%2Fdef"))
            .respond_with(ResponseTemplate::new(200).set_body_json(Foo { bar: 7 }))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let outcome: ApiOutcome<Foo, OAuthError> = client
            .post_form(
                "/oauth/token",
                &[("grant_type", "device"), ("device_code", "abc/def")],
            )
            .await
            .unwrap();
        assert_eq!(outcome, ApiOutcome::Ok(Foo { bar: 7 }));
    }

    // The reason this verb exists: a 400 carrying the protocol's own
    // error code is an answer, not a failure.
    #[tokio::test]
    async fn post_form_returns_a_terminal_rejection_as_a_protocol_answer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({"error": "authorization_pending"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let outcome: ApiOutcome<Foo, OAuthError> =
            client.post_form("/oauth/token", &[]).await.unwrap();
        assert_eq!(
            outcome,
            ApiOutcome::Error {
                status: 400,
                body: OAuthError {
                    error: "authorization_pending".to_string(),
                },
            },
        );
    }

    // A rejection that is not the protocol speaking — a proxy's error
    // page, a misrouted request — must stay as diagnosable as it is on
    // every other verb.
    #[tokio::test]
    async fn post_form_falls_back_to_a_cli_error_when_the_body_is_not_the_protocol() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(400).set_body_string("<html>go away</html>"))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let err = client
            .post_form::<Foo, OAuthError>("/oauth/token", &[])
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("400"), "got {message:?}");
        assert!(message.contains("go away"), "got {message:?}");
    }

    // Handing the response body to the caller must not cost the retry
    // policy: a 500 is still retried, and only the last one is handed
    // back.
    #[tokio::test]
    async fn post_form_still_retries_server_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .expect(3)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let err = client
            .post_form::<Foo, OAuthError>("/oauth/token", &[])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("500"), "got {err}");
    }

    // A 5xx that has run out of retries is the server failing, not
    // the protocol speaking — and its body can parse as the
    // protocol's error type anyway, since every field there is
    // optional.
    #[tokio::test]
    async fn post_form_keeps_the_full_diagnostic_for_an_exhausted_server_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(serde_json::json!({"error": "upstream down"})),
            )
            .expect(3)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let err = client
            .post_form::<Foo, OAuthError>("/oauth/token", &[])
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("500"), "got {message:?}");
        assert!(message.contains("url:"), "got {message:?}");
    }

    #[tokio::test]
    async fn post_form_no_response_accepts_an_empty_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/revoke"))
            .and(body_string("token=mut_secret"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        client
            .post_form_no_response("/oauth/revoke", &[("token", "mut_secret")])
            .await
            .unwrap();
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
    async fn requests_carry_user_agent_header() {
        // GitHub's REST API rejects requests with no User-Agent
        // ("403 Request forbidden by administrative rules"). The
        // reqwest builder doesn't set one by default, so dropping
        // the explicit `.user_agent(...)` call would silently
        // break every GitHub-backed command. Pin the header value
        // to `mergify-cli/<crate-version>` so a regression here
        // surfaces as a test failure, not as a prod outage.
        let server = MockServer::start().await;
        let expected_ua = format!("mergify-cli/{}", env!("CARGO_PKG_VERSION"));
        Mock::given(method("GET"))
            .and(path("/foo"))
            .and(header("User-Agent", expected_ua.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(Foo { bar: 1 }))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::GitHub);
        let _: Foo = client.get("/foo").await.unwrap();
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
    async fn post_empty_if_exists_sends_no_body_and_tolerates_404() {
        // `POST .../unstack` is documented as taking no request body,
        // so we must not send a JSON `null`; and a 404 (already
        // dissolved) satisfies the caller's postcondition.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/present"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/absent"))
            .respond_with(ResponseTemplate::new(404).set_body_string("gone"))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::GitHub);
        client.post_empty_if_exists("/present").await.unwrap();
        client.post_empty_if_exists("/absent").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert!(
            requests.iter().all(|r| r.body.is_empty()),
            "no request body may be sent",
        );
    }

    #[tokio::test]
    async fn post_empty_if_exists_propagates_other_4xx() {
        // Only 404 is "already done"; a 403 is a real failure the
        // caller must see.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/denied"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::GitHub);
        let err = client.post_empty_if_exists("/denied").await.unwrap_err();
        assert!(matches!(err, CliError::GitHubApi(_)), "got {err:?}");
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
    async fn put_no_response_if_exists_reports_whether_the_route_answered() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/present"))
            .and(body_json(serde_json::json!({"bar": 1})))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/absent"))
            .respond_with(ResponseTemplate::new(404).set_body_string("no such route"))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        assert!(
            client
                .put_no_response_if_exists("/present", &Foo { bar: 1 })
                .await
                .unwrap()
        );
        // 404 is the caller's branch, and terminal — one request, no
        // backoff, so the probe stays cheap.
        assert!(
            !client
                .put_no_response_if_exists("/absent", &Foo { bar: 1 })
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn put_no_response_if_exists_propagates_other_4xx() {
        // Only 404 is an answer about the route. A 403 is a real
        // failure, and swallowing it as `false` would silently downgrade
        // an auth problem into a fallback.
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/forbidden"))
            .respond_with(ResponseTemplate::new(403).set_body_string("denied"))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let err = client
            .put_no_response_if_exists("/forbidden", &Foo { bar: 1 })
            .await
            .unwrap_err();
        assert!(matches!(err, CliError::MergifyApi(_)));
        assert!(err.to_string().contains("403"));
    }

    #[tokio::test]
    async fn post_classifying_remaps_matched_status() {
        // The classifier turns the simulator's 422 into a config
        // error (exit 8) instead of the default Mergify API error.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/simulate"))
            .respond_with(
                ResponseTemplate::new(422)
                    .set_body_json(serde_json::json!({"detail": "bad config"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let err = client
            .post_classifying::<_, Foo>("/simulate", &Foo { bar: 1 }, |status, msg| {
                (status == 422).then(|| CliError::Configuration(msg.to_string()))
            })
            .await
            .unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)), "got {err:?}");
        assert!(err.to_string().contains("bad config"), "got {err}");
    }

    #[tokio::test]
    async fn post_classifying_falls_back_when_status_unmatched() {
        // A status the classifier ignores keeps the default flavor
        // mapping (Mergify API error here).
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/simulate"))
            .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let err = client
            .post_classifying::<_, Foo>("/simulate", &Foo { bar: 1 }, |status, msg| {
                (status == 422).then(|| CliError::Configuration(msg.to_string()))
            })
            .await
            .unwrap_err();
        assert!(matches!(err, CliError::MergifyApi(_)), "got {err:?}");
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

    #[tokio::test]
    async fn patch_sends_json_body_and_returns_deserialized_response() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/freeze/abc"))
            .and(body_json(Foo { bar: 1 }))
            .respond_with(ResponseTemplate::new(200).set_body_json(Foo { bar: 2 }))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let got: Foo = client.patch("/freeze/abc", &Foo { bar: 1 }).await.unwrap();
        assert_eq!(got, Foo { bar: 2 });
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

    struct RateLimited {
        attempts: Arc<AtomicU32>,
        fail_first: u32,
    }

    impl Respond for RateLimited {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if attempt < self.fail_first {
                // `Retry-After: 0` so the test honours the header path
                // without actually sleeping.
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "0")
                    .set_body_string("rate limited")
            } else {
                ResponseTemplate::new(200).set_body_json(Foo { bar: 7 })
            }
        }
    }

    #[tokio::test]
    async fn retries_on_rate_limit_then_succeeds() {
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicU32::new(0));
        Mock::given(method("GET"))
            .and(path("/foo"))
            .respond_with(RateLimited {
                attempts: Arc::clone(&attempts),
                fail_first: 1,
            })
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::GitHub);
        let got: Foo = client.get("/foo").await.unwrap();
        assert_eq!(got, Foo { bar: 7 });
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn bare_403_is_not_retried() {
        let server = MockServer::start().await;
        // No rate-limit headers → a permission/auth 403, which must
        // fail immediately. `expect(1)` fails on server drop if the
        // client retried.
        Mock::given(method("GET"))
            .and(path("/foo"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::GitHub);
        let err = client.get::<Foo>("/foo").await.unwrap_err();
        assert!(matches!(err, CliError::GitHubApi(_)));
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
    async fn json_detail_field_is_extracted_and_url_is_appended() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/foo"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(serde_json::json!({"detail": "Repository not found"})),
            )
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let msg = client.get::<Foo>("/foo").await.unwrap_err().to_string();
        // Clean sentence from `detail`, not the raw `{"detail": …}` body.
        assert!(
            msg.contains("HTTP 404 Not Found: Repository not found"),
            "expected extracted detail, got {msg}"
        );
        assert!(!msg.contains('{'), "raw JSON envelope leaked: {msg}");
        // Failing endpoint surfaced on a trailing url: line.
        assert!(msg.contains("\nurl: "), "missing url line: {msg}");
        assert!(msg.contains("/foo"), "url should name the path: {msg}");
    }

    #[tokio::test]
    async fn non_json_error_body_falls_back_to_raw_text_with_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/foo"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let msg = client.get::<Foo>("/foo").await.unwrap_err().to_string();
        assert!(msg.contains("boom"), "raw body should survive: {msg}");
        assert!(msg.contains("\nurl: "), "missing url line: {msg}");
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
    async fn get_with_query_appends_repeated_keys_and_percent_encodes_values() {
        let server = MockServer::start().await;
        let client = fast_client(&server, ApiFlavor::Mergify);

        Mock::given(method("GET"))
            .and(path("/lookup"))
            .respond_with(ResponseTemplate::new(200).set_body_json(Foo { bar: 1 }))
            .mount(&server)
            .await;

        let _: Foo = client
            .get_with_query(
                "/lookup",
                &[
                    ("test_name", "*test login*"),
                    ("test_name", "a&b?c"),
                    ("limit", "5"),
                ],
            )
            .await
            .unwrap();

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let raw_query = received[0].url.query().expect("expected a query string");
        // Repeated keys must preserve caller order; query-reserved
        // characters (`&`, `?`) must be percent-encoded so the server
        // doesn't mistake them for separators. Spaces become `+` (the
        // application/x-www-form-urlencoded convention `url` follows).
        // `*` is a sub-delim that servers parse literally, so it
        // passes through unencoded.
        assert_eq!(
            raw_query,
            "test_name=*test+login*&test_name=a%26b%3Fc&limit=5",
        );
    }

    #[tokio::test]
    async fn get_with_query_omits_question_mark_when_no_pairs() {
        let server = MockServer::start().await;
        let client = fast_client(&server, ApiFlavor::Mergify);

        Mock::given(method("GET"))
            .and(path("/foo"))
            .respond_with(ResponseTemplate::new(200).set_body_json(Foo { bar: 0 }))
            .mount(&server)
            .await;

        let _: Foo = client.get_with_query("/foo", &[]).await.unwrap();

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        assert!(
            received[0].url.query().is_none(),
            "no pairs must produce no `?`, got {:?}",
            received[0].url.query(),
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

    #[test]
    fn next_cursor_from_link_finds_the_next_rel() {
        let header = concat!(
            "<https://api.example/v1/repos/o/r/logs?cursor=first&per_page=10>; rel=\"first\", ",
            "<https://api.example/v1/repos/o/r/logs?cursor=abc123&per_page=10>; rel=\"next\", ",
            "<https://api.example/v1/repos/o/r/logs?cursor=last&per_page=10>; rel=\"last\"",
        );
        assert_eq!(next_cursor_from_link(header), Some("abc123".to_string()));
    }

    #[test]
    fn next_cursor_from_link_accepts_unquoted_rel() {
        let header = "<https://api.example/logs?cursor=zzz>; rel=next";
        assert_eq!(next_cursor_from_link(header), Some("zzz".to_string()));
    }

    #[test]
    fn next_cursor_from_link_is_none_without_a_next_rel() {
        let header = "<https://api.example/logs?cursor=first>; rel=\"first\"";
        assert_eq!(next_cursor_from_link(header), None);
    }

    #[test]
    fn next_cursor_from_link_is_none_when_the_next_url_has_no_cursor() {
        let header = "<https://api.example/logs?per_page=10>; rel=\"next\"";
        assert_eq!(next_cursor_from_link(header), None);
    }

    #[tokio::test]
    async fn get_page_returns_body_and_next_cursor() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/paged"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(
                        "link",
                        "<https://api.example/paged?cursor=next-cursor>; rel=\"next\"",
                    )
                    .set_body_json(Foo { bar: 1 }),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let page: Page<Foo> = client
            .get_page("/paged", &[("per_page", "10")])
            .await
            .unwrap();
        assert_eq!(page.body, Foo { bar: 1 });
        assert_eq!(page.next_cursor, Some("next-cursor".to_string()));
    }

    #[tokio::test]
    async fn get_page_has_no_cursor_on_the_last_page() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/paged"))
            .respond_with(ResponseTemplate::new(200).set_body_json(Foo { bar: 2 }))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let page: Page<Foo> = client.get_page("/paged", &[]).await.unwrap();
        assert_eq!(page.body, Foo { bar: 2 });
        assert_eq!(page.next_cursor, None);
    }

    #[tokio::test]
    async fn get_if_exists_returns_some_on_2xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/foo"))
            .and(header("Authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(Foo { bar: 7 }))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let got: Option<Foo> = client.get_if_exists("/foo").await.unwrap();
        assert_eq!(got, Some(Foo { bar: 7 }));
    }

    #[tokio::test]
    async fn get_if_exists_returns_none_on_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/missing"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let got: Option<Foo> = client.get_if_exists("/missing").await.unwrap();
        assert!(got.is_none(), "expected None on 404, got {got:?}");
    }

    #[tokio::test]
    async fn get_if_exists_surfaces_other_4xx_as_error() {
        // 403 / 401 / 422 etc. are real failures, not "doesn't
        // exist" — they must surface as `CliError`. Only 404 is
        // mapped to `None`.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/forbidden"))
            .respond_with(ResponseTemplate::new(403).set_body_string(r#"{"detail":"nope"}"#))
            .expect(1)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let err = client.get_if_exists::<Foo>("/forbidden").await.unwrap_err();
        assert!(
            matches!(err, CliError::MergifyApi(_)),
            "expected MergifyApi, got {err:?}",
        );
        assert!(err.to_string().contains("403"));
    }

    #[tokio::test]
    async fn get_if_exists_retries_5xx_then_succeeds() {
        // Same retry semantics as `get`: a 500 on the first
        // attempt should not short-circuit; the second attempt's
        // 200 must be returned as `Some`.
        struct FlakyRespond {
            calls: Arc<AtomicU32>,
        }
        impl Respond for FlakyRespond {
            fn respond(&self, _req: &Request) -> ResponseTemplate {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ResponseTemplate::new(500)
                } else {
                    ResponseTemplate::new(200).set_body_json(Foo { bar: 9 })
                }
            }
        }
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicU32::new(0));
        Mock::given(method("GET"))
            .and(path("/flaky"))
            .respond_with(FlakyRespond {
                calls: Arc::clone(&calls),
            })
            .expect(2)
            .mount(&server)
            .await;

        let client = fast_client(&server, ApiFlavor::Mergify);
        let got: Option<Foo> = client.get_if_exists("/flaky").await.unwrap();
        assert_eq!(got, Some(Foo { bar: 9 }));
        assert_eq!(calls.load(Ordering::SeqCst), 2, "expected two attempts");
    }
}
