//! `mergify` binary entry point.
//!
//! Every command is native — clap parses the full flag set and the
//! binary runs the matching [`NativeCommand`] in process. Any
//! invocation clap can't parse (unknown subcommand, missing
//! required flag, …) exits with clap's formatted error and exit
//! code 2; for unknown subcommands that includes a "did you mean
//! `<closest>`?" suggestion off clap's built-in Levenshtein
//! distance.

use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::CommandFactory;
use clap::Parser;
use clap::Subcommand;
use mergify_ci::git_refs::Format as GitRefsFormat;
use mergify_ci::git_refs::GitRefsOptions;
use mergify_ci::junit_process::JunitProcessOptions;
use mergify_ci::scopes_send::ScopesSendOptions;
use mergify_ci::tests_quarantine::GetOptions;
use mergify_ci::tests_quarantine::QuarantineOptions;
use mergify_ci::tests_quarantine::QuarantinedOptions;
use mergify_ci::tests_quarantine::UnquarantineOptions;
use mergify_ci::tests_show::TestsShowOptions;
use mergify_config::simulate::SimulateOptions;
use mergify_core::OutputMode;
use mergify_core::StdioOutput;
use mergify_core::pull_request::PullRequestRef;
use mergify_freeze::common::parse_naive_datetime;
use mergify_freeze::create::CreateOptions as FreezeCreateOptions;
use mergify_freeze::delete::DeleteOptions as FreezeDeleteOptions;
use mergify_freeze::list::ListOptions as FreezeListOptions;
use mergify_freeze::update::UpdateOptions as FreezeUpdateOptions;
use mergify_queue::pause::PauseOptions;
use mergify_queue::show::ShowOptions;
use mergify_queue::status::StatusOptions;
use mergify_queue::unpause::UnpauseOptions;

mod cli_schema;
mod self_update;

/// User-visible CLI version. `build.rs` normalises the
/// `MERGIFY_RELEASE_VERSION` env var the release workflow sets
/// from `$GITHUB_REF` (unset or empty => `CARGO_PKG_VERSION`,
/// i.e. the `0.0.0` placeholder), and writes the result to the
/// `MERGIFY_CLI_VERSION` rustc-env. Reading the normalised var
/// directly means this side can't surface an empty string.
const VERSION: &str = env!("MERGIFY_CLI_VERSION");

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
        Dispatch::Native(cmd) => run_native(cmd),
    }
}

/// Result of `detect_dispatch`. Kept as a single-variant enum so the
/// match in `main` is exhaustive — every dispatch path lands on a
/// native command, but pattern-matching makes that explicit rather
/// than implicit.
enum Dispatch {
    Native(NativeCommand),
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
    ("tests", "quarantines"),
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
    ("stack", "hooks"),
    ("stack", "list"),
    ("stack", "move"),
    ("stack", "new"),
    ("stack", "note"),
    ("stack", "open"),
    ("stack", "push"),
    ("stack", "reorder"),
    ("stack", "reword"),
    ("stack", "setup"),
    ("stack", "squash"),
    ("stack", "sync"),
    // Internal helpers. Stay hidden from `--help` (see the
    // `Subcommands::Internal` variant) but still need to be
    // dispatchable.
    ("_internal", "stack-local-commits"),
    ("_internal", "stack-remote-changes"),
    // Self-invocation target for the rebase-todo machinery — set
    // as `GIT_SEQUENCE_EDITOR` before `git rebase -i` so we can
    // rewrite the todo file in-process. Not a user-facing
    // command; not stable.
    ("_internal", "rebase-todo-rewrite"),
    // Emits the machine-readable CLI schema the docs site renders
    // into the command reference. Hidden; not a stable surface.
    ("_internal", "dump-cli-schema"),
    // Renders the roff man page to stdout, for packaging. Hidden.
    ("_internal", "man"),
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
    TestsQuarantineGet(TestsQuarantineGetOpts),
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
    /// tracking the resolved trunk.
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
    /// `mergify stack push [flags...]` — upsert the stack's PRs
    /// on GitHub. Full orchestrator: walks local commits, plans
    /// actions, optionally rebases on trunk, pushes branches,
    /// upserts PRs + comments, and tears down orphans.
    StackPush(StackPushOpts),
    /// `mergify stack list [--json] [--verbose]` — show each
    /// commit in the current stack with its PR + CI + review
    /// state.
    StackList(StackListOpts),
    /// `mergify stack open [<commit>]` — open the PR for a stack
    /// commit in the default browser.
    StackOpen(StackOpenOpts),
    /// `mergify stack hooks [--setup] [--force] [-f]` — show the
    /// status of the managed git hooks (or, with `--setup`,
    /// install/upgrade them).
    StackHooks(StackHooksOpts),
    /// `mergify stack setup [--force] [--check]` — alias for
    /// `stack hooks --setup`; `--check` reports status instead.
    StackSetup(StackSetupOpts),
    /// `_internal rebase-todo-rewrite --action <ACTION>
    /// --sha <SHA> <TODO_PATH>` — self-invocation target set as
    /// `GIT_SEQUENCE_EDITOR` by the rebase-family stack
    /// subcommands. Reads the rebase-todo at `TODO_PATH`,
    /// applies the named transformation, writes it back in place.
    /// Wire format is not stable.
    InternalRebaseTodoRewrite(InternalRebaseTodoRewriteOpts),
    /// `_internal dump-cli-schema` — serialize the clap command tree
    /// to JSON for the docs site. Pure introspection; no async, no I/O
    /// beyond stdout.
    InternalDumpCliSchema,
    /// `mergify completions <shell>` — print a shell completion script
    /// to stdout. Pure introspection.
    Completions(clap_complete::Shell),
    /// `_internal man` — render the roff man page to stdout for
    /// packaging. Pure introspection.
    InternalManPage,
    /// `mergify self-update [--force] [--check]` — replace the
    /// running binary with the latest release.
    SelfUpdate(self_update::Options),
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the Python CLI's flag surface 1:1"
)]
struct StackPushOpts {
    author: Option<String>,
    repository: Option<String>,
    branch_prefix: Option<String>,
    trunk: Option<(String, String)>,
    token: Option<String>,
    skip_rebase: bool,
    force_rebase: bool,
    next_only: bool,
    dry_run: bool,
    /// `None` = fall back to git config
    /// `mergify-cli.stack-create-as-draft` at dispatch time.
    create_as_draft: Option<bool>,
    /// `None` = fall back to git config
    /// `mergify-cli.stack-keep-pr-title-body` at dispatch time.
    keep_pull_request_title_and_body: Option<bool>,
    only_update_existing_pulls: bool,
    /// `None` = fall back to git config
    /// `mergify-cli.stack-revision-history` at dispatch time.
    revision_history: Option<bool>,
    no_verify: bool,
}

struct StackListOpts {
    author: Option<String>,
    repository: Option<String>,
    branch_prefix: Option<String>,
    trunk: Option<(String, String)>,
    token: Option<String>,
    json: bool,
    verbose: bool,
}

struct StackOpenOpts {
    commit: Option<String>,
    author: Option<String>,
    repository: Option<String>,
    branch_prefix: Option<String>,
    trunk: Option<(String, String)>,
    token: Option<String>,
}

struct StackHooksOpts {
    do_setup: bool,
    force: bool,
}

struct StackSetupOpts {
    force: bool,
    check: bool,
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

struct TestsQuarantineGetOpts {
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

/// Parse `argv` with clap and return the resolved native command.
///
/// Any clap parse failure — unknown subcommand, missing required
/// argument, bad flag value — prints clap's formatted error
/// (including the built-in "did you mean `<closest>`?" suggestion
/// for unknown subcommands) and exits with clap's status code.
/// `--help` / `--version` also flow through `err.exit()` which
/// prints to stdout and exits 0.
#[allow(clippy::too_many_lines)] // mostly mechanical match arms
fn detect_dispatch(argv: &[String]) -> Dispatch {
    let parsed = match CliRoot::try_parse_from(
        std::iter::once("mergify".to_string()).chain(argv.iter().cloned()),
    ) {
        Ok(parsed) => parsed,
        Err(err) => err.exit(),
    };
    // Resolve the color preference once, before any command builds a
    // theme via `Theme::detect`.
    mergify_tui::set_color_choice(parsed.color.into());
    init_tracing(parsed.verbose, parsed.debug);
    dispatch_from_parsed(parsed)
}

/// Install the tracing subscriber, writing structured logs to stderr
/// so stdout stays pipeable. The level comes from `-v` (info / debug /
/// trace), with `--debug` flooring at debug; an explicit `RUST_LOG`
/// overrides both. Only our own crates are raised — third-party deps
/// stay at `warn` so `-vv` doesn't drown in hyper/reqwest noise.
fn init_tracing(verbose: u8, debug: bool) {
    use tracing_subscriber::EnvFilter;

    let level = match verbose {
        0 if debug => "debug",
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let directives = format!(
        "warn,mergify_cli={level},mergify_core={level},mergify_stack={level},\
         mergify_ci={level},mergify_queue={level},mergify_freeze={level},\
         mergify_config={level},mergify_tui={level}"
    );
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(directives));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init();
}

/// Build the run options for `quarantines add`.
fn quarantine_opts(args: TestsQuarantineCliArgs) -> TestsQuarantineOpts {
    TestsQuarantineOpts {
        repository: args.repository,
        test_name: args.test_name,
        reason: args.reason,
        branch: args.branch,
        token: args.token,
        api_url: args.api_url,
        json: args.json,
    }
}

/// Build the run options for `quarantines remove`.
fn unquarantine_opts(args: TestsUnquarantineCliArgs) -> TestsUnquarantineOpts {
    TestsUnquarantineOpts {
        repository: args.repository,
        name_or_id: args.name_or_id,
        token: args.token,
        api_url: args.api_url,
        json: args.json,
    }
}

/// Build the run options for `quarantines list`.
fn quarantined_opts(args: TestsQuarantinedCliArgs) -> TestsQuarantinedOpts {
    TestsQuarantinedOpts {
        repository: args.repository,
        token: args.token,
        api_url: args.api_url,
        json: args.json,
    }
}

/// Build the run options for `quarantines get`.
fn quarantine_get_opts(args: TestsQuarantineGetCliArgs) -> TestsQuarantineGetOpts {
    TestsQuarantineGetOpts {
        repository: args.repository,
        name_or_id: args.name_or_id,
        token: args.token,
        api_url: args.api_url,
        json: args.json,
    }
}

#[allow(clippy::too_many_lines)] // mostly mechanical match arms
fn dispatch_from_parsed(parsed: CliRoot) -> Dispatch {
    let _ = parsed.debug; // already consumed by init_tracing(); ignored during dispatch
    match parsed.command {
        Subcommands::Stack(StackArgs { command }) => match command {
            StackSubcommand::New(cli) => Dispatch::Native(NativeCommand::StackNew(cli.into())),
            StackSubcommand::Note(cli) => Dispatch::Native(NativeCommand::StackNote(cli.into())),
            StackSubcommand::Edit(cli) => Dispatch::Native(NativeCommand::StackEdit(cli.into())),
            StackSubcommand::Drop(cli) => Dispatch::Native(NativeCommand::StackDrop(cli.into())),
            StackSubcommand::Fixup(cli) => Dispatch::Native(NativeCommand::StackFixup(cli.into())),
            StackSubcommand::Reword(cli) => {
                Dispatch::Native(NativeCommand::StackReword(cli.into()))
            }
            StackSubcommand::Reorder(cli) => {
                Dispatch::Native(NativeCommand::StackReorder(cli.into()))
            }
            StackSubcommand::Move(cli) => Dispatch::Native(NativeCommand::StackMove(cli.into())),
            StackSubcommand::Squash(cli) => match StackSquashOpts::try_from(cli) {
                Ok(opts) => Dispatch::Native(NativeCommand::StackSquash(opts)),
                Err(msg) => CliRoot::command()
                    .error(clap::error::ErrorKind::ValueValidation, msg)
                    .exit(),
            },
            StackSubcommand::Checkout(cli) => {
                Dispatch::Native(NativeCommand::StackCheckout(cli.into()))
            }
            StackSubcommand::Sync(cli) => Dispatch::Native(NativeCommand::StackSync(cli.into())),
            StackSubcommand::Push(cli) => Dispatch::Native(NativeCommand::StackPush(cli.into())),
            StackSubcommand::List(cli) => {
                Dispatch::Native(NativeCommand::StackList(StackListOpts {
                    verbose: parsed.verbose > 0,
                    ..cli.into()
                }))
            }
            StackSubcommand::Open(cli) => Dispatch::Native(NativeCommand::StackOpen(cli.into())),
            StackSubcommand::Hooks(cli) => Dispatch::Native(NativeCommand::StackHooks(cli.into())),
            StackSubcommand::Setup(cli) => Dispatch::Native(NativeCommand::StackSetup(cli.into())),
        },
        Subcommands::SelfUpdate(cli) => Dispatch::Native(NativeCommand::SelfUpdate(cli.into())),
        Subcommands::Completions(cli) => Dispatch::Native(NativeCommand::Completions(cli.shell)),
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
        Subcommands::Internal(InternalArgs {
            command: InternalSubcommand::DumpCliSchema,
        }) => Dispatch::Native(NativeCommand::InternalDumpCliSchema),
        Subcommands::Internal(InternalArgs {
            command: InternalSubcommand::Man,
        }) => Dispatch::Native(NativeCommand::InternalManPage),
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
            command: CiSubcommand::QueueInfo(QueueInfoCliArgs {}),
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
            command: TestsSubcommand::Quarantines(TestsQuarantinesArgs { command }),
        }) => Dispatch::Native(match command {
            QuarantinesSubcommand::Add(args) => {
                NativeCommand::TestsQuarantine(quarantine_opts(args))
            }
            QuarantinesSubcommand::Remove(args) => {
                NativeCommand::TestsUnquarantine(unquarantine_opts(args))
            }
            QuarantinesSubcommand::Get(args) => {
                NativeCommand::TestsQuarantineGet(quarantine_get_opts(args))
            }
            QuarantinesSubcommand::List(args) => {
                NativeCommand::TestsQuarantined(quarantined_opts(args))
            }
        }),
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
            command: QueueSubcommand::Show(ShowCliArgs { pr_number, json }),
        }) => Dispatch::Native(NativeCommand::QueueShow(QueueShowOpts {
            repository,
            token,
            api_url,
            pr_number,
            verbose: parsed.verbose > 0,
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
/// Resolved bundle of the shared per-command preamble the GitHub-
/// API-backed stack subcommands (`list`, `open`, `sync`,
/// `checkout`, and eventually `push`) all need. Built by
/// [`resolve_stack_context`] from the per-command CLI flags.
struct StackContext {
    client: mergify_core::HttpClient,
    slug: mergify_stack::stack_context::RepoSlug,
    author: String,
    branch_prefix: String,
    trunk: (String, String),
}

async fn resolve_stack_context(
    token: Option<&str>,
    author: Option<&str>,
    repository: Option<&str>,
    trunk: Option<(String, String)>,
    branch_prefix: Option<String>,
) -> Result<StackContext, mergify_core::CliError> {
    let token = mergify_core::auth::resolve_token(token)?;
    let github_server = mergify_stack::stack_context::resolve_github_server(None)?;
    let client = mergify_stack::remote_changes::default_client(github_server, &token)?;
    let trunk = if let Some((remote, branch)) = trunk {
        (remote, branch)
    } else {
        let t = mergify_stack::trunk::get_trunk(None).map_err(|e| {
            mergify_core::CliError::StackNotFound(format!(
                "could not determine trunk branch ({e}). Pass --trunk REMOTE/BRANCH."
            ))
        })?;
        (t.remote, t.branch)
    };
    let slug = mergify_stack::stack_context::resolve_repo(None, repository, &trunk.0)?;
    let author = if let Some(a) = author {
        a.to_string()
    } else {
        let user_payload: serde_json::Value = client.get("/user").await?;
        user_payload
            .get("login")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                mergify_core::CliError::GitHubApi("/user response missing `login`".to_string())
            })?
    };
    let branch_prefix = branch_prefix.unwrap_or_else(|| {
        mergify_stack::stack_context::resolve_default_branch_prefix(None, &author)
    });
    Ok(StackContext {
        client,
        slug,
        author,
        branch_prefix,
        trunk,
    })
}

/// Render `stack list` output to stdout in human-readable form.
/// Port of Python's `display_stack_list`. No colour codes — we
/// keep it plain so log scrapers don't have to strip ANSI; users
/// who want colour pipe through `bat -p` etc.
fn render_stack_list_text(out: &mergify_stack::commands::list::StackListOutput, verbose: bool) {
    println!("\nStack on {} -> {}:\n", out.branch, out.trunk);
    if out.entries.is_empty() {
        println!("  No commits in stack");
        return;
    }
    for entry in &out.entries {
        let short = &entry.commit_sha[..entry.commit_sha.len().min(7)];
        let status_label = match entry.status.as_str() {
            "merged" => "MERGED",
            "draft" => "DRAFT",
            "open" => "OPEN",
            "no_pr" => "NEW",
            other => other,
        };
        let conflict = if entry.mergeable == Some(false) {
            " (conflicting)"
        } else {
            ""
        };
        if let Some(num) = entry.pull_number {
            println!(
                "  [{status_label}] #{num} {title} ({short}){conflict}",
                title = entry.title,
            );
            // CI + review render on a single 5-space-indented line
            // joined by " | ", matching Python's display_stack_list
            // (a part is omitted entirely when its status is unknown).
            let ci_display = format_ci_display(entry, verbose);
            let review_display = format_review_display(entry, verbose);
            let parts: Vec<&str> = [ci_display.as_str(), review_display.as_str()]
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect();
            if !parts.is_empty() {
                println!("     {}", parts.join(" | "));
            }
            if let Some(url) = &entry.pull_url {
                println!("     {url}");
            }
        } else {
            println!("  [{status_label}] {title} ({short})", title = entry.title);
        }
        println!();
    }
}

/// Build the `CI: …` cell for a stack-list entry. Port of Python's
/// `_format_ci_display`: empty when the status is unknown; in
/// verbose mode with per-check data it lists each check with a
/// status glyph, otherwise it shows the coarse status label.
fn format_ci_display(
    entry: &mergify_stack::commands::list::StackListEntry,
    verbose: bool,
) -> String {
    if entry.ci_status == "unknown" {
        return String::new();
    }
    if verbose && !entry.ci_checks.is_empty() {
        let checks = entry
            .ci_checks
            .iter()
            .map(|c| {
                let glyph = match c.status.as_str() {
                    "success" => "✓",
                    "failure" => "✗",
                    _ => "●",
                };
                format!("{glyph} {}", c.name)
            })
            .collect::<Vec<_>>()
            .join(", ");
        return format!("CI: {checks}");
    }
    let text = match entry.ci_status.as_str() {
        "passing" => "✓ passing",
        "failing" => "✗ failing",
        "pending" => "● pending",
        other => other,
    };
    format!("CI: {text}")
}

/// Build the `Review: …` cell for a stack-list entry. Port of
/// Python's `_format_review_display`.
fn format_review_display(
    entry: &mergify_stack::commands::list::StackListEntry,
    verbose: bool,
) -> String {
    if entry.review_status == "unknown" {
        return String::new();
    }
    if verbose && !entry.reviews.is_empty() {
        let reviewers = entry
            .reviews
            .iter()
            .map(|r| match r.state.as_str() {
                "APPROVED" => format!("✓ {}", r.user),
                "CHANGES_REQUESTED" => format!("✗ {}", r.user),
                _ => r.user.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        return format!("Review: {reviewers}");
    }
    let text = match entry.review_status.as_str() {
        "approved" => "✓ approved",
        "changes_requested" => "✗ changes requested",
        "pending" => "● pending",
        other => other,
    };
    format!("Review: {text}")
}

/// Install / upgrade the git hooks. Prints one human line per
/// action performed, then a summary footer. Surfaces the same
/// outcome ``mergify_cli/stack/setup.py`` used to print.
fn run_stack_setup(force: bool) -> Result<(), mergify_core::CliError> {
    use mergify_stack::commands::setup::HookAction;
    let outcome =
        mergify_stack::commands::setup::install(&mergify_stack::commands::setup::Options {
            repo_dir: None,
            force,
        })?;
    let mut any_legacy_needs_force = false;
    for log in &outcome.logs {
        for action in &log.actions {
            match action {
                HookAction::ScriptInstalled | HookAction::ScriptUpdated => {
                    println!(
                        "Updating managed hook script: mergify-hooks/{}.sh",
                        log.hook_name
                    );
                }
                HookAction::WrapperInstalled => {
                    println!("Installing hook wrapper: {}", log.hook_name);
                }
                HookAction::WrapperMigrated => {
                    println!("Migrating legacy hook to new format: {}", log.hook_name);
                }
                HookAction::WrapperLegacyNeedsForce => {
                    println!(
                        "Found legacy hook: {} (run with --force to migrate)",
                        log.hook_name
                    );
                    any_legacy_needs_force = true;
                }
                HookAction::ScriptUpToDate | HookAction::WrapperAlreadyInstalled => {}
            }
        }
    }
    if outcome.notes_display_ref_added {
        println!("Added notes.displayRef = refs/notes/mergify/*");
    }
    if any_legacy_needs_force {
        println!("Some hooks are legacy. Run 'mergify stack hooks --setup --force' to migrate.");
    }
    Ok(())
}

/// Print the hooks status table. Mirrors ``_print_hooks_status``
/// in ``mergify_cli/stack/cli.py``.
fn render_hooks_status(status: &mergify_stack::commands::setup::HooksStatus) {
    use mergify_stack::commands::setup::WrapperStatus;
    let mut needs_setup = false;
    let mut needs_force = false;

    println!("\nGit Hooks Status:\n");
    for h in &status.git_hooks {
        println!("  {}:", h.hook_name);
        let wrapper_line = match h.wrapper_status {
            WrapperStatus::Installed => format!("    Wrapper: installed ({})", h.wrapper_path),
            WrapperStatus::Legacy => {
                needs_force = true;
                "    Wrapper: legacy (needs --force to migrate)".to_string()
            }
            WrapperStatus::Missing => {
                needs_setup = true;
                "    Wrapper: not installed".to_string()
            }
        };
        println!("{wrapper_line}");
        if h.script_installed {
            if h.script_needs_update {
                println!("    Script:  needs update ({})", h.script_path);
                needs_setup = true;
            } else {
                println!("    Script:  up to date ({})", h.script_path);
            }
        } else {
            println!("    Script:  not installed");
            needs_setup = true;
        }
        println!();
    }
    if needs_setup || needs_force {
        println!("Run 'mergify stack hooks --setup' to install/upgrade hooks.");
        if needs_force {
            println!("Run 'mergify stack hooks --setup --force' to force reinstall wrappers.");
        }
    } else {
        println!("All hooks are up to date.");
    }
}

/// Absolute path to the running `mergify` binary. The rebase-family
/// stack subcommands set it as `GIT_SEQUENCE_EDITOR` so the spawned
/// `git rebase -i` re-invokes this binary to rewrite the todo file in
/// place. Keeps the original I/O error as a `caused by:` source.
fn mergify_self_exe() -> Result<PathBuf, mergify_core::CliError> {
    std::env::current_exe().map_err(|e| {
        mergify_core::CliError::wrap(
            "could not locate current binary path for GIT_SEQUENCE_EDITOR",
            e,
        )
    })
}

#[allow(clippy::too_many_lines)] // one match arm per native command
fn run_native(cmd: NativeCommand) -> ExitCode {
    // Pure introspection — no async runtime, network, or shared output
    // machinery. Handle it before spinning up tokio.
    if matches!(cmd, NativeCommand::InternalDumpCliSchema) {
        return cli_schema::run();
    }
    // Completions and the man page are pure introspection over the
    // clap tree — emit to stdout and exit before tokio spins up.
    match cmd {
        NativeCommand::Completions(shell) => {
            let mut command = CliRoot::command();
            clap_complete::generate(shell, &mut command, "mergify", &mut std::io::stdout());
            return ExitCode::SUCCESS;
        }
        NativeCommand::InternalManPage => {
            return match clap_mangen::Man::new(CliRoot::command()).render(&mut std::io::stdout()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("mergify: render man page: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        _ => {}
    }

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
        NativeCommand::TestsQuarantineGet(opts) if opts.json => OutputMode::Json,
        NativeCommand::TestsQuarantined(opts) if opts.json => OutputMode::Json,
        _ => OutputMode::Human,
    };
    let mut output = StdioOutput::new(mode);

    let result: Result<mergify_core::ExitCode, mergify_core::CliError> = rt.block_on(async {
        match cmd {
            // Handled above, before the runtime was built.
            NativeCommand::InternalDumpCliSchema
            | NativeCommand::Completions(_)
            | NativeCommand::InternalManPage => {
                unreachable!("introspection commands are handled before the runtime starts")
            }
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
            NativeCommand::CiQueueInfo => mergify_ci::queue_info::run(&mut output)
                .map(|()| mergify_core::ExitCode::Success),
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
            NativeCommand::TestsQuarantineGet(opts) => {
                mergify_ci::tests_quarantine::get(
                    GetOptions {
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
                let mergify_binary = mergify_self_exe()?;
                let outcome = mergify_stack::commands::edit::run(
                    &mergify_stack::commands::edit::Options {
                        repo_dir: None,
                        commit_prefix: opts.commit_prefix.as_deref(),
                        mergify_binary: &mergify_binary,
                    },
                )?;
                match outcome {
                    mergify_stack::commands::edit::Outcome::PausedAt { .. } => {
                        // The "Editing commit:" notice is printed by
                        // edit::run before the rebase so it precedes
                        // git's output; here we add the post-rebase
                        // amend hint.
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
                let mergify_binary = mergify_self_exe()?;
                let outcome = mergify_stack::commands::drop::run(
                    &mergify_stack::commands::drop::Options {
                        repo_dir: None,
                        commit_prefixes: &opts.commit_prefixes,
                        dry_run: opts.dry_run,
                        mergify_binary: &mergify_binary,
                    },
                )?;
                match outcome {
                    mergify_stack::commands::drop::Outcome::Dropped { plan } => {
                        for line in mergify_stack::plan_display::render_plan("Drop plan:", &plan) {
                            println!("{line}");
                        }
                        println!("Commits dropped successfully.");
                    }
                    mergify_stack::commands::drop::Outcome::DryRun { plan } => {
                        for line in mergify_stack::plan_display::render_plan("Drop plan:", &plan) {
                            println!("{line}");
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
                let mergify_binary = mergify_self_exe()?;
                let outcome = mergify_stack::commands::fixup::run(
                    &mergify_stack::commands::fixup::Options {
                        repo_dir: None,
                        commit_prefixes: &opts.commit_prefixes,
                        dry_run: opts.dry_run,
                        mergify_binary: &mergify_binary,
                    },
                )?;
                match outcome {
                    mergify_stack::commands::fixup::Outcome::Squashed { plan } => {
                        for line in mergify_stack::plan_display::render_plan("Fixup plan:", &plan) {
                            println!("{line}");
                        }
                        println!("Commits squashed successfully.");
                    }
                    mergify_stack::commands::fixup::Outcome::DryRun { plan } => {
                        for line in mergify_stack::plan_display::render_plan("Fixup plan:", &plan) {
                            println!("{line}");
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
                let mergify_binary = mergify_self_exe()?;
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
                    mergify_stack::commands::reword::Outcome::Reworded { plan } => {
                        for line in mergify_stack::plan_display::render_plan("Reword plan:", &plan) {
                            println!("{line}");
                        }
                        println!("Commit reworded successfully.");
                    }
                    mergify_stack::commands::reword::Outcome::DryRun { plan } => {
                        for line in mergify_stack::plan_display::render_plan("Reword plan:", &plan) {
                            println!("{line}");
                        }
                        println!("Dry run — no changes made");
                    }
                    mergify_stack::commands::reword::Outcome::EmptyStack => {
                        println!("No commits in the stack");
                    }
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackReorder(opts) => {
                let mergify_binary = mergify_self_exe()?;
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
                        for line in mergify_stack::plan_display::render_plan("Reorder plan:", &plan)
                        {
                            println!("{line}");
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
                let mergify_binary = mergify_self_exe()?;
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
                        for line in mergify_stack::plan_display::render_plan("Move plan:", &plan) {
                            println!("{line}");
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
                let mergify_binary = mergify_self_exe()?;
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
                        for line in mergify_stack::plan_display::render_plan("Squash plan:", &plan) {
                            println!("{line}");
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
                        // An empty result is a not-found state, not a
                        // success — exit 3 so scripts can tell "checked
                        // out" from "nothing to check out" (matches
                        // `stack open`'s empty-stack handling).
                        println!("No stacked pull requests found");
                        Ok(mergify_core::ExitCode::StackNotFound)
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
                        Ok(mergify_core::ExitCode::Success)
                    }
                }
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

                let mergify_binary = mergify_self_exe()?;

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
                        // Direct `stack sync` shows git's rebase output.
                        quiet: false,
                        // Standalone sync fetches everything itself.
                        prefetched_remote_changes: None,
                        skip_trunk_fetch: false,
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
            NativeCommand::StackPush(opts) => {
                let ctx = resolve_stack_context(
                    opts.token.as_deref(),
                    opts.author.as_deref(),
                    opts.repository.as_deref(),
                    opts.trunk.clone(),
                    opts.branch_prefix.clone(),
                )
                .await?;
                let github_server = mergify_stack::stack_context::resolve_github_server(None)?;
                let mergify_binary = std::env::current_exe().map_err(|e| {
                    mergify_core::CliError::Generic(format!(
                        "could not locate current binary path: {e}"
                    ))
                })?;
                let github_server_str = github_server.as_str().trim_end_matches('/').to_string();
                let create_as_draft = opts.create_as_draft.unwrap_or_else(|| {
                    mergify_stack::stack_context::resolve_default_create_as_draft(None)
                });
                let keep_pull_request_title_and_body =
                    opts.keep_pull_request_title_and_body.unwrap_or_else(|| {
                        mergify_stack::stack_context::resolve_default_keep_pr_title_body(None)
                    });
                let revision_history = opts.revision_history.unwrap_or_else(|| {
                    mergify_stack::stack_context::resolve_default_revision_history(None)
                });
                let outcome = mergify_stack::commands::push::run(
                    &mergify_stack::commands::push::Options {
                        repo_dir: None,
                        client: &ctx.client,
                        mergify_binary: &mergify_binary,
                        github_server: &github_server_str,
                        trunk: (&ctx.trunk.0, &ctx.trunk.1),
                        author: &ctx.author,
                        branch_prefix: &ctx.branch_prefix,
                        user: &ctx.slug.owner,
                        repo: &ctx.slug.repo,
                        skip_rebase: opts.skip_rebase,
                        force_rebase: opts.force_rebase,
                        next_only: opts.next_only,
                        dry_run: opts.dry_run,
                        create_as_draft,
                        keep_pull_request_title_and_body,
                        only_update_existing_pulls: opts.only_update_existing_pulls,
                        revision_history,
                        no_verify: opts.no_verify,
                    },
                )
                .await?;
                // Dry-run buffers its plan and prints it here; a real
                // push streams progress live from `push::run` as each
                // step completes, so its transcript must not be
                // re-printed.
                if let mergify_stack::commands::push::Outcome::DryRun { log_lines, .. } = outcome {
                    for line in log_lines {
                        println!("{line}");
                    }
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackList(opts) => {
                let ctx = resolve_stack_context(
                    opts.token.as_deref(),
                    opts.author.as_deref(),
                    opts.repository.as_deref(),
                    opts.trunk.clone(),
                    opts.branch_prefix.clone(),
                )
                .await?;
                let stack = mergify_stack::commands::list::run(
                    &mergify_stack::commands::list::Options {
                        repo_dir: None,
                        client: &ctx.client,
                        user: &ctx.slug.owner,
                        repo: &ctx.slug.repo,
                        author: &ctx.author,
                        branch_prefix: &ctx.branch_prefix,
                        trunk: (&ctx.trunk.0, &ctx.trunk.1),
                        include_status: true,
                    },
                )
                .await?;
                if opts.json {
                    let json = serde_json::to_string_pretty(&stack).map_err(|e| {
                        mergify_core::CliError::Generic(format!(
                            "serialize stack list: {e}"
                        ))
                    })?;
                    println!("{json}");
                } else {
                    render_stack_list_text(&stack, opts.verbose);
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackOpen(opts) => {
                let ctx = resolve_stack_context(
                    opts.token.as_deref(),
                    opts.author.as_deref(),
                    opts.repository.as_deref(),
                    opts.trunk.clone(),
                    opts.branch_prefix.clone(),
                )
                .await?;
                let outcome = mergify_stack::commands::open::run(
                    &mergify_stack::commands::open::Options {
                        repo_dir: None,
                        client: &ctx.client,
                        user: &ctx.slug.owner,
                        repo: &ctx.slug.repo,
                        author: &ctx.author,
                        branch_prefix: &ctx.branch_prefix,
                        trunk: (&ctx.trunk.0, &ctx.trunk.1),
                        commit: opts.commit.as_deref(),
                    },
                )
                .await?;
                match outcome {
                    mergify_stack::commands::open::Outcome::Opened {
                        pull_number,
                        title,
                        pull_url,
                    } => {
                        println!("Opening PR #{pull_number}: {title}");
                        println!("  {pull_url}");
                        Ok(mergify_core::ExitCode::Success)
                    }
                    mergify_stack::commands::open::Outcome::EmptyStack => {
                        // Python exits STACK_NOT_FOUND (3) so callers
                        // can detect the empty stack via `$?`.
                        println!("No commits in stack");
                        Ok(mergify_core::ExitCode::StackNotFound)
                    }
                }
            }
            NativeCommand::StackHooks(opts) => {
                if opts.do_setup {
                    run_stack_setup(opts.force)?;
                } else {
                    let status = mergify_stack::commands::setup::status(None)?;
                    render_hooks_status(&status);
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::StackSetup(opts) => {
                if opts.check {
                    let status = mergify_stack::commands::setup::status(None)?;
                    render_hooks_status(&status);
                } else {
                    run_stack_setup(opts.force)?;
                }
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::SelfUpdate(opts) => {
                self_update::run(&opts).await?;
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
            // Print any preserved cause chain (CliError::wrap /
            // #[source]) so the underlying reason isn't lost.
            let mut source = std::error::Error::source(&err);
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            ExitCode::from(code.as_u8())
        }
    }
}

/// Mergify command-line interface.
///
/// Drive Mergify from your terminal. Validate and simulate your
/// configuration, manage and inspect the merge queue, schedule merge
/// freezes, look at the tests tracked by CI Insights, and create and
/// maintain stacked pull requests.
///
/// Most commands talk to the Mergify API and need a token. Set
/// `MERGIFY_TOKEN` (or `GITHUB_TOKEN`) to apply it to every command,
/// or pass `--token` to an individual command — there is no global
/// `--token` on `mergify` itself. Run `mergify <command> --help` (or
/// the git-style `mergify help <command>`) for detailed help and
/// options on any command.
#[derive(Parser)]
#[command(name = "mergify", version = VERSION)]
struct CliRoot {
    /// Increase log verbosity: -v info, -vv debug, -vvv trace. Logs
    /// go to stderr so stdout stays clean for piping. `RUST_LOG`
    /// overrides this.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Shorthand for at least debug-level logging (like -vv).
    #[arg(long, global = true)]
    debug: bool,

    /// When to use color in terminal output.
    #[arg(long, global = true, value_enum, default_value_t = ColorArg::Auto)]
    color: ColorArg,

    #[command(subcommand)]
    command: Subcommands,
}

/// `--color` choice. Mirrors [`mergify_tui::ColorChoice`]; kept
/// separate so the clap derive lives in the binary and `mergify-tui`
/// stays clap-free.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
enum ColorArg {
    #[default]
    Auto,
    Always,
    Never,
}

impl From<ColorArg> for mergify_tui::ColorChoice {
    fn from(c: ColorArg) -> Self {
        match c {
            ColorArg::Auto => Self::Auto,
            ColorArg::Always => Self::Always,
            ColorArg::Never => Self::Never,
        }
    }
}

#[derive(Subcommand)]
enum Subcommands {
    /// Validate and simulate your Mergify configuration.
    ///
    /// Check your `.mergify.yml` against the schema before pushing it,
    /// or simulate how Mergify's rules would evaluate against a given
    /// pull request using your local configuration.
    Config(ConfigArgs),
    /// Run Mergify CI Insights commands from your pipeline.
    ///
    /// Helpers meant to run inside a CI job: upload test reports,
    /// detect the build's git references, report the merge queue
    /// batch the build belongs to, and compute the scopes impacted by
    /// the changed files.
    Ci(CiArgs),
    /// Inspect the tests tracked by Mergify CI Insights.
    ///
    /// Look up a test's health and flakiness metrics, and manage the
    /// quarantine that keeps known-flaky tests from failing the merge
    /// queue.
    Tests(TestsArgs),
    /// Inspect and control the Mergify merge queue.
    ///
    /// Check the status of queued pull requests, inspect a single
    /// pull request's queue state, and pause or resume merging for a
    /// repository.
    Queue(QueueArgs),
    /// Schedule and manage merge freezes.
    ///
    /// Create, list, update, and delete freezes that temporarily stop
    /// the merge queue from merging — for release windows, incidents,
    /// or code freezes.
    Freeze(FreezeArgs),
    /// Create and maintain stacked pull requests.
    ///
    /// Manage a stack of dependent branches and their pull requests:
    /// create, push, sync, reorder, reword, squash, and check out
    /// stacks built on top of your trunk.
    Stack(StackArgs),
    /// Update mergify to the latest release.
    ///
    /// Download the newest published binary, verify it against the
    /// release `SHA256SUMS`, and atomically replace the running
    /// executable. Uses the same artifact and checksum contract as the
    /// `curl | sh` installer.
    #[command(name = "self-update")]
    SelfUpdate(SelfUpdateCli),
    /// Print a shell completion script (e.g. `mergify completions zsh`).
    Completions(CompletionsCli),
    /// Internal helpers the Python side of the wheel calls during
    /// the Python→Rust migration. Hidden from `--help` because it
    /// is not part of the user-facing CLI; the wire format is not
    /// stable and may change without notice. Do not depend on it
    /// from anywhere outside the Python code shipped in this same
    /// wheel.
    #[command(name = "_internal", hide = true)]
    Internal(InternalArgs),
}

#[derive(clap::Args)]
struct StackArgs {
    #[command(subcommand)]
    command: StackSubcommand,
}

/// Subcommands of `mergify stack`. clap sources each subcommand's
/// `about` from the variant doc comment (not the wrapped struct), so
/// the help text lives here.
#[derive(Subcommand)]
enum StackSubcommand {
    /// Create a new stack branch.
    ///
    /// Create a branch that tracks your trunk and start a fresh stack on
    /// top of it. The new branch is checked out by default; pass
    /// `--no-checkout` to only create the ref and stay on the current
    /// branch.
    New(StackNewCli),
    /// Attach a note explaining why a commit was amended.
    ///
    /// Record a note on a stack commit describing why it changed; the
    /// note is surfaced when reviewing the stack. Edit it in your editor
    /// or pass `-m`, add to an existing note with `--append`, or clear it
    /// with `--remove`. Defaults to the commit at HEAD.
    Note(StackNoteCli),
    /// Edit a commit in the stack.
    ///
    /// Pause an interactive rebase on the target commit so you can amend
    /// it, then resume. Omit the commit to start a fully interactive
    /// rebase of the whole stack.
    Edit(StackEditCli),
    /// Drop commits from the stack.
    ///
    /// Remove one or more commits from the stack and rebase the commits
    /// above them down. Each accepts a SHA prefix or a Change-Id prefix.
    /// Use `--dry-run` to preview the resulting order first.
    Drop(StackDropCli),
    /// Fold commits into their parent.
    ///
    /// Squash each given commit into the commit below it, discarding the
    /// folded commit's message (the parent's message is kept). Each
    /// accepts a SHA prefix or a Change-Id prefix. Use `--dry-run` to
    /// preview.
    Fixup(StackFixupCli),
    /// Change a commit's message.
    ///
    /// Rewrite the message of a commit in the stack. Pass `-m` to set the
    /// new message inline, or omit it to edit the message in your editor.
    /// Use `--dry-run` to preview.
    Reword(StackRewordCli),
    /// Reorder the stack's commits.
    ///
    /// Rebase the stack into the order you list. List every commit in the
    /// stack — all of them must appear, in the new order. Each accepts a
    /// SHA prefix or a Change-Id prefix. Use `--dry-run` to preview.
    Reorder(StackReorderCli),
    /// Move a commit within the stack.
    ///
    /// Move one commit to a new position in the stack: to the `first` or
    /// `last` slot, or `before`/`after` another commit (passed as
    /// TARGET). Use `--dry-run` to preview.
    Move(StackMoveCli),
    /// Squash commits into a target commit.
    ///
    /// Fold one or more source commits into a target commit, reordering
    /// them adjacent to it first. Use the form `SRC... into TARGET`. Pass
    /// `-m` to set the combined message; otherwise the target's message
    /// is kept. Use `--dry-run` to preview.
    Squash(StackSquashCli),
    /// Check out a pull request stack from GitHub.
    ///
    /// Fetch a stack of pull requests by name and create a local branch
    /// that tracks its leaf, so you can continue working on a stack
    /// created elsewhere (a teammate's, or your own from another
    /// machine).
    Checkout(StackCheckoutCli),
    /// Sync the stack with its trunk.
    ///
    /// Fetch the latest trunk, drop commits whose pull request has
    /// already merged, and rebase the remaining stack on top. Use
    /// `--dry-run` to preview.
    Sync(StackSyncCli),
    /// Push the stack and create or update its pull requests.
    ///
    /// Walk the local stack, optionally rebase it on trunk, push each
    /// commit to its own branch, and create or update the matching pull
    /// request and its Depends-On chain on GitHub. Use `--dry-run` to
    /// preview the plan and the rebase decision without touching
    /// anything.
    Push(StackPushCli),
    /// List the stack's commits and their pull requests.
    ///
    /// Show each commit in the current stack alongside its pull request,
    /// CI, and review state. Pass `--verbose` for per-check and
    /// per-reviewer detail, or `--json` for machine-readable output.
    List(StackListCli),
    /// Open a stack commit's pull request in the browser.
    ///
    /// Open the GitHub pull request for a commit in the stack in your
    /// default browser. Defaults to the commit at HEAD.
    Open(StackOpenCli),
    /// Show or install the stack git hooks.
    ///
    /// Report the status of the git hooks that keep `Change-Id` trailers
    /// on your commits (stacks rely on them to track commits across
    /// rebases). Pass `--setup` to install or upgrade them.
    Hooks(StackHooksCli),
    /// Install the stack git hooks.
    ///
    /// Install or upgrade the git hooks that add `Change-Id` trailers to
    /// your commits. This is an alias for `stack hooks --setup`; pass
    /// `--check` to report status instead of installing.
    Setup(StackSetupCli),
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
    /// Emit the machine-readable CLI schema (the JSON the docs site
    /// renders into the command reference) to stdout. Walks the clap
    /// command tree so every description and flag is sourced from the
    /// code, never hand-maintained. Not a stable user-facing surface.
    #[command(name = "dump-cli-schema")]
    DumpCliSchema,
    /// Render the roff man page to stdout, for packaging to install
    /// into `man/`. Not a stable user-facing surface.
    #[command(name = "man")]
    Man,
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

#[derive(clap::Args)]
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

#[derive(clap::Args)]
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

#[derive(clap::Args)]
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

#[derive(clap::Args)]
struct StackFixupCli {
    /// Commits to fold into their parent.
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

#[derive(clap::Args)]
struct StackRewordCli {
    /// Commit to reword. Accepts a SHA prefix or a Change-Id prefix.
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

#[derive(clap::Args)]
struct StackReorderCli {
    /// Every commit in the stack, in the order you want them rebased
    /// into. All stack commits must be listed.
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

#[derive(clap::Args)]
struct StackMoveCli {
    /// Commit to move. Accepts a SHA prefix or a Change-Id prefix.
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

// The `<SRC>... into <TARGET>` shape doesn't fit clap's positional
// model directly, so we accept a flat `Vec<String>` and split on the
// literal `into` keyword inside [`StackSquashOpts::try_from`].
#[derive(clap::Args)]
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

#[derive(clap::Args)]
struct StackCheckoutCli {
    /// Name of the stack to check out.
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

#[derive(clap::Args)]
struct StackSyncCli {
    /// Author of the stack. Defaults to the token's user.
    #[arg(long)]
    author: Option<String>,

    /// `owner/repo`. Falls back to the URL of `--trunk`'s remote.
    #[arg(long = "repository", alias = "repo")]
    repository: Option<String>,

    /// Override the stack branch prefix.
    #[arg(long = "branch-prefix")]
    branch_prefix: Option<String>,

    /// Show the plan without changing anything.
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

#[derive(clap::Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the Python CLI's flag surface 1:1"
)]
struct StackPushCli {
    /// Author of the stack. Defaults to the token's user.
    #[arg(long)]
    author: Option<String>,

    /// `owner/repo`. Falls back to the URL of `--trunk`'s remote.
    #[arg(long = "repository", alias = "repo")]
    repository: Option<String>,

    /// Override the stack branch prefix.
    #[arg(long = "branch-prefix")]
    branch_prefix: Option<String>,

    /// Target trunk as `REMOTE/BRANCH`. Defaults to the resolved
    /// trunk for the current branch.
    #[arg(short = 't', long = "trunk", value_parser = parse_remote_branch)]
    trunk: Option<(String, String)>,

    /// GitHub token (falls back to `MERGIFY_TOKEN` / `GITHUB_TOKEN`
    /// / `gh auth token`).
    #[arg(long)]
    token: Option<String>,

    /// Skip the rebase step. By default `stack push` rebases on
    /// trunk before pushing when there are no approvals to
    /// dismiss.
    #[arg(short = 'R', long = "skip-rebase", action = clap::ArgAction::SetTrue)]
    skip_rebase: bool,

    /// Force the rebase even when PRs are approved (the rebase
    /// will dismiss the reviews). Mutually exclusive with
    /// `--skip-rebase`.
    #[arg(
        long = "force-rebase",
        action = clap::ArgAction::SetTrue,
        conflicts_with = "skip_rebase"
    )]
    force_rebase: bool,

    /// Only push the bottom commit of the stack.
    #[arg(short = 'x', long = "next-only", action = clap::ArgAction::SetTrue)]
    next_only: bool,

    /// Dry-run: render the plan + the rebase decision and exit.
    #[arg(short = 'n', long = "dry-run", action = clap::ArgAction::SetTrue)]
    dry_run: bool,

    /// Open new PRs as drafts. Default falls back to git config
    /// `mergify-cli.stack-create-as-draft` (`false` when unset).
    #[arg(
        short = 'd',
        long = "draft",
        num_args = 0,
        default_missing_value = "true"
    )]
    create_as_draft: Option<bool>,

    /// Don't rewrite the PR title + body from the commit
    /// message; only update the rendered Depends-On chain in
    /// the body. Default falls back to git config
    /// `mergify-cli.stack-keep-pr-title-body` (`false` when unset).
    #[arg(
        short = 'k',
        long = "keep-pull-request-title-and-body",
        num_args = 0,
        default_missing_value = "true"
    )]
    keep_pull_request_title_and_body: Option<bool>,

    /// Don't create new PRs; surface would-be-created ones in
    /// the plan instead.
    #[arg(
        short = 'u',
        long = "only-update-existing-pulls",
        action = clap::ArgAction::SetTrue
    )]
    only_update_existing_pulls: bool,

    /// Suppress the revision-history sticky comment update.
    /// Default falls back to git config
    /// `mergify-cli.stack-revision-history` (`true` when unset).
    #[arg(
        long = "no-revision-history",
        num_args = 0,
        default_missing_value = "false"
    )]
    revision_history: Option<bool>,

    /// Pass `--no-verify` to `git push` (skips local pre-push
    /// hooks).
    #[arg(long = "no-verify", action = clap::ArgAction::SetTrue)]
    no_verify: bool,
}

impl From<StackPushCli> for StackPushOpts {
    fn from(cli: StackPushCli) -> Self {
        Self {
            author: cli.author,
            repository: cli.repository,
            branch_prefix: cli.branch_prefix,
            trunk: cli.trunk,
            token: cli.token,
            skip_rebase: cli.skip_rebase,
            force_rebase: cli.force_rebase,
            next_only: cli.next_only,
            dry_run: cli.dry_run,
            create_as_draft: cli.create_as_draft,
            keep_pull_request_title_and_body: cli.keep_pull_request_title_and_body,
            only_update_existing_pulls: cli.only_update_existing_pulls,
            revision_history: cli.revision_history,
            no_verify: cli.no_verify,
        }
    }
}

#[derive(clap::Args)]
struct StackListCli {
    /// Author of the stack. Defaults to the token's user.
    #[arg(long)]
    author: Option<String>,

    /// `owner/repo`. Falls back to the URL of `--trunk`'s remote.
    #[arg(long = "repository", alias = "repo")]
    repository: Option<String>,

    /// Override the stack branch prefix.
    #[arg(long = "branch-prefix")]
    branch_prefix: Option<String>,

    /// Target trunk as `REMOTE/BRANCH`. Defaults to the resolved
    /// trunk for the current branch.
    #[arg(short = 't', long = "trunk", value_parser = parse_remote_branch)]
    trunk: Option<(String, String)>,

    /// GitHub token (falls back to `MERGIFY_TOKEN` / `GITHUB_TOKEN`
    /// / `gh auth token`).
    #[arg(long)]
    token: Option<String>,

    /// Emit machine-readable JSON.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    json: bool,
}

impl From<StackListCli> for StackListOpts {
    fn from(cli: StackListCli) -> Self {
        Self {
            author: cli.author,
            repository: cli.repository,
            branch_prefix: cli.branch_prefix,
            trunk: cli.trunk,
            token: cli.token,
            json: cli.json,
            // Driven by the global `-v`/`--verbose` flag, applied in dispatch.
            verbose: false,
        }
    }
}

#[derive(clap::Args)]
struct StackOpenCli {
    /// Commit whose pull request to open. Accepts a SHA prefix or a
    /// Change-Id prefix; defaults to HEAD.
    commit: Option<String>,

    /// Author of the stack. Defaults to the token's user.
    #[arg(long)]
    author: Option<String>,

    /// `owner/repo`. Falls back to the URL of `--trunk`'s remote.
    #[arg(long = "repository", alias = "repo")]
    repository: Option<String>,

    /// Override the stack branch prefix.
    #[arg(long = "branch-prefix")]
    branch_prefix: Option<String>,

    /// Target trunk as `REMOTE/BRANCH`. Defaults to the resolved
    /// trunk for the current branch.
    #[arg(short = 't', long = "trunk", value_parser = parse_remote_branch)]
    trunk: Option<(String, String)>,

    /// GitHub token (falls back to `MERGIFY_TOKEN` / `GITHUB_TOKEN`
    /// / `gh auth token`).
    #[arg(long)]
    token: Option<String>,
}

impl From<StackOpenCli> for StackOpenOpts {
    fn from(cli: StackOpenCli) -> Self {
        Self {
            commit: cli.commit,
            author: cli.author,
            repository: cli.repository,
            branch_prefix: cli.branch_prefix,
            trunk: cli.trunk,
            token: cli.token,
        }
    }
}

#[derive(clap::Args)]
struct StackHooksCli {
    /// Install or upgrade hooks.
    #[arg(long = "setup", action = clap::ArgAction::SetTrue)]
    do_setup: bool,

    /// Force reinstall wrappers (use with --setup).
    #[arg(short = 'f', long = "force", action = clap::ArgAction::SetTrue)]
    force: bool,
}

impl From<StackHooksCli> for StackHooksOpts {
    fn from(cli: StackHooksCli) -> Self {
        Self {
            do_setup: cli.do_setup,
            force: cli.force,
        }
    }
}

#[derive(clap::Args)]
struct StackSetupCli {
    /// Force reinstall of hook wrappers, even if user modified them.
    #[arg(short = 'f', long = "force", action = clap::ArgAction::SetTrue)]
    force: bool,

    /// Check status only (use 'stack hooks' instead).
    #[arg(long = "check", action = clap::ArgAction::SetTrue)]
    check: bool,
}

impl From<StackSetupCli> for StackSetupOpts {
    fn from(cli: StackSetupCli) -> Self {
        Self {
            force: cli.force,
            check: cli.check,
        }
    }
}

/// Update mergify to the latest release.
///
/// Download the newest published binary, verify it against the
/// release `SHA256SUMS`, and atomically replace the running
/// executable. Pass `--check` to only compare versions, or `--force`
/// to reinstall even when already up to date.
#[derive(Parser)]
#[command(name = "self-update")]
struct SelfUpdateCli {
    /// Re-download and re-install even when the running binary
    /// already matches the latest release tag. Useful for
    /// repairing a corrupted install without bumping the version.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    force: bool,

    /// Print the current and latest release tags and exit without
    /// touching the binary.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    check: bool,
}

impl From<SelfUpdateCli> for self_update::Options {
    fn from(cli: SelfUpdateCli) -> Self {
        Self {
            force: cli.force,
            check_only: cli.check,
        }
    }
}

#[derive(clap::Args)]
struct CompletionsCli {
    /// Shell to generate a completion script for.
    #[arg(value_enum)]
    shell: clap_complete::Shell,
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

#[derive(clap::Args)]
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
    ///
    /// Check that your configuration file parses and conforms to the
    /// Mergify schema, reporting the first error with its location.
    /// Run it locally or in CI to catch mistakes before they reach
    /// the default branch. The file is auto-detected unless you pass
    /// `--config-file`.
    Validate(ValidateArgs),
    /// Simulate Mergify actions on a pull request using the local
    /// configuration.
    ///
    /// Evaluate your local configuration against a real pull request
    /// and report which rules match and what actions Mergify would
    /// take — without changing anything on GitHub. Useful to test a
    /// configuration change before pushing it.
    Simulate(SimulateCliArgs),
}

#[derive(clap::Args)]
struct ValidateArgs {}

#[derive(clap::Args)]
struct SimulateCliArgs {
    /// Pull request URL (e.g. <https://github.com/owner/repo/pull/123>).
    #[arg(value_name = "PULL_REQUEST_URL", value_parser = mergify_core::pull_request::parse_pr_url)]
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
    ///
    /// Upload the scopes affected by a pull request so CI Insights can
    /// attribute test results to them. Reads scopes from repeated
    /// --scope flags or from a file produced by "mergify ci scopes
    /// --write". Exits 0 without doing anything when no pull request
    /// can be determined.
    #[command(name = "scopes-send")]
    ScopesSend(ScopesSendCliArgs),
    /// Print the base/head git references for the current build.
    ///
    /// Detect and print the base and head git references for the build
    /// from the CI environment, as plain text, eval-friendly shell
    /// lines, or JSON. Useful for wiring later steps to the same refs
    /// Mergify computed.
    #[command(name = "git-refs")]
    GitRefs(GitRefsCliArgs),
    /// Print the current build's merge queue batch metadata (from the
    /// Mergify git note).
    ///
    /// Read the Mergify git note attached to the current HEAD and print
    /// the merge queue batch the build belongs to. Needs only plain
    /// git and no token, so it works in any CI runner.
    #[command(name = "queue-info")]
    QueueInfo(QueueInfoCliArgs),
    /// Give the list of scopes impacted by changed files.
    ///
    /// Compute the configured scopes impacted by the files changed
    /// between two git references, using your Mergify configuration.
    /// Print them, or write them to a file with --write for a later
    /// "mergify ci scopes-send".
    Scopes(ScopesCliArgs),
    /// Upload JUnit XML reports and ignore failed tests with
    /// Mergify's CI Insights Quarantine.
    ///
    /// Parse one or more JUnit XML reports, upload the results to CI
    /// Insights, and reconcile them against the quarantine so
    /// known-flaky tests don't fail the build. Accepts file paths or
    /// glob patterns; pass the runner's exit code with --test-exit-code
    /// to detect silent failures.
    #[command(name = "junit-process")]
    JunitProcess(JunitProcessCliArgs),
    /// Upload JUnit XML reports (deprecated: use junit-process).
    ///
    /// Deprecated alias for junit-process, kept for backward
    /// compatibility. It runs the same processing and prints a
    /// deprecation warning; use junit-process instead.
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

/// `queue-info` reads the `refs/notes/mergify/<branch>` git note
/// Mergify writes for the current `HEAD`, so it takes no arguments and
/// needs no GitHub token — plain git in any CI.
#[derive(clap::Args)]
struct QueueInfoCliArgs {}

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

    /// Pull request number. When omitted, it's detected from the CI
    /// environment: under GitHub Actions from the event payload
    /// (``.pull_request.number`` in ``GITHUB_EVENT_PATH``), under
    /// Buildkite from ``BUILDKITE_PULL_REQUEST``. When none can be
    /// detected the command prints a skip message and exits 0.
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
    ///
    /// Search CI Insights for one or more tests (by exact name or
    /// glob) and report their flakiness, failure rate, and recent
    /// history. Pass `--json` for machine-readable output.
    Show(TestsShowCliArgs),
    /// Manage the CI Insights quarantine.
    ///
    /// Add, remove, inspect, and list the tests held in the CI
    /// Insights quarantine — the set of known-flaky tests whose
    /// failures are ignored so they don't block the merge queue.
    Quarantines(TestsQuarantinesArgs),
}

#[derive(clap::Args)]
struct TestsQuarantinesArgs {
    #[command(subcommand)]
    command: QuarantinesSubcommand,
}

#[derive(Subcommand)]
enum QuarantinesSubcommand {
    /// Add a test to the CI Insights quarantine.
    ///
    /// Quarantine a test by its fully qualified name so its failures
    /// stop blocking the merge queue. A reason is required; scope it
    /// to a branch with `--branch`, or quarantine on all branches by
    /// default.
    Add(TestsQuarantineCliArgs),
    /// Remove a test from the CI Insights quarantine.
    ///
    /// Take a test out of the quarantine so its results count again.
    /// Identify it by test name or by quarantine id.
    Remove(TestsUnquarantineCliArgs),
    /// Print a single quarantine by test name or id.
    ///
    /// Show the details of one quarantined test — its reason, branch
    /// scope, and when it was added. Identify it by test name or by
    /// quarantine id.
    Get(TestsQuarantineGetCliArgs),
    /// List the tests currently in the CI Insights quarantine.
    ///
    /// Print every test currently held in the quarantine for the
    /// repository. Pass `--json` for machine-readable output.
    List(TestsQuarantinedCliArgs),
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
    /// or the quarantine id (as printed by `tests quarantines add`).
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
struct TestsQuarantineGetCliArgs {
    /// Quarantine to print: either the test's fully qualified name or
    /// the quarantine id (as printed by `tests quarantines add`).
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
    ///
    /// Stop the queue from merging any pull request until it is
    /// resumed — useful during an incident or release window. A
    /// reason is required and shown to your team; queued pull
    /// requests stay in place.
    Pause(PauseCliArgs),
    /// Unpause the merge queue for the repository.
    ///
    /// Resume merging after a pause. The queue picks up where it left
    /// off with the pull requests still queued.
    Unpause,
    /// Show merge queue status for the repository.
    ///
    /// List the pull requests currently in the queue with their
    /// position and state. Filter by branch with `--branch`, or pass
    /// `--json` for machine-readable output.
    Status(StatusCliArgs),
    /// Show detailed state of a pull request in the merge queue.
    ///
    /// Report the full queue state of a single pull request: its
    /// checks, the conditions it still needs to satisfy, and why it
    /// is or isn't mergeable. Pass `--verbose` for the full checks
    /// table and conditions tree, or `--json` for the raw response.
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
    ///
    /// Show the freezes currently configured for the repository, with
    /// their name, schedule, and state. Pass `--json` for
    /// machine-readable output.
    List(FreezeListCliArgs),
    /// Create a new scheduled freeze.
    ///
    /// Add a freeze that stops the merge queue from merging while it
    /// is active — for a release window, incident, or code freeze.
    Create(FreezeCreateCliArgs),
    /// Update an existing scheduled freeze.
    ///
    /// Change the settings of an existing freeze, identified by its
    /// ID. Only the fields you pass are changed; the rest are left as
    /// they are.
    Update(FreezeUpdateCliArgs),
    /// Delete a scheduled freeze.
    ///
    /// Remove a freeze by its ID. If the freeze is currently active a
    /// reason is required, and the queue resumes merging once it's
    /// gone.
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

    /// The schema is published as a release asset (not committed), so
    /// guard the generator structurally rather than against a golden
    /// file: it must serialize to valid JSON, expose every top-level
    /// group, surface both stack sources, and never leak the hidden
    /// `_internal` machinery into the public reference.
    #[test]
    fn cli_schema_is_well_formed() {
        let v: serde_json::Value =
            serde_json::from_str(&cli_schema::dump()).expect("schema is valid JSON");

        let groups: Vec<&str> = v["command"]["commands"]
            .as_array()
            .expect("commands array")
            .iter()
            .map(|c| c["name"].as_str().expect("group name"))
            .collect();
        assert_eq!(
            groups,
            [
                "config",
                "ci",
                "tests",
                "queue",
                "freeze",
                "stack",
                "self-update",
                "completions"
            ]
        );
        assert!(!groups.contains(&"_internal"), "hidden group leaked");

        let stack = v["command"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "stack")
            .expect("stack group");
        let sources: std::collections::BTreeSet<&str> = stack["commands"]
            .as_array()
            .expect("stack subcommands")
            .iter()
            .map(|c| c["source"].as_str().expect("source"))
            .collect();
        // Pure-Rust binary: every stack subcommand is native. Locked
        // in so a regression that quietly re-introduces a non-native
        // source (e.g. a hand-grafted shim) shows up here.
        assert_eq!(
            sources,
            std::collections::BTreeSet::from(["native"]),
            "expected only `native` stack subcommands",
        );

        // The stack subcommand set in the generated schema must match
        // the NATIVE_COMMANDS registry exactly. Under the old shim
        // these were two hand-maintained lists that could drift (a
        // dropped leaf would silently vanish from the reference); now
        // both derive from the same clap tree, so pin the invariant.
        let schema_stack: std::collections::BTreeSet<&str> = stack["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().expect("name"))
            .collect();
        let registry_stack: std::collections::BTreeSet<&str> = NATIVE_COMMANDS
            .iter()
            .filter(|(group, _)| *group == "stack")
            .map(|(_, sub)| *sub)
            .collect();
        assert_eq!(
            schema_stack, registry_stack,
            "stack subcommands in the schema drifted from NATIVE_COMMANDS",
        );

        // The exit-code contract rides alongside the command tree,
        // sourced from `mergify_core::ExitCode`. Pin the codes so a
        // dropped or renumbered variant surfaces here.
        let codes: Vec<u64> = v["exitCodes"]
            .as_array()
            .expect("exitCodes array")
            .iter()
            .map(|c| c["code"].as_u64().expect("code"))
            .collect();
        assert_eq!(codes, [0, 1, 3, 4, 5, 6, 7, 8]);
    }

    /// Golden snapshot of the whole CLI surface. Catches drift the
    /// structural checks above can't — a dropped flag, a renamed leaf
    /// subcommand, a changed `value_hint`, reworded help. Review
    /// intentional changes with `cargo insta review`. The
    /// version field is release-stamped, so redact it.
    #[test]
    fn cli_schema_golden() {
        let schema: serde_json::Value =
            serde_json::from_str(&cli_schema::dump()).expect("schema is valid JSON");
        insta::assert_json_snapshot!(schema, { ".cli.version" => "[version]" });
    }

    #[test]
    fn version_const_matches_release_env_or_falls_back_to_cargo_pkg_version() {
        // Mirror `build.rs` exactly: empty `MERGIFY_RELEASE_VERSION`
        // is treated the same as unset, both collapse to
        // `CARGO_PKG_VERSION`. The release path is exercised in
        // `build-wheels.yml`'s stamp step (sets a non-empty calver,
        // expects it in `--version`); this test pins both fallback
        // branches so build.rs's normalisation can't drift.
        let expected = match option_env!("MERGIFY_RELEASE_VERSION") {
            Some(v) if !v.is_empty() => v,
            _ => env!("CARGO_PKG_VERSION"),
        };
        assert_eq!(VERSION, expected);
    }

    #[test]
    fn cli_root_exposes_version_flag() {
        // Locked-in regression: a previous incarnation had
        // `disable_version_flag = true` because the version source
        // was wrong; switching to the env-driven const lets clap
        // render `--version` properly. If a future refactor
        // removes the `version = VERSION` attribute, `--version`
        // silently becomes an unknown flag and this catches it.
        //
        // `try_parse_from(...).err().expect(...)` over `expect_err`
        // because the Ok variant is `CliRoot` which doesn't
        // implement `Debug` (and shouldn't — it carries no debug
        // intent).
        let err = CliRoot::try_parse_from(["mergify", "--version"])
            .err()
            .expect("--version exits");
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
        assert!(
            err.to_string().contains(VERSION),
            "version output should contain VERSION ({VERSION}), got: {err}",
        );
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

    // Unknown stack subcommands exit via `clap::Error::exit()`
    // (process::exit(2) + "did you mean?" output), which can't
    // be unit-tested without subprocess plumbing. End-to-end
    // smoke is covered by clap's own conformance tests.

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

    #[test]
    fn tests_quarantines_add_dispatches_natively() {
        let parsed = parse(&[
            "tests",
            "quarantines",
            "add",
            "test_login",
            "--reason",
            "flaky",
            "-r",
            "owner/repo",
        ]);
        let Dispatch::Native(NativeCommand::TestsQuarantine(opts)) = dispatch_from_parsed(parsed)
        else {
            panic!("tests quarantines add must dispatch to TestsQuarantine");
        };
        assert_eq!(opts.test_name, "test_login");
        assert_eq!(opts.reason, "flaky");
        assert_eq!(opts.repository.as_deref(), Some("owner/repo"));
    }

    #[test]
    fn tests_quarantines_get_dispatches_natively() {
        let parsed = parse(&[
            "tests",
            "quarantines",
            "get",
            "test_login",
            "-r",
            "owner/repo",
        ]);
        let Dispatch::Native(NativeCommand::TestsQuarantineGet(opts)) =
            dispatch_from_parsed(parsed)
        else {
            panic!("tests quarantines get must dispatch to TestsQuarantineGet");
        };
        assert_eq!(opts.name_or_id, "test_login");
        assert_eq!(opts.repository.as_deref(), Some("owner/repo"));
    }

    #[test]
    fn removed_flat_quarantine_commands_are_rejected_by_clap() {
        // The deprecated flat commands were removed; only the
        // `quarantines` subgroup routes natively now. Without the
        // Python shim there's no fallback — clap rejects the
        // unknown subcommand at parse time. Use `try_parse_from`
        // (which returns Err instead of calling process::exit)
        // so we can assert in-process.
        for argv in [
            &["mergify", "tests", "quarantine"][..],
            &["mergify", "tests", "unquarantine"],
            &["mergify", "tests", "quarantined"],
        ] {
            assert!(
                CliRoot::try_parse_from(argv).is_err(),
                "{argv:?} must be rejected by clap (deprecated flat command)",
            );
        }
        // The replacement `quarantines` subgroup still parses
        // (proving the regression is targeted, not blanket).
        assert!(
            CliRoot::try_parse_from(["mergify", "tests", "quarantines", "add", "--help"])
                .is_err_and(|e| matches!(
                    e.kind(),
                    clap::error::ErrorKind::DisplayHelp
                        | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand,
                )),
            "the replacement subgroup must parse (help-display Err counts as parsed)",
        );
    }

    #[test]
    fn git_style_help_subcommand_is_supported() {
        // `mergify help` and `mergify help <cmd>` must be recognized
        // (clap surfaces help as a DisplayHelp "error"), not rejected
        // as an unknown subcommand the way they were while
        // `disable_help_subcommand` was set.
        for argv in [
            &["mergify", "help"][..],
            &["mergify", "help", "queue"],
            &["mergify", "help", "stack", "push"],
        ] {
            // `CliRoot` isn't `Debug`, so drop the Ok value with
            // `.err()` before inspecting the error kind.
            let kind = CliRoot::try_parse_from(argv).err().map(|e| e.kind());
            assert_eq!(
                kind,
                Some(clap::error::ErrorKind::DisplayHelp),
                "{argv:?} should display help, got {kind:?}",
            );
        }
    }
}
