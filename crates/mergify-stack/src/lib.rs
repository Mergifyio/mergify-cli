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
//! - [`change_type`] — patch-id-based rebase-vs-content
//!   classification for force-pushed PR heads, plus the
//!   `refs/pull/<n>/head` fetch helper. Leaf-only port from
//!   `mergify_cli/stack/push.py`; the bridge that lets Python
//!   consume it ships in a follow-up.
//! - [`stack_comment`] — the "this PR is part of a stack"
//!   sticky comment renderer + header recogniser. Pure
//!   markdown/JSON formatting ported from
//!   `mergify_cli/stack/push.py::StackComment`.
//! - [`replay`] — full port of `mergify_cli/stack/replay.py`:
//!   `git merge-tree` + `git diff-tree` to materialise the
//!   amendment, then `POST /git/trees` + `POST /git/commits`
//!   to upload a synthetic commit that the revision-history
//!   compare URL anchors at.
//! - [`revision_history`] — the "Revision history" sticky
//!   comment renderer + parser. Ported from
//!   `mergify_cli/stack/push.py::RevisionHistoryComment`.
//! - [`approvals`] — the rebase/no-rebase decision for
//!   `stack push`: skip the rebase when PRs are already
//!   approved (so the approvals aren't dismissed) unless the
//!   bottom of the stack has a real merge conflict with
//!   trunk. Ported from `mergify_cli/stack/approvals.py`.
//! - [`notes_push`] — `git fetch`/`git push` plumbing for
//!   `refs/notes/mergify/stack` + the per-PR refspecs that
//!   `stack push` lands atomically with `--force-with-lease`.
//!   Ported from `mergify_cli/stack/push.py::{fetch_notes_ref,
//!   _merge_remote_notes, push_branches}`.

pub mod approvals;
pub mod change_id;
pub mod change_type;
pub mod changes;
pub mod commands;
pub mod local_commits;
pub mod notes_push;
pub mod rebase_todo;
pub mod remote_changes;
pub mod replay;
pub mod revision_history;
pub mod slug;
pub mod stack_comment;
pub mod stack_context;
pub mod sync_status;
pub mod trunk;

#[cfg(test)]
pub(crate) mod test_env;
