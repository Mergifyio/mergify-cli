//! The name this machine calls itself.
//!
//! One caller: the `device_name` `auth login` sends when it opens a
//! grant, so the approval page's Token name field defaults to
//! `Mergify CLI on <hostname>` rather than to the static client name
//! every machine shares. The field is optional on the wire — a
//! machine that cannot name itself sends nothing and the server
//! keeps its own default — so nothing here is worth failing a login
//! over, and every failure is a `None`.

use std::process::Command;
use std::time::Duration;

/// How long `hostname` gets. It reads a name the kernel already
/// holds, so this is not a budget — it is the ceiling on how long a
/// wedged shim on `PATH` can hold up a login. `login` runs this
/// before anything is on screen, and a blank terminal is a worse
/// failure than a token named "Mergify CLI".
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);

/// The machine's hostname, as the machine reports it.
///
/// `hostname(1)` first: it ships on all three platforms and is the
/// only answer that is still right after the name changes under a
/// running shell. The variables are the fallback for a container
/// with no `hostname` on `PATH`.
///
/// Verbatim, on purpose. Not lower-cased, not stripped of a
/// `.local` suffix, not cut to length. The server sanitizes the
/// value (printable ASCII, collapsed whitespace, 60 characters) and
/// composes the label itself, so a client that trimmed first would
/// only disagree with it.
pub async fn name() -> Option<String> {
    // Off the runtime thread and on a deadline. The value decorates
    // the approval page and nothing waits on it, so a `hostname`
    // that does not come back is dropped rather than waited for.
    let from_command =
        tokio::time::timeout(LOOKUP_TIMEOUT, tokio::task::spawn_blocking(from_command))
            .await
            .ok()
            .and_then(Result::ok)
            .flatten();
    from_command.or_else(from_env)
}

fn from_command() -> Option<String> {
    let output = Command::new("hostname").output().ok()?;
    if !output.status.success() {
        return None;
    }
    // Lossy because a name is worth more than the byte that did not
    // decode: the server maps anything outside printable ASCII to a
    // space anyway.
    as_name(&String::from_utf8_lossy(&output.stdout))
}

fn from_env() -> Option<String> {
    // `COMPUTERNAME` is Windows' own; `HOSTNAME` is exported by some
    // shells and images and is worth asking for once the command is
    // gone.
    mergify_core::env::var_non_empty("COMPUTERNAME")
        .or_else(|| mergify_core::env::var_non_empty("HOSTNAME"))
        .as_deref()
        .and_then(as_name)
}

/// What a `hostname` process printed, as a name.
///
/// Dropping the line's own newline is reading the command's output,
/// not sanitizing the value — what is inside the line goes to the
/// server as it stands. A name that is only whitespace is no name.
fn as_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Whatever this machine is called, what comes back has to be
    // sendable: the newline `hostname` prints must not reach the
    // form body.
    #[tokio::test]
    async fn the_name_of_the_machine_running_the_suite_is_sendable() {
        if let Some(name) = name().await {
            assert!(!name.is_empty());
            assert_eq!(name.trim(), name, "a name must carry no surrounding space");
        }
    }

    #[test]
    fn a_printed_name_loses_its_line_and_nothing_else() {
        assert_eq!(as_name("work-laptop\n").as_deref(), Some("work-laptop"));
        // The `.local` stays: the server composes and caps the
        // label, and a client that trimmed here would disagree with
        // what the approval page shows.
        assert_eq!(
            as_name("some-macbook.local\n").as_deref(),
            Some("some-macbook.local"),
        );
    }

    #[test]
    fn a_machine_that_names_itself_nothing_has_no_name() {
        assert_eq!(as_name("  \n"), None);
        assert_eq!(as_name(""), None);
    }

    // The container case: no `hostname` on PATH, and the image sets
    // the variable instead.
    #[test]
    fn the_variables_are_the_fallback() {
        let from_windows = temp_env::with_vars(
            [
                ("COMPUTERNAME", Some("WIN-BOX")),
                ("HOSTNAME", Some("ignored")),
            ],
            from_env,
        );
        assert_eq!(from_windows.as_deref(), Some("WIN-BOX"));

        let from_shell = temp_env::with_vars(
            [("COMPUTERNAME", None), ("HOSTNAME", Some("build-42"))],
            from_env,
        );
        assert_eq!(from_shell.as_deref(), Some("build-42"));

        let from_nothing = temp_env::with_vars(
            [("COMPUTERNAME", None::<&str>), ("HOSTNAME", None)],
            from_env,
        );
        assert_eq!(from_nothing, None);
    }
}
