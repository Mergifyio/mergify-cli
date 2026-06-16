//! Live smoke tests against the real Mergify API. Port of
//! `func-tests/test_live_smoke.py` + `func-tests/conftest.py`.
//!
//! Each test fires when the real API's URL, auth, or wire format
//! diverges from what the CLI expects. API-hitting tests skip
//! (early-return with a `SKIP:` line) unless their token
//! (`LIVE_TEST_MERGIFY_TOKEN_CI` or `_ADMIN`) is set in the env;
//! locally-evaluated tests run unconditionally. Driven by
//! `.github/workflows/func-tests-live.yaml` on every PR.
//!
//! Implementation deliberately mirrors the Python version 1:1 —
//! same scrubbed env list, same assertion messages, same fixture
//! shape — so the port can't drift the contract by accident.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

const API_URL: &str = "https://api.mergify.com";
const REPOSITORY: &str = "mergify-clients-testing/mergify-cli-repo";
const PULL_REQUEST: &str = "1";

/// CLI invocation timeout. Mirrors Python `subprocess.run(timeout=30)`.
const CLI_TIMEOUT: Duration = Duration::from_secs(30);

/// Env vars the CLI auto-detects from the surrounding CI runner.
/// Scrub them so a developer running tests inside GitHub Actions
/// or Buildkite doesn't get different behavior than a clean
/// laptop run. Mirrors `conftest.py::_CI_ENV_VARS`.
const CI_ENV_VARS: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "GITHUB_REPOSITORY",
    "GITHUB_REF",
    "GITHUB_HEAD_REF",
    "GITHUB_BASE_REF",
    "GITHUB_EVENT_PATH",
    "GITHUB_EVENT_NAME",
    "GITHUB_OUTPUT",
    "GITHUB_STEP_SUMMARY",
    "GITHUB_TOKEN",
    "BUILDKITE",
    "BUILDKITE_PULL_REQUEST",
    "BUILDKITE_PULL_REQUEST_BASE_BRANCH",
    "BUILDKITE_BRANCH",
    "BUILDKITE_COMMIT",
    "MERGIFY_API_URL",
    "MERGIFY_TOKEN",
    "MERGIFY_CONFIG_PATH",
    "MERGIFY_TEST_EXIT_CODE",
    "ACTIONS_STEP_DEBUG",
];

struct CliResult {
    returncode: i32,
    stdout: String,
    stderr: String,
}

impl CliResult {
    /// Combined stream for grep-style assertions where the message
    /// could land on either stdout or stderr depending on the
    /// command. Matches Python `result.stdout + result.stderr`.
    fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    fn context(&self) -> String {
        format!("\nstdout:\n{}\nstderr:\n{}", self.stdout, self.stderr)
    }
}

/// Path to the freshly-built `mergify` binary. Cargo sets
/// `CARGO_BIN_EXE_<name>` for each `[[bin]]` target in the test
/// binary's env at compile time — no need to shell out to find
/// it, and no risk of accidentally picking up an installed
/// binary on `$PATH`.
fn mergify_binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_mergify"))
}

/// Run `mergify <args>` with a scrubbed env and a fresh tmp cwd.
///
/// Mirrors Python `conftest.py::cli` exactly: closes stdin so an
/// accidental interactive prompt fails fast instead of blocking;
/// caps wall-clock at [`CLI_TIMEOUT`] so a pathological hang
/// doesn't drag the CI matrix down with it.
///
/// **Concurrency.** Cargo's stock test harness runs every
/// `#[test]` in this binary in a single process across a thread
/// pool (default = number of CPUs). A shared CWD or shared
/// tempdir would let parallel tests race on filesystem state, so
/// each call allocates its own `TempDir` that lives only for the
/// duration of the call.
fn cli(args: &[&str]) -> CliResult {
    cli_with(args, &[], None)
}

/// Variant of [`cli`] that adds env vars and an explicit cwd.
/// Tests that need to drop a config file before invoking the CLI
/// (e.g. `ci_scopes_select_all_when_no_base`) pass `cwd` so the
/// CLI runs inside the directory holding that file.
fn cli_with(args: &[&str], extra_env: &[(&str, &str)], cwd: Option<&Path>) -> CliResult {
    let scrub: HashSet<&str> = CI_ENV_VARS.iter().copied().collect();
    let mut cmd = Command::new(mergify_binary());
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for (k, v) in std::env::vars() {
        if !scrub.contains(k.as_str()) {
            cmd.env(k, v);
        }
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    // Per-call cwd dir. Either honour the caller's explicit cwd
    // (kept by them across the call), or allocate a fresh
    // `TempDir` here that drops after the subprocess exits.
    let owned_tmp = if cwd.is_none() {
        Some(tempfile::tempdir().expect("alloc per-call tmpdir"))
    } else {
        None
    };
    let cwd_path: &Path = cwd.unwrap_or_else(|| owned_tmp.as_ref().unwrap().path());
    cmd.current_dir(cwd_path);

    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("spawn mergify {args:?}: {e}"));
    let pid = child.id();
    // `wait_timeout` keeps the test from hanging forever on a
    // hung binary. On timeout we kill the process and surface a
    // clear panic rather than letting cargo's own timeout (longer
    // and less specific) absorb it.
    let Some(status) = wait_timeout(&mut child, CLI_TIMEOUT) else {
        let _ = child.kill();
        let _ = child.wait();
        panic!("mergify {args:?} exceeded {CLI_TIMEOUT:?} (pid {pid})");
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut s) = child.stdout.take() {
        use std::io::Read;
        let _ = s.read_to_string(&mut stdout);
    }
    if let Some(mut s) = child.stderr.take() {
        use std::io::Read;
        let _ = s.read_to_string(&mut stderr);
    }
    CliResult {
        // `code()` is `None` on signal termination; surface it as
        // a sentinel exit so assertions still produce a useful
        // error message instead of unwrap-panicking on `None`.
        returncode: status.code().unwrap_or(-1),
        stdout,
        stderr,
    }
}

/// Poll [`Child::try_wait`] up to `timeout`. `std` doesn't ship
/// a built-in wait-with-timeout (the `wait_timeout` crate does,
/// but pulling it in for one call site is overkill). Loop with a
/// short sleep — the CLI either exits in milliseconds (most
/// tests) or the 30-second cap fires.
fn wait_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("wait on mergify child: {e}"),
        }
    }
}

/// Look up `LIVE_TEST_MERGIFY_TOKEN_CI`. Mirrors Python
/// `live_token` fixture — empty / unset = skip the test (early
/// return with `SKIP:` printed to stderr so the cargo test log
/// shows what was skipped).
fn live_token() -> Option<String> {
    let token = std::env::var("LIVE_TEST_MERGIFY_TOKEN_CI")
        .unwrap_or_default()
        .trim()
        .to_string();
    (!token.is_empty()).then_some(token)
}

/// Token for queue-admin endpoints (pause/unpause, freeze CRUD,
/// queue status/show). Separated from [`live_token`] because the
/// CI-scoped token is rejected with 403 by the queue-management
/// family — keeping the CI token narrow is intentional.
fn live_admin_token() -> Option<String> {
    let token = std::env::var("LIVE_TEST_MERGIFY_TOKEN_ADMIN")
        .unwrap_or_default()
        .trim()
        .to_string();
    (!token.is_empty()).then_some(token)
}

/// Helper for "early-return on missing token". Cargo's stock
/// test harness doesn't have a "skip" outcome — early-returning
/// counts as pass. Logging `SKIP:` to stderr makes the elided
/// run visible in the CI log so a missing token in CI doesn't
/// silently mark the test green.
macro_rules! skip_if_unset {
    ($var:expr) => {
        match $var {
            Some(v) => v,
            None => {
                eprintln!("SKIP: {} (env var unset)", stringify!($var));
                return;
            }
        }
    };
}

/// RAII cleanup for `queue_pause_unpause_roundtrip` — runs
/// `queue unpause` from `Drop` so the test leaves the canary
/// repo's queue unpaused even when an assertion panics
/// mid-test. Defined at module level (not inside the test fn) to
/// avoid clippy's `items_after_statements`. Warns instead of
/// asserting in `drop` so a real failure isn't masked by a
/// cleanup-time panic.
struct UnpauseOnDrop<'a> {
    token: &'a str,
}
impl Drop for UnpauseOnDrop<'_> {
    fn drop(&mut self) {
        let unpause = cli(&[
            "queue",
            "unpause",
            "--api-url",
            API_URL,
            "--token",
            self.token,
            "--repository",
            REPOSITORY,
        ]);
        if unpause.returncode != 0 {
            eprintln!("WARNING: cleanup unpause failed{}", unpause.context());
        }
        if !unpause.stdout.contains("Queue resumed") {
            eprintln!(
                "WARNING: cleanup unpause didn't print confirmation{}",
                unpause.context()
            );
        }
    }
}

/// RAII cleanup for `freeze_create_update_delete_roundtrip` —
/// runs `freeze delete` from `Drop` so the test leaves no
/// orphan freezes on the canary repo. Same warn-don't-panic
/// posture as [`UnpauseOnDrop`].
struct DeleteFreezeOnDrop<'a> {
    token: &'a str,
    freeze_id: &'a str,
    reason: &'a str,
}
impl Drop for DeleteFreezeOnDrop<'_> {
    fn drop(&mut self) {
        let delete = cli(&[
            "freeze",
            "--api-url",
            API_URL,
            "--token",
            self.token,
            "--repository",
            REPOSITORY,
            "delete",
            self.freeze_id,
            "--reason",
            &format!("{}-cleanup", self.reason),
        ]);
        if delete.returncode != 0 {
            eprintln!("WARNING: freeze cleanup delete failed{}", delete.context());
        }
        if !delete.stdout.to_lowercase().contains("deleted") {
            eprintln!(
                "WARNING: freeze cleanup delete didn't print confirmation{}",
                delete.context()
            );
        }
    }
}

// ---------------------------------------------------------------
// Queue endpoints (admin token).
// ---------------------------------------------------------------

#[test]
fn queue_pause_unpause_roundtrip() {
    // `PUT` + `DELETE /v1/repos/{owner}/{repo}/merge-queue/pause`.
    //
    // Uses the admin-scoped token because pause/unpause hits the
    // queue-admin endpoint and the CI-scoped token is rejected
    // (403) by design.
    //
    // Round-trip so the test repo's queue is left in the same
    // state we found it in even when an assertion fails (unpause
    // runs from the cleanup guard). Tolerant of a leaked paused
    // state from a previous interrupted run — the second pause
    // just refreshes the reason.
    let token = skip_if_unset!(live_admin_token());

    let pause = cli(&[
        "queue",
        "pause",
        "--api-url",
        API_URL,
        "--token",
        &token,
        "--repository",
        REPOSITORY,
        "--reason",
        "func-tests-live-smoke",
        "--yes-i-am-sure",
    ]);

    // Module-level `UnpauseOnDrop` runs the cleanup in Drop so
    // an assertion panic below still unpauses the canary repo.
    let _guard = UnpauseOnDrop { token: &token };

    assert_eq!(pause.returncode, 0, "queue pause failed{}", pause.context());
    assert!(
        pause.stdout.contains("Queue paused"),
        "queue pause did not print confirmation{}",
        pause.context()
    );
    // Guard runs here, asserts unpause behaved.
}

#[test]
fn queue_status() {
    // `GET /v1/repos/{owner}/{repo}/merge-queue/status`.
    //
    // `--json` mode is a passthrough of the API response, so the
    // smoke test only checks that the call succeeds and parses as
    // JSON. The contract preserved across the Python → Rust port
    // is the URL, the auth, and that the response is a JSON
    // object.
    let token = skip_if_unset!(live_admin_token());

    // Group-level options (`--token` / `--api-url` /
    // `--repository`) come BEFORE the subcommand. Click required
    // this on the Python side (options live on the `@queue`
    // group); clap accepts both orders via `global = true`. Put
    // them on the group so the same invocation shape works
    // against both ends of the port.
    let result = cli(&[
        "queue",
        "--api-url",
        API_URL,
        "--token",
        &token,
        "--repository",
        REPOSITORY,
        "status",
        "--json",
    ]);
    assert_eq!(
        result.returncode,
        0,
        "queue status failed{}",
        result.context()
    );
    let payload: Value = serde_json::from_str(&result.stdout).unwrap_or_else(|e| {
        panic!(
            "queue status --json emitted non-JSON output\nerror: {e}\nstdout:\n{}",
            result.stdout
        )
    });
    assert!(
        payload.is_object(),
        "queue status --json must emit a JSON object\nstdout:\n{}",
        result.stdout
    );
}

#[test]
fn queue_show_not_in_queue() {
    // `GET /v1/repos/{owner}/{repo}/merge-queue/pull/{n}` 404 path.
    //
    // Calls with a PR number that is almost certainly not in the
    // queue (the test repo has far fewer than this many PRs).
    // Both Python and Rust special-case 404 with the same
    // user-facing message and `MERGIFY_API_ERROR` exit code (6)
    // — that contract is what this test pins.
    //
    // Testing the 404 path (instead of a real queued PR) makes
    // the test independent of whether PR #1 happens to be queued
    // at run time. The endpoint reachability, auth, and 404
    // mapping are the parts that would silently break on a URL
    // or schema drift.
    let token = skip_if_unset!(live_admin_token());

    let result = cli(&[
        "queue",
        "--api-url",
        API_URL,
        "--token",
        &token,
        "--repository",
        REPOSITORY,
        "show",
        "99999999",
    ]);
    assert_eq!(
        result.returncode,
        6,
        "expected MERGIFY_API_ERROR (6), got {}{}",
        result.returncode,
        result.context()
    );
    assert!(
        result
            .combined()
            .to_lowercase()
            .contains("not in the merge queue"),
        "expected 'not in the merge queue' message{}",
        result.context()
    );
}

// ---------------------------------------------------------------
// Freeze endpoints (admin token).
// ---------------------------------------------------------------

#[test]
fn freeze_list() {
    // `GET /v1/repos/{owner}/{repo}/scheduled_freeze`.
    //
    // `--json` mode is a passthrough of the inner
    // `scheduled_freezes` array. The smoke test only checks the
    // call succeeds and parses as a JSON array — the contract
    // preserved across the Python → Rust port is the URL, the
    // auth, and the array shape of the `--json` output.
    let token = skip_if_unset!(live_admin_token());

    let result = cli(&[
        "freeze",
        "--api-url",
        API_URL,
        "--token",
        &token,
        "--repository",
        REPOSITORY,
        "list",
        "--json",
    ]);
    assert_eq!(
        result.returncode,
        0,
        "freeze list failed{}",
        result.context()
    );
    let payload: Value = serde_json::from_str(&result.stdout).unwrap_or_else(|e| {
        panic!(
            "freeze list --json emitted non-JSON output\nerror: {e}\nstdout:\n{}",
            result.stdout
        )
    });
    assert!(
        payload.is_array(),
        "freeze list --json must emit a JSON array\nstdout:\n{}",
        result.stdout
    );
}

#[test]
fn freeze_create_update_delete_roundtrip() {
    // `POST` + `PATCH` + `POST .../{id}/delete` round-trip on
    // `/v1/repos/{owner}/{repo}/scheduled_freeze`.
    //
    // Schedules a freeze far in the future (2099) so we don't
    // disturb real merges in the test repo. Cleanup runs from a
    // Drop guard so the freeze is deleted even if an assertion in
    // the middle of the test fails — and the test is also
    // tolerant of a leaked freeze from a previous interrupted run
    // (the create still succeeds because each run uses a unique
    // reason; the orphan can be cleaned up out of band).
    //
    // The Mergify API requires `delete_reason` on every delete
    // (the Python `--reason` help text says "required if freeze
    // is active", but the server returns 422 for a missing key
    // regardless of the freeze's active state). The test always
    // passes `--reason` so the cleanup succeeds on the
    // server-validated path.
    let token = skip_if_unset!(live_admin_token());

    // Unique reason so concurrent or repeated runs don't fight
    // over the same row. The Python suite uses
    // `uuid.uuid4().hex[:8]`; reproduce that entropy with
    // `tempfile`'s name-generation (32 hex chars from
    // `getrandom`), truncated to 8.
    let suffix = {
        let dir = tempfile::tempdir().expect("tempdir for entropy");
        let name = dir
            .path()
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("00000000")
            .to_string();
        // tempdir names are like ".tmpXXXXXX" or "tmpXXXXXX" with
        // ~6+ random chars after the prefix. Take a tail slice so
        // we don't include the literal prefix.
        let tail: String = name.chars().rev().take(8).collect();
        tail.chars().rev().collect::<String>()
    };
    let reason = format!("func-tests-live-smoke-{suffix}");

    let create = cli(&[
        "freeze",
        "--api-url",
        API_URL,
        "--token",
        &token,
        "--repository",
        REPOSITORY,
        "create",
        "--reason",
        &reason,
        "--timezone",
        "UTC",
        "--start",
        "2099-01-01T00:00:00",
        "--end",
        "2099-01-02T00:00:00",
    ]);
    assert_eq!(
        create.returncode,
        0,
        "freeze create failed{}",
        create.context()
    );

    // Pull the UUID out of the create output. The Python side
    // emits `ID: <uuid>` via `_print_freeze`; the Rust port emits
    // the same block. Search the combined stream because a
    // warning could end up on stderr on either side.
    let combined = create.combined();
    let id_re = regex::Regex::new(r"ID:\s+([0-9a-fA-F-]{36})").unwrap();
    let freeze_id = id_re
        .captures(&combined)
        .and_then(|c| c.get(1))
        .map_or_else(
            || {
                panic!(
                    "could not find freeze ID in create output{}",
                    create.context()
                )
            },
            |m| m.as_str().to_string(),
        );

    let _guard = DeleteFreezeOnDrop {
        token: &token,
        freeze_id: &freeze_id,
        reason: &reason,
    };

    let updated_reason = format!("{reason}-updated");
    let update = cli(&[
        "freeze",
        "--api-url",
        API_URL,
        "--token",
        &token,
        "--repository",
        REPOSITORY,
        "update",
        &freeze_id,
        "--reason",
        &updated_reason,
    ]);
    assert_eq!(
        update.returncode,
        0,
        "freeze update failed{}",
        update.context()
    );
    assert!(
        update.stdout.contains(&updated_reason),
        "freeze update did not echo the new reason{}",
        update.context()
    );
    // Guard runs here.
}

// ---------------------------------------------------------------
// CI commands — locally evaluated, no token needed.
// ---------------------------------------------------------------

#[test]
fn ci_git_refs_fallback() {
    // `mergify ci git-refs` falls back to `HEAD^..HEAD` when no
    // CI provider env is set. The scrubbed env (CI_ENV_VARS) and
    // tmp cwd land the detector on the literal-string fallback.
    let result = cli(&["ci", "git-refs"]);
    assert_eq!(
        result.returncode,
        0,
        "ci git-refs failed{}",
        result.context()
    );
    // Pin the exact two-line output. Substring matches would let
    // added lines slip through silently, which defeats the "pin
    // the contract" intent.
    assert_eq!(
        result.stdout, "Base: HEAD^\nHead: HEAD\n",
        "output drifted from the pinned format\nstdout: {:?}",
        result.stdout
    );
}

#[test]
fn ci_queue_info_outside_mq() {
    // `mergify ci queue-info` exits INVALID_STATE (7) when not
    // running on an MQ draft PR. Scrubbed env + tmp cwd force
    // the "no MQ context" path.
    let result = cli(&["ci", "queue-info"]);
    assert_eq!(
        result.returncode,
        7,
        "expected INVALID_STATE (7), got {}{}",
        result.returncode,
        result.context()
    );
    assert!(
        result.combined().to_lowercase().contains("merge queue"),
        "expected MQ-context message{}",
        result.context()
    );
}

#[test]
fn ci_scopes_select_all_when_no_base() {
    // `mergify ci scopes` with `--head HEAD` and no `--base` lists
    // every configured scope as touched. Locally evaluated, no
    // git, no API — the no-base branch is the one path through
    // the command that doesn't shell out to `git diff`, so the
    // test stays hermetic in the tmp cwd.
    let tmp = tempfile::tempdir().expect("tempdir for config");
    let config_path = tmp.path().join("mergify.yml");
    std::fs::write(
        &config_path,
        "scopes:\n\
         \x20\x20source:\n\
         \x20\x20\x20\x20files:\n\
         \x20\x20\x20\x20\x20\x20backend:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20include: ['mergify_cli/**']\n\
         \x20\x20\x20\x20\x20\x20frontend:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20include: ['web/**']\n",
    )
    .expect("write config");

    let config_str = config_path.to_string_lossy().into_owned();
    let result = cli(&["ci", "scopes", "--config", &config_str, "--head", "HEAD"]);
    assert_eq!(result.returncode, 0, "ci scopes failed{}", result.context());
    let combined = result.combined();
    for scope in &["backend", "frontend"] {
        assert!(
            combined.contains(scope),
            "expected scope '{scope}' in the 'select all' output{}",
            result.context()
        );
    }
}

// ---------------------------------------------------------------
// CI endpoints (CI-scoped token).
// ---------------------------------------------------------------

#[test]
fn scopes_send() {
    // `POST /v1/repos/{owner}/{repo}/pulls/{n}/scopes`.
    let token = skip_if_unset!(live_token());

    let result = cli(&[
        "ci",
        "scopes-send",
        "--api-url",
        API_URL,
        "--token",
        &token,
        "--repository",
        REPOSITORY,
        "--pull-request",
        PULL_REQUEST,
        "--scope",
        "func-tests-live-smoke",
    ]);
    assert_eq!(
        result.returncode,
        0,
        "scopes-send failed{}",
        result.context()
    );
}

#[test]
fn tests_show_no_match() {
    // `GET /v1/ci/{owner}/repositories/{repo}/search/tests`
    // round-trip. Queries a guaranteed-nonexistent name so the
    // test is independent of whatever live test data the canary
    // repository currently holds. A green run proves auth, URL
    // routing, and JSON deserialization for the search endpoint
    // — the empty-match path returns exit 0 with a
    // `{"tests": []}` payload on stdout.
    let token = skip_if_unset!(live_token());

    let result = cli(&[
        "tests",
        "show",
        "--api-url",
        API_URL,
        "--token",
        &token,
        "--repository",
        REPOSITORY,
        "--json",
        "__mergify_cli_smoke_no_such_test__",
    ]);
    assert_eq!(
        result.returncode,
        0,
        "tests show failed{}",
        result.context()
    );
    let payload: Value = serde_json::from_str(&result.stdout).unwrap_or_else(|e| {
        panic!(
            "tests show --json emitted non-JSON output\nerror: {e}\nstdout:\n{}",
            result.stdout
        )
    });
    assert_eq!(
        payload,
        serde_json::json!({"tests": []}),
        "expected empty `tests` list for nonexistent test name, got:\n{}",
        result.stdout
    );
}

#[test]
fn junit_process() {
    // OTLP traces upload + quarantine check round-trip.
    //
    // Uses a fixture with one failing test so the quarantine
    // endpoint is actually called (`junit-process` short-circuits
    // the quarantine call when the report has zero failures,
    // which makes an all-passing fixture useless as a canary).
    // Asserts on stdout rather than exit code:
    //
    // - `junit-process` swallows OTLP upload errors into a stdout
    //   warning ("reports not uploaded") without affecting the
    //   exit code, so a 5xx on `/ci/traces` would not surface as
    //   failure.
    // - The exit code reflects whether failures are quarantined
    //   on the live tenant, which is a state the tests don't
    //   control.
    //
    // A green run is one where neither endpoint logged an error
    // string into stdout.
    let token = skip_if_unset!(live_token());

    let junit_fixture = junit_fail_fixture();
    let junit_str = junit_fixture.to_string_lossy().into_owned();
    let result = cli(&[
        "ci",
        "junit-process",
        "--api-url",
        API_URL,
        "--token",
        &token,
        "--repository",
        REPOSITORY,
        "--tests-target-branch",
        "main",
        &junit_str,
    ]);

    assert!(
        !result.stdout.contains(" not uploaded"),
        "OTLP traces endpoint did not accept upload{}",
        result.context()
    );
    assert!(
        !result.stdout.contains("Failed to check quarantine"),
        "quarantine endpoint did not respond{}",
        result.context()
    );
}

/// Path to the `JUnit` fixture used by [`junit_process`]. Lives
/// next to the test source so cargo packages it consistently.
fn junit_fail_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("junit_fail.xml")
}
