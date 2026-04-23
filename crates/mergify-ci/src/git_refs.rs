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
//! `base=` / `head=` lines. When `BUILDKITE=true` it invokes
//! `buildkite-agent meta-data set` for base/head/source.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

use mergify_core::CliError;
use mergify_core::Output;
use serde::Serialize;

use crate::github_event::GitHubEvent;
use crate::github_event::PULL_REQUEST_EVENTS;
use crate::github_event::load as load_event;
use crate::queue_metadata::MergeQueueMetadata;
use crate::queue_metadata::extract_from_event;
use crate::queue_metadata::parse_yaml_block;

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
    fn as_str(self) -> &'static str {
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

/// Trait-object-compatible hook for reading merge-queue git notes.
///
/// The real implementation shells out to `git`. Tests inject a stub
/// so detection can exercise the note-driven branches without
/// touching a real repository.
pub type NotesReader<'a> = &'a dyn Fn(&str, &str) -> Option<MergeQueueMetadata>;

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
    if env::var("BUILDKITE").as_deref() == Ok("true") {
        if let Some(refs) = detect_from_buildkite(notes_reader) {
            return Ok(refs);
        }
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
    if let Ok(branch) = env::var("BUILDKITE_BRANCH") {
        if !branch.is_empty() {
            if let Some(note) = notes_reader(&branch, &commit) {
                return Some(References {
                    base: Some(note.checking_base_sha),
                    head: commit,
                    source: ReferencesSource::MergeQueue,
                });
            }
        }
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

    if let Some(pr) = &event.pull_request {
        if let Some(head_ref) = &pr.head {
            if let Some(branch) = head_ref.r#ref.as_deref() {
                if let Some(note) = notes_reader(branch, &head_ref.sha) {
                    return Ok(Some(References {
                        base: Some(note.checking_base_sha),
                        head,
                        source: ReferencesSource::MergeQueue,
                    }));
                }
            }
        }
    }

    if let Some(meta) = extract_from_event(event, output)? {
        return Ok(Some(References {
            base: Some(meta.checking_base_sha),
            head,
            source: ReferencesSource::MergeQueue,
        }));
    }

    if let Some(pr) = &event.pull_request {
        if let Some(base) = &pr.base {
            return Ok(Some(References {
                base: Some(base.sha.clone()),
                head,
                source: ReferencesSource::GithubEventPullRequest,
            }));
        }
    }

    if let Some(repo) = &event.repository {
        if let Some(default_branch) = &repo.default_branch {
            return Ok(Some(References {
                base: Some(default_branch.clone()),
                head,
                source: ReferencesSource::GithubEventPullRequest,
            }));
        }
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
#[must_use]
pub fn real_notes_reader(branch: &str, head_sha: &str) -> Option<MergeQueueMetadata> {
    let notes_ref_short = format!("mergify/{branch}");
    let notes_ref = format!("refs/notes/{notes_ref_short}");

    let fetch = Command::new("git")
        .args([
            "fetch",
            "--no-tags",
            "--quiet",
            "origin",
            &format!("+{notes_ref}:{notes_ref}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if !fetch.success() {
        return None;
    }

    let output = Command::new("git")
        .args([
            "notes",
            &format!("--ref={notes_ref_short}"),
            "show",
            head_sha,
        ])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let content = String::from_utf8(output.stdout).ok()?;
    let meta: MergeQueueMetadata = serde_yaml_ng::from_str(&content).ok()?;
    // Python also guards against non-dict payloads; `from_str` into
    // our typed struct already enforces the shape, so just return.
    Some(meta)
}

#[allow(dead_code)]
fn parse_notes_payload(content: &str) -> Option<MergeQueueMetadata> {
    // Exposed for unit-testing the YAML parsing independently of
    // the git subprocess.
    parse_yaml_block(content).or_else(|| serde_yaml_ng::from_str(content).ok())
}

fn emit(refs: &References, format: Format, output: &mut dyn Output) -> std::io::Result<()> {
    match format {
        Format::Text => output.emit(&(), &mut |w: &mut dyn Write| {
            writeln!(w, "Base: {}", refs.base.as_deref().unwrap_or(""))?;
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

fn write_github_output(refs: &References) -> std::io::Result<()> {
    let Some(path) = env::var("GITHUB_OUTPUT").ok().filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(PathBuf::from(path))?;
    writeln!(file, "base={}", refs.base.as_deref().unwrap_or(""))?;
    writeln!(file, "head={}", refs.head)?;
    Ok(())
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
    use mergify_core::OutputMode;
    use mergify_core::StdioOutput;
    use tempfile::TempDir;

    use super::*;

    type SharedBytes = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

    struct Captured {
        output: StdioOutput,
        stdout: SharedBytes,
    }

    fn make_output() -> Captured {
        let stdout: SharedBytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stderr: SharedBytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = StdioOutput::with_sinks(
            OutputMode::Human,
            SharedWriter(std::sync::Arc::clone(&stdout)),
            SharedWriter(std::sync::Arc::clone(&stderr)),
        );
        Captured { output, stdout }
    }

    fn no_notes(_branch: &str, _sha: &str) -> Option<MergeQueueMetadata> {
        None
    }

    fn write_event(dir: &TempDir, payload: &serde_json::Value) -> PathBuf {
        let path = dir.path().join("event.json");
        std::fs::write(&path, serde_json::to_vec(payload).unwrap()).unwrap();
        path
    }

    #[test]
    fn falls_back_to_head_pair_when_no_event() {
        let mut cap = make_output();
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
        let mut cap = make_output();
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
        let mut cap = make_output();
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
                    "body": "prelude\n```yaml\nchecking_base_sha: mq-base\n```",
                    "head": {"sha": "mq-head", "ref": "mq/main/0"},
                },
            }),
        );
        let mut cap = make_output();
        let refs = temp_env::with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(path.to_str().unwrap())),
                ("BUILDKITE", None),
            ],
            || detect(&mut cap.output, &no_notes).unwrap(),
        );
        assert_eq!(refs.base.as_deref(), Some("mq-base"));
        assert_eq!(refs.head, "mq-head");
        assert_eq!(refs.source, ReferencesSource::MergeQueue);
    }

    #[test]
    fn mq_notes_beat_body_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_event(
            &dir,
            &serde_json::json!({
                "pull_request": {
                    "title": "merge queue: batch",
                    "body": "```yaml\nchecking_base_sha: body-sha\n```",
                    "head": {"sha": "mq-head", "ref": "mq/main/0"},
                },
            }),
        );
        let note_reader = |branch: &str, sha: &str| {
            if branch == "mq/main/0" && sha == "mq-head" {
                Some(MergeQueueMetadata {
                    checking_base_sha: "note-sha".to_string(),
                    pull_requests: Vec::new(),
                    previous_failed_batches: Vec::new(),
                })
            } else {
                None
            }
        };
        let mut cap = make_output();
        let refs = temp_env::with_vars(
            [
                ("GITHUB_EVENT_NAME", Some("pull_request")),
                ("GITHUB_EVENT_PATH", Some(path.to_str().unwrap())),
                ("BUILDKITE", None),
            ],
            || detect(&mut cap.output, &note_reader).unwrap(),
        );
        assert_eq!(refs.base.as_deref(), Some("note-sha"));
    }

    #[test]
    fn errors_when_pr_event_missing_base() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_event(
            &dir,
            &serde_json::json!({"pull_request": {"head": {"sha": "h"}}}),
        );
        let mut cap = make_output();
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
        let mut cap = make_output();
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
        let mut cap = make_output();
        emit(&refs, Format::Text, &mut cap.output).unwrap();
        let stdout = String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap();
        assert_eq!(stdout, "Base: b\nHead: h\n");
    }

    #[test]
    fn emits_shell_format() {
        let refs = References {
            base: Some("main".into()),
            head: "has space".into(),
            source: ReferencesSource::MergeQueue,
        };
        let mut cap = make_output();
        emit(&refs, Format::Shell, &mut cap.output).unwrap();
        let stdout = String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap();
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
        let mut cap = make_output();
        emit(&refs, Format::Json, &mut cap.output).unwrap();
        let stdout = String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap();
        assert_eq!(
            stdout.trim_end(),
            r#"{"base":null,"head":"HEAD","source":"github_event_other"}"#
        );
    }

    #[test]
    fn format_parse_round_trips() {
        assert!(matches!(Format::parse("text"), Ok(Format::Text)));
        assert!(matches!(Format::parse("shell"), Ok(Format::Shell)));
        assert!(matches!(Format::parse("json"), Ok(Format::Json)));
        assert!(Format::parse("yaml").is_err());
    }

    struct SharedWriter(SharedBytes);
    impl std::io::Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
