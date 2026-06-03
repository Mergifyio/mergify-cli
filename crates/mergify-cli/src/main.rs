//! `mergify` binary entry point.
//!
//! Dispatch logic: every invocation is speculatively parsed with
//! clap. The clap tree covers both worlds:
//!
//! - **Natively-ported commands** ([`NATIVE_COMMANDS`]) — clap
//!   parses the full flag set and the binary runs them in process.
//! - **Python-shimmed commands** (`stack` is the last one left)
//!   — clap registers them as stub variants with a catch-all
//!   `args: Vec<String>`. That way
//!   `mergify --help` and `mergify <group> --help` list the entire
//!   CLI surface, but the captured argv is forwarded verbatim to
//!   the Python implementation by [`mergify_py_shim::run`].
//!
//! Invocations clap can't parse at all (typos, unknown groups)
//! still fall through to the Python shim with the original argv,
//! so its "no such command" message reaches the user.
//!
//! As each Python command is ported to Rust, its stub variant is
//! promoted to a real clap definition, a matching entry lands in
//! [`NATIVE_COMMANDS`], and the shim fallback shrinks accordingly.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use clap::Subcommand;
use mergify_ci::git_refs::Format as GitRefsFormat;
use mergify_ci::git_refs::GitRefsOptions;
use mergify_ci::junit_process::JunitProcessOptions;
use mergify_ci::scopes_send::ScopesSendOptions;
use mergify_ci::tests_quarantine::QuarantineOptions;
use mergify_ci::tests_quarantine::QuarantinedOptions;
use mergify_ci::tests_quarantine::UnquarantineOptions;
use mergify_ci::tests_show::TestsShowOptions;
use mergify_config::simulate::PullRequestRef;
use mergify_config::simulate::SimulateOptions;
use mergify_core::OutputMode;
use mergify_core::StdioOutput;
use mergify_freeze::common::parse_naive_datetime;
use mergify_freeze::create::CreateOptions as FreezeCreateOptions;
use mergify_freeze::delete::DeleteOptions as FreezeDeleteOptions;
use mergify_freeze::list::ListOptions as FreezeListOptions;
use mergify_freeze::update::UpdateOptions as FreezeUpdateOptions;
use mergify_queue::pause::PauseOptions;
use mergify_queue::show::ShowOptions;
use mergify_queue::status::StatusOptions;
use mergify_queue::unpause::UnpauseOptions;

fn main() -> ExitCode {
    let argv: Vec<String> = env::args().skip(1).collect();

    // Test hook used by `test_binary_build.py` to verify the
    // wheel-installed binary produces UTF-8 output (especially on
    // Windows). The Python entry-point printed these markers from
    // `cli.py::main` before any subcommand ran; now that the Rust
    // binary handles `--help` natively the Python path is no
    // longer guaranteed to fire, so the marker has to live here.
    // The Rust binary is UTF-8 native on every platform — we don't
    // need (or do) the Python `os.execv` re-exec trick — so we
    // report `utf8_mode=1` on Windows (matching the post-re-exec
    // expectation) and `utf8_mode=0` elsewhere.
    if env::var_os("MERGIFY_CLI_TESTING_UTF8_MODE").is_some() {
        let utf8_mode = u8::from(cfg!(target_os = "windows"));
        println!("utf8_mode={utf8_mode}");
        println!("✅");
    }

    // Hidden flag — used by `mergify_cli/tests/queue/test_skill.py`
    // (and any future cross-language test) to learn the set of
    // commands the Rust binary handles natively without resorting
    // to a hardcoded list that drifts. Format is one
    // `<group> <subcommand>` pair per line.
    if argv.first().is_some_and(|a| a == "--list-native-commands") {
        for (group, sub) in NATIVE_COMMANDS {
            println!("{group} {sub}");
        }
        return ExitCode::SUCCESS;
    }

    match detect_dispatch(&argv) {
        Some(Dispatch::Native(cmd)) => run_native(cmd),
        Some(Dispatch::Shim(forwarded)) => run_py_shim(&forwarded),
        None => run_py_shim(&argv),
    }
}

fn run_py_shim(argv: &[String]) -> ExitCode {
    match mergify_py_shim::run(argv) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("mergify: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Outcome of speculatively parsing the argv with clap.
enum Dispatch {
    /// argv resolved to a natively-ported command — run it in-process.
    Native(NativeCommand),
    /// argv resolved to a clap *stub* for a Python-shimmed command.
    /// The captured argv (with the group/subcommand restored at the
    /// front) is forwarded to Python verbatim — including `--help`,
    /// which our stubs deliberately let pass through.
    Shim(Vec<String>),
}

fn prepend_one(head: &str, tail: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(tail.len() + 1);
    out.push(head.to_string());
    out.extend(tail);
    out
}

/// Re-inject the global `--debug` flag at the front of the forwarded
/// argv so Python's root group sees it. Clap consumed the flag when
/// parsing the Rust-side argv, but the Python CLI declares it at
/// root too — leaving it off would silently drop the user's intent
/// for shimmed commands.
fn inject_global_flags(debug: bool, argv: Vec<String>) -> Vec<String> {
    if !debug {
        return argv;
    }
    let mut out = Vec::with_capacity(argv.len() + 1);
    out.push("--debug".to_string());
    out.extend(argv);
    out
}

/// Single source of truth for the `(group, subcommand)` pairs the
/// Rust binary handles natively. Used by [`looks_native`] for argv
/// recognition and by the `--list-native-commands` hidden flag so
/// out-of-process tests can discover the list without hard-coding
/// it. Add new entries here when porting a command; the matching
/// `clap` `Subcommands` variant is what actually wires it up.
const NATIVE_COMMANDS: &[(&str, &str)] = &[
    ("config", "validate"),
    ("config", "simulate"),
    ("ci", "scopes"),
    ("ci", "scopes-send"),
    ("ci", "git-refs"),
    ("ci", "queue-info"),
    ("ci", "junit-process"),
    ("ci", "junit-upload"),
    ("tests", "show"),
    ("tests", "quarantine"),
    ("tests", "unquarantine"),
    ("tests", "quarantined"),
    ("queue", "pause"),
    ("queue", "unpause"),
    ("queue", "status"),
    ("queue", "show"),
    ("freeze", "list"),
    ("freeze", "create"),
    ("freeze", "update"),
    ("freeze", "delete"),
    ("stack", "checkout"),
    ("stack", "drop"),
    ("stack", "edit"),
    ("stack", "fixup"),
    ("stack", "move"),
    ("stack", "new"),
    ("stack", "note"),
    ("stack", "reorder"),
    ("stack", "reword"),
    ("stack", "squash"),
    ("stack", "sync"),
    // Internal Python migration helpers. Listed so `looks_native`
    // routes `mergify _internal …` past the shim fallback when
    // clap rejects it, but they stay hidden from `--help` (see
    // the `Subcommands::Internal` variant).
    ("_internal", "stack-local-commits"),
    ("_internal", "stack-remote-changes"),
    // Self-invocation target for the rebase-todo machinery — set
    // as `GIT_SEQUENCE_EDITOR` before `git rebase -i` so we can
    // rewrite the todo file in-process. Not a user-facing
    // command; not stable.
    ("_internal", "rebase-todo-rewrite"),
];

/// Native commands the Rust binary handles without delegating to
/// the Python shim.
enum NativeCommand {
    ConfigValidate {
        config_file: Option<PathBuf>,
    },
    ConfigSimulate(ConfigSimulateOpts),
    CiScopes(CiScopesOpts),
    CiScopesSend(CiScopesSendOpts),
    CiGitRefs {
        format: GitRefsFormat,
    },
    CiQueueInfo,
    CiJunitProcess(CiJunitProcessOpts),
    /// Deprecated alias for `CiJunitProcess`. Same orchestrator,
    /// same args; the dispatcher prints a deprecation warning to
    /// stderr before running. Matches Python's `deprecated=...`
    /// click decorator on `ci junit-upload`.
    CiJunitUpload(CiJunitProcessOpts),
    TestsShow(TestsShowOpts),
    TestsQuarantine(TestsQuarantineOpts),
    TestsUnquarantine(TestsUnquarantineOpts),
    TestsQuarantined(TestsQuarantinedOpts),
    QueuePause(QueuePauseOpts),
    QueueUnpause(QueueUnpauseOpts),
    QueueStatus(QueueStatusOpts),
    QueueShow(QueueShowOpts),
    FreezeList(FreezeListOpts),
    FreezeCreate(FreezeCreateOpts),
    FreezeUpdate(FreezeUpdateOpts),
    FreezeDelete(FreezeDeleteOpts),
    /// `_internal stack-local-commits --base <sha> --head <ref>` —
    /// Python migration helper. Runs `git log` for the stack
    /// range, parses each commit's `Change-Id:` trailer, prints
    /// the result as a JSON array. Used by `mergify_cli/stack/changes.py`
    /// while the surrounding stack discovery logic is still
    /// Python. Wire format is not stable.
    InternalStackLocalCommits(InternalStackLocalCommitsOpts),
    /// `_internal stack-remote-changes --github-server URL --token T
    /// --user U --repo R --stack-prefix P --author A` — Python
    /// migration helper. Searches GitHub for the open + merged PRs
    /// belonging to the stack, groups them by Change-Id, prints a
    /// JSON array of `{change_id, pull}` records. Wire format is
    /// not stable.
    InternalStackRemoteChanges(InternalStackRemoteChangesOpts),
    /// `mergify stack new <name> [--base REMOTE/BRANCH]
    /// [--checkout/--no-checkout]` — create a new stack branch
    /// tracking the resolved trunk. First stack subcommand to land
    /// natively; the rest still shim to Python.
    StackNew(StackNewOpts),
    /// `mergify stack note [<commit>] [-m <msg>] [--append]
    /// [--remove]` — attach/append/remove the "why was this commit
    /// amended" note on `refs/notes/mergify/stack`.
    StackNote(StackNoteOpts),
    /// `mergify stack edit [<commit>]` — pause an interactive
    /// rebase at the target commit so the user can amend it.
    StackEdit(StackEditOpts),
    /// `mergify stack drop <COMMIT>... [--dry-run]` — drop one or
    /// more commits from the stack via the rebase-todo machinery.
    StackDrop(StackDropOpts),
    /// `mergify stack fixup <COMMIT>... [--dry-run]` — fold one
    /// or more commits into their parents via the rebase-todo
    /// machinery.
    StackFixup(StackFixupOpts),
    /// `mergify stack reword <COMMIT> [-m <msg>] [--dry-run]` —
    /// change a commit's message in place.
    StackReword(StackRewordOpts),
    /// `mergify stack reorder <COMMIT>... [--dry-run]` — rebase
    /// the stack with the requested commit order.
    StackReorder(StackReorderOpts),
    /// `mergify stack move <COMMIT> <POSITION> [<TARGET>]
    /// [--dry-run]` — move a single commit within the stack.
    StackMove(StackMoveOpts),
    /// `mergify stack squash <SRC>... into <TARGET> [-m <msg>]
    /// [--dry-run]` — fold several commits into a target,
    /// reordering them adjacent first.
    StackSquash(StackSquashOpts),
    /// `mergify stack checkout <NAME>` — fetch a stack of pull
    /// requests from GitHub and create a local branch tracking
    /// the leaf head.
    StackCheckout(StackCheckoutOpts),
    /// `mergify stack sync [--dry-run]` — rebase the stack onto
    /// trunk, dropping commits whose PR has merged.
    StackSync(StackSyncOpts),
    /// `_internal rebase-todo-rewrite --action <ACTION>
    /// --sha <SHA> <TODO_PATH>` — self-invocation target set as
    /// `GIT_SEQUENCE_EDITOR` by the rebase-family stack
    /// subcommands. Reads the rebase-todo at `TODO_PATH`,
    /// applies the named transformation, writes it back in place.
    /// Wire format is not stable.
    InternalRebaseTodoRewrite(InternalRebaseTodoRewriteOpts),
}

struct StackEditOpts {
    /// `None` for an interactive rebase (no commit pre-selected);
    /// `Some(prefix)` to pause the rebase on the matching commit.
    commit_prefix: Option<String>,
}

struct StackDropOpts {
    commit_prefixes: Vec<String>,
    dry_run: bool,
}

struct StackFixupOpts {
    commit_prefixes: Vec<String>,
    dry_run: bool,
}

struct StackRewordOpts {
    commit_prefix: String,
    message: Option<String>,
    dry_run: bool,
}

struct StackReorderOpts {
    commit_prefixes: Vec<String>,
    dry_run: bool,
}

struct StackMoveOpts {
    commit_prefix: String,
    position: StackMovePosition,
    target_prefix: Option<String>,
    dry_run: bool,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum StackMovePosition {
    First,
    Last,
    Before,
    After,
}

struct StackSquashOpts {
    src_prefixes: Vec<String>,
    target_prefix: String,
    message: Option<String>,
    dry_run: bool,
}

struct StackCheckoutOpts {
    name: String,
    author: Option<String>,
    repository: Option<String>,
    branch: Option<String>,
    branch_prefix: Option<String>,
    dry_run: bool,
    /// `Some((remote, branch))` from `--trunk REMOTE/BRANCH`;
    /// `None` falls back to `trunk::get_trunk` at runtime.
    trunk: Option<(String, String)>,
    /// GitHub token; resolved via `mergify_core::auth::resolve_token`
    /// when None.
    token: Option<String>,
}

struct StackSyncOpts {
    author: Option<String>,
    repository: Option<String>,
    branch_prefix: Option<String>,
    dry_run: bool,
    trunk: Option<(String, String)>,
    token: Option<String>,
}

struct InternalRebaseTodoRewriteOpts {
    /// Which transformation to apply. New variants land with the
    /// respective port slices (today: `edit`, `drop`, `fixup`,
    /// `reword`, `exec-after`, `reorder`, `squash`).
    action: InternalRebaseAction,
    /// Target commit SHA — used by `edit`, `reword`, `exec-after`,
    /// and (optionally) `squash` for the post-fixup exec.
    sha: Option<String>,
    /// Comma-separated commit SHAs — used by `drop`, `fixup`,
    /// `reorder`, `squash` (the full new order in this case).
    shas: Option<String>,
    /// Comma-separated SHAs that should fold as `fixup` — used
    /// by `squash`.
    fixup_shas: Option<String>,
    /// Shell command to inject as an `exec` line — used by
    /// `exec-after`, `squash`.
    command: Option<String>,
    /// Path to the rebase-todo file git wrote.
    todo_path: PathBuf,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum InternalRebaseAction {
    Edit,
    Drop,
    Fixup,
    Reword,
    ExecAfter,
    Reorder,
    Squash,
}

struct StackNoteOpts {
    /// Commit to target — `None` means HEAD. Accepts a SHA prefix,
    /// a ref (`HEAD~1`, branch name, etc.), or a Change-Id prefix
    /// (resolved against the stack walk).
    commit: Option<String>,
    /// Inline message; `None` means "open `$GIT_EDITOR`". Mutually
    /// exclusive with `remove`.
    message: Option<String>,
    /// Concatenate to the existing note instead of replacing.
    append: bool,
    /// Remove the note. Mutually exclusive with `message` /
    /// `append`.
    remove: bool,
}

struct StackNewOpts {
    name: String,
    /// `Some((remote, branch))` for an explicit `--base`; `None`
    /// means "resolve the trunk".
    base: Option<(String, String)>,
    checkout: bool,
}

struct InternalStackLocalCommitsOpts {
    base: String,
    head: String,
    repo_dir: Option<PathBuf>,
}

struct InternalStackRemoteChangesOpts {
    github_server: url::Url,
    token: Option<String>,
    user: String,
    repo: String,
    stack_prefix: String,
    author: String,
}

struct ConfigSimulateOpts {
    config_file: Option<PathBuf>,
    pull_request: PullRequestRef,
    token: Option<String>,
    api_url: Option<String>,
}

struct CiScopesOpts {
    config: Option<PathBuf>,
    base: Option<String>,
    head: Option<String>,
    write: Option<PathBuf>,
}

struct CiScopesSendOpts {
    repository: Option<String>,
    pull_request: Option<u64>,
    token: Option<String>,
    api_url: Option<String>,
    scopes: Vec<String>,
    scopes_json: Option<PathBuf>,
    scopes_file: Option<PathBuf>,
    file_deprecated: Option<PathBuf>,
}

struct CiJunitProcessOpts {
    api_url: Option<String>,
    token: Option<String>,
    repository: Option<String>,
    test_framework: Option<String>,
    test_language: Option<String>,
    tests_target_branch: Option<String>,
    test_exit_code: Option<i32>,
    files: Vec<String>,
}

struct QueuePauseOpts {
    repository: Option<String>,
    token: Option<String>,
    api_url: Option<String>,
    reason: String,
    yes_i_am_sure: bool,
}

struct QueueUnpauseOpts {
    repository: Option<String>,
    token: Option<String>,
    api_url: Option<String>,
}

struct QueueStatusOpts {
    repository: Option<String>,
    token: Option<String>,
    api_url: Option<String>,
    branch: Option<String>,
    output_json: bool,
}

struct QueueShowOpts {
    repository: Option<String>,
    token: Option<String>,
    api_url: Option<String>,
    pr_number: u64,
    verbose: bool,
    output_json: bool,
}

struct FreezeListOpts {
    repository: Option<String>,
    token: Option<String>,
    api_url: Option<String>,
    output_json: bool,
}

struct TestsShowOpts {
    repository: Option<String>,
    test_names: Vec<String>,
    token: Option<String>,
    api_url: Option<String>,
    pipeline_name: Vec<String>,
    pipeline_name_exclude: Vec<String>,
    job_name: Vec<String>,
    job_name_exclude: Vec<String>,
    per_page: Option<u32>,
    json: bool,
}

struct TestsQuarantineOpts {
    repository: Option<String>,
    test_name: String,
    reason: String,
    branch: Option<String>,
    token: Option<String>,
    api_url: Option<String>,
    json: bool,
}

struct TestsUnquarantineOpts {
    repository: Option<String>,
    name_or_id: String,
    token: Option<String>,
    api_url: Option<String>,
    json: bool,
}

struct TestsQuarantinedOpts {
    repository: Option<String>,
    token: Option<String>,
    api_url: Option<String>,
    json: bool,
}

struct FreezeCreateOpts {
    repository: Option<String>,
    token: Option<String>,
    api_url: Option<String>,
    reason: String,
    timezone: Option<String>,
    start: Option<chrono::NaiveDateTime>,
    end: Option<chrono::NaiveDateTime>,
    matching_conditions: Vec<String>,
    exclude_conditions: Vec<String>,
}

struct FreezeUpdateOpts {
    repository: Option<String>,
    token: Option<String>,
    api_url: Option<String>,
    freeze_id: String,
    reason: Option<String>,
    timezone: Option<String>,
    start: Option<chrono::NaiveDateTime>,
    end: Option<chrono::NaiveDateTime>,
    matching_conditions: Option<Vec<String>>,
    exclude_conditions: Option<Vec<String>>,
}

struct FreezeDeleteOpts {
    repository: Option<String>,
    token: Option<String>,
    api_url: Option<String>,
    freeze_id: String,
    delete_reason: Option<String>,
}

/// Heuristic: does argv look like the user intended a native
/// subcommand?
///
/// Used as a fallback when clap rejects the input — if the user
/// clearly meant a native command, surface clap's error rather
/// than silently dispatching to the Python shim. We look for two
/// *consecutive* tokens forming a `(group, subcommand)` pair so a
/// flag value like `--repository config` doesn't accidentally
/// classify the invocation as native.
fn looks_native(argv: &[String]) -> bool {
    argv.windows(2).any(|pair| {
        NATIVE_COMMANDS
            .iter()
            .any(|(g, s)| pair[0] == *g && pair[1] == *s)
    })
}

/// Did clap exit on `--help` / `-h` / `--version`? Those return a
/// special `Err` whose `kind()` is `DisplayHelp` /
/// `DisplayHelpOnMissingArgumentOrSubcommand` / `DisplayVersion`;
/// callers should always honor them and exit (printing the help /
/// version) instead of falling through to the Python shim or
/// surfacing them as argument errors.
fn is_help_or_version(err: &clap::Error) -> bool {
    matches!(
        err.kind(),
        clap::error::ErrorKind::DisplayHelp
            | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    )
}

/// Try to recognize the invocation as a native command.
///
/// Returns ``None`` when the argv doesn't look like a native
/// command — callers fall back to the Python shim, which produces
/// the same error messages as before the port started. When the
/// argv obviously targets a native command (per [`looks_native`])
/// but clap can't parse it — e.g. the user gave an unknown flag
/// or omitted a required argument — this function prints clap's
/// formatted error to stderr and exits the process with clap's
/// exit code (2), matching the Python CLI's behavior for argument
/// errors.
///
/// Argument *values* that are accepted by clap as `String` but
/// fail later domain validation (e.g. an `--api-url` that doesn't
/// parse as a URL) surface as [`mergify_core::CliError`] instead
/// — the corresponding exit code is the one chosen by the command
/// implementation (typically [`mergify_core::ExitCode::Configuration`]
/// = 8), not 2.
#[allow(clippy::too_many_lines)] // mostly mechanical match arms
fn detect_dispatch(argv: &[String]) -> Option<Dispatch> {
    let looks_native = looks_native(argv);

    let parsed = match CliRoot::try_parse_from(
        std::iter::once("mergify".to_string()).chain(argv.iter().cloned()),
    ) {
        Ok(parsed) => parsed,
        Err(err) if is_help_or_version(&err) => {
            // ``--help`` at the binary's root or for any natively
            // dispatched (sub)command is handled by clap. The
            // top-level help now lists `stack` and the shimmed
            // `ci` subcommands too, because they're registered as
            // clap stub variants — that's how a single
            // `mergify --help` covers the full CLI surface.
            // ``err.exit()`` prints to stdout and calls
            // ``process::exit(0)``.
            err.exit()
        }
        Err(err) if looks_native => {
            // Native intent + clap rejection = surface clap's error
            // and exit. ``err.exit()`` prints to stderr and calls
            // ``process::exit``; does not return.
            err.exit()
        }
        Err(_) => return None,
    };

    Some(dispatch_from_parsed(parsed))
}

/// Route a captured `mergify stack <args…>` invocation to either
/// the native stack subcommand handler or the Python shim.
///
/// `stack` is a hybrid group during the port: today only `new` is
/// native, every other subcommand still runs through `mergify-py-shim`.
/// The decision is made by inspecting the first positional arg
/// after `stack` — if it names a natively-ported subcommand, we
/// secondary-parse the rest with clap and dispatch native;
/// otherwise we forward the whole argv to Python verbatim.
///
/// `--help` for shimmed subcommands (and the bare `stack --help`)
/// falls through to Python, which prints the full help listing
/// including the Python-only subcommands. Adding new native stack
/// subcommands later means adding a branch here and a matching
/// `NATIVE_COMMANDS` entry.
fn dispatch_stack(debug: bool, args: Vec<String>) -> Dispatch {
    match args.first().map(String::as_str) {
        Some("new") => {
            // `args[0]` is the subcommand — clap consumes it as
            // the program name in the secondary parse, leaving
            // `args[1..]` as the actual arguments.
            let parsed = match StackNewCli::try_parse_from(&args) {
                Ok(p) => p,
                Err(err) => err.exit(),
            };
            Dispatch::Native(NativeCommand::StackNew(StackNewOpts::from(parsed)))
        }
        Some("note") => {
            let parsed = match StackNoteCli::try_parse_from(&args) {
                Ok(p) => p,
                Err(err) => err.exit(),
            };
            Dispatch::Native(NativeCommand::StackNote(StackNoteOpts::from(parsed)))
        }
        Some("edit") => {
            let parsed = match StackEditCli::try_parse_from(&args) {
                Ok(p) => p,
                Err(err) => err.exit(),
            };
            Dispatch::Native(NativeCommand::StackEdit(StackEditOpts::from(parsed)))
        }
        Some("drop") => {
            let parsed = match StackDropCli::try_parse_from(&args) {
                Ok(p) => p,
                Err(err) => err.exit(),
            };
            Dispatch::Native(NativeCommand::StackDrop(StackDropOpts::from(parsed)))
        }
        Some("fixup") => {
            let parsed = match StackFixupCli::try_parse_from(&args) {
                Ok(p) => p,
                Err(err) => err.exit(),
            };
            Dispatch::Native(NativeCommand::StackFixup(StackFixupOpts::from(parsed)))
        }
        Some("reword") => {
            let parsed = match StackRewordCli::try_parse_from(&args) {
                Ok(p) => p,
                Err(err) => err.exit(),
            };
            Dispatch::Native(NativeCommand::StackReword(StackRewordOpts::from(parsed)))
        }
        Some("reorder") => {
            let parsed = match StackReorderCli::try_parse_from(&args) {
                Ok(p) => p,
                Err(err) => err.exit(),
            };
            Dispatch::Native(NativeCommand::StackReorder(StackReorderOpts::from(parsed)))
        }
        Some("move") => {
            let parsed = match StackMoveCli::try_parse_from(&args) {
                Ok(p) => p,
                Err(err) => err.exit(),
            };
            Dispatch::Native(NativeCommand::StackMove(StackMoveOpts::from(parsed)))
        }
        Some("squash") => {
            let parsed = match StackSquashCli::try_parse_from(&args) {
                Ok(p) => p,
                Err(err) => err.exit(),
            };
            match StackSquashOpts::try_from(parsed) {
                Ok(opts) => Dispatch::Native(NativeCommand::StackSquash(opts)),
                Err(msg) => {
                    eprintln!("error: {msg}");
                    std::process::exit(2);
                }
            }
        }
        Some("checkout") => {
            let parsed = match StackCheckoutCli::try_parse_from(&args) {
                Ok(p) => p,
                Err(err) => err.exit(),
            };
            Dispatch::Native(NativeCommand::StackCheckout(StackCheckoutOpts::from(
                parsed,
            )))
        }
        Some("sync") => {
            let parsed = match StackSyncCli::try_parse_from(&args) {
                Ok(p) => p,
                Err(err) => err.exit(),
            };
            Dispatch::Native(NativeCommand::StackSync(StackSyncOpts::from(parsed)))
        }
        _ => Dispatch::Shim(inject_global_flags(debug, prepend_one("stack", args))),
    }
}

#[allow(clippy::too_many_lines)] // mostly mechanical match arms
fn dispatch_from_parsed(parsed: CliRoot) -> Dispatch {
    let debug = parsed.debug;
    match parsed.command {
        Subcommands::Stack(ShimmedArgs { args }) => dispatch_stack(debug, args),
        Subcommands::Internal(InternalArgs {
            command:
                InternalSubcommand::StackLocalCommits(InternalStackLocalCommitsArgs {
                    base,
                    head,
                    repo_dir,
                }),
        }) => Dispatch::Native(NativeCommand::InternalStackLocalCommits(
            InternalStackLocalCommitsOpts {
                base,
                head,
                repo_dir,
            },
        )),
        Subcommands::Internal(InternalArgs {
            command:
                InternalSubcommand::StackRemoteChanges(InternalStackRemoteChangesArgs {
                    github_server,
                    token,
                    user,
                    repo,
                    stack_prefix,
                    author,
                }),
        }) => Dispatch::Native(NativeCommand::InternalStackRemoteChanges(
            InternalStackRemoteChangesOpts {
                github_server,
                token,
                user,
                repo,
                stack_prefix,
                author,
            },
        )),
        Subcommands::Internal(InternalArgs {
            command:
                InternalSubcommand::RebaseTodoRewrite(InternalRebaseTodoRewriteArgs {
                    action,
                    sha,
                    shas,
                    fixup_shas,
                    command,
                    todo_path,
                }),
        }) => Dispatch::Native(NativeCommand::InternalRebaseTodoRewrite(
            InternalRebaseTodoRewriteOpts {
                action,
                sha,
                shas,
                fixup_shas,
                command,
                todo_path,
            },
        )),
        Subcommands::Ci(CiArgs {
            command:
                CiSubcommand::Scopes(ScopesCliArgs {
                    config,
                    base,
                    head,
                    write,
                }),
        }) => Dispatch::Native(NativeCommand::CiScopes(CiScopesOpts {
            config,
            base,
            head,
            write,
        })),
        Subcommands::Ci(CiArgs {
            command:
                CiSubcommand::JunitProcess(JunitProcessCliArgs {
                    api_url,
                    token,
                    repository,
                    test_framework,
                    test_language,
                    tests_target_branch,
                    test_exit_code,
                    files,
                }),
        }) => Dispatch::Native(NativeCommand::CiJunitProcess(CiJunitProcessOpts {
            api_url,
            token,
            repository,
            test_framework,
            test_language,
            tests_target_branch,
            test_exit_code,
            files,
        })),
        Subcommands::Ci(CiArgs {
            command:
                CiSubcommand::JunitUpload(JunitProcessCliArgs {
                    api_url,
                    token,
                    repository,
                    test_framework,
                    test_language,
                    tests_target_branch,
                    test_exit_code,
                    files,
                }),
        }) => Dispatch::Native(NativeCommand::CiJunitUpload(CiJunitProcessOpts {
            api_url,
            token,
            repository,
            test_framework,
            test_language,
            tests_target_branch,
            test_exit_code,
            files,
        })),
        Subcommands::Config(ConfigArgs {
            config_file,
            command: ConfigSubcommand::Validate(_),
        }) => Dispatch::Native(NativeCommand::ConfigValidate { config_file }),
        Subcommands::Config(ConfigArgs {
            config_file,
            command:
                ConfigSubcommand::Simulate(SimulateCliArgs {
                    pull_request,
                    token,
                    api_url,
                }),
        }) => Dispatch::Native(NativeCommand::ConfigSimulate(ConfigSimulateOpts {
            config_file,
            pull_request,
            token,
            api_url,
        })),
        Subcommands::Ci(CiArgs {
            command:
                CiSubcommand::ScopesSend(ScopesSendCliArgs {
                    repository,
                    pull_request,
                    token,
                    api_url,
                    scope,
                    scopes_json,
                    scopes_file,
                    file_deprecated,
                }),
        }) => Dispatch::Native(NativeCommand::CiScopesSend(CiScopesSendOpts {
            repository,
            pull_request,
            token,
            api_url,
            scopes: scope,
            scopes_json,
            scopes_file,
            file_deprecated,
        })),
        Subcommands::Ci(CiArgs {
            command: CiSubcommand::GitRefs(GitRefsCliArgs { format }),
        }) => Dispatch::Native(NativeCommand::CiGitRefs { format }),
        Subcommands::Ci(CiArgs {
            command: CiSubcommand::QueueInfo,
        }) => Dispatch::Native(NativeCommand::CiQueueInfo),
        Subcommands::Tests(TestsArgs {
            command:
                TestsSubcommand::Show(TestsShowCliArgs {
                    repository,
                    test_names,
                    token,
                    api_url,
                    pipeline_name,
                    pipeline_name_exclude,
                    job_name,
                    job_name_exclude,
                    per_page,
                    json,
                }),
        }) => Dispatch::Native(NativeCommand::TestsShow(TestsShowOpts {
            repository,
            test_names,
            token,
            api_url,
            pipeline_name,
            pipeline_name_exclude,
            job_name,
            job_name_exclude,
            per_page,
            json,
        })),
        Subcommands::Tests(TestsArgs {
            command:
                TestsSubcommand::Quarantine(TestsQuarantineCliArgs {
                    test_name,
                    repository,
                    reason,
                    branch,
                    token,
                    api_url,
                    json,
                }),
        }) => Dispatch::Native(NativeCommand::TestsQuarantine(TestsQuarantineOpts {
            repository,
            test_name,
            reason,
            branch,
            token,
            api_url,
            json,
        })),
        Subcommands::Tests(TestsArgs {
            command:
                TestsSubcommand::Unquarantine(TestsUnquarantineCliArgs {
                    name_or_id,
                    repository,
                    token,
                    api_url,
                    json,
                }),
        }) => Dispatch::Native(NativeCommand::TestsUnquarantine(TestsUnquarantineOpts {
            repository,
            name_or_id,
            token,
            api_url,
            json,
        })),
        Subcommands::Tests(TestsArgs {
            command:
                TestsSubcommand::Quarantined(TestsQuarantinedCliArgs {
                    repository,
                    token,
                    api_url,
                    json,
                }),
        }) => Dispatch::Native(NativeCommand::TestsQuarantined(TestsQuarantinedOpts {
            repository,
            token,
            api_url,
            json,
        })),
        Subcommands::Queue(QueueArgs {
            repository,
            token,
            api_url,
            command:
                QueueSubcommand::Pause(PauseCliArgs {
                    reason,
                    yes_i_am_sure,
                }),
        }) => Dispatch::Native(NativeCommand::QueuePause(QueuePauseOpts {
            repository,
            token,
            api_url,
            reason,
            yes_i_am_sure,
        })),
        Subcommands::Queue(QueueArgs {
            repository,
            token,
            api_url,
            command: QueueSubcommand::Unpause,
        }) => Dispatch::Native(NativeCommand::QueueUnpause(QueueUnpauseOpts {
            repository,
            token,
            api_url,
        })),
        Subcommands::Queue(QueueArgs {
            repository,
            token,
            api_url,
            command: QueueSubcommand::Status(StatusCliArgs { branch, json }),
        }) => Dispatch::Native(NativeCommand::QueueStatus(QueueStatusOpts {
            repository,
            token,
            api_url,
            branch,
            output_json: json,
        })),
        Subcommands::Queue(QueueArgs {
            repository,
            token,
            api_url,
            command:
                QueueSubcommand::Show(ShowCliArgs {
                    pr_number,
                    verbose,
                    json,
                }),
        }) => Dispatch::Native(NativeCommand::QueueShow(QueueShowOpts {
            repository,
            token,
            api_url,
            pr_number,
            verbose,
            output_json: json,
        })),
        Subcommands::Freeze(FreezeArgs {
            repository,
            token,
            api_url,
            command: FreezeSubcommand::List(FreezeListCliArgs { json }),
        }) => Dispatch::Native(NativeCommand::FreezeList(FreezeListOpts {
            repository,
            token,
            api_url,
            output_json: json,
        })),
        Subcommands::Freeze(FreezeArgs {
            repository,
            token,
            api_url,
            command:
                FreezeSubcommand::Create(FreezeCreateCliArgs {
                    reason,
                    timezone,
                    condition,
                    start,
                    end,
                    exclude,
                }),
        }) => Dispatch::Native(NativeCommand::FreezeCreate(FreezeCreateOpts {
            repository,
            token,
            api_url,
            reason,
            timezone,
            start,
            end,
            matching_conditions: condition,
            exclude_conditions: exclude,
        })),
        Subcommands::Freeze(FreezeArgs {
            repository,
            token,
            api_url,
            command:
                FreezeSubcommand::Update(FreezeUpdateCliArgs {
                    freeze_id,
                    reason,
                    timezone,
                    condition,
                    start,
                    end,
                    exclude,
                }),
        }) => Dispatch::Native(NativeCommand::FreezeUpdate(FreezeUpdateOpts {
            repository,
            token,
            api_url,
            freeze_id,
            reason,
            timezone,
            start,
            end,
            // Python's "include the list when the flag was passed
            // at least once" maps to `Some(vec)` only when the user
            // actually supplied a value. clap collects multiple
            // `-c`/`-e` into a `Vec<String>`, so an empty vec is
            // indistinguishable from "flag never given" at this
            // boundary — treat empty as `None` for parity.
            matching_conditions: if condition.is_empty() {
                None
            } else {
                Some(condition)
            },
            exclude_conditions: if exclude.is_empty() {
                None
            } else {
                Some(exclude)
            },
        })),
        Subcommands::Freeze(FreezeArgs {
            repository,
            token,
            api_url,
            command:
                FreezeSubcommand::Delete(FreezeDeleteCliArgs {
                    freeze_id,
                    delete_reason,
                }),
        }) => Dispatch::Native(NativeCommand::FreezeDelete(FreezeDeleteOpts {
            repository,
            token,
            api_url,
            freeze_id,
            delete_reason,
        })),
    }
}

#[allow(clippy::too_many_lines)] // mostly mechanical match arms
fn run_native(cmd: NativeCommand) -> ExitCode {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("mergify: could not start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    // The `tests` commands honor their `--json` flag through the
    // shared `StdioOutput` machinery; everything else writes Human and
    // manages its own JSON via run-time flags.
    let mode = match &cmd {
        NativeCommand::TestsShow(opts) if opts.json => OutputMode::Json,
        NativeCommand::TestsQuarantine(opts) if opts.json => OutputMode::Json,
        NativeCommand::TestsUnquarantine(opts) if opts.json => OutputMode::Json,
        NativeCommand::TestsQuarantined(opts) if opts.json => OutputMode::Json,
        _ => OutputMode::Human,
    };
    let mut output = StdioOutput::new(mode);

    let result: Result<mergify_core::ExitCode, mergify_core::CliError> = rt.block_on(async {
        match cmd {
            NativeCommand::ConfigValidate { config_file } => {
                mergify_config::validate::run(config_file.as_deref(), &mut output)
                    .await
                    .map(|()| mergify_core::ExitCode::Success)
            }
            NativeCommand::ConfigSimulate(opts) => mergify_config::simulate::run(
                SimulateOptions {
                    pull_request: &opts.pull_request,
                    config_file: opts.config_file.as_deref(),
                    token: opts.token.as_deref(),
                    api_url: opts.api_url.as_deref(),
                },
                &mut output,
            )
            .await
            .map(|()| mergify_core::ExitCode::Success),
            NativeCommand::CiScopesSend(opts) => mergify_ci::scopes_send::run(
                ScopesSendOptions {
                    repository: opts.repository.as_deref(),
                    pull_request: opts.pull_request,
                    token: opts.token.as_deref(),
                    api_url: opts.api_url.as_deref(),
                    scopes: &opts.scopes,
                    scopes_json: opts.scopes_json.as_deref(),
                    scopes_file: opts.scopes_file.as_deref(),
                    deprecated_file: opts.file_deprecated.as_deref(),
                },
                &mut output,
            )
            .await
            .map(|()| mergify_core::ExitCode::Success),
            NativeCommand::CiGitRefs { format } => {
                mergify_ci::git_refs::run(&GitRefsOptions { format }, &mut output)
                    .map(|()| mergify_core::ExitCode::Success)
            }
            NativeCommand::CiQueueInfo => {
                mergify_ci::queue_info::run(&mut output).map(|()| mergify_core::ExitCode::Success)
            }
            NativeCommand::CiJunitProcess(opts) => {
                mergify_ci::junit_process::run(
                    JunitProcessOptions {
                        api_url: opts.api_url.as_deref(),
                        token: opts.token.as_deref(),
                        repository: opts.repository.as_deref(),
                        test_framework: opts.test_framework.as_deref(),
                        test_language: opts.test_language.as_deref(),
                        tests_target_branch: opts.tests_target_branch.as_deref(),
                        test_exit_code: opts.test_exit_code,
                        files: &opts.files,
                    },
                    &mut output,
                )
                .await
            }
            NativeCommand::CiJunitUpload(opts) => {
                // Match Python's `@ci.command(deprecated="...")`
                // behavior: click prints a warning to stderr on
                // first invocation before running the command body.
                // The orchestrator is identical to junit-process,
                // so we just forward.
                eprintln!(
                    "DeprecationWarning: 'junit-upload' is deprecated, use `junit-process` instead.",
                );
                mergify_ci::junit_process::run(
                    JunitProcessOptions {
                        api_url: opts.api_url.as_deref(),
                        token: opts.token.as_deref(),
                        repository: opts.repository.as_deref(),
                        test_framework: opts.test_framework.as_deref(),
                        test_language: opts.test_language.as_deref(),
                        tests_target_branch: opts.tests_target_branch.as_deref(),
                        test_exit_code: opts.test_exit_code,
                        files: &opts.files,
                    },
                    &mut output,
                )
                .await
            }
            NativeCommand::CiScopes(opts) => mergify_ci::scopes_detect::run(
                mergify_ci::scopes_detect::ScopesOptions {
                    config: opts.config.as_deref(),
                    base: opts.base.as_deref(),
                    head: opts.head.as_deref(),
                    write: opts.write.as_deref(),
                },
                &mut output,
            )
            .map(|()| mergify_core::ExitCode::Success),
            NativeCommand::TestsShow(opts) => {
                mergify_ci::tests_show::run(
                    TestsShowOptions {
                        repository: opts.repository.as_deref(),
                        test_names: &opts.test_names,
                        token: opts.token.as_deref(),
                        api_url: opts.api_url.as_deref(),
                        pipeline_name: &opts.pipeline_name,
                        pipeline_name_exclude: &opts.pipeline_name_exclude,
                        job_name: &opts.job_name,
                        job_name_exclude: &opts.job_name_exclude,
                        per_page: opts.per_page,
                    },
                    &mut output,
                )
                .await
            }
            NativeCommand::TestsQuarantine(opts) => {
                mergify_ci::tests_quarantine::quarantine(
                    QuarantineOptions {
                        repository: opts.repository.as_deref(),
                        test_name: &opts.test_name,
                        reason: &opts.reason,
                        branch: opts.branch.as_deref(),
                        token: opts.token.as_deref(),
                        api_url: opts.api_url.as_deref(),
                    },
                    &mut output,
                )
                .await
            }
            NativeCommand::TestsUnquarantine(opts) => {
                mergify_ci::tests_quarantine::unquarantine(
                    UnquarantineOptions {
                        repository: opts.repository.as_deref(),
                        name_or_id: &opts.name_or_id,
                        token: opts.token.as_deref(),
                        api_url: opts.api_url.as_deref(),
                    },
                    &mut output,
                )
                .await
            }
            NativeCommand::TestsQuarantined(opts) => {
                mergify_ci::tests_quarantine::quarantined(
                    QuarantinedOptions {
                        repository: opts.repository.as_deref(),
                        token: opts.token.as_deref(),
                        api_url: opts.api_url.as_deref(),
                    },
                    &mut output,
                )
                .await
            }
            NativeCommand::QueuePause(opts) => mergify_queue::pause::run(
                PauseOptions {
                    repository: opts.repository.as_deref(),
                    token: opts.token.as_deref(),
                    api_url: opts.api_url.as_deref(),
                    reason: &opts.reason,
                    yes_i_am_sure: opts.yes_i_am_sure,
                },
                &mut output,
            )
            .await
            .map(|()| mergify_core::ExitCode::Success),
            NativeCommand::QueueUnpause(opts) => mergify_queue::unpause::run(
                UnpauseOptions {
                    repository: opts.repository.as_deref(),
                    token: opts.token.as_deref(),
                    api_url: opts.api_url.as_deref(),
                },
                &mut output,
            )
            .await
            .map(|()| mergify_core::ExitCode::Success),
            NativeCommand::QueueStatus(opts) => mergify_queue::status::run(
                StatusOptions {
                    repository: opts.repository.as_deref(),
                    token: opts.token.as_deref(),
                    api_url: opts.api_url.as_deref(),
                    branch: opts.branch.as_deref(),
                    output_json: opts.output_json,
                },
                &mut output,
            )
            .await
            .map(|()| mergify_core::ExitCode::Success),
            NativeCommand::QueueShow(opts) => mergify_queue::show::run(
                ShowOptions {
                    repository: opts.repository.as_deref(),
                    token: opts.token.as_deref(),
                    api_url: opts.api_url.as_deref(),
                    pr_number: opts.pr_number,
                    verbose: opts.verbose,
                    output_json: opts.output_json,
                },
                &mut output,
            )
            .await
            .map(|()| mergify_core::ExitCode::Success),
            NativeCommand::FreezeList(opts) => mergify_freeze::list::run(
                FreezeListOptions {
                    repository: opts.repository.as_deref(),
                    token: opts.token.as_deref(),
                    api_url: opts.api_url.as_deref(),
                    output_json: opts.output_json,
                },
                &mut output,
            )
            .await
            .map(|()| mergify_core::ExitCode::Success),
            NativeCommand::FreezeCreate(opts) => mergify_freeze::create::run(
                FreezeCreateOptions {
                    repository: opts.repository.as_deref(),
                    token: opts.token.as_deref(),
                    api_url: opts.api_url.as_deref(),
                    reason: &opts.reason,
                    timezone: opts.timezone.as_deref(),
                    start: opts.start,
                    end: opts.end,
                    matching_conditions: &opts.matching_conditions,
                    exclude_conditions: &opts.exclude_conditions,
                },
                &mut output,
            )
            .await
            .map(|()| mergify_core::ExitCode::Success),
            NativeCommand::FreezeUpdate(opts) => mergify_freeze::update::run(
                FreezeUpdateOptions {
                    repository: opts.repository.as_deref(),
                    token: opts.token.as_deref(),
                    api_url: opts.api_url.as_deref(),
                    freeze_id: &opts.freeze_id,
                    reason: opts.reason.as_deref(),
                    timezone: opts.timezone.as_deref(),
                    start: opts.start,
                    end: opts.end,
                    matching_conditions: opts.matching_conditions.as_deref(),
                    exclude_conditions: opts.exclude_conditions.as_deref(),
                },
                &mut output,
            )
            .await
            .map(|()| mergify_core::ExitCode::Success),
            NativeCommand::FreezeDelete(opts) => mergify_freeze::delete::run(
                FreezeDeleteOptions {
                    repository: opts.repository.as_deref(),
                    token: opts.token.as_deref(),
                    api_url: opts.api_url.as_deref(),
                    freeze_id: &opts.freeze_id,
                    delete_reason: opts.delete_reason.as_deref(),
                },
                &mut output,
            )
            .await
            .map(|()| mergify_core::ExitCode::Success),
            NativeCommand::StackNew(opts) => {
                let base = opts
                    .base
                    .map(|(remote, branch)| mergify_stack::commands::new::Base {
                        remote,
                        branch,
                    });
                let outcome =
                    mergify_stack::commands::new::run(None, &opts.name, base, opts.checkout)?;
                if let Some(auto_set) = &outcome.upstream_auto_set {
                    // Yellow notice — matches `utils.get_trunk`'s
                    // print when it auto-sets upstream tracking.
                    eprintln!(
                        "Upstream not set for {branch}, automatically set to {remote}/{target}",
                        branch = auto_set.current_branch,
                        remote = auto_set.remote,
                        target = auto_set.branch,
                    );
                }
                println!(
                    "Created branch '{name}' tracking {base}",
                    name = outcome.branch_name,
                    base = outcome.base_refspec,
                );
                if outcome.checked_out {
                    println!("Switched to branch '{}'", outcome.branch_name);
                } else {
                    println!(
                        "Run 'git checkout {}' to switch to the new branch",
                        outcome.branch_name,
                    );
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackNote(opts) => {
                let action = if opts.remove {
                    mergify_stack::commands::note::Action::Remove
                } else if let Some(msg) = opts.message {
                    if opts.append {
                        mergify_stack::commands::note::Action::Append(msg)
                    } else {
                        mergify_stack::commands::note::Action::Set(msg)
                    }
                } else {
                    mergify_stack::commands::note::Action::FromEditor
                };
                let outcome = mergify_stack::commands::note::run(
                    None,
                    opts.commit.as_deref(),
                    action,
                )?;
                match outcome {
                    mergify_stack::commands::note::Outcome::Attached { sha, subject } => {
                        println!(
                            "Note attached to {short} {subject}.",
                            short = &sha[..sha.len().min(12)],
                        );
                    }
                    mergify_stack::commands::note::Outcome::Removed { sha, subject } => {
                        println!(
                            "Note removed from {short} {subject}.",
                            short = &sha[..sha.len().min(12)],
                        );
                    }
                    mergify_stack::commands::note::Outcome::NoNoteToRemove {
                        sha,
                        subject,
                    } => {
                        println!(
                            "No note on {short} {subject}.",
                            short = &sha[..sha.len().min(12)],
                        );
                    }
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackEdit(opts) => {
                let mergify_binary = std::env::current_exe().map_err(|e| {
                    mergify_core::CliError::Generic(format!(
                        "could not locate current binary path for GIT_SEQUENCE_EDITOR: {e}"
                    ))
                })?;
                let outcome = mergify_stack::commands::edit::run(
                    &mergify_stack::commands::edit::Options {
                        repo_dir: None,
                        commit_prefix: opts.commit_prefix.as_deref(),
                        mergify_binary: &mergify_binary,
                    },
                )?;
                match outcome {
                    mergify_stack::commands::edit::Outcome::PausedAt { commit } => {
                        let short = &commit.sha[..commit.sha.len().min(12)];
                        println!("Editing commit: {short} {subject}", subject = commit.subject);
                        println!("Amend the commit, then run: git rebase --continue");
                    }
                    mergify_stack::commands::edit::Outcome::EmptyStack => {
                        println!("No commits in the stack");
                    }
                    mergify_stack::commands::edit::Outcome::InteractiveCompleted => {}
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackDrop(opts) => {
                let mergify_binary = std::env::current_exe().map_err(|e| {
                    mergify_core::CliError::Generic(format!(
                        "could not locate current binary path for GIT_SEQUENCE_EDITOR: {e}"
                    ))
                })?;
                let outcome = mergify_stack::commands::drop::run(
                    &mergify_stack::commands::drop::Options {
                        repo_dir: None,
                        commit_prefixes: &opts.commit_prefixes,
                        dry_run: opts.dry_run,
                        mergify_binary: &mergify_binary,
                    },
                )?;
                match outcome {
                    mergify_stack::commands::drop::Outcome::Dropped { dropped } => {
                        for c in &dropped {
                            let short = &c.sha[..c.sha.len().min(12)];
                            println!("Dropping: {short} {subject}", subject = c.subject);
                        }
                        println!("Commits dropped successfully.");
                    }
                    mergify_stack::commands::drop::Outcome::DryRun { plan } => {
                        println!("Drop plan:");
                        for c in &plan {
                            let short = &c.sha[..c.sha.len().min(12)];
                            println!("  drop {short} {subject}", subject = c.subject);
                        }
                        println!("Dry run — no changes made");
                    }
                    mergify_stack::commands::drop::Outcome::EmptyStack => {
                        println!("No commits in the stack");
                    }
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackFixup(opts) => {
                let mergify_binary = std::env::current_exe().map_err(|e| {
                    mergify_core::CliError::Generic(format!(
                        "could not locate current binary path for GIT_SEQUENCE_EDITOR: {e}"
                    ))
                })?;
                let outcome = mergify_stack::commands::fixup::run(
                    &mergify_stack::commands::fixup::Options {
                        repo_dir: None,
                        commit_prefixes: &opts.commit_prefixes,
                        dry_run: opts.dry_run,
                        mergify_binary: &mergify_binary,
                    },
                )?;
                match outcome {
                    mergify_stack::commands::fixup::Outcome::Squashed { fixed_up } => {
                        for c in &fixed_up {
                            let short = &c.sha[..c.sha.len().min(12)];
                            println!("Fixing up: {short} {subject}", subject = c.subject);
                        }
                        println!("Commits squashed successfully.");
                    }
                    mergify_stack::commands::fixup::Outcome::DryRun { plan } => {
                        println!("Fixup plan:");
                        for c in &plan {
                            let short = &c.sha[..c.sha.len().min(12)];
                            println!("  fixup {short} {subject}", subject = c.subject);
                        }
                        println!("Dry run — no changes made");
                    }
                    mergify_stack::commands::fixup::Outcome::EmptyStack => {
                        println!("No commits in the stack");
                    }
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackReword(opts) => {
                let mergify_binary = std::env::current_exe().map_err(|e| {
                    mergify_core::CliError::Generic(format!(
                        "could not locate current binary path for GIT_SEQUENCE_EDITOR: {e}"
                    ))
                })?;
                let outcome = mergify_stack::commands::reword::run(
                    &mergify_stack::commands::reword::Options {
                        repo_dir: None,
                        commit_prefix: &opts.commit_prefix,
                        message: opts.message.as_deref(),
                        dry_run: opts.dry_run,
                        mergify_binary: &mergify_binary,
                    },
                )?;
                match outcome {
                    mergify_stack::commands::reword::Outcome::Reworded { commit } => {
                        let short = &commit.sha[..commit.sha.len().min(12)];
                        println!(
                            "Reworded {short} {subject}",
                            subject = commit.subject,
                        );
                    }
                    mergify_stack::commands::reword::Outcome::DryRun {
                        commit,
                        inline_message,
                    } => {
                        let short = &commit.sha[..commit.sha.len().min(12)];
                        let verb = if inline_message { "amend" } else { "reword" };
                        println!("Reword plan:");
                        println!("  {verb} {short} {subject}", subject = commit.subject);
                        println!("Dry run — no changes made");
                    }
                    mergify_stack::commands::reword::Outcome::EmptyStack => {
                        println!("No commits in the stack");
                    }
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackReorder(opts) => {
                let mergify_binary = std::env::current_exe().map_err(|e| {
                    mergify_core::CliError::Generic(format!(
                        "could not locate current binary path for GIT_SEQUENCE_EDITOR: {e}"
                    ))
                })?;
                let outcome = mergify_stack::commands::reorder::run(
                    &mergify_stack::commands::reorder::Options {
                        repo_dir: None,
                        commit_prefixes: &opts.commit_prefixes,
                        dry_run: opts.dry_run,
                        mergify_binary: &mergify_binary,
                    },
                )?;
                match outcome {
                    mergify_stack::commands::reorder::Outcome::Reordered { plan }
                    | mergify_stack::commands::reorder::Outcome::DryRun { plan } => {
                        println!("Reorder plan:");
                        for (i, c) in plan.iter().enumerate() {
                            let short = &c.sha[..c.sha.len().min(12)];
                            println!("  {n}. {short} {subject}", n = i + 1, subject = c.subject);
                        }
                        if opts.dry_run {
                            println!("Dry run — no changes made");
                        } else {
                            println!("Stack reordered successfully.");
                        }
                    }
                    mergify_stack::commands::reorder::Outcome::AlreadyInOrder => {
                        println!("Stack is already in the requested order");
                    }
                    mergify_stack::commands::reorder::Outcome::EmptyStack => {
                        println!("No commits in the stack");
                    }
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackMove(opts) => {
                let mergify_binary = std::env::current_exe().map_err(|e| {
                    mergify_core::CliError::Generic(format!(
                        "could not locate current binary path for GIT_SEQUENCE_EDITOR: {e}"
                    ))
                })?;
                let position = match opts.position {
                    StackMovePosition::First => mergify_stack::commands::move_cmd::Position::First,
                    StackMovePosition::Last => mergify_stack::commands::move_cmd::Position::Last,
                    StackMovePosition::Before => mergify_stack::commands::move_cmd::Position::Before,
                    StackMovePosition::After => mergify_stack::commands::move_cmd::Position::After,
                };
                let outcome = mergify_stack::commands::move_cmd::run(
                    &mergify_stack::commands::move_cmd::Options {
                        repo_dir: None,
                        commit_prefix: &opts.commit_prefix,
                        position,
                        target_prefix: opts.target_prefix.as_deref(),
                        dry_run: opts.dry_run,
                        mergify_binary: &mergify_binary,
                    },
                )?;
                match outcome {
                    mergify_stack::commands::move_cmd::Outcome::Moved { plan }
                    | mergify_stack::commands::move_cmd::Outcome::DryRun { plan } => {
                        println!("Move plan:");
                        for (i, c) in plan.iter().enumerate() {
                            let short = &c.sha[..c.sha.len().min(12)];
                            println!("  {n}. {short} {subject}", n = i + 1, subject = c.subject);
                        }
                        if opts.dry_run {
                            println!("Dry run — no changes made");
                        } else {
                            println!("Commit moved successfully.");
                        }
                    }
                    mergify_stack::commands::move_cmd::Outcome::AlreadyInPosition => {
                        println!("Commit is already in the requested position");
                    }
                    mergify_stack::commands::move_cmd::Outcome::EmptyStack => {
                        println!("No commits in the stack");
                    }
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackSquash(opts) => {
                let mergify_binary = std::env::current_exe().map_err(|e| {
                    mergify_core::CliError::Generic(format!(
                        "could not locate current binary path for GIT_SEQUENCE_EDITOR: {e}"
                    ))
                })?;
                let outcome = mergify_stack::commands::squash::run(
                    &mergify_stack::commands::squash::Options {
                        repo_dir: None,
                        src_prefixes: &opts.src_prefixes,
                        target_prefix: &opts.target_prefix,
                        message: opts.message.as_deref(),
                        dry_run: opts.dry_run,
                        mergify_binary: &mergify_binary,
                    },
                )?;
                match outcome {
                    mergify_stack::commands::squash::Outcome::Squashed { plan }
                    | mergify_stack::commands::squash::Outcome::DryRun { plan } => {
                        println!("Squash plan:");
                        for (i, c) in plan.iter().enumerate() {
                            let short = &c.sha[..c.sha.len().min(12)];
                            println!("  {n}. {short} {subject}", n = i + 1, subject = c.subject);
                        }
                        if opts.dry_run {
                            println!("Dry run — no changes made");
                        } else {
                            println!("Commits squashed successfully.");
                        }
                    }
                    mergify_stack::commands::squash::Outcome::EmptyStack => {
                        println!("No commits in the stack");
                    }
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackCheckout(opts) => {
                let token = mergify_core::auth::resolve_token(opts.token.as_deref())?;
                let github_server =
                    mergify_stack::stack_context::resolve_github_server(None)?;
                let client = mergify_stack::remote_changes::default_client(
                    github_server,
                    &token,
                )?;

                // Trunk: explicit --trunk wins; otherwise resolve.
                let trunk = if let Some((remote, branch)) = opts.trunk {
                    (remote, branch)
                } else {
                    let t = mergify_stack::trunk::get_trunk(None).map_err(|e| {
                        mergify_core::CliError::StackNotFound(format!(
                            "could not determine trunk branch ({e}). Pass --trunk REMOTE/BRANCH."
                        ))
                    })?;
                    (t.remote, t.branch)
                };
                let remote = &trunk.0;

                let slug = mergify_stack::stack_context::resolve_repo(
                    None,
                    opts.repository.as_deref(),
                    remote,
                )?;

                // Author: explicit wins; else GET /user.
                let author = if let Some(a) = opts.author.as_deref() {
                    a.to_string()
                } else {
                    let user_payload: serde_json::Value = client.get("/user").await?;
                    user_payload
                        .get("login")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            mergify_core::CliError::GitHubApi(
                                "/user response missing `login`".to_string(),
                            )
                        })?
                };

                let branch_prefix = opts.branch_prefix.unwrap_or_else(|| {
                    mergify_stack::stack_context::resolve_default_branch_prefix(None, &author)
                });

                let outcome = mergify_stack::commands::checkout::run(
                    &mergify_stack::commands::checkout::Options {
                        repo_dir: None,
                        client: &client,
                        user: &slug.owner,
                        repo: &slug.repo,
                        author: &author,
                        branch_prefix: &branch_prefix,
                        name: &opts.name,
                        local_branch: opts.branch.as_deref(),
                        remote,
                        dry_run: opts.dry_run,
                    },
                )
                .await?;

                match outcome {
                    mergify_stack::commands::checkout::Outcome::NoStackedPrs => {
                        println!("No stacked pull requests found");
                    }
                    mergify_stack::commands::checkout::Outcome::CheckedOut {
                        chain,
                        created,
                        local_branch,
                        upstream,
                    } => {
                        println!("Stacked pull requests:");
                        for pr in &chain {
                            println!(
                                "* #{n} {title}  {url}",
                                n = pr.number,
                                title = pr.title,
                                url = pr.html_url,
                            );
                            println!("  {} -> {}", pr.base_ref, pr.head_ref);
                        }
                        if created {
                            println!(
                                "Checked out '{local_branch}' tracking {upstream}",
                            );
                        }
                    }
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackSync(opts) => {
                let token = mergify_core::auth::resolve_token(opts.token.as_deref())?;
                let github_server = mergify_stack::stack_context::resolve_github_server(None)?;
                let client =
                    mergify_stack::remote_changes::default_client(github_server, &token)?;

                let trunk = if let Some((remote, branch)) = opts.trunk {
                    (remote, branch)
                } else {
                    let t = mergify_stack::trunk::get_trunk(None).map_err(|e| {
                        mergify_core::CliError::StackNotFound(format!(
                            "could not determine trunk branch ({e}). Pass --trunk REMOTE/BRANCH."
                        ))
                    })?;
                    (t.remote, t.branch)
                };
                let slug = mergify_stack::stack_context::resolve_repo(
                    None,
                    opts.repository.as_deref(),
                    &trunk.0,
                )?;
                let author = if let Some(a) = opts.author.as_deref() {
                    a.to_string()
                } else {
                    let user_payload: serde_json::Value = client.get("/user").await?;
                    user_payload
                        .get("login")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            mergify_core::CliError::GitHubApi(
                                "/user response missing `login`".to_string(),
                            )
                        })?
                };
                let branch_prefix = opts.branch_prefix.unwrap_or_else(|| {
                    mergify_stack::stack_context::resolve_default_branch_prefix(None, &author)
                });

                let mergify_binary = std::env::current_exe().map_err(|e| {
                    mergify_core::CliError::Generic(format!(
                        "could not locate current binary path for GIT_SEQUENCE_EDITOR: {e}"
                    ))
                })?;

                let outcome = mergify_stack::commands::sync::run(
                    &mergify_stack::commands::sync::Options {
                        repo_dir: None,
                        client: &client,
                        user: &slug.owner,
                        repo: &slug.repo,
                        author: &author,
                        branch_prefix: &branch_prefix,
                        trunk: (&trunk.0, &trunk.1),
                        dry_run: opts.dry_run,
                        mergify_binary: &mergify_binary,
                    },
                )
                .await?;

                match outcome {
                    mergify_stack::commands::sync::Outcome::DryRun(status) => {
                        if status.all_merged() {
                            println!(
                                "All commits in the stack have been merged into {trunk_branch}.",
                                trunk_branch = trunk.1,
                            );
                            println!(
                                "You can switch to {trunk_branch} with: git checkout {trunk_branch}",
                                trunk_branch = trunk.1,
                            );
                        } else if status.up_to_date() {
                            println!("Stack is up to date.");
                        } else {
                            println!("Dry run: the following merged commits would be removed:");
                            for m in &status.merged {
                                println!("  - {title} (#{num}, merged)", title = m.title, num = m.pull_number);
                            }
                            println!(
                                "\n{} commit(s) would remain in the stack.",
                                status.remaining.len()
                            );
                        }
                    }
                    mergify_stack::commands::sync::Outcome::Synced {
                        status,
                        dropped_count,
                    } => {
                        if status.all_merged() {
                            println!(
                                "All commits in the stack have been merged into {trunk_branch}.",
                                trunk_branch = trunk.1,
                            );
                            println!(
                                "You can switch to {trunk_branch} with: git checkout {trunk_branch}",
                                trunk_branch = trunk.1,
                            );
                        } else if dropped_count == 0 {
                            println!("Stack is up to date.");
                        } else {
                            for m in &status.merged {
                                println!("  ✓ Dropped: {title} (#{num})", title = m.title, num = m.pull_number);
                            }
                            println!(
                                "Dropped {dropped_count} merged commit(s). {} commit(s) remaining in the stack.",
                                status.remaining.len()
                            );
                        }
                    }
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::InternalRebaseTodoRewrite(opts) => {
                let action = match opts.action {
                    InternalRebaseAction::Edit => {
                        let sha = opts.sha.ok_or_else(|| {
                            mergify_core::CliError::InvalidState(
                                "_internal rebase-todo-rewrite --action edit requires --sha"
                                    .to_string(),
                            )
                        })?;
                        mergify_stack::rebase_todo::Action::Edit { sha }
                    }
                    InternalRebaseAction::Drop => {
                        let raw = opts.shas.ok_or_else(|| {
                            mergify_core::CliError::InvalidState(
                                "_internal rebase-todo-rewrite --action drop requires --shas"
                                    .to_string(),
                            )
                        })?;
                        let shas = raw
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect();
                        mergify_stack::rebase_todo::Action::Drop { shas }
                    }
                    InternalRebaseAction::Fixup => {
                        let raw = opts.shas.ok_or_else(|| {
                            mergify_core::CliError::InvalidState(
                                "_internal rebase-todo-rewrite --action fixup requires --shas"
                                    .to_string(),
                            )
                        })?;
                        let shas = raw
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect();
                        mergify_stack::rebase_todo::Action::Fixup { shas }
                    }
                    InternalRebaseAction::Reword => {
                        let sha = opts.sha.ok_or_else(|| {
                            mergify_core::CliError::InvalidState(
                                "_internal rebase-todo-rewrite --action reword requires --sha"
                                    .to_string(),
                            )
                        })?;
                        mergify_stack::rebase_todo::Action::Reword { sha }
                    }
                    InternalRebaseAction::Reorder => {
                        let raw = opts.shas.ok_or_else(|| {
                            mergify_core::CliError::InvalidState(
                                "_internal rebase-todo-rewrite --action reorder requires --shas"
                                    .to_string(),
                            )
                        })?;
                        let ordered_shas = raw
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect();
                        mergify_stack::rebase_todo::Action::Reorder { ordered_shas }
                    }
                    InternalRebaseAction::Squash => {
                        let raw_shas = opts.shas.ok_or_else(|| {
                            mergify_core::CliError::InvalidState(
                                "_internal rebase-todo-rewrite --action squash requires --shas"
                                    .to_string(),
                            )
                        })?;
                        let ordered_shas: Vec<String> = raw_shas
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect();
                        let raw_fixup = opts.fixup_shas.ok_or_else(|| {
                            mergify_core::CliError::InvalidState(
                                "_internal rebase-todo-rewrite --action squash requires --fixup-shas"
                                    .to_string(),
                            )
                        })?;
                        let fixup_shas: Vec<String> = raw_fixup
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect();
                        mergify_stack::rebase_todo::Action::Squash {
                            ordered_shas,
                            fixup_shas,
                            exec_after_sha: opts.sha,
                            exec_command: opts.command,
                        }
                    }
                    InternalRebaseAction::ExecAfter => {
                        let sha = opts.sha.ok_or_else(|| {
                            mergify_core::CliError::InvalidState(
                                "_internal rebase-todo-rewrite --action exec-after requires --sha"
                                    .to_string(),
                            )
                        })?;
                        let command = opts.command.ok_or_else(|| {
                            mergify_core::CliError::InvalidState(
                                "_internal rebase-todo-rewrite --action exec-after requires --command"
                                    .to_string(),
                            )
                        })?;
                        mergify_stack::rebase_todo::Action::ExecAfter { sha, command }
                    }
                };
                let original = std::fs::read_to_string(&opts.todo_path).map_err(|e| {
                    mergify_core::CliError::Generic(format!(
                        "read rebase-todo at {}: {e}",
                        opts.todo_path.display()
                    ))
                })?;
                let rewritten = mergify_stack::rebase_todo::rewrite(&original, &action)?;
                std::fs::write(&opts.todo_path, rewritten).map_err(|e| {
                    mergify_core::CliError::Generic(format!(
                        "write rebase-todo at {}: {e}",
                        opts.todo_path.display()
                    ))
                })?;
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::InternalStackLocalCommits(opts) => {
                // Run `git log` for the stack range, parse each
                // commit's `Change-Id:` trailer, emit a JSON array
                // on stdout. The Python `stack/changes.py` consumer
                // deserializes it back into the existing `LocalChange`
                // pipeline. A missing Change-Id propagates as
                // `CliError::InvalidState` so Python sees the same
                // exit code it would have raised inline.
                let repo_dir = mergify_stack::local_commits::resolve_repo_dir(opts.repo_dir);
                let commits =
                    mergify_stack::local_commits::read(&repo_dir, &opts.base, &opts.head)?;
                let json = serde_json::to_string(&commits).map_err(|e| {
                    mergify_core::CliError::Generic(format!(
                        "serialize stack-local-commits output: {e}",
                    ))
                })?;
                println!("{json}");
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::InternalStackRemoteChanges(opts) => {
                // Search GitHub for PRs belonging to the stack and
                // group them by Change-Id. The Python `stack/changes.py`
                // consumer deserializes the JSON array back into the
                // `RemoteChanges` dict it always built itself.
                //
                // Token comes from `--token` when supplied; otherwise
                // `auth::resolve_token` reads `MERGIFY_TOKEN` /
                // `GITHUB_TOKEN` / `gh auth token` so the Python
                // caller can pass it via the subprocess env and keep
                // it out of `ps`/process listings.
                let token =
                    mergify_core::auth::resolve_token(opts.token.as_deref())?;
                let client = mergify_stack::remote_changes::default_client(
                    opts.github_server,
                    &token,
                )?;
                let changes = mergify_stack::remote_changes::get_remote_changes(
                    &client,
                    &opts.user,
                    &opts.repo,
                    &opts.stack_prefix,
                    &opts.author,
                )
                .await?;
                let json = serde_json::to_string(&changes).map_err(|e| {
                    mergify_core::CliError::Generic(format!(
                        "serialize stack-remote-changes output: {e}",
                    ))
                })?;
                println!("{json}");
                Ok(mergify_core::ExitCode::Success)
            }
        }
    });

    match result {
        Ok(code) => ExitCode::from(code.as_u8()),
        Err(err) => {
            let code = err.exit_code();
            eprintln!("mergify: {err}");
            ExitCode::from(code.as_u8())
        }
    }
}

#[derive(Parser)]
#[command(name = "mergify", disable_help_subcommand = true)]
#[command(disable_version_flag = true)]
struct CliRoot {
    /// Enable verbose debug logging. Mirrors the Python CLI's
    /// top-level `--debug` flag so the same invocations work
    /// against either binary; native commands accept it as a no-op
    /// today (no native code path consults it yet), shimmed ones
    /// re-inject it into the forwarded argv so the Python side can
    /// honor it.
    #[arg(long, global = true)]
    debug: bool,

    #[command(subcommand)]
    command: Subcommands,
}

#[derive(Subcommand)]
enum Subcommands {
    /// Manage Mergify configuration.
    Config(ConfigArgs),
    /// Mergify CI-related commands.
    Ci(CiArgs),
    /// Inspect tests tracked by Mergify CI Insights.
    Tests(TestsArgs),
    /// Manage the Mergify merge queue.
    Queue(QueueArgs),
    /// Manage scheduled freezes.
    Freeze(FreezeArgs),
    /// Manage stacked pull requests.
    Stack(ShimmedArgs),
    /// Internal helpers the Python side of the wheel calls during
    /// the Python→Rust migration. Hidden from `--help` because it
    /// is not part of the user-facing CLI; the wire format is not
    /// stable and may change without notice. Do not depend on it
    /// from anywhere outside the Python code shipped in this same
    /// wheel.
    #[command(name = "_internal", hide = true)]
    Internal(InternalArgs),
}

/// Catch-all positional args for a shimmed subcommand. We surface
/// the command natively through clap (so `--help` listings are
/// complete) but the execution still has to reach the Python
/// implementation. `disable_help_flag` keeps clap from rendering
/// its own placeholder help when the user does
/// `mergify <group> <shimmed> --help`; the `--help` falls into
/// `args` and we forward it to Python, which prints the real help.
#[derive(clap::Args)]
#[command(disable_help_flag = true)]
struct ShimmedArgs {
    /// All arguments forwarded verbatim to the Python implementation.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    args: Vec<String>,
}

#[derive(clap::Args)]
struct InternalArgs {
    #[command(subcommand)]
    command: InternalSubcommand,
}

#[derive(Subcommand)]
enum InternalSubcommand {
    /// Walk the local stack commits in `<base>..<head>` and print
    /// a JSON array of `{commit_sha, title, message, change_id, slug}`.
    /// Used by the Python side of `mergify stack <cmd>` during
    /// migration to centralise the `git log` + `Change-Id:`
    /// extraction. Not a stable user-facing surface.
    #[command(name = "stack-local-commits")]
    StackLocalCommits(InternalStackLocalCommitsArgs),
    /// Search GitHub for the open + merged PRs belonging to a
    /// stack and group them by `Change-Id`. Used by the Python
    /// side of `mergify stack <cmd>` during migration to
    /// centralise the GitHub search + per-PR fetch + change-id
    /// regrouping. Not a stable user-facing surface.
    #[command(name = "stack-remote-changes")]
    StackRemoteChanges(InternalStackRemoteChangesArgs),
    /// Self-invocation target for the rebase-family stack
    /// subcommands. `mergify stack <cmd>` sets
    /// `GIT_SEQUENCE_EDITOR` to a command line ending in this
    /// subcommand before spawning `git rebase -i`; git invokes
    /// it on the freshly-written rebase-todo file. Not a stable
    /// user-facing surface.
    #[command(name = "rebase-todo-rewrite")]
    RebaseTodoRewrite(InternalRebaseTodoRewriteArgs),
}

#[derive(clap::Args)]
struct InternalRebaseTodoRewriteArgs {
    /// Transformation to apply. New variants land with the
    /// respective port slices (today: `edit`, `drop`, `fixup`,
    /// `reword`, `exec-after`).
    #[arg(long, value_enum)]
    action: InternalRebaseAction,
    /// Target SHA — required for `edit`, `reword`, `exec-after`.
    #[arg(long)]
    sha: Option<String>,
    /// Comma-separated SHAs — required for `drop`, `fixup`,
    /// `reorder`, `squash` (full new order).
    #[arg(long)]
    shas: Option<String>,
    /// Comma-separated SHAs to fold as fixup — used by `squash`.
    #[arg(long = "fixup-shas")]
    fixup_shas: Option<String>,
    /// Shell command to inject as an `exec` line — required for
    /// `exec-after`, optional for `squash`.
    #[arg(long)]
    command: Option<String>,
    /// Path to the rebase-todo file git wrote; positional so it
    /// catches whatever git's `sh -c "$EDITOR \"$@\"" sh <path>`
    /// hands us.
    todo_path: PathBuf,
}

/// `mergify stack new <name>` — clap definition for the natively-
/// ported `stack new` subcommand. Parsed as a side step after the
/// top-level clap pass captures `Stack(ShimmedArgs)`, so the rest
/// of the `stack` group still flows through the Python shim.
#[derive(Parser)]
#[command(name = "new", about = "Create a new stack branch")]
struct StackNewCli {
    /// Name of the new branch.
    name: String,

    /// Base branch to fork from, formatted as `REMOTE/BRANCH`
    /// (e.g. `origin/main`). When omitted, the trunk is resolved
    /// from the current branch's tracking info or
    /// `refs/remotes/origin/HEAD`.
    #[arg(long, short = 'b', value_parser = parse_remote_branch)]
    base: Option<(String, String)>,

    /// Checkout the new branch after creation. This is the default.
    /// Pass `--no-checkout` to keep the current branch checked out.
    #[arg(
        long = "checkout",
        action = clap::ArgAction::SetTrue,
        conflicts_with = "no_checkout",
    )]
    #[allow(dead_code)] // default-on; only consumed for `conflicts_with`
    checkout: bool,

    /// Leave the current branch checked out and just create the
    /// new branch ref.
    #[arg(long = "no-checkout", action = clap::ArgAction::SetTrue)]
    no_checkout: bool,
}

impl From<StackNewCli> for StackNewOpts {
    fn from(cli: StackNewCli) -> Self {
        Self {
            name: cli.name,
            base: cli.base,
            // Default is checkout-on; `--no-checkout` is the only
            // way to flip it. `--checkout` is accepted for parity
            // with the Python click flag pair but is a no-op since
            // it matches the default.
            checkout: !cli.no_checkout,
        }
    }
}

/// `mergify stack edit [<commit>]` — clap definition for the
/// natively-ported `stack edit` subcommand. Same secondary-parse
/// pattern as `stack new` / `stack note`.
#[derive(Parser)]
#[command(name = "edit", about = "Edit the stack history")]
struct StackEditCli {
    /// Commit to pause the rebase on. Accepts a SHA prefix or a
    /// Change-Id prefix; omit for a fully interactive rebase.
    commit: Option<String>,
}

impl From<StackEditCli> for StackEditOpts {
    fn from(cli: StackEditCli) -> Self {
        Self {
            commit_prefix: cli.commit,
        }
    }
}

/// `mergify stack drop <COMMIT>... [--dry-run]` — clap definition
/// for the natively-ported `stack drop` subcommand.
#[derive(Parser)]
#[command(name = "drop", about = "Drop commits from the stack")]
struct StackDropCli {
    /// Commits to drop. Each accepts a SHA prefix or a Change-Id
    /// prefix.
    #[arg(required = true)]
    commits: Vec<String>,

    /// Show the plan without dropping.
    #[arg(short = 'n', long = "dry-run", action = clap::ArgAction::SetTrue)]
    dry_run: bool,
}

impl From<StackDropCli> for StackDropOpts {
    fn from(cli: StackDropCli) -> Self {
        Self {
            commit_prefixes: cli.commits,
            dry_run: cli.dry_run,
        }
    }
}

/// `mergify stack fixup <COMMIT>... [--dry-run]` — clap definition.
#[derive(Parser)]
#[command(
    name = "fixup",
    about = "Fixup commits into their parent (drops their messages)"
)]
struct StackFixupCli {
    #[arg(required = true)]
    commits: Vec<String>,

    /// Show the plan without rebasing.
    #[arg(short = 'n', long = "dry-run", action = clap::ArgAction::SetTrue)]
    dry_run: bool,
}

impl From<StackFixupCli> for StackFixupOpts {
    fn from(cli: StackFixupCli) -> Self {
        Self {
            commit_prefixes: cli.commits,
            dry_run: cli.dry_run,
        }
    }
}

/// `mergify stack reword <COMMIT> [-m <msg>] [--dry-run]`.
#[derive(Parser)]
#[command(name = "reword", about = "Change a commit's message")]
struct StackRewordCli {
    commit: String,

    /// New message. When omitted, `git rebase -i` pauses at the
    /// target and opens `$GIT_EDITOR`.
    #[arg(short = 'm', long = "message")]
    message: Option<String>,

    /// Show the plan without rebasing.
    #[arg(short = 'n', long = "dry-run", action = clap::ArgAction::SetTrue)]
    dry_run: bool,
}

impl From<StackRewordCli> for StackRewordOpts {
    fn from(cli: StackRewordCli) -> Self {
        Self {
            commit_prefix: cli.commit,
            message: cli.message,
            dry_run: cli.dry_run,
        }
    }
}

/// `mergify stack reorder <COMMIT>... [--dry-run]`.
#[derive(Parser)]
#[command(name = "reorder", about = "Reorder the stack's commits")]
struct StackReorderCli {
    #[arg(required = true)]
    commits: Vec<String>,

    /// Show the plan without reordering.
    #[arg(short = 'n', long = "dry-run", action = clap::ArgAction::SetTrue)]
    dry_run: bool,
}

impl From<StackReorderCli> for StackReorderOpts {
    fn from(cli: StackReorderCli) -> Self {
        Self {
            commit_prefixes: cli.commits,
            dry_run: cli.dry_run,
        }
    }
}

/// `mergify stack move <COMMIT> <POSITION> [<TARGET>] [--dry-run]`.
#[derive(Parser)]
#[command(name = "move", about = "Move a commit within the stack")]
struct StackMoveCli {
    /// Commit to move.
    commit: String,

    /// Where to put it: `first`, `last`, `before`, `after`.
    #[arg(value_enum)]
    position: StackMovePosition,

    /// Required when `position` is `before` or `after`.
    target: Option<String>,

    /// Show the plan without moving.
    #[arg(short = 'n', long = "dry-run", action = clap::ArgAction::SetTrue)]
    dry_run: bool,
}

impl From<StackMoveCli> for StackMoveOpts {
    fn from(cli: StackMoveCli) -> Self {
        Self {
            commit_prefix: cli.commit,
            position: cli.position,
            target_prefix: cli.target,
            dry_run: cli.dry_run,
        }
    }
}

/// `mergify stack squash <SRC>... into <TARGET> [-m <msg>]
/// [--dry-run]`. The `<SRC>... into <TARGET>` shape doesn't fit
/// clap's positional model directly, so we accept a flat
/// `Vec<String>` and split on the literal `into` keyword inside
/// [`StackSquashOpts::try_from`].
#[derive(Parser)]
#[command(name = "squash", about = "Squash commits into a target commit")]
struct StackSquashCli {
    /// `SRC1 SRC2 ... into TARGET` — must contain exactly one
    /// `into` token; everything before is a source, the single
    /// token after is the target.
    #[arg(required = true, num_args = 3..)]
    tokens: Vec<String>,

    /// Final commit message (required to rename; otherwise the
    /// target's message is kept).
    #[arg(short = 'm', long = "message")]
    message: Option<String>,

    /// Show the plan without rebasing.
    #[arg(short = 'n', long = "dry-run", action = clap::ArgAction::SetTrue)]
    dry_run: bool,
}

/// `mergify stack checkout <NAME>`.
#[derive(Parser)]
#[command(name = "checkout", about = "Checkout the pull requests stack")]
struct StackCheckoutCli {
    name: String,

    /// Author of the stack. Defaults to the token's user.
    #[arg(long)]
    author: Option<String>,

    /// `owner/repo`. Falls back to the URL of `--trunk`'s remote.
    #[arg(long = "repository", alias = "repo")]
    repository: Option<String>,

    /// Local branch name. Defaults to the normalised NAME.
    #[arg(long)]
    branch: Option<String>,

    /// Override the stack branch prefix.
    #[arg(long = "branch-prefix")]
    branch_prefix: Option<String>,

    /// Show the plan without checking out.
    #[arg(short = 'n', long = "dry-run", action = clap::ArgAction::SetTrue)]
    dry_run: bool,

    /// Target trunk as `REMOTE/BRANCH`. Defaults to the resolved
    /// trunk for the current branch.
    #[arg(short = 't', long = "trunk", value_parser = parse_remote_branch)]
    trunk: Option<(String, String)>,

    /// GitHub token (falls back to `MERGIFY_TOKEN` / `GITHUB_TOKEN`
    /// / `gh auth token`).
    #[arg(long)]
    token: Option<String>,
}

impl From<StackCheckoutCli> for StackCheckoutOpts {
    fn from(cli: StackCheckoutCli) -> Self {
        Self {
            name: cli.name,
            author: cli.author,
            repository: cli.repository,
            branch: cli.branch,
            branch_prefix: cli.branch_prefix,
            dry_run: cli.dry_run,
            trunk: cli.trunk,
            token: cli.token,
        }
    }
}

/// `mergify stack sync [--dry-run]`.
#[derive(Parser)]
#[command(
    name = "sync",
    about = "Sync the stack: fetch trunk, remove merged commits, rebase"
)]
struct StackSyncCli {
    #[arg(long)]
    author: Option<String>,

    #[arg(long = "repository", alias = "repo")]
    repository: Option<String>,

    #[arg(long = "branch-prefix")]
    branch_prefix: Option<String>,

    #[arg(short = 'n', long = "dry-run", action = clap::ArgAction::SetTrue)]
    dry_run: bool,

    #[arg(short = 't', long = "trunk", value_parser = parse_remote_branch)]
    trunk: Option<(String, String)>,

    #[arg(long)]
    token: Option<String>,
}

impl From<StackSyncCli> for StackSyncOpts {
    fn from(cli: StackSyncCli) -> Self {
        Self {
            author: cli.author,
            repository: cli.repository,
            branch_prefix: cli.branch_prefix,
            dry_run: cli.dry_run,
            trunk: cli.trunk,
            token: cli.token,
        }
    }
}

impl TryFrom<StackSquashCli> for StackSquashOpts {
    type Error = String;

    fn try_from(cli: StackSquashCli) -> Result<Self, Self::Error> {
        let into_positions: Vec<usize> = cli
            .tokens
            .iter()
            .enumerate()
            .filter_map(|(i, t)| (t == "into").then_some(i))
            .collect();
        if into_positions.len() != 1 {
            return Err(
                "squash requires exactly one 'into' keyword: SRC... into TARGET".to_string(),
            );
        }
        let idx = into_positions[0];
        let srcs: Vec<String> = cli.tokens[..idx].to_vec();
        let after = &cli.tokens[idx + 1..];
        if srcs.is_empty() {
            return Err("at least one source commit required before 'into'".to_string());
        }
        if after.len() != 1 {
            return Err("exactly one target commit required after 'into'".to_string());
        }
        Ok(Self {
            src_prefixes: srcs,
            target_prefix: after[0].clone(),
            message: cli.message,
            dry_run: cli.dry_run,
        })
    }
}

/// `mergify stack note [<commit>]` — clap definition for the
/// natively-ported `stack note` subcommand. Same secondary-parse
/// pattern as `stack new`.
#[derive(Parser)]
#[command(
    name = "note",
    about = "Attach a 'why was this commit amended' note to a commit"
)]
struct StackNoteCli {
    /// Target commit. Accepts a SHA prefix, a ref (`HEAD~1`,
    /// branch name, …), or a Change-Id prefix (resolved against
    /// the stack walk). Defaults to HEAD.
    commit: Option<String>,

    /// Note message. If omitted, opens `$GIT_EDITOR` /
    /// `$VISUAL` / `$EDITOR` / `vi` on a tempfile.
    #[arg(short = 'm', long = "message")]
    message: Option<String>,

    /// Append to an existing note instead of replacing.
    #[arg(long = "append", action = clap::ArgAction::SetTrue)]
    append: bool,

    /// Remove the note on the target commit. Mutually exclusive
    /// with `--message` and `--append`.
    #[arg(
        long = "remove",
        action = clap::ArgAction::SetTrue,
        conflicts_with_all = ["message", "append"],
    )]
    remove: bool,
}

impl From<StackNoteCli> for StackNoteOpts {
    fn from(cli: StackNoteCli) -> Self {
        Self {
            commit: cli.commit,
            message: cli.message,
            append: cli.append,
            remove: cli.remove,
        }
    }
}

/// Parse a `REMOTE/BRANCH` argument into its two parts. Matches
/// the Python `trunk_type` click callback in
/// `mergify_cli/stack/cli.py`: split on the first `/`, so branch
/// names containing `/` (e.g. `release/2026.06`) survive intact;
/// the error message is kept verbatim so users see the same text
/// regardless of which `stack` subcommand is parsing.
fn parse_remote_branch(value: &str) -> Result<(String, String), String> {
    value
        .split_once('/')
        .map(|(r, b)| (r.to_string(), b.to_string()))
        .ok_or_else(|| "Trunk is invalid. It must be origin/branch-name".to_string())
}

#[derive(clap::Args)]
struct InternalStackLocalCommitsArgs {
    /// Base revision — anything `git` accepts (typically a merge-
    /// base SHA). The range is exclusive of this commit.
    #[arg(long)]
    base: String,
    /// Head revision — typically the local stack branch name.
    /// The range is inclusive of this commit.
    #[arg(long)]
    head: String,
    /// Repository working tree to run `git` in. Defaults to the
    /// process CWD.
    #[arg(long = "repo-dir", value_name = "DIR")]
    repo_dir: Option<PathBuf>,
}

#[derive(clap::Args)]
struct InternalStackRemoteChangesArgs {
    /// GitHub API base URL (e.g. `https://api.github.com`).
    #[arg(long = "github-server")]
    github_server: url::Url,
    /// Bearer token. Optional — when omitted the binary falls
    /// back to `mergify_core::auth::resolve_token` (which reads
    /// `MERGIFY_TOKEN` / `GITHUB_TOKEN` / `gh auth token`). The
    /// Python caller should prefer setting `MERGIFY_TOKEN` in
    /// the subprocess env over passing `--token` so the value
    /// doesn't surface in `ps`/process listings.
    #[arg(long)]
    token: Option<String>,
    /// Repository owner.
    #[arg(long)]
    user: String,
    /// Repository name.
    #[arg(long)]
    repo: String,
    /// Stack branch prefix (e.g. `stack/main` — the search query
    /// becomes `head:<prefix>/`).
    #[arg(long = "stack-prefix")]
    stack_prefix: String,
    /// PR author to filter on. Limits the search to PRs the
    /// current user owns — `mergify stack` only manages its own.
    #[arg(long)]
    author: String,
}

#[derive(clap::Args)]
struct ConfigArgs {
    /// Path to the Mergify configuration file (auto-detected if not
    /// provided).
    #[arg(long = "config-file", short = 'f', global = true)]
    config_file: Option<PathBuf>,

    #[command(subcommand)]
    command: ConfigSubcommand,
}

#[derive(Subcommand)]
enum ConfigSubcommand {
    /// Validate the Mergify configuration file against the schema.
    Validate(ValidateArgs),
    /// Simulate Mergify actions on a pull request using the local
    /// configuration.
    Simulate(SimulateCliArgs),
}

#[derive(clap::Args)]
struct ValidateArgs {}

#[derive(clap::Args)]
struct SimulateCliArgs {
    /// Pull request URL (e.g. <https://github.com/owner/repo/pull/123>).
    #[arg(value_name = "PULL_REQUEST_URL", value_parser = mergify_config::simulate::parse_pr_url)]
    pull_request: PullRequestRef,

    /// Mergify or GitHub token. Falls back to ``MERGIFY_TOKEN`` and
    /// then ``GITHUB_TOKEN`` env vars.
    #[arg(long, short = 't')]
    token: Option<String>,

    /// Mergify API URL. Falls back to ``MERGIFY_API_URL`` env var,
    /// then to the default.
    #[arg(long = "api-url", short = 'u')]
    api_url: Option<String>,
}

#[derive(clap::Args)]
struct CiArgs {
    #[command(subcommand)]
    command: CiSubcommand,
}

#[derive(Subcommand)]
// CiSubcommand variant docstrings double as `mergify ci --help`
// entries — clap renders them verbatim, so backticks would surface
// as literal characters to the user. Suppress doc_markdown here
// so the help text reads naturally.
#[allow(clippy::doc_markdown)]
enum CiSubcommand {
    /// Send scopes tied to a pull request to Mergify.
    #[command(name = "scopes-send")]
    ScopesSend(ScopesSendCliArgs),
    /// Print the base/head git references for the current build.
    #[command(name = "git-refs")]
    GitRefs(GitRefsCliArgs),
    /// Print the merge queue batch metadata for the current draft PR.
    #[command(name = "queue-info")]
    QueueInfo,
    /// Give the list of scopes impacted by changed files.
    Scopes(ScopesCliArgs),
    /// Upload JUnit XML reports and ignore failed tests with
    /// Mergify's CI Insights Quarantine.
    #[command(name = "junit-process")]
    JunitProcess(JunitProcessCliArgs),
    /// Upload JUnit XML reports (deprecated: use `junit-process`).
    #[command(name = "junit-upload")]
    JunitUpload(JunitProcessCliArgs),
}

#[derive(clap::Args)]
struct GitRefsCliArgs {
    /// Output format: `text` (default), `shell` for eval-friendly
    /// `MERGIFY_GIT_REFS_*` lines, or `json` for a single JSON
    /// object.
    #[arg(
        long = "format",
        default_value = "text",
        value_parser = mergify_ci::git_refs::Format::parse,
    )]
    format: GitRefsFormat,
}

#[derive(clap::Args)]
struct ScopesCliArgs {
    /// Path to YAML config file. Falls back to the
    /// `MERGIFY_CONFIG_PATH` env var, then auto-detects
    /// `.mergify.yml`, `.mergify/config.yml`, or
    /// `.github/mergify.yml`.
    //
    // The env var lookup is intentionally *not* delegated to
    // clap's `env = ...` attribute: callers (notably the
    // `gha-mergify-ci` action) set `MERGIFY_CONFIG_PATH=""` to
    // mean "auto-detect", and clap's `env` parser treats an
    // empty env value as a present-but-empty flag value, which
    // fails the parse with `a value is required for '--config'`.
    // The env var is consulted inside
    // `mergify_ci::scopes_detect::resolve_config_path` instead,
    // where empty correctly falls through to auto-detect. The
    // matching regression tests are
    // `ci_scopes_parses_when_mergify_config_path_env_var_is_empty`
    // (clap parse) and
    // `resolve_config_path_treats_empty_env_var_as_unset`
    // (lower-level resolver).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Base git reference to use to look for changed files.
    #[arg(long)]
    base: Option<String>,

    /// Head git reference to use to look for changed files.
    #[arg(long)]
    head: Option<String>,

    /// Write the detected scopes to a file (JSON, consumed by
    /// `ci scopes-send --scopes-json`).
    #[arg(long = "write", short = 'w')]
    write: Option<PathBuf>,
}

#[derive(clap::Args)]
struct ScopesSendCliArgs {
    /// Repository full name (owner/repo). Detected from the CI
    /// environment or the local git remote when omitted.
    #[arg(long, short = 'r')]
    repository: Option<String>,

    /// Pull request number. Falls back to ``GITHUB_EVENT_PATH``
    /// (reads ``.pull_request.number``). When neither is available
    /// the command prints a skip message and exits 0.
    #[arg(long = "pull-request", short = 'p')]
    pull_request: Option<u64>,

    /// Mergify or GitHub token. Falls back to ``MERGIFY_TOKEN`` and
    /// then ``GITHUB_TOKEN`` env vars.
    #[arg(long, short = 't')]
    token: Option<String>,

    /// Mergify API URL. Falls back to ``MERGIFY_API_URL`` env var,
    /// then to the default.
    #[arg(long = "api-url", short = 'u')]
    api_url: Option<String>,

    /// Scope to upload (repeatable).
    #[arg(long = "scope", short = 's')]
    scope: Vec<String>,

    /// JSON file containing scopes to upload (output of
    /// ``mergify ci scopes --write``).
    #[arg(long = "scopes-json")]
    scopes_json: Option<PathBuf>,

    /// Plain-text file with one scope per line.
    #[arg(long = "scopes-file")]
    scopes_file: Option<PathBuf>,

    /// Deprecated alias for ``--scopes-json``.
    #[arg(long = "file", short = 'f', hide = true)]
    file_deprecated: Option<PathBuf>,
}

#[derive(clap::Args)]
// Help text is rendered verbatim by clap; backticks would surface
// as literal characters to the user. Suppress the doc_markdown
// lint just for this struct so the docstrings read naturally in
// `--help` output.
#[allow(clippy::doc_markdown)]
struct JunitProcessCliArgs {
    /// Mergify API URL. Falls back to ``MERGIFY_API_URL`` env var,
    /// then to the default (`https://api.mergify.com`).
    #[arg(long = "api-url", short = 'u')]
    api_url: Option<String>,

    /// CI Issues application key. Falls back to ``MERGIFY_TOKEN``.
    #[arg(long, short = 't')]
    token: Option<String>,

    /// Repository full name (owner/repo). Detected from the CI
    /// environment or the local git remote when omitted.
    #[arg(long, short = 'r')]
    repository: Option<String>,

    /// Test framework label (e.g. `pytest`). Optional; passed as a
    /// span attribute.
    #[arg(long = "test-framework")]
    test_framework: Option<String>,

    /// Test language label (e.g. `python`). Optional; passed as a
    /// span attribute.
    #[arg(long = "test-language")]
    test_language: Option<String>,

    /// Branch the quarantine API should look up tests on. Defaults
    /// to the PR base branch, or the head branch as a fallback.
    #[arg(long = "tests-target-branch", short = 'b')]
    tests_target_branch: Option<String>,

    /// Exit code of the test runner. When this is non-zero but no
    /// failures appear in the JUnit report, the run is flagged
    /// as a silent failure. Falls back to ``MERGIFY_TEST_EXIT_CODE``.
    //
    // The env-var fallback is intentionally NOT delegated to
    // clap's `env = ...` attribute: callers (notably the
    // `gha-mergify-ci` action) set `MERGIFY_TEST_EXIT_CODE=""`
    // when no exit code is available, meaning "no value". Clap
    // would try to parse the empty string as `i32` and exit
    // parsing with `invalid value '' for '--test-exit-code':
    // cannot parse integer from empty string`. The env var is
    // consulted inside `junit_process::command::resolve_test_exit_code`
    // instead, where empty correctly maps to `None`. Same trap
    // hit the `--config` flag — see `ScopesCliArgs::config`.
    #[arg(long = "test-exit-code", short = 'e')]
    test_exit_code: Option<i32>,

    /// JUnit XML files or glob patterns (e.g.
    /// `reports/**/*.xml`). At least one path or pattern is required.
    #[arg(value_name = "FILE", required = true, num_args = 1..)]
    files: Vec<String>,
}

#[derive(clap::Args)]
struct TestsArgs {
    #[command(subcommand)]
    command: TestsSubcommand,
}

#[derive(Subcommand)]
enum TestsSubcommand {
    /// Look up tests by name and print their health and metrics.
    Show(TestsShowCliArgs),
    /// Add a test to the CI Insights quarantine.
    Quarantine(TestsQuarantineCliArgs),
    /// Remove a test from the CI Insights quarantine.
    Unquarantine(TestsUnquarantineCliArgs),
    /// List the tests currently in the CI Insights quarantine.
    Quarantined(TestsQuarantinedCliArgs),
}

#[derive(clap::Args)]
struct TestsShowCliArgs {
    /// Test name(s) to look up. Glob patterns (`*`, `?`) are
    /// supported by the API.
    #[arg(value_name = "NAME", required = true, num_args = 1..)]
    test_names: Vec<String>,

    /// Repository full name (owner/repo). Detected from the CI
    /// environment or the local git remote when omitted.
    #[arg(
        long,
        short = 'r',
        value_parser = mergify_ci::detector::parse_owner_repo,
    )]
    repository: Option<String>,

    /// Mergify or GitHub token. Falls back to ``MERGIFY_TOKEN`` and
    /// then ``GITHUB_TOKEN`` env vars.
    #[arg(long, short = 't')]
    token: Option<String>,

    /// Mergify API URL. Falls back to ``MERGIFY_API_URL`` env var,
    /// then to the default.
    #[arg(long = "api-url", short = 'u')]
    api_url: Option<String>,

    /// Restrict matches to the given pipeline name(s).
    #[arg(long = "pipeline-name")]
    pipeline_name: Vec<String>,

    /// Exclude matches from the given pipeline name(s).
    #[arg(long = "pipeline-name-exclude")]
    pipeline_name_exclude: Vec<String>,

    /// Restrict matches to the given job name(s).
    #[arg(long = "job-name")]
    job_name: Vec<String>,

    /// Exclude matches from the given job name(s).
    #[arg(long = "job-name-exclude")]
    job_name_exclude: Vec<String>,

    /// Maximum number of identities the search endpoint may return
    /// per page (1–100, server default is 10).
    #[arg(long = "per-page", value_parser = clap::value_parser!(u32).range(1..=100))]
    per_page: Option<u32>,

    /// Emit a single JSON document to stdout instead of human prose.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct TestsQuarantineCliArgs {
    /// Fully qualified name of the test to quarantine.
    #[arg(value_name = "NAME")]
    test_name: String,

    /// Repository full name (owner/repo). Detected from the CI
    /// environment or the local git remote when omitted.
    #[arg(
        long,
        short = 'r',
        value_parser = mergify_ci::detector::parse_owner_repo,
    )]
    repository: Option<String>,

    /// Reason recorded for quarantining the test.
    #[arg(long)]
    reason: String,

    /// Branch name or pattern to scope the quarantine to. Omit to
    /// quarantine on all branches.
    #[arg(long, short = 'b')]
    branch: Option<String>,

    /// Mergify or GitHub token. Falls back to ``MERGIFY_TOKEN`` and
    /// then ``GITHUB_TOKEN`` env vars.
    #[arg(long, short = 't')]
    token: Option<String>,

    /// Mergify API URL. Falls back to ``MERGIFY_API_URL`` env var,
    /// then to the default.
    #[arg(long = "api-url", short = 'u')]
    api_url: Option<String>,

    /// Emit a single JSON document to stdout instead of human prose.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct TestsUnquarantineCliArgs {
    /// Test to remove from quarantine: either its fully qualified name
    /// or the quarantine id (as printed by `tests quarantine`).
    #[arg(value_name = "NAME_OR_ID")]
    name_or_id: String,

    /// Repository full name (owner/repo). Detected from the CI
    /// environment or the local git remote when omitted.
    #[arg(
        long,
        short = 'r',
        value_parser = mergify_ci::detector::parse_owner_repo,
    )]
    repository: Option<String>,

    /// Mergify or GitHub token. Falls back to ``MERGIFY_TOKEN`` and
    /// then ``GITHUB_TOKEN`` env vars.
    #[arg(long, short = 't')]
    token: Option<String>,

    /// Mergify API URL. Falls back to ``MERGIFY_API_URL`` env var,
    /// then to the default.
    #[arg(long = "api-url", short = 'u')]
    api_url: Option<String>,

    /// Emit a single JSON document to stdout instead of human prose.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct TestsQuarantinedCliArgs {
    /// Repository full name (owner/repo). Detected from the CI
    /// environment or the local git remote when omitted.
    #[arg(
        long,
        short = 'r',
        value_parser = mergify_ci::detector::parse_owner_repo,
    )]
    repository: Option<String>,

    /// Mergify or GitHub token. Falls back to ``MERGIFY_TOKEN`` and
    /// then ``GITHUB_TOKEN`` env vars.
    #[arg(long, short = 't')]
    token: Option<String>,

    /// Mergify API URL. Falls back to ``MERGIFY_API_URL`` env var,
    /// then to the default.
    #[arg(long = "api-url", short = 'u')]
    api_url: Option<String>,

    /// Emit a single JSON document to stdout instead of human prose.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct QueueArgs {
    /// Mergify or GitHub token. Falls back to ``MERGIFY_TOKEN`` and
    /// then ``GITHUB_TOKEN`` env vars.
    #[arg(long, short = 't', global = true)]
    token: Option<String>,

    /// Mergify API URL. Falls back to ``MERGIFY_API_URL`` env var,
    /// then to the default.
    #[arg(long = "api-url", short = 'u', global = true)]
    api_url: Option<String>,

    /// Repository full name (owner/repo). Falls back to
    /// ``GITHUB_REPOSITORY`` env var.
    #[arg(long, short = 'r', global = true)]
    repository: Option<String>,

    #[command(subcommand)]
    command: QueueSubcommand,
}

#[derive(Subcommand)]
enum QueueSubcommand {
    /// Pause the merge queue for the repository.
    Pause(PauseCliArgs),
    /// Unpause the merge queue for the repository.
    Unpause,
    /// Show merge queue status for the repository.
    Status(StatusCliArgs),
    /// Show detailed state of a pull request in the merge queue.
    Show(ShowCliArgs),
}

#[derive(clap::Args)]
struct PauseCliArgs {
    /// Reason for pausing the queue (max 255 characters).
    #[arg(long, value_parser = mergify_queue::pause::parse_reason)]
    reason: String,

    /// Skip the confirmation prompt. Required in non-interactive
    /// sessions.
    #[arg(long = "yes-i-am-sure", default_value_t = false)]
    yes_i_am_sure: bool,
}

#[derive(clap::Args)]
struct StatusCliArgs {
    /// Filter the queue by branch name.
    #[arg(long, short = 'b')]
    branch: Option<String>,

    /// Emit the raw API response as a single JSON document.
    #[arg(long, default_value_t = false)]
    json: bool,
}

#[derive(clap::Args)]
struct ShowCliArgs {
    /// Pull request number to inspect.
    #[arg(value_name = "PR_NUMBER")]
    pr_number: u64,

    /// Show the full checks table and the conditions tree instead
    /// of compact summaries.
    #[arg(long, short = 'v', default_value_t = false)]
    verbose: bool,

    /// Emit the raw API response as a single JSON document.
    #[arg(long, default_value_t = false)]
    json: bool,
}

#[derive(clap::Args)]
struct FreezeArgs {
    /// Mergify or GitHub token. Falls back to ``MERGIFY_TOKEN`` and
    /// then ``GITHUB_TOKEN`` env vars.
    #[arg(long, short = 't', global = true)]
    token: Option<String>,

    /// Mergify API URL. Falls back to ``MERGIFY_API_URL`` env var,
    /// then to the default.
    #[arg(long = "api-url", short = 'u', global = true)]
    api_url: Option<String>,

    /// Repository full name (owner/repo). Falls back to
    /// ``GITHUB_REPOSITORY`` env var.
    #[arg(long, short = 'r', global = true)]
    repository: Option<String>,

    #[command(subcommand)]
    command: FreezeSubcommand,
}

#[derive(Subcommand)]
enum FreezeSubcommand {
    /// List scheduled freezes for a repository.
    List(FreezeListCliArgs),
    /// Create a new scheduled freeze.
    Create(FreezeCreateCliArgs),
    /// Update an existing scheduled freeze.
    Update(FreezeUpdateCliArgs),
    /// Delete a scheduled freeze.
    Delete(FreezeDeleteCliArgs),
}

#[derive(clap::Args)]
struct FreezeListCliArgs {
    /// Emit the raw `scheduled_freezes` array as a single JSON
    /// document.
    #[arg(long, default_value_t = false)]
    json: bool,
}

#[derive(clap::Args)]
struct FreezeCreateCliArgs {
    /// Reason for the freeze.
    #[arg(long, required = true)]
    reason: String,

    /// IANA timezone name (e.g. ``Europe/Paris``, ``US/Eastern``).
    /// Defaults to the system timezone when omitted.
    #[arg(long)]
    timezone: Option<String>,

    /// Matching condition (repeatable, e.g. `-c base=main`).
    #[arg(long = "condition", short = 'c')]
    condition: Vec<String>,

    /// Start time in ISO 8601 format (default: now).
    #[arg(long, value_parser = parse_naive_datetime_arg)]
    start: Option<chrono::NaiveDateTime>,

    /// End time in ISO 8601 format (default: no end / emergency freeze).
    #[arg(long, value_parser = parse_naive_datetime_arg)]
    end: Option<chrono::NaiveDateTime>,

    /// Exclude condition (repeatable, e.g. `-e label=hotfix`).
    #[arg(long = "exclude", short = 'e')]
    exclude: Vec<String>,
}

#[derive(clap::Args)]
struct FreezeUpdateCliArgs {
    /// Freeze ID (UUID).
    #[arg(value_name = "FREEZE_ID")]
    freeze_id: String,

    /// Reason for the freeze.
    #[arg(long)]
    reason: Option<String>,

    /// IANA timezone name.
    #[arg(long)]
    timezone: Option<String>,

    /// Matching condition (repeatable, e.g. `-c base=main`).
    /// Passing the flag one or more times replaces the existing
    /// list with the values supplied. Omitting `-c` entirely
    /// leaves the stored list untouched.
    #[arg(long = "condition", short = 'c')]
    condition: Vec<String>,

    /// Start time in ISO 8601 format.
    #[arg(long, value_parser = parse_naive_datetime_arg)]
    start: Option<chrono::NaiveDateTime>,

    /// End time in ISO 8601 format.
    #[arg(long, value_parser = parse_naive_datetime_arg)]
    end: Option<chrono::NaiveDateTime>,

    /// Exclude condition (repeatable).
    #[arg(long = "exclude", short = 'e')]
    exclude: Vec<String>,
}

#[derive(clap::Args)]
struct FreezeDeleteCliArgs {
    /// Freeze ID (UUID).
    #[arg(value_name = "FREEZE_ID")]
    freeze_id: String,

    /// Reason for deleting the freeze (required if the freeze is
    /// currently active).
    #[arg(long = "reason")]
    delete_reason: Option<String>,
}

/// clap `value_parser` shim for `--start` / `--end`. Delegates to
/// [`parse_naive_datetime`] and converts the typed `CliError` into a
/// stringified parser error so clap can render it as a normal
/// argument error.
fn parse_naive_datetime_arg(value: &str) -> Result<chrono::NaiveDateTime, String> {
    parse_naive_datetime(value).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> CliRoot {
        CliRoot::try_parse_from(
            std::iter::once("mergify".to_string()).chain(argv.iter().map(|s| (*s).to_string())),
        )
        .expect("argv parses")
    }

    #[test]
    fn ci_scopes_parses_when_mergify_config_path_env_var_is_empty() {
        // Regression for monorepo#33423 / gha-mergify-ci:
        // the action sets `MERGIFY_CONFIG_PATH=""` (empty) when
        // the caller didn't pin a config path, expecting
        // auto-detect. The previous `ScopesCliArgs::config`
        // declaration used `env = "MERGIFY_CONFIG_PATH"` on
        // clap's side, which interpreted the empty env value as
        // a present-but-empty `--config` flag and exited parsing
        // with `a value is required for '--config'`. The clap
        // env hook has been dropped — env lookup lives inside
        // `scopes_detect::resolve_config_path` where empty is
        // correctly treated as unset. Pin that here so the hook
        // can't sneak back in.
        let parsed = temp_env::with_var("MERGIFY_CONFIG_PATH", Some(""), || {
            CliRoot::try_parse_from([
                "mergify".to_string(),
                "ci".to_string(),
                "scopes".to_string(),
                "--write".to_string(),
                "scopes.json".to_string(),
            ])
            .expect("argv parses with empty MERGIFY_CONFIG_PATH")
        });
        let Dispatch::Native(NativeCommand::CiScopes(opts)) = dispatch_from_parsed(parsed) else {
            panic!("ci scopes must dispatch natively");
        };
        // `--config` was never supplied; the empty env var must
        // not surface as a value (which would change the
        // downstream resolver's branch).
        assert!(opts.config.is_none(), "got: {:?}", opts.config);
    }

    #[test]
    fn ci_junit_process_parses_when_mergify_test_exit_code_env_var_is_empty() {
        // Second instance of the same class of regression as
        // `ci_scopes_parses_when_…`: `gha-mergify-ci` exports
        // `MERGIFY_TEST_EXIT_CODE=""` when the previous step
        // didn't produce a runner exit code. Previously the clap
        // `env = "MERGIFY_TEST_EXIT_CODE"` attribute on
        // `--test-exit-code` tried to parse `""` as `i32` and
        // exited parsing with `invalid value '' for
        // '--test-exit-code': cannot parse integer from empty
        // string`. The clap env hook has been dropped — env
        // lookup lives in `junit_process::command::resolve_test_exit_code`
        // where empty is correctly treated as `None`. Pin that
        // here so the hook can't sneak back in.
        let parsed = temp_env::with_var("MERGIFY_TEST_EXIT_CODE", Some(""), || {
            CliRoot::try_parse_from([
                "mergify".to_string(),
                "ci".to_string(),
                "junit-process".to_string(),
                "report.xml".to_string(),
            ])
            .expect("argv parses with empty MERGIFY_TEST_EXIT_CODE")
        });
        let Dispatch::Native(NativeCommand::CiJunitProcess(opts)) = dispatch_from_parsed(parsed)
        else {
            panic!("ci junit-process must dispatch natively");
        };
        // `--test-exit-code` was never supplied; the empty env
        // var must not surface as a value.
        assert!(
            opts.test_exit_code.is_none(),
            "got: {:?}",
            opts.test_exit_code,
        );
    }

    #[test]
    fn root_debug_flag_accepted_before_native_command() {
        // Without this, clap would reject `--debug` and exit before
        // any dispatch — the regression we just fixed.
        let parsed = parse(&["--debug", "ci", "git-refs"]);
        assert!(parsed.debug, "--debug should be parsed as true");
        assert!(matches!(
            parsed.command,
            Subcommands::Ci(CiArgs {
                command: CiSubcommand::GitRefs(_)
            })
        ));
    }

    #[test]
    fn root_debug_flag_accepted_after_native_group() {
        // `--debug` is declared `global = true`, so it's recognised
        // at any point along the subcommand chain. clap's
        // hand-off prefers root, but users sometimes type it after
        // the group name — both must work.
        let parsed = parse(&["queue", "--debug", "status"]);
        assert!(parsed.debug);
    }

    #[test]
    fn shimmed_dispatch_reinjects_debug_at_argv_head() {
        // Clap consumes the root `--debug`; without re-injection,
        // the Python side (which declares its own root `--debug`)
        // would never see the flag.
        let parsed = parse(&["--debug", "stack", "push"]);
        let Dispatch::Shim(argv) = dispatch_from_parsed(parsed) else {
            panic!("stack must dispatch to the Python shim");
        };
        assert_eq!(argv, vec!["--debug", "stack", "push"]);
    }

    #[test]
    fn shimmed_dispatch_omits_debug_when_not_set() {
        let parsed = parse(&["stack", "push"]);
        let Dispatch::Shim(argv) = dispatch_from_parsed(parsed) else {
            panic!("stack must dispatch to the Python shim");
        };
        // No `--debug` prefix when the user didn't pass one — we
        // don't want to silently flip Python into verbose mode.
        assert_eq!(argv, vec!["stack", "push"]);
    }

    #[test]
    fn ci_junit_upload_dispatches_natively_via_deprecated_alias() {
        // `ci junit-upload` is the deprecated alias for
        // `junit-process`. Both must dispatch to the native
        // orchestrator; the alias gets its own
        // `NativeCommand::CiJunitUpload` variant so `run_native`
        // can print the deprecation warning before forwarding.
        let parsed = parse(&[
            "ci",
            "junit-upload",
            "-r",
            "owner/repo",
            "-t",
            "tok",
            "-b",
            "main",
            "report.xml",
        ]);
        let Dispatch::Native(NativeCommand::CiJunitUpload(opts)) = dispatch_from_parsed(parsed)
        else {
            panic!("ci junit-upload must dispatch to the native CiJunitUpload variant");
        };
        assert_eq!(opts.repository.as_deref(), Some("owner/repo"));
        assert_eq!(opts.token.as_deref(), Some("tok"));
        assert_eq!(opts.tests_target_branch.as_deref(), Some("main"));
        assert_eq!(opts.files, vec!["report.xml"]);
    }
}
