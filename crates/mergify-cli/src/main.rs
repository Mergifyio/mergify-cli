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
    ("queue", "pause"),
    ("queue", "unpause"),
    ("queue", "status"),
    ("queue", "show"),
    ("freeze", "list"),
    ("freeze", "create"),
    ("freeze", "update"),
    ("freeze", "delete"),
    // Internal Python migration helpers. Listed so `looks_native`
    // routes `mergify _internal …` past the shim fallback when
    // clap rejects it, but they stay hidden from `--help` (see
    // the `Subcommands::Internal` variant).
    ("_internal", "junit-parse"),
    ("_internal", "junit-upload"),
    ("_internal", "stack-local-commits"),
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
    QueuePause(QueuePauseOpts),
    QueueUnpause(QueueUnpauseOpts),
    QueueStatus(QueueStatusOpts),
    QueueShow(QueueShowOpts),
    FreezeList(FreezeListOpts),
    FreezeCreate(FreezeCreateOpts),
    FreezeUpdate(FreezeUpdateOpts),
    FreezeDelete(FreezeDeleteOpts),
    /// `_internal junit-parse <FILE>` — Python migration helper.
    /// Reads the `JUnit` XML file, parses it with the native Rust
    /// parser, prints the resulting cases as a JSON array. Wire
    /// format is not stable; only the Python code shipped in this
    /// wheel may consume it.
    InternalJunitParse {
        file: PathBuf,
    },
    /// `_internal junit-upload <FILE>… --token … --api-url … …`
    /// — Python migration helper. Parses every file, builds the
    /// OTLP `ExportTraceServiceRequest` with the quarantined set
    /// baked in, POSTs gzipped protobuf to the traces endpoint.
    /// Wire format is not stable; only the Python code shipped in
    /// this wheel may consume it.
    InternalJunitUpload(InternalJunitUploadOpts),
    /// `_internal stack-local-commits --base <sha> --head <ref>` —
    /// Python migration helper. Runs `git log` for the stack
    /// range, parses each commit's `Change-Id:` trailer, prints
    /// the result as a JSON array. Used by `mergify_cli/stack/changes.py`
    /// while the surrounding stack discovery logic is still
    /// Python. Wire format is not stable.
    InternalStackLocalCommits(InternalStackLocalCommitsOpts),
}

struct InternalJunitUploadOpts {
    api_url: String,
    token: String,
    repository: String,
    run_id: String,
    test_framework: Option<String>,
    test_language: Option<String>,
    mergify_test_job_name: Option<String>,
    quarantined: Vec<String>,
    files: Vec<PathBuf>,
}

struct InternalStackLocalCommitsOpts {
    base: String,
    head: String,
    repo_dir: Option<PathBuf>,
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
    repository: String,
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
    repository: String,
    test_name: String,
    reason: String,
    branch: Option<String>,
    token: Option<String>,
    api_url: Option<String>,
    json: bool,
}

struct TestsUnquarantineOpts {
    repository: String,
    name_or_id: String,
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

#[allow(clippy::too_many_lines)] // mostly mechanical match arms
fn dispatch_from_parsed(parsed: CliRoot) -> Dispatch {
    let debug = parsed.debug;
    match parsed.command {
        Subcommands::Stack(ShimmedArgs { args }) => {
            Dispatch::Shim(inject_global_flags(debug, prepend_one("stack", args)))
        }
        Subcommands::Internal(InternalArgs {
            command: InternalSubcommand::JunitParse(InternalJunitParseArgs { file }),
        }) => Dispatch::Native(NativeCommand::InternalJunitParse { file }),
        Subcommands::Internal(InternalArgs {
            command:
                InternalSubcommand::JunitUpload(InternalJunitUploadArgs {
                    api_url,
                    token,
                    repository,
                    run_id,
                    test_framework,
                    test_language,
                    mergify_test_job_name,
                    quarantined,
                    files,
                }),
        }) => Dispatch::Native(NativeCommand::InternalJunitUpload(
            InternalJunitUploadOpts {
                api_url,
                token,
                repository,
                run_id,
                test_framework,
                test_language,
                mergify_test_job_name,
                quarantined,
                files,
            },
        )),
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
                        repository: &opts.repository,
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
                        repository: &opts.repository,
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
                        repository: &opts.repository,
                        name_or_id: &opts.name_or_id,
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
            NativeCommand::InternalJunitParse { file } => {
                // Read the JUnit XML, parse it with the native
                // parser, emit the full `ParseResult` as JSON on
                // stdout — `{"suite_names": [...], "cases": [...]}`.
                // The Python `junit_to_spans` consumer in this same
                // wheel pipes the bytes back into the existing span
                // builder. Failures surface as a `CliError::Generic`
                // and exit non-zero — Python wraps that into
                // `InvalidJunitXMLError(stderr)`.
                let bytes = std::fs::read(&file).map_err(|e| {
                    mergify_core::CliError::Generic(format!("cannot read {}: {e}", file.display()))
                })?;
                let parsed = mergify_ci::junit_process::junit::parse(&bytes)?;
                let json = serde_json::to_string(&parsed).map_err(|e| {
                    mergify_core::CliError::Generic(format!("serialize junit-parse output: {e}"))
                })?;
                println!("{json}");
                Ok(mergify_core::ExitCode::Success)
            }
            NativeCommand::InternalJunitUpload(opts) => {
                // Parse every file, concatenate their cases /
                // suite_names, build OTLP spans with the quarantine
                // set baked in, POST. The Python orchestrator that
                // calls this has already done the quarantine check
                // and passes the names via repeated `--quarantined`.
                let mut all_cases = Vec::new();
                let mut all_suite_names = Vec::new();
                for path in &opts.files {
                    let bytes = std::fs::read(path).map_err(|e| {
                        mergify_core::CliError::Generic(format!(
                            "cannot read {}: {e}",
                            path.display(),
                        ))
                    })?;
                    let parsed = mergify_ci::junit_process::junit::parse(&bytes)?;
                    all_suite_names.extend(parsed.suite_names);
                    all_cases.extend(parsed.cases);
                }
                let parsed = mergify_ci::junit_process::junit::ParseResult {
                    suite_names: all_suite_names,
                    cases: all_cases,
                };

                let metadata = mergify_ci::junit_process::spans::UploadMetadata {
                    test_framework: opts.test_framework,
                    test_language: opts.test_language,
                    mergify_test_job_name: opts.mergify_test_job_name.or_else(|| {
                        env::var("MERGIFY_TEST_JOB_NAME")
                            .ok()
                            .filter(|s| !s.is_empty())
                    }),
                    run_id: Some(opts.run_id),
                    quarantined: opts.quarantined.into_iter().collect(),
                };
                let built = mergify_ci::junit_process::spans::build_traces(&parsed, &metadata)?;

                // No spans → nothing to send. Matches Python's
                // existing `if not spans: return` short-circuit.
                if built.request.resource_spans.is_empty() {
                    return Ok(mergify_core::ExitCode::Success);
                }

                let client = mergify_ci::junit_process::upload::default_client();
                mergify_ci::junit_process::upload::upload(
                    &client,
                    &opts.api_url,
                    &opts.token,
                    &opts.repository,
                    &built.request,
                )
                .await
                .map_err(|e| mergify_core::CliError::Generic(e.to_string()))?;
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
    /// Parse a `JUnit` XML file and print the parsed test cases
    /// as a JSON array to stdout. Used by the Python side of the
    /// `junit-process` command during migration; not a stable
    /// user-facing surface.
    #[command(name = "junit-parse")]
    JunitParse(InternalJunitParseArgs),
    /// Parse `JUnit` XML files, build the OTLP `ExportTraceServiceRequest`
    /// (one session span + one suite span per `<testsuite>` + one
    /// case span per `<testcase>`, tagged with the caller-supplied
    /// quarantine set), and POST it as gzipped protobuf to
    /// `{api_url}/v1/repos/{repository}/ci/traces`. Used by the
    /// Python side of the `junit-process` command during
    /// migration to replace the `opentelemetry-exporter-otlp-proto-http`
    /// upload path; not a stable user-facing surface.
    #[command(name = "junit-upload")]
    JunitUpload(InternalJunitUploadArgs),
    /// Walk the local stack commits in `<base>..<head>` and print
    /// a JSON array of `{commit_sha, title, message, change_id}`.
    /// Used by the Python side of `mergify stack <cmd>` during
    /// migration to centralise the `git log` + `Change-Id:`
    /// extraction. Not a stable user-facing surface.
    #[command(name = "stack-local-commits")]
    StackLocalCommits(InternalStackLocalCommitsArgs),
}

#[derive(clap::Args)]
struct InternalJunitParseArgs {
    /// Path to the `JUnit` XML file to parse.
    #[arg(value_name = "FILE")]
    file: PathBuf,
}

#[derive(clap::Args)]
struct InternalJunitUploadArgs {
    /// Mergify API base URL (e.g. `https://api.mergify.com`).
    #[arg(long = "api-url")]
    api_url: String,
    /// Mergify CI Insights bearer token.
    #[arg(long)]
    token: String,
    /// Repository the spans belong to, as `owner/repo`.
    #[arg(long)]
    repository: String,
    /// 16-character hex run identifier the Python orchestrator
    /// already printed to its UI. The session span's 8-byte ID
    /// decodes from this so wire spans line up with what the
    /// user sees in the CLI report.
    #[arg(long = "run-id")]
    run_id: String,
    /// Optional `test.framework` attribute applied to every span.
    #[arg(long = "test-framework")]
    test_framework: Option<String>,
    /// Optional `test.language` attribute applied to every span.
    #[arg(long = "test-language")]
    test_language: Option<String>,
    /// Optional `mergify.test.job.name` resource attribute. Falls
    /// back to `MERGIFY_TEST_JOB_NAME` env var when omitted.
    #[arg(long = "mergify-test-job-name")]
    mergify_test_job_name: Option<String>,
    /// Test names the quarantine API reported as currently
    /// quarantined. Each case span whose `name` matches gets
    /// `cicd.test.quarantined = true`. Repeatable.
    #[arg(long = "quarantined", value_name = "TEST_NAME")]
    quarantined: Vec<String>,
    /// `JUnit` XML files to parse and upload spans for.
    #[arg(value_name = "FILE", required = true, num_args = 1..)]
    files: Vec<PathBuf>,
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
    /// Path to YAML config file. Falls back to
    /// ``MERGIFY_CONFIG_PATH`` env var, then auto-detects
    /// `.mergify.yml`, `.mergify/config.yml`, or
    /// `.github/mergify.yml`.
    #[arg(long, env = "MERGIFY_CONFIG_PATH")]
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
    /// Repository full name (owner/repo). Falls back to
    /// ``GITHUB_REPOSITORY`` env var.
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

    /// Repository full name (owner/repo). Auto-detected from the
    /// CI environment when omitted.
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
    #[arg(long = "test-exit-code", short = 'e', env = "MERGIFY_TEST_EXIT_CODE")]
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
}

#[derive(clap::Args)]
struct TestsShowCliArgs {
    /// Test name(s) to look up. Glob patterns (`*`, `?`) are
    /// supported by the API.
    #[arg(value_name = "NAME", required = true, num_args = 1..)]
    test_names: Vec<String>,

    /// Repository full name (owner/repo).
    #[arg(
        long,
        short = 'r',
        required = true,
        value_parser = mergify_ci::detector::parse_owner_repo,
    )]
    repository: String,

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

    /// Repository full name (owner/repo).
    #[arg(
        long,
        short = 'r',
        required = true,
        value_parser = mergify_ci::detector::parse_owner_repo,
    )]
    repository: String,

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

    /// Repository full name (owner/repo).
    #[arg(
        long,
        short = 'r',
        required = true,
        value_parser = mergify_ci::detector::parse_owner_repo,
    )]
    repository: String,

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
