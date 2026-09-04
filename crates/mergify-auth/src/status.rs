//! `mergify auth status` — is there a credential, whose is it, and
//! does the API still accept it?
//!
//! The last question is the one only the API can answer. A stored
//! token carries an expiry, but it can be revoked from the dashboard
//! long before that, so a `status` that only read the local file
//! would happily call a dead credential live.

use std::io::Write;

use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use mergify_core::CliError;
use mergify_core::CredentialStore;
use mergify_core::Output;
use mergify_core::auth;
use mergify_core::credentials::StoredCredential;
use serde::Serialize;
use url::Url;

use crate::identity;
use crate::identity::Identity;

pub struct StatusOptions<'a> {
    pub api_url: Option<&'a str>,
    pub store: &'a CredentialStore,
}

#[derive(Serialize)]
struct StatusResult {
    api_url: String,
    login: Option<String>,
    stored_in: String,
    expires_at: Option<String>,
    /// The environment variable Mergify commands would use instead
    /// of the stored credential, if one is set.
    overridden_by: Option<String>,
}

/// Run the `auth status` command.
pub async fn run(opts: StatusOptions<'_>, output: &mut dyn Output) -> Result<(), CliError> {
    let api_url = auth::resolve_api_url(opts.api_url)?;

    let overriding = auth::overriding_env_var();

    let Some(stored) = opts.store.get(&api_url)? else {
        // "Not logged in" would be the same lie as "logged in" is
        // when a credential is overridden, pointing the other way:
        // with `MERGIFY_TOKEN` exported every Mergify command
        // authenticates fine, and telling the user to log in would
        // send them to fix something that is not broken.
        if let Some(name) = overriding {
            return emit_env_only(output, &api_url, name);
        }
        return Err(CliError::Configuration(format!(
            "not logged in to {api_url}. Run `mergify auth login`.",
        )));
    };

    match identity::whoami(api_url.clone(), &stored.credential.token).await? {
        Identity::Known(user) => emit(
            output,
            &api_url,
            &stored,
            Some(&user.login),
            overriding,
            Utc::now(),
        ),
        // The credential is real enough to be stored and useless
        // enough that every command will fail. Saying "logged in"
        // here would be the one answer a user cannot act on.
        Identity::Refused => Err(CliError::Configuration(format!(
            "the credential stored for {api_url} is no longer valid — it may have been \
             revoked or have expired. Run `mergify auth login`.",
        ))),
        // A deployment older than `GET /v1/user`. There is a
        // credential and no way to check it; report both.
        Identity::Unsupported => emit(output, &api_url, &stored, None, overriding, Utc::now()),
    }
}

/// What to print when there is no stored credential but the
/// environment supplies one. Not a failure: every Mergify command
/// authenticates, which is what the user asked about.
fn emit_env_only(
    output: &mut dyn Output,
    api_url: &Url,
    name: &'static str,
) -> Result<(), CliError> {
    let result = StatusResult {
        api_url: api_url.to_string(),
        login: None,
        stored_in: name.to_string(),
        expires_at: None,
        overridden_by: Some(name.to_string()),
    };
    let theme = mergify_tui::Theme::detect();
    output.emit(&result, &mut |w: &mut dyn Write| {
        writeln!(
            w,
            "{green}✓{reset} Authenticated to {api_url} with {name} from the environment.",
            green = theme.green.render(),
            reset = theme.reset,
        )?;
        writeln!(
            w,
            "  No credential is stored on this machine. Run `mergify auth login` to store one."
        )
    })?;
    Ok(())
}

fn emit(
    output: &mut dyn Output,
    api_url: &Url,
    stored: &StoredCredential,
    login: Option<&str>,
    overriding: Option<&'static str>,
    now: DateTime<Utc>,
) -> Result<(), CliError> {
    // Seconds, not the nanoseconds `Utc::now()` carries: this is a
    // date a year out, and six digits of subsecond precision on it
    // is noise.
    let expires_at = stored
        .credential
        .expires_at
        .map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true));
    let expired = stored.credential.expires_at.is_some_and(|at| at <= now);
    let result = StatusResult {
        api_url: api_url.to_string(),
        login: login.map(str::to_owned),
        stored_in: stored.location.to_string(),
        expires_at: expires_at.clone(),
        overridden_by: overriding.map(str::to_owned),
    };
    let theme = mergify_tui::Theme::detect();
    let location = stored.location.to_string();
    output.emit(&result, &mut |w: &mut dyn Write| {
        match login {
            Some(login) => writeln!(
                w,
                "{green}✓{reset} Logged in to {api_url} as {login}.",
                green = theme.green.render(),
                reset = theme.reset,
            )?,
            None => writeln!(
                w,
                "{green}✓{reset} Logged in to {api_url}. This deployment cannot confirm \
                 which account the credential belongs to.",
                green = theme.green.render(),
                reset = theme.reset,
            )?,
        }
        writeln!(w, "  Credential: {location}")?;
        if let Some(at) = &expires_at {
            // `relative_time` takes an absolute delta, so asking it
            // for a future rendering of a past moment prints an
            // expired credential as "expires in about a year".
            let (label, relative) = if expired {
                ("Expired:", mergify_tui::relative_time(at, now, false))
            } else {
                ("Expires:", mergify_tui::relative_time(at, now, true))
            };
            writeln!(w, "  {label:<11} {at} ({relative})")?;
        }
        // Without this line, "logged in" would be the answer to a
        // question the user did not ask: what commands actually send
        // is the environment variable, not the credential above.
        if let Some(name) = overriding {
            writeln!(
                w,
                "\n{warn}Note:{reset} {name} is set, so Mergify commands use it instead of \
                 this credential.",
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
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;
    use crate::testing::file_store;
    use crate::testing::with_mergify_token;

    fn credential() -> Credential {
        Credential {
            token: "mut_secret".to_string(),
            expires_at: Some(
                DateTime::parse_from_rfc3339("2027-09-04T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
        }
    }

    async fn mount_user(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path("/v1/user"))
            .respond_with(response)
            .mount(server)
            .await;
    }

    #[test]
    fn status_reports_the_account_the_store_and_the_expiry() {
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            mount_user(
                &server,
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": 42, "login": "sileht"})),
            )
            .await;
            let (dir, store) = file_store();
            store
                .set(&Url::parse(&server.uri()).unwrap(), &credential())
                .unwrap();
            let mut captured = Captured::human();

            run(
                StatusOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();

            let stdout = captured.stdout();
            assert!(stdout.contains("as sileht."), "got {stdout:?}");
            assert!(stdout.contains("credentials.json"), "got {stdout:?}");
            assert!(stdout.contains("2027-09-04"), "got {stdout:?}");
            assert!(
                !stdout.contains("mut_secret"),
                "the token must never be printed, got {stdout:?}",
            );
            drop(dir);
        });
    }

    // The inverse of the lie the override note prevents: with
    // `MERGIFY_TOKEN` exported every Mergify command authenticates,
    // so "not logged in" would send the user to fix something that
    // is not broken.
    #[test]
    fn status_reports_an_environment_credential_when_nothing_is_stored() {
        let (dir, store) = file_store();
        let mut captured = Captured::human();
        let stdout = with_mergify_token(Some("mut_from_the_environment"), async {
            let server = MockServer::start().await;
            run(
                StatusOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();
            captured.stdout()
        });

        assert!(stdout.contains("Authenticated to"), "got {stdout:?}");
        assert!(stdout.contains("MERGIFY_TOKEN"), "got {stdout:?}");
        assert!(stdout.contains("No credential is stored"), "got {stdout:?}");
        assert!(
            !stdout.contains("mut_from_the_environment"),
            "the token must never be printed, got {stdout:?}",
        );
        drop(dir);
    }

    // Same reason as in `login`: proving the renderer can print the
    // note proves nothing about `run` asking it to.
    #[test]
    fn status_reads_the_overriding_env_var_from_the_environment() {
        let (dir, store) = file_store();
        let mut captured = Captured::human();
        let stdout = with_mergify_token(Some("env-token"), async {
            let server = MockServer::start().await;
            mount_user(
                &server,
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": 42, "login": "sileht"})),
            )
            .await;
            store
                .set(&Url::parse(&server.uri()).unwrap(), &credential())
                .unwrap();
            run(
                StatusOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();
            captured.stdout()
        });

        assert!(
            stdout.contains("MERGIFY_TOKEN is set, so Mergify commands use it"),
            "got {stdout:?}",
        );
        drop(dir);
    }

    #[test]
    fn status_without_a_credential_says_so_and_fails() {
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            let (dir, store) = file_store();
            let mut captured = Captured::human();

            let err = run(
                StatusOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap_err();

            assert!(err.to_string().contains("not logged in"), "got {err}");
            assert!(err.to_string().contains("auth login"), "got {err}");
            assert!(
                server.received_requests().await.unwrap().is_empty(),
                "with nothing stored there is nothing to check against the API",
            );
            drop(dir);
        });
    }

    // A credential the dashboard revoked is still on disk. Reading
    // only the disk would call it live.
    #[test]
    fn status_reports_a_revoked_credential_as_invalid() {
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            mount_user(
                &server,
                ResponseTemplate::new(403)
                    .set_body_json(serde_json::json!({"detail": "forbidden"})),
            )
            .await;
            let (dir, store) = file_store();
            store
                .set(&Url::parse(&server.uri()).unwrap(), &credential())
                .unwrap();
            let mut captured = Captured::human();

            let err = run(
                StatusOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap_err();

            assert!(err.to_string().contains("no longer valid"), "got {err}");
            assert!(err.to_string().contains("auth login"), "got {err}");
            drop(dir);
        });
    }

    fn stored() -> StoredCredential {
        StoredCredential {
            credential: credential(),
            location: mergify_core::credentials::Location::Keychain,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-04T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    // The note that keeps "logged in" from being the answer to a
    // question the user did not ask: a `MERGIFY_TOKEN` in the shell
    // is what commands actually send.
    #[test]
    fn an_overriding_env_var_is_reported() {
        let mut captured = Captured::human();
        emit(
            &mut captured.output,
            &Url::parse("https://api.mergify.com").unwrap(),
            &stored(),
            Some("sileht"),
            Some("MERGIFY_TOKEN"),
            now(),
        )
        .unwrap();

        let stdout = captured.stdout();
        assert!(stdout.contains("as sileht."), "got {stdout:?}");
        assert!(
            stdout.contains("MERGIFY_TOKEN is set, so Mergify commands use it"),
            "got {stdout:?}",
        );
    }

    // `relative_time` works on an absolute delta, so a past moment
    // asked for a future rendering reads as "expires in a year".
    #[test]
    fn an_expired_credential_is_not_rendered_as_a_future_one() {
        let mut captured = Captured::human();
        let mut stored = stored();
        stored.credential.expires_at = Some(
            DateTime::parse_from_rfc3339("2025-09-04T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        emit(
            &mut captured.output,
            &Url::parse("https://api.mergify.com").unwrap(),
            &stored,
            None,
            None,
            now(),
        )
        .unwrap();

        let stdout = captured.stdout();
        assert!(stdout.contains("Expired:"), "got {stdout:?}");
        assert!(stdout.contains("ago"), "got {stdout:?}");
        assert!(!stdout.contains("(~"), "got {stdout:?}");
    }

    #[test]
    fn no_note_when_nothing_overrides_the_credential() {
        let mut captured = Captured::human();
        emit(
            &mut captured.output,
            &Url::parse("https://api.mergify.com").unwrap(),
            &stored(),
            Some("sileht"),
            None,
            now(),
        )
        .unwrap();

        let stdout = captured.stdout();
        assert!(!stdout.contains("Note:"), "got {stdout:?}");
        assert!(stdout.contains("the system keychain"), "got {stdout:?}");
    }

    #[test]
    fn status_says_when_the_deployment_cannot_confirm_the_account() {
        with_mergify_token(None, async {
            let server = MockServer::start().await;
            mount_user(
                &server,
                ResponseTemplate::new(404)
                    .set_body_json(serde_json::json!({"detail": "Not Found"})),
            )
            .await;
            let (dir, store) = file_store();
            store
                .set(&Url::parse(&server.uri()).unwrap(), &credential())
                .unwrap();
            let mut captured = Captured::human();

            run(
                StatusOptions {
                    api_url: Some(&server.uri()),
                    store: &store,
                },
                &mut captured.output,
            )
            .await
            .unwrap();

            let stdout = captured.stdout();
            assert!(stdout.contains("Logged in to"), "got {stdout:?}");
            assert!(stdout.contains("cannot confirm"), "got {stdout:?}");
            drop(dir);
        });
    }
}
