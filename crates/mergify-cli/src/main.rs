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
use mergify_config::simulate::PullRequestRef;
use mergify_config::simulate::SimulateOptions;
use mergify_core::OutputMode;
use mergify_core::StdioOutput;

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
}

struct ConfigSimulateOpts {
    config_file: Option<PathBuf>,
    pull_request: PullRequestRef,
    token: Option<String>,
    api_url: Option<String>,
}

/// Try to recognize the invocation as a native command.
///
/// Returns ``None`` when the argv doesn't look like a native
/// command — callers fall back to the Python shim, which produces
/// the same error messages as before the port started. When the
/// argv obviously targets a native command (contains ``config``
/// and ``validate``/``simulate``) but clap can't parse it — e.g.
/// the user gave a bad flag or an invalid URL — this function
/// prints clap's formatted error to stderr and exits the process
/// with clap's exit code (2), matching the Python CLI's behavior
/// for argument errors.
fn detect_native(argv: &[String]) -> Option<NativeCommand> {
    let looks_native = {
        let has_config = argv.iter().any(|a| a == "config");
        let has_native_sub = argv.iter().any(|a| a == "validate" || a == "simulate");
        has_config && has_native_sub
    };

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
