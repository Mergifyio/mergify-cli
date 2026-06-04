//! Native pieces of `mergify stack`, ported from
//! `mergify_cli/stack/`.
//!
//! Today this crate ships:
//! - the stack-discovery walker that backs every stack subcommand:
//!   read the local commits in `<base>..<head>`, parse each
//!   commit's `Change-Id:` trailer, and return one structured
//!   record per commit. The Python side reaches it via the hidden
//!   `_internal stack-local-commits` subcommand on the `mergify`
//!   binary; once `mergify stack list` itself is native the same
//!   module is reused without the subprocess hop.
//! - [`trunk::get_trunk`] — resolve `<remote>/<branch>` for the
//!   current branch, ported from `utils.get_trunk`. Used by
//!   `stack new` and reusable by future `stack drop`/`stack edit`
//!   ports.
//! - [`commands::new`] — the native implementation of
//!   `mergify stack new`. First stack subcommand to land natively
//!   (the rest still shim to Python).

pub mod change_id;
pub mod commands;
pub mod local_commits;
pub mod rebase_todo;
pub mod remote_changes;
pub mod slug;
pub mod trunk;

#[cfg(test)]
pub(crate) mod test_env;
