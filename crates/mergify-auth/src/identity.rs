//! `GET /v1/user` — which account a credential belongs to.
//!
//! The device grant's token response says nothing about who
//! approved it, and every other `user`-authenticated route on the
//! API takes an `{owner}` or `{owner}/{repository}` path parameter,
//! so there is no other way to turn a credential into a name.
//!
//! The three answers are three different sentences to the user, and
//! reporting any of them as another would be a lie:
//!
//! - **200** — the credential is live, and this is whose it is.
//! - **403** — refused. Revoked from the dashboard, expired, or not
//!   a user credential at all (an application key lands here).
//!   **401** counts as the same answer: it is what RFC 6750 says a
//!   rejected bearer token gets, it is what this API's own device
//!   endpoints answer for a bad client, and any gateway in front of
//!   a self-hosted install can produce one on its own. Reporting it
//!   as an unexpected status would replace "run `mergify auth
//!   login`" with a sentence naming no remedy.
//! - **404** — the deployment predates the route. Self-hosted
//!   installs upgrade on their own schedule, so this is a normal
//!   answer, not a broken one.

use mergify_core::ApiFlavor;
use mergify_core::ApiOutcome;
use mergify_core::CliError;
use mergify_core::HttpClient;
use serde::Deserialize;
use url::Url;

const USER_PATH: &str = "/v1/user";

/// The account a credential belongs to.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct User {
    pub id: u64,
    pub login: String,
}

/// What `GET /v1/user` had to say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    Known(User),
    /// The API refused the credential.
    Refused,
    /// This deployment does not serve the route.
    Unsupported,
}

/// A rejection body. Every refusal from the API app carries
/// `FastAPI`'s `detail`, and an unexpected status is the one case
/// where the server's own sentence is all the caller has to go on.
#[derive(Deserialize)]
struct Rejection {
    #[serde(default)]
    detail: Option<String>,
}

/// Ask the API who `token` belongs to.
pub async fn whoami(api_url: Url, token: &str) -> Result<Identity, CliError> {
    let client = HttpClient::new(api_url, token, ApiFlavor::Mergify)?;
    match client.get_outcome::<User, Rejection>(USER_PATH).await? {
        ApiOutcome::Ok(user) => Ok(Identity::Known(user)),
        ApiOutcome::Error {
            status: 401 | 403, ..
        } => Ok(Identity::Refused),
        ApiOutcome::Error { status: 404, .. } => Ok(Identity::Unsupported),
        ApiOutcome::Error { status, body } => Err(CliError::MergifyApi(match body.detail {
            Some(detail) => format!("unexpected HTTP {status} from {USER_PATH}: {detail}"),
            None => format!("unexpected HTTP {status} from {USER_PATH}"),
        })),
    }
}

/// The login name behind `token`, or `None` for every reason there
/// might not be one — including a failure to ask.
///
/// For callers that only want to put a name in a sentence and have
/// nothing to do with the difference: `auth login` has just minted
/// the credential, so a network hiccup on the way to `/v1/user`
/// must not turn a successful login into a failed command.
pub async fn login_name(api_url: Url, token: &str) -> Option<String> {
    match whoami(api_url, token).await {
        Ok(Identity::Known(user)) => Some(user.login),
        Ok(other) => {
            tracing::debug!(?other, "the API did not name the credential's owner");
            None
        }
        Err(e) => {
            tracing::debug!(error = %e, "could not ask the API who the credential belongs to");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    fn url(server: &MockServer) -> Url {
        Url::parse(&server.uri()).unwrap()
    }

    async fn mount(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path("/v1/user"))
            .and(header("Authorization", "Bearer mut_secret"))
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn a_live_credential_names_its_owner() {
        let server = MockServer::start().await;
        mount(
            &server,
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"id": 42, "login": "sileht"})),
        )
        .await;

        assert_eq!(
            whoami(url(&server), "mut_secret").await.unwrap(),
            Identity::Known(User {
                id: 42,
                login: "sileht".to_string(),
            }),
        );
    }

    // 403 is what every `/v1` route answers for a refused
    // credential; 401 is below.
    #[tokio::test]
    async fn a_refused_credential_is_not_an_error() {
        let server = MockServer::start().await;
        mount(
            &server,
            ResponseTemplate::new(403).set_body_json(serde_json::json!({"detail": "forbidden"})),
        )
        .await;

        assert_eq!(
            whoami(url(&server), "mut_secret").await.unwrap(),
            Identity::Refused,
        );
    }

    // Not what the Mergify API itself answers, but what a gateway in
    // front of a self-hosted install can. The user's move is the
    // same either way, so the answer has to be too.
    #[tokio::test]
    async fn a_401_is_a_refusal_too() {
        let server = MockServer::start().await;
        mount(
            &server,
            ResponseTemplate::new(401).set_body_json(serde_json::json!({"detail": "expired"})),
        )
        .await;

        assert_eq!(
            whoami(url(&server), "mut_secret").await.unwrap(),
            Identity::Refused,
        );
    }

    // A self-hosted install older than the route. Normal, not broken.
    #[tokio::test]
    async fn a_deployment_without_the_route_is_not_an_error() {
        let server = MockServer::start().await;
        mount(
            &server,
            ResponseTemplate::new(404).set_body_json(serde_json::json!({"detail": "Not Found"})),
        )
        .await;

        assert_eq!(
            whoami(url(&server), "mut_secret").await.unwrap(),
            Identity::Unsupported,
        );
    }

    #[tokio::test]
    async fn login_name_swallows_a_refusal() {
        let server = MockServer::start().await;
        mount(
            &server,
            ResponseTemplate::new(403).set_body_json(serde_json::json!({"detail": "forbidden"})),
        )
        .await;

        assert_eq!(login_name(url(&server), "mut_secret").await, None);
    }

    // The arm that justifies `login` calling this at all: a request
    // that fails outright must not turn a login the server already
    // granted into a failed command. A 200 whose body is not a user
    // reaches that arm without the three retries a 5xx would cost.
    #[tokio::test]
    async fn login_name_swallows_a_failure_to_ask() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/user"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"unexpected": true})),
            )
            .mount(&server)
            .await;

        assert_eq!(login_name(url(&server), "mut_secret").await, None);
        // …and the typed call still surfaces it, so the swallowing is
        // `login_name`'s decision rather than a hidden one.
        assert!(whoami(url(&server), "mut_secret").await.is_err());
    }
}
