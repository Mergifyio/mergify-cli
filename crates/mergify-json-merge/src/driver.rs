//! The git side: `mergify merge-driver json %A %O %B`.
//!
//! git hands the driver three temporary files — ours (`%A`), the merge
//! base (`%O`) and theirs (`%B`) — and reads the result back from `%A`.
//! Exit 0 means merged; anything from 1 to 127 means conflicted, with
//! `%A` as the conflicted content. (128 and above is a dead driver and
//! aborts the whole merge, which is why nothing here may crash.)
//!
//! When the structural merge declines, the driver does what git would
//! have done without it: a line merge, `git merge-file`, which writes
//! conflict markers into `%A` for a human to resolve. So enabling the
//! driver never turns a merge git completes cleanly into a conflict,
//! with one exception that is the point: a clean line merge of three
//! valid JSON files whose result is NOT valid JSON (two edits around
//! one trailing comma) is reported as a conflict instead of landing.

use std::borrow::Cow;
use std::fs;
use std::path::Path;
use std::process::Command;

use mergify_core::CliError;

use crate::merge::Decline;
use crate::merge::Reason;
use crate::merge::Side;
use crate::merge::merge;
use crate::value;

pub struct DriverOptions<'a> {
    /// `%A`: ours, overwritten with the result.
    pub ours: &'a Path,
    /// `%O`: the merge base; empty when both sides added the file.
    pub base: &'a Path,
    /// `%B`: theirs.
    pub theirs: &'a Path,
    /// `%L`: the conflict-marker length git would use for this path.
    pub marker_size: Option<u32>,
    /// `%P`: the path being merged, for messages only.
    pub path: Option<&'a str>,
}

/// Merge, writing the result over `opts.ours`. `Ok` is a clean merge;
/// [`CliError::Conflict`] is one left for a human, markers and all.
pub fn run(opts: &DriverOptions<'_>) -> Result<(), CliError> {
    let read = |path: &Path, side: Side| {
        fs::read(path).map_err(|e| CliError::wrap(format!("read {side} ({})", path.display()), e))
    };
    let ours = read(opts.ours, Side::Ours)?;
    let base = read(opts.base, Side::Base)?;
    let theirs = read(opts.theirs, Side::Theirs)?;

    // git's own trivial cases, settled on bytes before parsing anything.
    if ours == theirs || base == theirs {
        return Ok(());
    }
    if base == ours {
        return write(opts.ours, &theirs);
    }

    let decline = match utf8(&base, &ours, &theirs) {
        Ok((b, o, t)) => match merge(b, o, t) {
            Ok(Cow::Borrowed(_)) => return Ok(()),
            Ok(Cow::Owned(text)) => return write(opts.ours, text.as_bytes()),
            Err(decline) => decline,
        },
        Err(decline) => decline,
    };
    line_merge(opts, &decline)
}

fn utf8<'a>(
    base: &'a [u8],
    ours: &'a [u8],
    theirs: &'a [u8],
) -> Result<(&'a str, &'a str, &'a str), Decline> {
    let check = |bytes: &'a [u8], side| {
        std::str::from_utf8(bytes).map_err(|e| Decline {
            pointer: String::new(),
            reason: Reason::NotJson {
                side,
                error: format!("not UTF-8 ({e})"),
            },
        })
    };
    Ok((
        check(base, Side::Base)?,
        check(ours, Side::Ours)?,
        check(theirs, Side::Theirs)?,
    ))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    fs::write(path, bytes)
        .map_err(|e| CliError::wrap(format!("write the merge result ({})", path.display()), e))
}

fn line_merge(opts: &DriverOptions<'_>, decline: &Decline) -> Result<(), CliError> {
    let at = opts.path.map(|p| format!("{p}: ")).unwrap_or_default();
    let mut git = Command::new("git");
    git.arg("merge-file");
    if let Some(size) = opts.marker_size {
        git.arg(format!("--marker-size={size}"));
    }
    git.args(["-L", "ours", "-L", "base", "-L", "theirs"])
        .arg(opts.ours)
        .arg(opts.base)
        .arg(opts.theirs);
    let status = git
        .status()
        .map_err(|e| CliError::wrap(format!("{at}run git merge-file"), e))?;
    match status.code() {
        Some(0) => {
            // All three inputs parsed if the structural merge got far
            // enough to decline on something else, so the line merge's
            // result has to parse too.
            if !matches!(decline.reason, Reason::NotJson { .. }) {
                let merged = fs::read(opts.ours)
                    .map_err(|e| CliError::wrap(format!("{at}read the line merge"), e))?;
                let valid =
                    std::str::from_utf8(&merged).is_ok_and(|text| value::parse(text).is_ok());
                if !valid {
                    return Err(CliError::Conflict(format!(
                        "{at}{decline}; the line merge git falls back to produced invalid JSON"
                    )));
                }
            }
            tracing::info!("{at}{decline}; merged line by line instead");
            Ok(())
        }
        Some(1..=127) => Err(CliError::Conflict(format!(
            "{at}{decline}; left conflict markers"
        ))),
        _ => Err(CliError::Generic(format!(
            "{at}git merge-file failed ({status})"
        ))),
    }
}
