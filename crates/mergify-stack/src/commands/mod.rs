//! Native implementations of `mergify stack <subcommand>`. One
//! module per subcommand. The `Stack(StackArgs)` variant in the
//! main binary dispatches into here for ported subcommands; the
//! rest still shim to Python.

pub mod drop;
pub mod edit;
pub mod fixup;
#[path = "move_cmd.rs"]
pub mod move_cmd;
pub mod new;
pub mod note;
pub mod reorder;
pub mod reword;
pub mod squash;
