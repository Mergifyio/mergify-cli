//! `mergify ci git-refs` — print the base/head git references for
//! the current build.
//!
//! Detection order (matches Python):
//!
//! 1. Buildkite env (`BUILDKITE=true`) — also consults the engine's
//!    `refs/notes/mergify/<branch>` namespace when the branch is
//!    known, to override the target branch with the MQ checking
//!    base.
//! 2. GitHub event payload — `pull_request`/`push` events with
//!    various fallbacks (git note, MQ PR body, base SHA, default
//!    branch).
//! 3. Plain `HEAD^..HEAD` when no event is available.
//!
//! Output formats:
//!
//! - `text` (default): `Base: <ref>` and `Head: <ref>` on two lines.
//! - `shell`: `MERGIFY_GIT_REFS_{BASE,HEAD,SOURCE}=...` lines, each
//!   single-quoted via `shlex`-style quoting so the caller can `eval`
//!   them.
//! - `json`: one JSON object on a single line.
//!
//! Side-effects: when `$GITHUB_OUTPUT` is set the command appends
//! the `base` and `head` step outputs through the crate's
//! `github_output` module, which writes them in the runner's heredoc
//! form so no value can declare an output of its own. When
//! `BUILDKITE=true` it invokes `buildkite-agent meta-data set` for
//! base/head/source.

use std::env;
use std::io::Write;
use std::process::Command;

use mergify_core::CliError;
use mergify_core::Output;
use serde::Serialize;

use crate::github_event::GitHubEvent;
use crate::github_event::PULL_REQUEST_EVENTS;
use crate::github_event::load as load_event;
use crate::queue_metadata::extract_from_event;

const BUILDKITE_BASE_METADATA_KEY: &str = "mergify-ci.base";
const BUILDKITE_HEAD_METADATA_KEY: &str = "mergify-ci.head";
const BUILDKITE_SOURCE_METADATA_KEY: &str = "mergify-ci.source";

/// Provenance tag for the detected references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferencesSource {
    Manual,
    MergeQueue,
    FallbackLastCommit,
    GithubEventOther,
    GithubEventPullRequest,
    GithubEventPush,
    BuildkitePullRequest,
}

impl ReferencesSource {
    /// Wire-format string for the source provenance. Used in
    /// emitted JSON / shell / markdown across `git-refs` and
    /// `scopes`, so the names are part of the CLI's public
    /// contract.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::MergeQueue => "merge_queue",
            Self::FallbackLastCommit => "fallback_last_commit",
            Self::GithubEventOther => "github_event_other",
            Self::GithubEventPullRequest => "github_event_pull_request",
            Self::GithubEventPush => "github_event_push",
            Self::BuildkitePullRequest => "buildkite_pull_request",
        }
    }
}

#[derive(Debug, Clone)]
pub struct References {
    pub base: Option<String>,
    pub head: String,
    pub source: ReferencesSource,
}

/// Trait-object-compatible hook for reading the merge-queue checking
/// base SHA from the engine's git note.
///
/// git-refs only needs that one field, so the reader yields it directly
/// rather than a half-populated struct. The real implementation shells
/// out to `git`; tests inject a stub so detection can exercise the
/// note-driven branches without touching a real repository.
pub type NotesReader<'a> = &'a dyn Fn(&str, &str) -> Option<String>;

#[derive(Serialize)]
struct JsonOutput<'a> {
    base: Option<&'a str>,
    head: &'a str,
    source: &'a str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Shell,
    Json,
}

impl Format {
    /// Clap value-parser for `--format`.
    ///
    /// # Errors
    ///
    /// Returns a message when `value` is not one of `text`, `shell`,
    /// or `json`.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "text" => Ok(Self::Text),
            "shell" => Ok(Self::Shell),
            "json" => Ok(Self::Json),
            other => Err(format!(
                "invalid format {other:?} (expected text, shell, or json)"
            )),
        }
    }
}

pub struct GitRefsOptions {
    pub format: Format,
}

/// Run the `ci git-refs` command.
pub fn run(opts: &GitRefsOptions, output: &mut dyn Output) -> Result<(), CliError> {
    let notes_reader: NotesReader = &real_notes_reader;
    let refs = detect(output, notes_reader)?;
    emit(&refs, opts.format, output)?;
    write_github_output(&refs)?;
    write_buildkite_metadata(&refs)?;
    Ok(())
}

/// Detect base/head references using the current environment.
///
/// `notes_reader` is injected so tests can bypass the git
/// subprocess. Production callers pass [`real_notes_reader`].
///
/// # Errors
///
/// Returns `CliError::Generic` when the event is a pull-request or
/// push event but no base SHA can be derived — matches Python's
/// `BaseNotFoundError`.
pub fn detect(
    output: &mut dyn Output,
    notes_reader: NotesReader<'_>,
) -> Result<References, CliError> {
    if env::var("BUILDKITE").as_deref() == Ok("true")
        && let Some(refs) = detect_from_buildkite(notes_reader)
    {
        return Ok(refs);
    }

    let Some((event_name, event)) = load_event() else {
        return Ok(References {
            base: Some("HEAD^".to_string()),
            head: "HEAD".to_string(),
            source: ReferencesSource::FallbackLastCommit,
        });
    };

    if PULL_REQUEST_EVENTS.contains(&event_name.as_str()) {
        if let Some(refs) = detect_from_pull_request_event(&event, output, notes_reader)? {
            return Ok(refs);
        }
    } else if event_name == "push" {
        if let Some(refs) = detect_from_push_event(&event) {
            return Ok(refs);
        }
    } else {
        return Ok(References {
            base: None,
            head: "HEAD".to_string(),
            source: ReferencesSource::GithubEventOther,
        });
    }

    Err(CliError::Generic(
        "Could not detect base SHA. Provide GITHUB_EVENT_NAME / GITHUB_EVENT_PATH.".to_string(),
    ))
}

fn detect_from_buildkite(notes_reader: NotesReader<'_>) -> Option<References> {
    let pr = env::var("BUILDKITE_PULL_REQUEST").ok()?;
    if pr.is_empty() || pr == "false" {
        return None;
    }
    let commit = env::var("BUILDKITE_COMMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "HEAD".to_string());
    if let Ok(branch) = env::var("BUILDKITE_BRANCH")
        && !branch.is_empty()
        && let Some(base) = notes_reader(&branch, &commit)
    {
        return Some(References {
            base: Some(base),
            head: commit,
            source: ReferencesSource::MergeQueue,
        });
    }
    let base_branch = env::var("BUILDKITE_PULL_REQUEST_BASE_BRANCH")
        .ok()
        .filter(|s| !s.is_empty())?;
    Some(References {
        base: Some(base_branch),
        head: commit,
        source: ReferencesSource::BuildkitePullRequest,
    })
}

fn detect_from_pull_request_event(
    event: &GitHubEvent,
    output: &mut dyn Output,
    notes_reader: NotesReader<'_>,
) -> std::io::Result<Option<References>> {
    let head = event
        .pull_request
        .as_ref()
        .and_then(|pr| pr.head.as_ref())
        .map_or_else(|| "HEAD".to_string(), |r| r.sha.clone());

    if let Some(pr) = &event.pull_request
        && let Some(head_ref) = &pr.head
        && let Some(branch) = head_ref.r#ref.as_deref()
        && let Some(base) = notes_reader(branch, &head_ref.sha)
    {
        return Ok(Some(References {
            base: Some(base),
            head,
            source: ReferencesSource::MergeQueue,
        }));
    }

    if let Some(meta) = extract_from_event(event, output)? {
        // Anyone who can open a pull request writes this body, so the
        // value only becomes a base once it looks like one. Either way
        // a payload we can't use is not metadata: fall through to the
        // PR base below, which is the *wrong* base for a genuine batch
        // build, so warn rather than let it be diagnosed silently.
        match checking_base_sha(&meta) {
            Some(base) if is_object_name(&base) => {
                return Ok(Some(References {
                    base: Some(base),
                    head,
                    source: ReferencesSource::MergeQueue,
                }));
            }
            Some(rejected) => output.status(&format!(
                "WARNING: MQ pull request checking_base_sha {} is not a git object name, skipping metadata extraction",
                elided(&rejected),
            ))?,
            // No such key — the shape a future engine rename produces.
            None => output.status(
                "WARNING: MQ pull request metadata without checking_base_sha, skipping metadata extraction",
            )?,
        }
    }

    if let Some(pr) = &event.pull_request
        && let Some(base) = &pr.base
    {
        return Ok(Some(References {
            base: Some(base.sha.clone()),
            head,
            source: ReferencesSource::GithubEventPullRequest,
        }));
    }

    if let Some(repo) = &event.repository
        && let Some(default_branch) = &repo.default_branch
    {
        return Ok(Some(References {
            base: Some(default_branch.clone()),
            head,
            source: ReferencesSource::GithubEventPullRequest,
        }));
    }

    Ok(None)
}

fn detect_from_push_event(event: &GitHubEvent) -> Option<References> {
    let head = event
        .after
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "HEAD".to_string());

    if let Some(before) = event.before.as_deref().filter(|s| !s.is_empty()) {
        return Some(References {
            base: Some(before.to_string()),
            head,
            source: ReferencesSource::GithubEventPush,
        });
    }

    let default_branch = event
        .repository
        .as_ref()
        .and_then(|r| r.default_branch.clone())?;
    Some(References {
        base: Some(default_branch),
        head: "HEAD".to_string(),
        source: ReferencesSource::GithubEventPush,
    })
}

/// Production implementation of [`NotesReader`]. Shells out to
/// `git fetch` + `git notes show` and swallows any failure as `None`
/// so callers can transparently fall through to other detection
/// paths.
///
/// `read_note` returns the note's full payload; we pull just
/// `checking_base_sha` out of it, so a note that lacks the field falls
/// through to the other detection paths.
#[must_use]
pub fn real_notes_reader(branch: &str, head_sha: &str) -> Option<String> {
    let notes_ref_short = format!("mergify/{branch}");
    let notes_ref = format!("refs/notes/{notes_ref_short}");

    if !crate::git::succeeds(&[
        "fetch",
        "--no-tags",
        "--quiet",
        "origin",
        &format!("+{notes_ref}:{notes_ref}"),
    ]) {
        return None;
    }

    checking_base_sha(&crate::git::read_note(&notes_ref_short, head_sha)?)
}

/// Whether `value` is a full git object name: 40 lowercase hex
/// digits (SHA-1) or 64 (SHA-256).
///
/// The gate this backs is on the *pull request body* payload alone,
/// and that asymmetry is the point. `queue_metadata::extract_from_event`
/// admits a body on the title prefix `merge queue: ` and nothing
/// else, so whoever opens a pull request writes the value; the git
/// note comes from the engine, over a push to `origin`. A value
/// admitted here leaves the CLI as `base`: into `$GITHUB_OUTPUT` and
/// the `--format=shell` eval for the caller's workflow, into
/// `ci scopes`' git invocations, and onto stderr, which GitHub
/// Actions also scans for `::` workflow commands. Something like
/// `--output=/tmp/x` is read as an *option* by whichever of those
/// gets it first (MRGFY-8845).
///
/// Gating the note as well would be worse, not better: a note the
/// check rejected would fall through to the body path, handing the
/// untrusted payload the precedence the note is supposed to hold, and
/// with no warning since `real_notes_reader` has no `Output`.
///
/// Full object names only, no abbreviations. The engine types the
/// field `github_types.SHAType` and writes a full SHA, and
/// `scopes_detect::changed_files::is_sha` routes only full SHAs as
/// revisions — an abbreviated one is fetched as a branch name and
/// fails the run, so accepting it would buy nothing.
fn is_object_name(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Pull `checking_base_sha` out of an engine merge-queue payload —
/// either the git note ([`real_notes_reader`]) or the YAML block in
/// the MQ draft PR body (`queue_metadata`), which carry the same dump.
/// Both read it through here so neither is coupled to the payload's
/// other keys, which the engine renames independently.
///
/// The value is returned as the payload spells it. The body path
/// holds it to [`is_object_name`] before use; see there for why the
/// note path does not.
fn checking_base_sha(note: &serde_json::Value) -> Option<String> {
    match note.get("checking_base_sha")? {
        serde_json::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Render an untrusted scalar for a warning: `{:?}`-escaped, and cut
/// to a length a log can hold.
///
/// Both halves are load-bearing. `output.status` writes to stderr,
/// which GitHub Actions scans for `::` workflow commands, and `{:?}`
/// escapes the newline that would be needed to start one. Nothing
/// upstream bounds the scalar — `queue_metadata::parse_yaml_block`
/// joins the whole fenced block — and `{:?}` expands a control
/// character to six chars, so the cut keeps a rejected value from
/// filling the log.
fn elided(value: &str) -> String {
    const LIMIT: usize = 64;
    let head: String = value.chars().take(LIMIT).collect();
    if head.len() == value.len() {
        format!("{head:?}")
    } else {
        format!("{head:?} ({} bytes total)", value.len())
    }
}

fn emit(refs: &References, format: Format, output: &mut dyn Output) -> std::io::Result<()> {
    match format {
        Format::Text => output.emit(&(), &mut |w: &mut dyn Write| {
            // A missing base renders as the literal "None" to match
            // Python's f-string (`f"Base: {ref.base}"` with
            // ref.base = None). The Shell arm keeps the empty string
            // since that's a shell variable value, not display text.
            writeln!(w, "Base: {}", refs.base.as_deref().unwrap_or("None"))?;
            writeln!(w, "Head: {}", refs.head)
        }),
        Format::Shell => output.emit(&(), &mut |w: &mut dyn Write| {
            writeln!(
                w,
                "MERGIFY_GIT_REFS_BASE={}",
                shell_quote(refs.base.as_deref().unwrap_or(""))
            )?;
            writeln!(w, "MERGIFY_GIT_REFS_HEAD={}", shell_quote(&refs.head))?;
            writeln!(
                w,
                "MERGIFY_GIT_REFS_SOURCE={}",
                shell_quote(refs.source.as_str())
            )
        }),
        Format::Json => {
            let payload = JsonOutput {
                base: refs.base.as_deref(),
                head: &refs.head,
                source: refs.source.as_str(),
            };
            output.emit(&payload, &mut |w: &mut dyn Write| {
                let rendered = serde_json::to_string(&payload)
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
                writeln!(w, "{rendered}")
            })
        }
    }
}

/// Best-effort POSIX shell quoting. Mirrors `shlex.quote`: empty and
/// "safe" strings stay bare, everything else is single-quoted with
/// embedded `'` rewritten to `'"'"'`.
fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let safe = value.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-' | '_')
    });
    if safe {
        return value.to_string();
    }
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

fn write_github_output(refs: &References) -> Result<(), CliError> {
    crate::github_output::append(&[
        ("base", refs.base.as_deref().unwrap_or("")),
        ("head", &refs.head),
    ])
}

fn write_buildkite_metadata(refs: &References) -> std::io::Result<()> {
    if env::var("BUILDKITE").as_deref() != Ok("true") {
        return Ok(());
    }
    if let Some(base) = refs.base.as_deref() {
        buildkite_meta_data_set(BUILDKITE_BASE_METADATA_KEY, base)?;
    }
    buildkite_meta_data_set(BUILDKITE_HEAD_METADATA_KEY, &refs.head)?;
    buildkite_meta_data_set(BUILDKITE_SOURCE_METADATA_KEY, refs.source.as_str())?;
    Ok(())
}

fn buildkite_meta_data_set(key: &str, value: &str) -> std::io::Result<()> {
    let status = Command::new("buildkite-agent")
        .args(["meta-data", "set", key, value])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "buildkite-agent meta-data set {key} exited with status {status}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mergify_test_support::Captured;
    use tempfile::TempDir;

    use super::*;

    /// Full object names, since that is now all `is_object_name`
    /// admits and the engine only ever writes one.
    const MQ_BASE: &str = "cafef00dcafef00dcafef00dcafef00dcafef00d";
    const NOTE_BASE: &str = "0badc0de0badc0de0badc0de0badc0de0badc0de";

    fn no_notes(_branch: &str, _sha: &str) -> Option<String> {
        None
    }

    #[test]
    fn checking_base_sha_reads_the_scalar_as_the_payload_spells_it() {
        use serde_json::json;
        assert_eq!(
            checking_base_sha(&json!({"checking_base_sha": MQ_BASE, "scopes": ["x"]})),
            Some(MQ_BASE.to_string()),
        );
        // Read verbatim: the shape check is `is_object_name`, applied
        // by the pull request body path and not by the note path.
        assert_eq!(
            checking_base_sha(&json!({"checking_base_sha": "--output=/tmp/x"})),
            Some("--output=/tmp/x".to_string()),
        );
        // No field, or one the engine nulled because the batch has no
        // checking base → None, so detection falls through.
        assert_eq!(checking_base_sha(&json!({"pull_requests": []})), None);
        assert_eq!(checking_base_sha(&json!({"checking_base_sha": null})), None);
    }

    #[test]
    fn is_object_name_admits_only_full_hex_object_names() {
        // Anyone who can open a pull request writes the body this
        // gates, and the value ends up as `base` in the caller's
        // workflow. A leading `-` above all: every consumer of `base`
        // reads it as an option (MRGFY-8845).
        for value in [
            "--output=/home/runner/.gitconfig",
            "-x",
            "main",
            "HEAD^",
            "../etc/passwd",
            &format!("{MQ_BASE}\nbase=pwned"),
            &format!("{MQ_BASE} pwned"),
            // Abbreviated: `changed_files::is_sha` would fetch it as a
            // branch name. Off by one either way. Uppercase, which the
            // engine never writes. Empty.
            "cafef00d",
            &"a".repeat(39),
            &"a".repeat(41),
            &MQ_BASE.to_uppercase(),
            "",
        ] {
            assert!(!is_object_name(value), "should reject {value:?}");
        }
        // SHA-1 and the SHA-256 spelling a future engine would write.
        assert!(is_object_name(MQ_BASE));
        assert!(is_object_name(&"b2".repeat(32)));
    }

    #[test]
    fn elided_escapes_and_bounds_an_untrusted_scalar() {
        // A newline must not survive as one, or the warning could
        // start a `::` workflow command on GitHub Actions.
        assert_eq!(elided("a\nb"), r#""a\nb""#);
        // Nothing upstream bounds the scalar, so a long one is cut and
        // says so rather than filling the log.
        let rendered = elided(&"x".repeat(300));
        assert!(rendered.ends_with(" (300 bytes total)"), "got: {rendered}");
        assert!(rendered.len() < 100, "got: {rendered}");
    }

    fn write_event(dir: &TempDir, payload: &serde_json::Value) -> PathBuf {
        let path = dir.path().join("event.json");
        std::fs::write(&path, serde_json::to_vec(payload).unwrap()).unwrap();
        path
    }

    #[test]
    fn falls_back_to_head_pair_when_no_event() {
        let mut cap = Captured::human();
        let refs = temp_env::with_vars_unset(
            ["GITHUB_EVENT_NAME", "GITHUB_EVENT_PATH", "BUILDKITE"],
            || detect(&mut cap.output, &no_notes).unwrap(),
        );
        assert_eq!(refs.base.as_deref(), Some("HEAD^"));
        assert_eq!(refs.head, "HEAD");
        assert_eq!(refs.source, ReferencesSource::FallbackLastCommit);
    }

    #[test]
    fn detects_from_pull_request_base() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_event(
            &dir,
            &serde_json::json!({
                "pull_request": {
                    "base": {"sha": "base-sha"},
                    "head": {"sha": "head-sha", "ref": "feat/x"},
                },
            }),
        );
        let mut cap = Captured::human();
        let refs = temp_env::with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(path.to_str().unwrap())),
                ("BUILDKITE", None),
            ],
            || detect(&mut cap.output, &no_notes).unwrap(),
        );
        assert_eq!(refs.base.as_deref(), Some("base-sha"));
        assert_eq!(refs.head, "head-sha");
        assert_eq!(refs.source, ReferencesSource::GithubEventPullRequest);
    }

    #[test]
    fn detects_from_push_before_sha() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_event(
            &dir,
            &serde_json::json!({"before": "old-sha", "after": "new-sha"}),
        );
        let mut cap = Captured::human();
        let refs = temp_env::with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("push")),
                ("GITHUB_EVENT_PATH", Some(path.to_str().unwrap())),
                ("BUILDKITE", None),
            ],
            || detect(&mut cap.output, &no_notes).unwrap(),
        );
        assert_eq!(refs.base.as_deref(), Some("old-sha"));
        assert_eq!(refs.head, "new-sha");
        assert_eq!(refs.source, ReferencesSource::GithubEventPush);
    }

    #[test]
    fn detects_mq_from_pr_body_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_event(
            &dir,
            &serde_json::json!({
                "pull_request": {
                    "title": "merge queue: batch",
                    "body": format!("prelude\n```yaml\nchecking_base_sha: {MQ_BASE}\n```"),
                    "head": {"sha": "mq-head", "ref": "mq/main/0"},
                },
            }),
        );
        let mut cap = Captured::human();
        let refs = temp_env::with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(path.to_str().unwrap())),
                ("BUILDKITE", None),
            ],
            || detect(&mut cap.output, &no_notes).unwrap(),
        );
        assert_eq!(refs.base.as_deref(), Some(MQ_BASE));
        assert_eq!(refs.head, "mq-head");
        assert_eq!(refs.source, ReferencesSource::MergeQueue);
    }

    #[test]
    fn mq_body_with_post_contract_batch_still_yields_base() {
        // The ticket's exact failure mode (MRGFY-8104): a batch body
        // carries a non-empty `previous_failed_batches`, and a
        // post-contract engine emits `batch_pr_number` with no
        // `draft_pr_number`. The old rigid struct required
        // `draft_pr_number`, so the whole document failed to parse and
        // `checking_base_sha` was silently lost, dropping git-refs to
        // the wrong-base `GithubEventPullRequest` fallthrough. The
        // generic parse must keep the base and the MergeQueue source.
        let dir = tempfile::tempdir().unwrap();
        let path = write_event(
            &dir,
            &serde_json::json!({
                "pull_request": {
                    "title": "merge queue: batch",
                    "body": format!("```yaml\nchecking_base_sha: {MQ_BASE}\nprevious_failed_batches:\n  - batch_pr_number: 42\n    checked_pull_requests:\n      - 7\n```"),
                    "head": {"sha": "mq-head", "ref": "mq/main/0"},
                    "base": {"sha": "wrong-base"},
                },
            }),
        );
        let mut cap = Captured::human();
        let refs = temp_env::with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(path.to_str().unwrap())),
                ("BUILDKITE", None),
            ],
            || detect(&mut cap.output, &no_notes).unwrap(),
        );
        assert_eq!(refs.base.as_deref(), Some(MQ_BASE));
        assert_eq!(refs.source, ReferencesSource::MergeQueue);
    }

    #[test]
    fn mq_body_metadata_without_base_warns_and_falls_through() {
        // A valid metadata block that carries no checking_base_sha
        // (the shape a future engine key rename would produce) must not
        // silently pick the wrong base: git-refs falls through to the
        // PR base and warns so the operator sees why.
        let dir = tempfile::tempdir().unwrap();
        let path = write_event(
            &dir,
            &serde_json::json!({
                "pull_request": {
                    "title": "merge queue: batch",
                    "body": "```yaml\npull_requests:\n  - number: 7\n```",
                    "head": {"sha": "mq-head", "ref": "mq/main/0"},
                    "base": {"sha": "pr-base"},
                },
            }),
        );
        let mut cap = Captured::human();
        let refs = temp_env::with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(path.to_str().unwrap())),
                ("BUILDKITE", None),
            ],
            || detect(&mut cap.output, &no_notes).unwrap(),
        );
        assert_eq!(refs.base.as_deref(), Some("pr-base"));
        assert_eq!(refs.source, ReferencesSource::GithubEventPullRequest);
        assert!(
            cap.stderr().contains("without checking_base_sha"),
            "got: {:?}",
            cap.stderr()
        );
    }

    #[test]
    fn mq_body_with_option_like_base_warns_and_falls_through() {
        // MRGFY-8845: `extract_from_event` admits any pull request
        // whose title starts with "merge queue: ", so the reporter's
        // fork PR put `--output=<path>` in the body and git-refs
        // emitted it as `base`, where the workflow's next git call
        // read it as an option. A value that is not an object name is
        // not metadata: fall through to the PR base and say so.
        let dir = tempfile::tempdir().unwrap();
        let path = write_event(
            &dir,
            &serde_json::json!({
                "pull_request": {
                    "title": "merge queue: batch",
                    "body": "```yaml\nchecking_base_sha: --output=/home/runner/.gitconfig\n```",
                    "head": {"sha": "mq-head", "ref": "mq/main/0"},
                    "base": {"sha": "pr-base"},
                },
            }),
        );
        let mut cap = Captured::human();
        let refs = temp_env::with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(path.to_str().unwrap())),
                ("BUILDKITE", None),
            ],
            || detect(&mut cap.output, &no_notes).unwrap(),
        );
        assert_eq!(refs.base.as_deref(), Some("pr-base"));
        assert_eq!(refs.source, ReferencesSource::GithubEventPullRequest);
        assert!(
            cap.stderr().contains("is not a git object name"),
            "got: {:?}",
            cap.stderr()
        );
    }

    #[test]
    fn rejected_checking_base_sha_cannot_inject_a_workflow_command() {
        // The warning echoes the rejected value, and GitHub Actions
        // scans a step's stderr for `::` workflow commands — so the
        // escaping matters as much as the rejection. A newline in the
        // value must not start a line of its own.
        let dir = tempfile::tempdir().unwrap();
        let path = write_event(
            &dir,
            &serde_json::json!({
                "pull_request": {
                    "title": "merge queue: batch",
                    "body": "```yaml\nchecking_base_sha: \"cafef00d\\n::error::pwned\"\n```",
                    "head": {"sha": "mq-head", "ref": "mq/main/0"},
                    "base": {"sha": "pr-base"},
                },
            }),
        );
        let mut cap = Captured::human();
        let refs = temp_env::with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(path.to_str().unwrap())),
                ("BUILDKITE", None),
            ],
            || detect(&mut cap.output, &no_notes).unwrap(),
        );
        assert_eq!(refs.base.as_deref(), Some("pr-base"));
        let stderr = cap.stderr();
        assert!(
            !stderr.lines().any(|l| l.starts_with("::")),
            "got: {stderr:?}",
        );
        assert!(
            stderr.contains(r"cafef00d\n::error::pwned"),
            "got: {stderr:?}",
        );
    }

    #[test]
    fn mq_notes_beat_body_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_event(
            &dir,
            &serde_json::json!({
                "pull_request": {
                    "title": "merge queue: batch",
                    "body": format!("```yaml\nchecking_base_sha: {MQ_BASE}\n```"),
                    "head": {"sha": "mq-head", "ref": "mq/main/0"},
                },
            }),
        );
        let note_reader = |branch: &str, sha: &str| {
            if branch == "mq/main/0" && sha == "mq-head" {
                Some(NOTE_BASE.to_string())
            } else {
                None
            }
        };
        let mut cap = Captured::human();
        let refs = temp_env::with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(path.to_str().unwrap())),
                ("BUILDKITE", None),
            ],
            || detect(&mut cap.output, &note_reader).unwrap(),
        );
        assert_eq!(refs.base.as_deref(), Some(NOTE_BASE));
    }

    #[test]
    fn errors_when_pr_event_missing_base() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_event(
            &dir,
            &serde_json::json!({"pull_request": {"head": {"sha": "h"}}}),
        );
        let mut cap = Captured::human();
        let err = temp_env::with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(path.to_str().unwrap())),
                ("BUILDKITE", None),
            ],
            || detect(&mut cap.output, &no_notes).unwrap_err(),
        );
        assert!(err.to_string().contains("Could not detect base SHA"));
    }

    #[test]
    fn detects_buildkite_pull_request() {
        let mut cap = Captured::human();
        let refs = temp_env::with_vars(
            [
                ("BUILDKITE", Some("true")),
                ("BUILDKITE_PULL_REQUEST", Some("42")),
                ("BUILDKITE_COMMIT", Some("sha-head")),
                ("BUILDKITE_BRANCH", Some("feat/x")),
                ("BUILDKITE_PULL_REQUEST_BASE_BRANCH", Some("main")),
                ("GITHUB_EVENT_NAME", None),
                ("GITHUB_EVENT_PATH", None),
            ],
            || detect(&mut cap.output, &no_notes).unwrap(),
        );
        assert_eq!(refs.base.as_deref(), Some("main"));
        assert_eq!(refs.head, "sha-head");
        assert_eq!(refs.source, ReferencesSource::BuildkitePullRequest);
    }

    #[test]
    fn shell_quote_basic_cases() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("feat/x"), "feat/x");
        assert_eq!(shell_quote("has space"), "'has space'");
        assert_eq!(shell_quote("bob's"), "'bob'\"'\"'s'");
    }

    #[test]
    fn emits_text_format() {
        let refs = References {
            base: Some("b".into()),
            head: "h".into(),
            source: ReferencesSource::GithubEventPush,
        };
        let mut cap = Captured::human();
        emit(&refs, Format::Text, &mut cap.output).unwrap();
        let stdout = cap.stdout();
        assert_eq!(stdout, "Base: b\nHead: h\n");
    }

    #[test]
    fn emits_text_format_with_none_base() {
        // A missing base (e.g. workflow_dispatch / schedule events)
        // renders the literal "None", matching Python's f-string.
        let refs = References {
            base: None,
            head: "HEAD".into(),
            source: ReferencesSource::GithubEventOther,
        };
        let mut cap = Captured::human();
        emit(&refs, Format::Text, &mut cap.output).unwrap();
        let stdout = cap.stdout();
        assert_eq!(stdout, "Base: None\nHead: HEAD\n");
    }

    #[test]
    fn emits_shell_format() {
        let refs = References {
            base: Some("main".into()),
            head: "has space".into(),
            source: ReferencesSource::MergeQueue,
        };
        let mut cap = Captured::human();
        emit(&refs, Format::Shell, &mut cap.output).unwrap();
        let stdout = cap.stdout();
        assert!(stdout.contains("MERGIFY_GIT_REFS_BASE=main"));
        assert!(stdout.contains("MERGIFY_GIT_REFS_HEAD='has space'"));
        assert!(stdout.contains("MERGIFY_GIT_REFS_SOURCE=merge_queue"));
    }

    #[test]
    fn emits_json_format() {
        let refs = References {
            base: None,
            head: "HEAD".into(),
            source: ReferencesSource::GithubEventOther,
        };
        let mut cap = Captured::human();
        emit(&refs, Format::Json, &mut cap.output).unwrap();
        let stdout = cap.stdout();
        assert_eq!(
            stdout.trim_end(),
            r#"{"base":null,"head":"HEAD","source":"github_event_other"}"#
        );
    }

    #[test]
    fn github_output_never_lets_base_declare_another_output() {
        // `base` is the one reference that can come from a payload
        // written by hand, so the step outputs go out in heredoc form
        // rather than as `base=<value>` lines a newline could split
        // (MRGFY-8845). `checking_base_sha` validation already keeps
        // this value out, so the newline here stands in for whatever
        // reaches `base` next.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gha_output");
        let refs = References {
            base: Some(format!("{MQ_BASE}\nevil=1")),
            head: NOTE_BASE.into(),
            source: ReferencesSource::MergeQueue,
        };
        temp_env::with_var("GITHUB_OUTPUT", Some(path.to_str().unwrap()), || {
            write_github_output(&refs).unwrap();
        });
        let written = std::fs::read_to_string(&path).unwrap();
        // Parse it the way the runner does, so the assertion is about
        // the outputs the step declares rather than about the bytes.
        let mut lines = written.lines();
        let mut outputs: Vec<(String, String)> = Vec::new();
        while let Some(line) = lines.next() {
            let (name, delimiter) = line.split_once("<<").expect("heredoc form");
            let mut value = Vec::new();
            for body in lines.by_ref() {
                if body == delimiter {
                    break;
                }
                value.push(body);
            }
            outputs.push((name.to_string(), value.join("\n")));
        }
        assert_eq!(
            outputs,
            [
                ("base".to_string(), format!("{MQ_BASE}\nevil=1")),
                ("head".to_string(), NOTE_BASE.to_string()),
            ],
        );
    }

    #[test]
    fn format_parse_round_trips() {
        assert!(matches!(Format::parse("text"), Ok(Format::Text)));
        assert!(matches!(Format::parse("shell"), Ok(Format::Shell)));
        assert!(matches!(Format::parse("json"), Ok(Format::Json)));
        assert!(Format::parse("yaml").is_err());
    }
}
