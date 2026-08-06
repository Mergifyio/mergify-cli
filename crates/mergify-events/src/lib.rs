//! Shared client for the Mergify activity log
//! (`GET /v1/repos/<owner>/<repo>/logs`).
//!
//! The activity log carries every event type the engine records —
//! the `action.queue.*` family, the workflow actions, `ci_insights.*`,
//! `command.*`, … — behind one filtered, paginated endpoint. Its
//! contract has sharp edges that every hand-rolled consumer gets
//! subtly wrong, so this crate owns them once:
//!
//! - **The window is always explicit.** The API defaults
//!   `received_from` to `received_to - 1 day`, which makes a pull
//!   request dequeued last week look identical to one never queued.
//!   A [`Window`] carries both bounds and every request sends them;
//!   the silent 1-day default is unreachable from here.
//! - **The 93-day cap is a typed error.** The API rejects a range
//!   wider than [`MAX_SPAN_DAYS`] with a raw 422 while retention is
//!   [`RETENTION_DAYS`] days. [`Window`] refuses to construct such a
//!   range ([`WindowError`]), and "everything retained" is spelled
//!   [`Window::retained`] — so the 422 cannot happen and the widest
//!   useful window has a name.
//! - **Pagination is followed to completion.** The endpoint pages
//!   with opaque cursors in RFC 5988 `Link` headers; [`fetch`]
//!   follows them until the window is exhausted (or an explicit
//!   [`Query::limit`] is reached) rather than silently returning the
//!   first page.
//! - **Ordering is guaranteed newest-first.** The API serves events
//!   newest-first; [`fetch`] enforces it after collecting, so it is a
//!   promise of this crate rather than an observation about today's
//!   server.
//! - **Unknown fields pass through verbatim.** An [`Event`] keeps the
//!   API's raw object untouched next to a decoded envelope of the
//!   fields every event shares — a newer engine cannot break an older
//!   CLI, and `--json` consumers see Mergify's contract, not this
//!   crate's.
//!
//! On top of the raw client sits the queue-leave **explain layer**
//! ([`queue_leave`]): decode a pull request's last
//! `action.queue.leave` and render why it left — the engine's own
//! reason prose, the failing checks with their job URLs, and the next
//! step. `queue show`'s dequeue diagnosis is a consumer of this
//! crate, not a second implementation of the contract.

pub mod client;
pub mod event;
pub mod list;
pub mod queue_leave;
pub mod window;

pub use client::{Query, fetch};
pub use event::Event;
pub use window::{MAX_SPAN_DAYS, RETENTION_DAYS, Window, WindowError};
