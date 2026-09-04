//! `mergify auth logout` — revoke the stored credential and forget it.

use std::io::Write;

use mergify_core::CliError;
use mergify_core::CredentialStore;
use mergify_core::Output;
use mergify_core::auth;
use serde::Serialize;
use url::Url;

use crate::device;

pub struct LogoutOptions<'a> {
    pub api_url: Option<&'a str>,
    pub store: &'a CredentialStore,
}

#[derive(Serialize)]
struct LogoutResult {
    api_url: String,
    /// Whether there was a credential to remove. `false` is a
    /// successful no-op, not a failure: `logout` promises that the
    /// machine holds no credential afterwards, and it does.
    was_logged_in: bool,
    /// Whether the revocation request was accepted. It is not a
    /// promise that the token is dead: RFC 7009 has the endpoint
    /// answer 200 for any string, so this says the server took the
    /// request, never that it found something to delete.
    revoked: bool,
    /// The environment variable that still authenticates every
    /// Mergify command after this logout, if one is set.
    overridden_by: Option<String>,
}

/// Run the `auth logout` command.
pub async fn run(opts: LogoutOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let api_url = auth::resolve_api_url(opts.api_url)?;
    let overriding = auth::overriding_env_var();

    let stored = opts.store.get(&api_url)?;
    let Some(stored) = stored else {
        // Still ask the store to forget: a keychain that refused to
        // be *read* looks empty to `get` while holding the
        // credential, and `delete` is the call that finds out.
        let removed = opts.store.delete(&api_url)?;
        return emit(output, &api_url, removed, false, overriding);
    };

    // Revoke first, then forget: the server is the copy that matters,
    // and a token whose only trace was the file we just deleted can
    // no longer be revoked from here at all.
    let client = device::client(api_url.clone())?;
    let revocation = device::revoke(&client, &stored.credential.token).await;

    // Removed either way. A `logout` that left the credential in
    // place because the network blinked would have done nothing at
    // all, which is the worse of the two half-states.
    opts.store.delete(&api_url)?;

    // A failed revocation is loud but not fatal. The postcondition
    // the user asked for — this machine no longer holds the
    // credential — holds, and failing the command would make
    // `auth logout` unusable in the teardown scripts that are
    // exactly where it belongs.
    let revoked = match revocation {
        Ok(()) => true,
        Err(e) => {
            output.status(&format!(
                "Warning: the credential was removed from this machine, but the Mergify API \
                 could not be told to revoke it ({e}). Revoke it from the CLI Tokens page of \
                 your Mergify dashboard.",
            ))?;
            false
        }
    };

    emit(output, &api_url, true, revoked, overriding)
}

fn emit(
    output: &mut dyn Output,
    api_url: &Url,
    was_logged_in: bool,
    revoked: bool,
    overriding: Option<&'static str>,
) -> Result<(), CliError> {
    let result = LogoutResult {
        api_url: api_url.to_string(),
        was_logged_in,
        revoked,
        overridden_by: overriding.map(str::to_owned),
    };
    let theme = mergify_tui::Theme::detect();
    output.emit(&result, &mut |w: &mut dyn Write| {
        if was_logged_in {
            // Says only what is true. The revocation endpoint
            // answers 200 for any string and is rate limited per IP,
            // so "the token is dead everywhere" is a claim this
            // command is not in a position to make.
            writeln!(w, "Logged out of {api_url}.")?;
        } else {
            writeln!(w, "No credential stored for {api_url}.")?;
        }
        // The more dangerous direction of the same omission `login`
        // and `status` cover: the user believes access is gone.
        if let Some(name) = overriding {
            writeln!(
                w,
                "\n{warn}Note:{reset} {name} is still set, so Mergify commands remain \
                 authenticated with it.",
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
    use mergify_core::Credential;
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

    fn credential() -> Credential {
        Credential {
            token: "mut_secret".to_string(),
            expires_at: None,
        }
    }

    #[test]
    fn logout_revokes_server_side_and_forgets_locally() {
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/oauth/revoke"))
                .and(body_string_contains("token=mut_secret"))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&server)
                .await;
            let (dir, store) = file_store();
            let api_url = Url::parse(&server.uri()).unwrap();
            store.set(&api_url, &credential()).unwrap();
            let mut captured = Captured::human();

            run(
                LogoutOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();

            assert_eq!(store.get(&api_url).unwrap(), None);
            assert!(captured.stdout().contains("Logged out of"));
            drop(dir);
        });
    }

    // Nothing stored is a successful no-op: the postcondition the
    // user asked for already holds.
    #[test]
    fn logout_without_a_credential_calls_nothing_and_succeeds() {
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            let (dir, store) = file_store();
            let mut captured = Captured::human();

            run(
                LogoutOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();

            assert!(captured.stdout().contains("No credential stored"));
            assert!(
                server.received_requests().await.unwrap().is_empty(),
                "logout must not call an API it has no credential for",
            );
            drop(dir);
        });
    }

    // The dangerous direction of the omission: the user believes
    // access is gone, and every Mergify command still authenticates.
    #[test]
    fn logout_says_when_an_environment_credential_still_authenticates() {
        let (dir, store) = file_store();
        let mut captured = Captured::human();
        let stdout = with_mergify_token(Some("mut_from_the_environment"), async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/oauth/revoke"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;
            store
                .set(&Url::parse(&server.uri()).unwrap(), &credential())
                .unwrap();
            run(
                LogoutOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();
            captured.stdout()
        });

        assert!(stdout.contains("Logged out of"), "got {stdout:?}");
        assert!(
            stdout.contains("MERGIFY_TOKEN is still set"),
            "got {stdout:?}",
        );
        drop(dir);
    }

    // A failed revocation still clears the machine, says so, and does
    // not fail the command: the postcondition the user asked for
    // holds, and `auth logout` belongs in teardown scripts.
    #[test]
    fn a_failed_revocation_still_removes_the_local_credential() {
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/oauth/revoke"))
                .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                    "error": "invalid_client",
                    "error_description": "Unknown client_id.",
                })))
                .mount(&server)
                .await;
            let (dir, store) = file_store();
            let api_url = Url::parse(&server.uri()).unwrap();
            store.set(&api_url, &credential()).unwrap();
            let mut captured = Captured::human();

            run(
                LogoutOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();

            assert!(
                captured.stderr().contains("CLI Tokens page"),
                "got {:?}",
                captured.stderr(),
            );
            assert_eq!(
                store.get(&api_url).unwrap(),
                None,
                "the local credential must be gone even when the revocation failed",
            );
            drop(dir);
        });
    }
}
