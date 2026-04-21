//! `mergify` binary entry point.
//!
//! Dispatch logic:
//! - If the invocation is `mergify config validate [--config-file
//!   PATH]`, run it natively via `mergify_config::validate`.
//! - Anything else is handed to `mergify_py_shim::run`, which
//!   extracts the embedded Python source on first use and invokes
//!   `python3 -m mergify_cli`.
//!
//! As each command ports (Phase 1.4+), native dispatch grows and
//! the shim fallback shrinks. Phase 6 deletes the shim entirely.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use clap::Subcommand;
use mergify_core::OutputMode;
use mergify_core::StdioOutput;

fn main() -> ExitCode {
    let argv: Vec<String> = env::args().skip(1).collect();

    if let Some(NativeCommand::ConfigValidate(opts)) = detect_native(&argv) {
        return run_native_config_validate(&opts);
    }

    match mergify_py_shim::run(&argv) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("mergify: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_native_config_validate(opts: &ConfigValidateOpts) -> ExitCode {
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
    let result = rt.block_on(mergify_config::validate::run(
        opts.config_file.as_deref(),
        &mut output,
    ));

    match result {
        Ok(()) => ExitCode::from(mergify_core::ExitCode::Success.as_u8()),
        Err(err) => {
            let code = err.exit_code();
            // Commands that emit their own details through `Output`
            // (e.g. `config validate`'s per-error list) still get a
            // top-level "mergify: <message>" line appended, matching
            // the Python CLI's behavior.
            eprintln!("mergify: {err}");
            ExitCode::from(code.as_u8())
        }
    }
}

/// Recognised native commands, paired with their pre-parsed options.
enum NativeCommand {
    ConfigValidate(ConfigValidateOpts),
}

#[derive(Debug, Default)]
struct ConfigValidateOpts {
    config_file: Option<PathBuf>,
}

/// Try to recognise the invocation as a native command. Returns
/// `None` when the argv doesn't match any native command or when
/// clap rejects it — in both cases the caller falls back to the
/// Python shim (which produces the same error messages as before
/// the port started).
fn detect_native(argv: &[String]) -> Option<NativeCommand> {
    // Quick cheap check: argv must contain "config" followed by
    // "validate" to possibly be a native match. This avoids running
    // clap on every command.
    let has_config_validate = argv
        .iter()
        .position(|a| a == "config")
        .is_some_and(|i| argv.get(i + 1).is_some_and(|a| a == "validate"));
    if !has_config_validate {
        return None;
    }

    match CliRoot::try_parse_from(
        std::iter::once("mergify".to_string()).chain(argv.iter().cloned()),
    ) {
        Ok(CliRoot {
            command:
                Subcommands::Config(ConfigArgs {
                    config_file,
                    command: ConfigSubcommand::Validate(_),
                }),
        }) => Some(NativeCommand::ConfigValidate(ConfigValidateOpts {
            config_file,
        })),
        _ => None,
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
}

#[derive(clap::Args)]
struct ValidateArgs {}
