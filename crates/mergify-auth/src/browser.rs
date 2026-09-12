//! Putting the verification page in front of the user.
//!
//! A convenience, never a step in the grant. There is no browser on
//! a CI runner, in a container, or at the far end of an SSH session,
//! and `auth login` has to work on all three — so every failure here
//! is a debug line, and the URL is printed whether this succeeds or
//! not.
//!
//! No crate for this: each platform is one process spawn, and the
//! workspace's dependency policy is not worth spending on thirty
//! lines of [`Command`].

use std::io;
use std::process::Command;
use std::process::Stdio;

/// How [`crate::login`] reaches a browser.
///
/// A trait rather than a free function so the suite can watch the
/// URL go past without opening a window on whoever ran `cargo test`.
pub trait Browser {
    /// Show `url`, or say why not. An `Err` is never fatal.
    fn open(&self, url: &str) -> io::Result<()>;
}

/// The user's own browser, through the platform's URL opener.
pub struct SystemBrowser;

impl Browser for SystemBrowser {
    fn open(&self, url: &str) -> io::Result<()> {
        launch(command_for(url)?)
    }
}

#[cfg(target_os = "macos")]
fn command_for(url: &str) -> io::Result<Command> {
    // macOS has no `DISPLAY` to consult, so the SSH variables are
    // the only signal that the screen `open` would use is not the
    // one the user is looking at. It belongs to whoever is sitting
    // at the machine, who did not ask to approve anything and would
    // be handed a page with somebody else's user code on it.
    //
    // The Linux arm deliberately asks a different question rather
    // than this one: an SSH session there can have a forwarded
    // display, which is the case where opening *is* right, and
    // `DISPLAY` is what tells the two apart. macOS has nothing
    // equivalent to forward.
    if mergify_core::env::var_non_empty("SSH_CONNECTION").is_some()
        || mergify_core::env::var_non_empty("SSH_TTY").is_some()
    {
        return Err(io::Error::other(
            "an SSH session: the browser would open on the machine's own screen",
        ));
    }
    let mut command = Command::new("open");
    command.arg(url);
    Ok(command)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn command_for(url: &str) -> io::Result<Command> {
    // With no graphical session `xdg-open` falls through to a
    // terminal browser (`www-browser`, `w3m`, `lynx`), which takes
    // over the very terminal the user is reading the code from. A
    // headless box and an SSH session are two of the three cases
    // this command exists to keep working, so they get the printed
    // URL and nothing else.
    //
    // The display rather than the SSH variables, on purpose: an SSH
    // session with X11 forwarding has a display, and it is the
    // user's own — refusing it would withhold the browser from the
    // one remote case that can show one. The converse, an rc file
    // that exports `DISPLAY=:0` unconditionally, then opens on the
    // remote machine's monitor; that configuration sends every
    // GUI-launching command there and is not ours to second-guess.
    if mergify_core::env::var_non_empty("DISPLAY").is_none()
        && mergify_core::env::var_non_empty("WAYLAND_DISPLAY").is_none()
    {
        return Err(io::Error::other(
            "no graphical session: neither DISPLAY nor WAYLAND_DISPLAY is set",
        ));
    }
    let mut command = Command::new("xdg-open");
    command.arg(url);
    Ok(command)
}

#[cfg(windows)]
fn command_for(url: &str) -> io::Result<Command> {
    use std::os::windows::process::CommandExt;

    let mut command = Command::new("cmd");
    // `raw_arg`, because the whole point is the quoting: the
    // standard escaping would leave the URL unquoted and `cmd`
    // would act on what is in it.
    command.arg("/C").raw_arg(windows_start_argument(url)?);
    Ok(command)
}

/// The verbatim `cmd` command line that opens `url`.
///
/// `start` is a `cmd` builtin, so there is no reaching it except
/// through a shell that re-parses its own command line — and an `&`
/// or a `|` in a URL is a command separator to that shell.
/// `--api-url` decides which host writes that URL, which makes this
/// the same threat `device::checked_uri` exists for. Quoting the URL
/// makes those characters literal, and that is only sound because
/// the URL cannot carry a quote of its own: it came through
/// `Url::parse`, which percent-encodes `"` in every component it can
/// appear in. Anything that proves otherwise is refused rather than
/// handed to `cmd`.
///
/// A `%` is refused for the same reason, and it is the subtler one:
/// `cmd` expands `%NAME%` on its command line *after* this check has
/// run, inside the quotes as well as outside, and a command line has
/// no escape for it (`%%` only works in a batch file). So a URL
/// whose percent-escapes happen to bracket a variable name opens a
/// different page than the one printed — the drift `verification_url`
/// exists to prevent — and a variable whose value holds a `"` closes
/// the quote this function just proved could not be closed. Refusing
/// costs a browser that would have opened; the URL is printed either
/// way.
///
/// The empty `""` is `start`'s window-title argument. Without it
/// `start` reads the quoted URL as the title and opens nothing.
///
/// Compiled on every platform so the suite can pin it anywhere; only
/// Windows calls it.
#[cfg(any(windows, test))]
fn windows_start_argument(url: &str) -> io::Result<String> {
    if url.contains(['"', '%']) || url.chars().any(char::is_control) {
        return Err(io::Error::other(
            "the verification URL holds a character this client will not hand to cmd",
        ));
    }
    Ok(format!("start \"\" \"{url}\""))
}

fn launch(mut command: Command) -> io::Result<()> {
    // Whatever the opener has to say is not the user's problem: the
    // browser opens or it does not, and the URL is printed either
    // way. Its output would land in the middle of the code the user
    // is trying to read.
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // The opener exits as soon as the browser has the URL, but
    // `login` then sits in the poll loop for minutes. A child nobody
    // waits on is a zombie for all of it.
    //
    // `Builder::spawn` rather than `thread::spawn`, which panics
    // when the OS refuses a thread. This module promises the login
    // proceeds whatever happens here, and a panic would take the
    // login down with it — a zombie until the process exits is the
    // cheaper of the two failures.
    let reaper = std::thread::Builder::new().spawn(move || match child.wait() {
        // What the opener made of the URL is its own business, but a
        // non-zero exit is the only sign the page never opened, and
        // `open` reports "no application knows this URL" that way
        // and nowhere else — its stderr went to /dev/null above.
        Ok(status) if !status.success() => {
            tracing::debug!(%status, "the browser opener exited with an error");
        }
        Ok(_) => {}
        Err(e) => tracing::debug!(error = %e, "could not wait for the browser opener"),
    });
    if let Err(e) = reaper {
        tracing::debug!(error = %e, "could not reap the browser opener");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A `&` in a query string is ordinary, and to `cmd` it is a
    // command separator. It has to end up inside the quotes.
    #[test]
    fn the_windows_command_line_quotes_the_url() {
        assert_eq!(
            windows_start_argument("https://dashboard.mergify.com/device?user_code=BCDF-GHJK&x=1")
                .unwrap(),
            "start \"\" \"https://dashboard.mergify.com/device?user_code=BCDF-GHJK&x=1\"",
        );
    }

    // The quoting above is the only thing between a hostile
    // `--api-url` and `cmd`, so a URL that could close the quote is
    // not opened at all.
    #[test]
    fn a_url_carrying_a_quote_is_not_handed_to_cmd() {
        assert!(windows_start_argument("https://evil.example/\"&calc").is_err());
        assert!(windows_start_argument("https://evil.example/\r\nhi").is_err());
    }

    // `cmd` expands `%NAME%` after this check, and the value it
    // splices in is not held to the rule the check just applied.
    #[test]
    fn a_url_carrying_a_percent_is_not_handed_to_cmd() {
        assert!(windows_start_argument("https://evil.example/?a=%SOMEVAR%").is_err());
    }

    // The whole "never fail the login" promise rests on this
    // returning rather than panicking when the opener is not there
    // — the CI runner and the container both reach it that way.
    #[test]
    fn an_opener_that_is_not_installed_is_an_error_not_a_panic() {
        assert!(launch(Command::new("mergify-no-such-url-opener")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn launching_an_opener_that_exists_succeeds() {
        launch(Command::new("true")).unwrap();
    }

    // An opener that starts and then refuses the URL is the one
    // failure the caller cannot see: `launch` has already returned
    // `Ok` by the time the child exits, and the reaper only has a
    // `debug!` to say so with. What this pins is that the reaper
    // handles that status instead of unwinding on it.
    #[cfg(unix)]
    #[test]
    fn an_opener_that_exits_non_zero_still_lets_the_login_proceed() {
        launch(Command::new("false")).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_opens_the_url_with_open() {
        let command = temp_env::with_vars(
            [("SSH_CONNECTION", None::<&str>), ("SSH_TTY", None::<&str>)],
            || command_for("https://dashboard.mergify.com/device"),
        )
        .unwrap();
        assert_eq!(command.get_program(), "open");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["https://dashboard.mergify.com/device"],
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_graphical_session_gets_xdg_open() {
        let command = temp_env::with_vars(
            [("DISPLAY", Some(":0")), ("WAYLAND_DISPLAY", None::<&str>)],
            || command_for("https://dashboard.mergify.com/device"),
        )
        .unwrap();
        assert_eq!(command.get_program(), "xdg-open");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["https://dashboard.mergify.com/device"],
        );
    }

    // Over SSH to a Mac, `open` reaches the screen of whoever is
    // sitting at that machine, not the person who ran the command.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_ssh_session_to_a_mac_opens_nothing() {
        let opened = temp_env::with_vars(
            [
                ("SSH_CONNECTION", Some("10.0.0.1 52000 10.0.0.2 22")),
                ("SSH_TTY", None),
            ],
            || command_for("https://dashboard.mergify.com/device").is_ok(),
        );
        assert!(
            !opened,
            "an SSH session must not open a browser on the host"
        );
    }

    // The SSH session the device grant exists for: `xdg-open` here
    // would hand the URL to a terminal browser and eat the terminal.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_headless_session_opens_nothing() {
        let opened = temp_env::with_vars(
            [("DISPLAY", None::<&str>), ("WAYLAND_DISPLAY", None::<&str>)],
            || command_for("https://dashboard.mergify.com/device").is_ok(),
        );
        assert!(!opened, "a session with no display must not run xdg-open");
    }
}
