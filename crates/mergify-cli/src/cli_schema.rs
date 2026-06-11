//! Machine-readable CLI schema — the CLI's equivalent of the API's
//! `OpenAPI` spec. [`build`] walks the clap `Command` tree rooted at
//! [`crate::CliRoot`] and emits every command, subcommand, and
//! argument (descriptions, flags, defaults, value sets) as JSON. The
//! docs site renders that JSON the same way it renders the `OpenAPI`
//! spec, so the published reference can never drift from the binary:
//! every field is sourced from a clap getter that the derive macro
//! populates from the Rust doc comments and `#[arg]`/`#[command]`
//! attributes — there is no hand-maintained docs metadata.

use std::collections::BTreeSet;

use clap::CommandFactory;
use serde::Serialize;

use crate::CliRoot;
use crate::StackCheckoutCli;
use crate::StackDropCli;
use crate::StackEditCli;
use crate::StackFixupCli;
use crate::StackHooksCli;
use crate::StackListCli;
use crate::StackMoveCli;
use crate::StackNewCli;
use crate::StackNoteCli;
use crate::StackOpenCli;
use crate::StackPushCli;
use crate::StackReorderCli;
use crate::StackRewordCli;
use crate::StackSetupCli;
use crate::StackSquashCli;
use crate::StackSyncCli;

/// Internal contract version. Bump when a field is renamed or removed
/// so the docs renderer can move in lockstep.
const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliSchema {
    schema_version: u32,
    generator: &'static str,
    cli: CliInfo,
    command: CommandNode,
}

#[derive(Serialize)]
struct CliInfo {
    name: String,
    version: &'static str,
    about: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandNode {
    /// Leaf name, e.g. `add`.
    name: String,
    /// Full invocation path, e.g. `["mergify", "tests", "quarantines", "add"]`.
    /// Drives the docs slug and grouping.
    path: Vec<String>,
    about: Option<String>,
    long_about: Option<String>,
    usage: String,
    aliases: Vec<String>,
    subcommand_required: bool,
    /// Always `"native"` — every command is introspected from
    /// clap now that the Python tree is gone. Kept as a field so
    /// the consumer-side schema doesn't have to special-case its
    /// removal across `SCHEMA_VERSION` bumps.
    source: &'static str,
    args: Vec<ArgNode>,
    commands: Vec<CommandNode>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ArgNode {
    id: String,
    /// `positional`, `flag` (takes no value), or `option`.
    kind: &'static str,
    short: Option<String>,
    long: Option<String>,
    value_names: Vec<String>,
    help: Option<String>,
    long_help: Option<String>,
    required: bool,
    global: bool,
    default: Option<String>,
    possible_values: Vec<PossibleValueNode>,
    /// clap's `ValueRange` rendering, e.g. `1`, `0..=1`, `1..`.
    num_args: Option<String>,
    env: Option<String>,
    value_hint: Option<&'static str>,
}

#[derive(Serialize)]
struct PossibleValueNode {
    name: String,
    help: Option<String>,
}

/// Build the schema for the whole CLI.
pub fn build() -> CliSchema {
    let mut root = CliRoot::command();
    // clap injects argument defaults (num_args, actions, value hints)
    // and propagates global args only during `build()`; walking an
    // unbuilt command yields null/wrong values for those fields.
    root.build();

    // Seed the path root from clap's configured name so `command.path[0]`
    // can't drift from `cli.name`.
    let command = command_node(&root, vec![root.get_name().to_string()], &BTreeSet::new());

    CliSchema {
        schema_version: SCHEMA_VERSION,
        generator: "mergify _internal dump-cli-schema",
        cli: CliInfo {
            name: root.get_name().to_string(),
            version: crate::VERSION,
            about: root.get_about().map(ToString::to_string),
        },
        command,
    }
}

/// Serialize the schema as pretty JSON with a trailing newline.
pub fn dump() -> String {
    let mut s = serde_json::to_string_pretty(&build()).expect("CLI schema serializes");
    s.push('\n');
    s
}

/// Print the schema to stdout. Entry point for `_internal dump-cli-schema`.
pub fn run() -> std::process::ExitCode {
    print!("{}", dump());
    std::process::ExitCode::SUCCESS
}

fn command_node(
    cmd: &clap::Command,
    path: Vec<String>,
    inherited_globals: &BTreeSet<String>,
) -> CommandNode {
    let about = cmd.get_about().map(ToString::to_string);
    // clap returns None for `long_about` when only the short `///`
    // line is set; mirror its own short→long fallback so the field is
    // never spuriously empty.
    let long_about = cmd
        .get_long_about()
        .map(ToString::to_string)
        .or_else(|| about.clone());

    // Each global arg is emitted once, at the command where it's
    // declared; descendants inherit it (clap propagates it into their
    // `get_arguments()` after `build()`) so we skip it there.
    let mut globals_seen = inherited_globals.clone();
    let mut args = Vec::new();
    for arg in cmd.get_arguments() {
        let id = arg.get_id().as_str();
        // clap's synthetic `--help`/`--version` and any `hide`-d arg
        // (e.g. the `stack` shim's trailing var-arg) are not part of
        // the user-facing reference.
        if id == "help" || id == "version" || arg.is_hide_set() {
            continue;
        }
        if arg.is_global_set() {
            if inherited_globals.contains(id) {
                continue;
            }
            globals_seen.insert(id.to_string());
        }
        args.push(arg_node(arg));
    }

    // `stack` is the last Python-shimmed group: its native subcommands
    // live in standalone parser structs outside the walkable tree, and
    // the rest are still in Python. Graft both in.
    let commands = if path.last().map(String::as_str) == Some("stack") {
        stack_subcommands(&path)
    } else {
        cmd.get_subcommands()
            .filter(|sub| !sub.is_hide_set())
            .map(|sub| {
                let mut child = path.clone();
                child.push(sub.get_name().to_string());
                command_node(sub, child, &globals_seen)
            })
            .collect()
    };

    let usage = render_usage(cmd, &path);

    CommandNode {
        name: cmd.get_name().to_string(),
        path,
        about,
        long_about,
        usage,
        aliases: cmd.get_visible_aliases().map(ToString::to_string).collect(),
        subcommand_required: cmd.is_subcommand_required_set(),
        source: "native",
        args,
        commands,
    }
}

fn arg_node(arg: &clap::Arg) -> ArgNode {
    let help = arg.get_help().map(ToString::to_string);
    let long_help = arg
        .get_long_help()
        .map(ToString::to_string)
        .or_else(|| help.clone());

    let defaults: Vec<String> = arg
        .get_default_values()
        .iter()
        .map(|v| v.to_string_lossy().into_owned())
        .collect();
    let default = (!defaults.is_empty()).then(|| defaults.join(","));

    let possible_values = arg
        .get_possible_values()
        .into_iter()
        .filter(|pv| !pv.is_hide_set())
        .map(|pv| PossibleValueNode {
            name: pv.get_name().to_string(),
            help: pv.get_help().map(ToString::to_string),
        })
        .collect();

    let kind = arg_kind(arg);
    // Flags take no value, but clap still derives a placeholder value
    // name from the id (e.g. `DEBUG` for `--debug`) — drop it so the
    // reference doesn't imply `--debug <DEBUG>`.
    let value_names = if kind == "flag" {
        Vec::new()
    } else {
        arg.get_value_names()
            .map(|names| names.iter().map(ToString::to_string).collect())
            .unwrap_or_default()
    };

    ArgNode {
        id: arg.get_id().as_str().to_string(),
        kind,
        short: arg.get_short().map(|c| c.to_string()),
        long: arg.get_long().map(ToString::to_string),
        value_names,
        help,
        long_help,
        required: arg.is_required_set(),
        global: arg.is_global_set(),
        default,
        possible_values,
        num_args: arg.get_num_args().map(|r| r.to_string()),
        env: arg.get_env().map(|e| e.to_string_lossy().into_owned()),
        value_hint: value_hint(arg),
    }
}

fn arg_kind(arg: &clap::Arg) -> &'static str {
    use clap::ArgAction;
    if arg.is_positional() {
        "positional"
    } else if matches!(
        arg.get_action(),
        ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count
    ) {
        "flag"
    } else {
        "option"
    }
}

fn value_hint(arg: &clap::Arg) -> Option<&'static str> {
    use clap::ValueHint;
    Some(match arg.get_value_hint() {
        ValueHint::Unknown => return None,
        ValueHint::AnyPath => "anyPath",
        ValueHint::FilePath => "filePath",
        ValueHint::DirPath => "dirPath",
        ValueHint::ExecutablePath => "executablePath",
        ValueHint::CommandName => "commandName",
        ValueHint::CommandString => "commandString",
        ValueHint::CommandWithArguments => "commandWithArguments",
        ValueHint::Username => "username",
        ValueHint::Hostname => "hostname",
        ValueHint::Url => "url",
        ValueHint::EmailAddress => "emailAddress",
        _ => "other",
    })
}

/// Render the usage line, overriding the bin name so it shows the full
/// invocation path (`mergify tests quarantines add …`). `render_usage`
/// needs `&mut`, so clone — the walk stays immutable.
fn render_usage(cmd: &clap::Command, path: &[String]) -> String {
    let mut cmd = cmd.clone().bin_name(path.join(" "));
    let usage = cmd.render_usage().to_string();
    // `render_usage` prefixes "Usage: "; the schema carries the bare line.
    usage.strip_prefix("Usage: ").unwrap_or(&usage).to_string()
}

/// Subcommands of `stack`: every native parser grafted in. They're
/// side-parsed outside `CliRoot` (the `stack` group is a
/// `trailing_var_arg` forwarder for clap's "did you mean?" suggestion
/// machinery), so a plain walk of `CliRoot` would silently drop them
/// from the published reference.
fn stack_subcommands(stack_path: &[String]) -> Vec<CommandNode> {
    let mut out = Vec::new();

    for mut native in [
        StackCheckoutCli::command(),
        StackDropCli::command(),
        StackEditCli::command(),
        StackFixupCli::command(),
        StackHooksCli::command(),
        StackListCli::command(),
        StackMoveCli::command(),
        StackNewCli::command(),
        StackNoteCli::command(),
        StackOpenCli::command(),
        StackPushCli::command(),
        StackReorderCli::command(),
        StackRewordCli::command(),
        StackSetupCli::command(),
        StackSquashCli::command(),
        StackSyncCli::command(),
    ] {
        native.build();
        let mut child = stack_path.to_vec();
        child.push(native.get_name().to_string());
        out.push(command_node(&native, child, &BTreeSet::new()));
    }

    out
}
