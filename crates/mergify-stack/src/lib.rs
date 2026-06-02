//! Native pieces of `mergify stack`, ported from
//! `mergify_cli/stack/`.
//!
//! Today this crate ships the stack-discovery walker that backs
//! every stack subcommand: read the local commits in
//! `<base>..<head>`, parse each commit's `Change-Id:` trailer,
//! and return one structured record per commit. The Python side
//! reaches it via the hidden `_internal stack-local-commits`
//! subcommand on the `mergify` binary; once `mergify stack list`
//! itself is native the same module is reused without the
//! subprocess hop.

pub mod change_id;
pub mod local_commits;
pub mod remote_changes;
pub mod slug;
