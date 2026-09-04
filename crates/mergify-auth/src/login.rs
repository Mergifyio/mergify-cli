//! `mergify auth login` — obtain a Mergify credential and store it.

use std::io::Write;

use chrono::DateTime;
use chrono::Utc;
use mergify_core::CliError;
use mergify_core::Credential;
use mergify_core::CredentialStore;
use mergify_core::Output;
use mergify_core::auth;
use mergify_core::credentials::Location;
use serde::Serialize;
use url::Url;

use crate::device;
use crate::identity;

pub struct LoginOptions<'a> {
    pub api_url: Option<&'a str>,
    pub store: &'a CredentialStore,
}

/// What `auth login` produced, for the JSON rendering `Output`
/// requires. Deliberately without the token: nothing prints the
/// secret, ever.
#[derive(Serialize)]
struct LoginResult {
    api_url: String,
    login: Option<String>,
    stored_in: String,
    /// The environment variable that will be used *instead of* the
    /// credential just stored, if one is set.
    overridden_by: Option<String>,
}

/// Run the `auth login` command.
pub async fn run(opts: LoginOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let api_url = auth::resolve_api_url(opts.api_url)?;
    let client = device::client(api_url.clone())?;

    // Before any grant exists, not merely before it is spent. A
    // store this machine cannot read is a corrupt file or a keychain
    // holding non-JSON, and reading it after the code is on screen
    // means the user is already at the approval page when the
    // command dies — and whatever they approve there mints a token
    // nothing stores and nothing revokes. `authorize` needs no
    // store, so this costs nothing.
    let previous = opts.store.get(&api_url)?;

    let authorization = device::authorize(&client).await?;
    output.status(&instructions(&authorization))?;

    let token = device::poll(
        &client,
        &authorization.device_code,
        device::PollSchedule::from_authorization(&authorization),
    )
    .await?;

    let credential = Credential {
        expires_at: token
            .expires_in
            .and_then(|seconds| expires_at(Utc::now(), seconds)),
        token: token.access_token,
    };
    let location = match opts.store.set(&api_url, &credential) {
        Ok(location) => location,
        Err(e) => {
            // The server minted a token this machine cannot keep.
            // Leaving it live would burn one of the user's twenty
            // for nothing, and every retry would burn another.
            revoke_quietly(&client, &credential.token).await;
            return Err(e);
        }
    };

    // The credential this one replaces is now unreachable from here
    // and still live on the server for its full year. A user who
    // logs in again on the same machine — after a token stopped
    // working, or on a reprovisioned box — would otherwise leak one
    // per login until they hit the cap and only the dashboard could
    // clear it.
    if let Some(previous) = &previous
        && let Err(e) = device::revoke(&client, &previous.credential.token).await
    {
        // Out loud, not a debug line. This is the leak the block
        // above exists to prevent, and the user is the only one who
        // can finish it: the old token is live for its full year and
        // no longer reachable from this machine. `logout` says the
        // same thing when its revocation fails.
        output.status(&format!(
            "Warning: the credential this login replaces could not be revoked ({e}). Revoke \
             it from the CLI Tokens page of your Mergify dashboard — it stays valid, and \
             counts against your token limit, until you do.",
        ))?;
    }
    // After the credential is stored, never before: a name is a nice
    // sentence, and losing the token the server already minted
    // because the network blinked on the way to `/v1/user` is not a
    // trade worth making.
    let login = identity::login_name(api_url.clone(), &credential.token).await;

    // `login` is the moment the user is actually watching. Telling
    // them here that something in their shell outranks what they
    // just approved is the difference between a puzzling 403 an hour
    // later and one line now — and `auth status`, which says the
    // same thing, is a command they have no reason to run after a
    // login that appeared to succeed.
    //
    // With one correction: when the variable holds the credential
    // this login just replaced and revoked, "commands use it
    // instead" is worse than no note at all. Re-logging in with the
    // token exported is the obvious way to reach this.
    let overriding = auth::overriding_env_var();
    let overriding = match &previous {
        Some(previous) if auth::overriding_env_var_holds(&previous.credential.token) => {
            output.status(
                "Warning: MERGIFY_TOKEN holds the credential this login replaced, which has \
                 been revoked. Unset it so Mergify commands use the new one.",
            )?;
            None
        }
        _ => overriding,
    };

    emit(output, &api_url, login.as_deref(), &location, overriding)
}

/// Ask the server to revoke a token this machine could not store,
/// and say nothing if it will not.
///
/// The command is already returning its own error, and the user
/// never asked about this token: it was minted a moment ago and
/// never reached the store, so there is nothing they can do about it
/// that the error above does not already lead to.
async fn revoke_quietly(client: &mergify_core::HttpClient, token: &str) {
    if let Err(e) = device::revoke(client, token).await {
        tracing::debug!(error = %e, "could not revoke the credential we could not store");
    }
}

/// What the user has to do, on stderr, while the poll loop waits.
fn instructions(authorization: &device::Authorization) -> String {
    let theme = mergify_tui::Theme::detect();
    let url = authorization
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&authorization.verification_uri);
    format!(
        "Open this URL to authorize the Mergify CLI:\n\n    {url}\n\n\
         and confirm this code:\n\n    {bold}{code}{reset}\n\n\
         Waiting for approval…",
        bold = theme.bold.render(),
        code = authorization.user_code,
        reset = theme.reset,
    )
}

/// The absolute moment a token minted `now` runs out. `None` when
/// the server's `expires_in` does not fit a timestamp, which is a
/// server that has stopped making sense rather than a reason to
/// throw the credential away.
fn expires_at(now: DateTime<Utc>, expires_in_seconds: u64) -> Option<DateTime<Utc>> {
    now.checked_add_signed(chrono::TimeDelta::try_seconds(
        i64::try_from(expires_in_seconds).ok()?,
    )?)
}

fn emit(
    output: &mut dyn Output,
    api_url: &Url,
    login: Option<&str>,
    location: &Location,
    overriding: Option<&'static str>,
) -> Result<(), CliError> {
    let result = LoginResult {
        api_url: api_url.to_string(),
        login: login.map(str::to_owned),
        stored_in: location.to_string(),
        overridden_by: overriding.map(str::to_owned),
    };
    let theme = mergify_tui::Theme::detect();
    output.emit(&result, &mut |w: &mut dyn Write| {
        let who = match login {
            Some(login) => format!(" as {login}"),
            None => String::new(),
        };
        writeln!(
            w,
            "{green}✓{reset} Logged in to {api_url}{who}.",
            green = theme.green.render(),
            reset = theme.reset,
        )?;
        writeln!(w, "Credential stored in {location}.")?;
        if let Some(name) = overriding {
            writeln!(
                w,
                "\n{warn}Note:{reset} {name} is set, so Mergify commands use it instead of \
                 the credential you just stored. Unset it to use this login.",
                warn = theme.warn.render(),
                reset = theme.reset,
            )?;
        }
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use mergify_test_support::Captured;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::body_string_contains;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;
    use crate::testing::file_store;
    use crate::testing::with_mergify_token;

    fn authorization() -> device::Authorization {
        device::Authorization {
            device_code: "dev-secret".to_string(),
            user_code: "BCDF-GHJK".to_string(),
            verification_uri: "https://dashboard.mergify.com/device".to_string(),
            verification_uri_complete: Some(
                "https://dashboard.mergify.com/device?user_code=BCDF-GHJK".to_string(),
            ),
            expires_in: Some(600),
            interval: Some(0),
        }
    }

    async fn mount_flow(server: &MockServer, with_identity: bool) {
        Mock::given(method("POST"))
            .and(path("/v1/oauth/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-secret",
                "user_code": "BCDF-GHJK",
                "verification_uri": "https://dashboard.mergify.com/device",
                "verification_uri_complete":
                    "https://dashboard.mergify.com/device?user_code=BCDF-GHJK",
                "expires_in": 600,
                "interval": 0,
            })))
            .expect(1)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "mut_secret",
                "token_type": "bearer",
                "expires_in": 31_536_000,
            })))
            .expect(1)
            .mount(server)
            .await;
        let user = if with_identity {
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 42,
                "login": "sileht",
            }))
        } else {
            ResponseTemplate::new(404).set_body_json(serde_json::json!({"detail": "Not Found"}))
        };
        Mock::given(method("GET"))
            .and(path("/v1/user"))
            .respond_with(user)
            .expect(1)
            .mount(server)
            .await;
    }

    #[test]
    fn login_stores_the_credential_and_names_the_account() {
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            mount_flow(&server, true).await;
            let (dir, store) = file_store();
            let mut captured = Captured::human();

            run(
                LoginOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();

            let api_url = Url::parse(&server.uri()).unwrap();
            let stored = store.get(&api_url).unwrap().unwrap();
            assert_eq!(stored.credential.token, "mut_secret");
            assert!(
                stored.credential.expires_at.is_some(),
                "the server's expires_in must become a stored expiry",
            );

            let stdout = captured.stdout();
            assert!(stdout.contains("Logged in to"), "got {stdout:?}");
            assert!(stdout.contains("as sileht."), "got {stdout:?}");
            assert!(
                !stdout.contains("mut_secret"),
                "the token must never be printed, got {stdout:?}",
            );
            drop(dir);
        });
    }

    // The verification URL and the code are the whole point of the
    // command, and they go to stderr so stdout stays the result.
    #[test]
    fn the_code_and_url_are_printed_to_stderr() {
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            mount_flow(&server, true).await;
            let (dir, store) = file_store();
            let mut captured = Captured::human();

            run(
                LoginOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();

            let stderr = captured.stderr();
            assert!(stderr.contains("BCDF-GHJK"), "got {stderr:?}");
            assert!(
                stderr.contains("https://dashboard.mergify.com/device?user_code=BCDF-GHJK"),
                "got {stderr:?}",
            );
            drop(dir);
        });
    }

    // The note has to reach the user from `run`, not merely be
    // printable by the renderer: a `MERGIFY_TOKEN` in the shell
    // makes every command ignore the credential this one just
    // stored, and `login` is the last moment anybody is watching.
    #[test]
    fn login_reports_an_overriding_env_var() {
        let (dir, store) = file_store();
        let mut captured = Captured::human();
        let stdout = with_mergify_token(Some("env-token"), async {
            let server = MockServer::start().await;
            mount_flow(&server, true).await;
            run(
                LoginOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();
            captured.stdout()
        });

        assert!(stdout.contains("Logged in to"), "got {stdout:?}");
        assert!(
            stdout.contains("MERGIFY_TOKEN is set, so Mergify commands use it"),
            "got {stdout:?}",
        );
        drop(dir);
    }

    // A deployment too old to serve `/v1/user` still logs in; it just
    // cannot say whose credential it minted.
    #[test]
    fn login_succeeds_when_the_deployment_cannot_name_the_account() {
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            mount_flow(&server, false).await;
            let (dir, store) = file_store();
            let mut captured = Captured::human();

            run(
                LoginOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();

            let api_url = Url::parse(&server.uri()).unwrap();
            assert!(store.get(&api_url).unwrap().is_some());
            let stdout = captured.stdout();
            assert!(stdout.contains("Logged in to"), "got {stdout:?}");
            assert!(!stdout.contains(" as "), "got {stdout:?}");
            drop(dir);
        });
    }

    // The credential a re-login replaces is unreachable from this
    // machine afterwards and still live on the server for its full
    // year. Leaking one per login walks the user into the token cap,
    // which only the dashboard can clear.
    #[test]
    fn login_revokes_the_credential_it_replaces() {
        let (dir, store) = file_store();
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            mount_flow(&server, true).await;
            Mock::given(method("POST"))
                .and(path("/v1/oauth/revoke"))
                .and(body_string_contains("token=mut_previous"))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&server)
                .await;
            let api_url = Url::parse(&server.uri()).unwrap();
            store
                .set(
                    &api_url,
                    &Credential {
                        token: "mut_previous".to_string(),
                        expires_at: None,
                    },
                )
                .unwrap();
            let mut captured = Captured::human();

            run(
                LoginOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();

            assert_eq!(
                store.get(&api_url).unwrap().unwrap().credential.token,
                "mut_secret",
            );
        });
        drop(dir);
    }

    // A refusal from the server must leave nothing behind.
    // The store is read before any grant exists, so an unreadable
    // one fails before the user has a code in front of them. Read
    // after, the command dies with the approval page already open,
    // and whatever they approve there mints a token nothing stores
    // and nothing can revoke.
    #[test]
    fn an_unreadable_store_fails_before_a_grant_is_opened() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("credentials.json");
        std::fs::write(&file, b"{ this is not json").unwrap();
        let store = mergify_core::CredentialStore::file_at(file);
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            let mut captured = Captured::human();

            let err = run(
                LoginOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap_err();

            assert!(err.to_string().contains("credentials.json"), "got {err}");
            assert!(
                server.received_requests().await.unwrap().is_empty(),
                "no grant may be opened before the store is known to be readable",
            );
            assert_eq!(captured.stderr(), "");
        });
        drop(dir);
    }

    // A revocation that fails leaves the replaced token live for its
    // full year, reachable only from the dashboard. Saying so is the
    // whole point of revoking it here.
    #[test]
    fn a_failed_revocation_of_the_replaced_credential_is_said_out_loud() {
        let (dir, store) = file_store();
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            mount_flow(&server, true).await;
            Mock::given(method("POST"))
                .and(path("/v1/oauth/revoke"))
                .respond_with(ResponseTemplate::new(429).append_header("retry-after", "0"))
                .mount(&server)
                .await;
            let api_url = Url::parse(&server.uri()).unwrap();
            store
                .set(
                    &api_url,
                    &Credential {
                        token: "mut_previous".to_string(),
                        expires_at: None,
                    },
                )
                .unwrap();
            let mut captured = Captured::human();

            run(
                LoginOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();

            // The login itself still succeeded.
            assert_eq!(
                store.get(&api_url).unwrap().unwrap().credential.token,
                "mut_secret",
            );
            let stderr = captured.stderr();
            assert!(stderr.contains("could not be revoked"), "got {stderr:?}");
            assert!(stderr.contains("CLI Tokens"), "got {stderr:?}");
        });
        drop(dir);
    }

    // Re-logging in with the old credential exported: the variable
    // now holds a revoked token, so "commands use it instead" would
    // send the user off with a dead one.
    #[test]
    fn login_says_to_unset_an_env_var_holding_the_replaced_credential() {
        let (dir, store) = file_store();
        with_mergify_token(Some("mut_previous"), async {
            let server = MockServer::start().await;
            mount_flow(&server, true).await;
            Mock::given(method("POST"))
                .and(path("/v1/oauth/revoke"))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&server)
                .await;
            let api_url = Url::parse(&server.uri()).unwrap();
            store
                .set(
                    &api_url,
                    &Credential {
                        token: "mut_previous".to_string(),
                        expires_at: None,
                    },
                )
                .unwrap();
            let mut captured = Captured::human();

            run(
                LoginOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();

            let stderr = captured.stderr();
            assert!(
                stderr.contains("holds the credential this login replaced"),
                "got {stderr:?}",
            );
            let stdout = captured.stdout();
            assert!(!stdout.contains("Note:"), "got {stdout:?}");
        });
        drop(dir);
    }

    #[test]
    fn a_denied_grant_stores_nothing() {
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/oauth/device/code"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "device_code": "dev-secret",
                    "user_code": "BCDF-GHJK",
                    "verification_uri": "https://dashboard.mergify.com/device",
                    "expires_in": 600,
                    "interval": 0,
                })))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/v1/oauth/token"))
                .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                    "error": "access_denied",
                    "error_description": "The request was denied by the user.",
                })))
                .mount(&server)
                .await;
            let (dir, store) = file_store();
            let mut captured = Captured::human();

            let err = run(
                LoginOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap_err();

            assert!(err.to_string().contains("denied by the user"), "got {err}");
            let api_url = Url::parse(&server.uri()).unwrap();
            assert_eq!(store.get(&api_url).unwrap(), None);
            drop(dir);
        });
    }

    // Without `verification_uri_complete` the user has to type the
    // code, so the plain page is the one to point them at.
    #[test]
    fn the_instructions_fall_back_to_the_plain_verification_url() {
        let mut authorization = authorization();
        authorization.verification_uri_complete = None;
        let rendered = instructions(&authorization);
        assert!(
            rendered.contains("https://dashboard.mergify.com/device\n"),
            "got {rendered:?}",
        );
        assert!(rendered.contains("BCDF-GHJK"), "got {rendered:?}");
    }

    #[test]
    fn expires_at_turns_the_servers_lifetime_into_a_moment() {
        let now = DateTime::parse_from_rfc3339("2026-09-04T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            expires_at(now, 31_536_000).unwrap().to_rfc3339(),
            "2027-09-04T00:00:00+00:00",
        );
    }

    #[test]
    fn a_nonsensical_lifetime_yields_no_expiry_rather_than_a_panic() {
        let now = DateTime::parse_from_rfc3339("2026-09-04T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(expires_at(now, u64::MAX), None);
    }
}
