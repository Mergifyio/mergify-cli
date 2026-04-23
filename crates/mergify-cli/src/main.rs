//! `mergify` binary entry point.
//!
//! Dispatch logic: every invocation is speculatively parsed with
//! clap, which knows about the native commands
//! ([`ConfigSubcommand::Validate`], [`ConfigSubcommand::Simulate`]).
//! If clap succeeds with a known native variant the binary runs
//! that code path natively. Any parse failure — including
//! subcommands clap doesn't know about (``stack push``, ``ci
//! junit-process``, …) — falls through to [`mergify_py_shim::run`],
//! which hands the original argv to ``python3 -m mergify_cli``.
//!
//! As each command ports (Phase 1.4+), new variants land on the
//! clap enum and the shim fallback shrinks. Phase 6 deletes the
//! shim entirely.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use clap::Subcommand;
use mergify_ci::scopes_send::ScopesSendOptions;
use mergify_config::simulate::PullRequestRef;
use mergify_config::simulate::SimulateOptions;
use mergify_core::OutputMode;
use mergify_core::StdioOutput;
use mergify_queue::pause::PauseOptions;
use mergify_queue::unpause::UnpauseOptions;

fn main() -> ExitCode {
    let argv: Vec<String> = env::args().skip(1).collect();

    if let Some(cmd) = detect_native(&argv) {
        return run_native(cmd);
    }

    match mergify_py_shim::run(&argv) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("mergify: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Native commands the Rust binary handles without delegating to
/// the Python shim.
enum NativeCommand {
    ConfigValidate { config_file: Option<PathBuf> },
    ConfigSimulate(ConfigSimulateOpts),
    CiScopesSend(CiScopesSendOpts),
    QueuePause(QueuePauseOpts),
    QueueUnpause(QueueUnpauseOpts),
}

struct ConfigSimulateOpts {
    config_file: Option<PathBuf>,
    pull_request: PullRequestRef,
    token: Option<String>,
    api_url: Option<String>,
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
        matches!(
            (pair[0].as_str(), pair[1].as_str()),
            ("config", "validate" | "simulate")
                | ("ci", "scopes-send")
                | ("queue", "pause" | "unpause"),
        )
    })
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
fn detect_native(argv: &[String]) -> Option<NativeCommand> {
    let looks_native = looks_native(argv);

    let parsed = match CliRoot::try_parse_from(
        std::iter::once("mergify".to_string()).chain(argv.iter().cloned()),
    ) {
        Ok(parsed) => parsed,
        Err(err) if looks_native => {
            // Native intent + clap rejection = surface clap's error
            // and exit. ``err.exit()`` prints to stderr and calls
            // ``process::exit``; does not return.
            err.exit()
        }
        Err(_) => return None,
    };

    match parsed.command {
        Subcommands::Config(ConfigArgs {
            config_file,
            command: ConfigSubcommand::Validate(_),
        }) => Some(NativeCommand::ConfigValidate { config_file }),
        Subcommands::Config(ConfigArgs {
            config_file,
            command:
                ConfigSubcommand::Simulate(SimulateCliArgs {
                    pull_request,
                    token,
                    api_url,
                }),
        }) => Some(NativeCommand::ConfigSimulate(ConfigSimulateOpts {
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
        }) => Some(NativeCommand::CiScopesSend(CiScopesSendOpts {
            repository,
            pull_request,
            token,
            api_url,
            scopes: scope,
            scopes_json,
            scopes_file,
            file_deprecated,
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
        }) => Some(NativeCommand::QueuePause(QueuePauseOpts {
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
        }) => Some(NativeCommand::QueueUnpause(QueueUnpauseOpts {
            repository,
            token,
            api_url,
        })),
    }
}

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

    let mut output = StdioOutput::new(OutputMode::Human);

    let result = rt.block_on(async {
        match cmd {
            NativeCommand::ConfigValidate { config_file } => {
                mergify_config::validate::run(config_file.as_deref(), &mut output).await
            }
            NativeCommand::ConfigSimulate(opts) => {
                mergify_config::simulate::run(
                    SimulateOptions {
                        pull_request: &opts.pull_request,
                        config_file: opts.config_file.as_deref(),
                        token: opts.token.as_deref(),
                        api_url: opts.api_url.as_deref(),
                    },
                    &mut output,
                )
                .await
            }
            NativeCommand::CiScopesSend(opts) => {
                mergify_ci::scopes_send::run(
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
            }
            NativeCommand::QueuePause(opts) => {
                mergify_queue::pause::run(
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
            }
            NativeCommand::QueueUnpause(opts) => {
                mergify_queue::unpause::run(
                    UnpauseOptions {
                        repository: opts.repository.as_deref(),
                        token: opts.token.as_deref(),
                        api_url: opts.api_url.as_deref(),
                    },
                    &mut output,
                )
                .await
            }
        }
    });

    match result {
        Ok(()) => ExitCode::from(mergify_core::ExitCode::Success.as_u8()),
        Err(err) => {
            let code = err.exit_code();
            eprintln!("mergify: {err}");
            ExitCode::from(code.as_u8())
        }
    }
}

#[derive(Parser)]
#[command(name = "mergify", disable_help_subcommand = true)]
#[command(disable_version_flag = true, disable_help_flag = true)]
struct CliRoot {
    #[command(subcommand)]
    command: Subcommands,
}

#[derive(Subcommand)]
enum Subcommands {
    /// Manage Mergify configuration.
    Config(ConfigArgs),
    /// Mergify CI-related commands.
    Ci(CiArgs),
    /// Manage the Mergify merge queue.
    Queue(QueueArgs),
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
enum CiSubcommand {
    /// Send scopes tied to a pull request to Mergify.
    #[command(name = "scopes-send")]
    ScopesSend(ScopesSendCliArgs),
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
