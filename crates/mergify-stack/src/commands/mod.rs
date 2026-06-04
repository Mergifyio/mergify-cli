//! Native implementations of `mergify stack <subcommand>`. One
//! module per subcommand. The `Stack(StackArgs)` variant in the
//! main binary dispatches into here for ported subcommands; the
//! rest still shim to Python.

pub mod drop;
pub mod edit;
pub mod new;
pub mod note;
