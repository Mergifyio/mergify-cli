//! Structural three-way merge of JSON files, as a git merge driver.
//!
//! git's line merge reports a conflict whenever two edits touch the same
//! or adjacent lines. In a JSON file that is mostly noise: two pull
//! requests bumping neighbouring dependencies in a `package.json`, or
//! adding different endpoints to a generated schema, conflict for no
//! reason a person would recognise. This crate merges by structure
//! instead, and is run by git as `mergify merge-driver json %A %O %B`.
//!
//! The bar is **correct whenever it claims success, declining otherwise**.
//! A decline falls back to git's own line merge (see [`driver`]), so it
//! costs exactly what having no driver costs; a wrong merge would ship a
//! file nobody reviewed.
//!
//! # What it merges, and what it declines
//!
//! - **Objects** merge key by key. A key one side changed takes that
//!   side's value; a key both changed is merged recursively, and two
//!   different scalars (the same dependency bumped to two versions)
//!   decline. A key one side deleted is deleted, unless the other side
//!   changed it, which declines. An object both sides added is merged
//!   the same way with every key new.
//! - **Key order** is ours'. A key only theirs added goes in its sorted
//!   place when both sides keep the object sorted by key, and otherwise
//!   right after the key that precedes it in theirs. A reorder theirs
//!   made to keys ours also has is not carried over: the values all are,
//!   the order is ours'.
//! - **Arrays** merge element by element (diff3 over elements). A
//!   stretch only one side changed takes that side's version. A stretch
//!   both changed merges only when both edited the same elements in
//!   place — same count, nothing moved — and each element is then merged
//!   recursively; this is what lets two edits to two fields of the same
//!   `OpenAPI` parameter merge. Everything else declines, **including two
//!   insertions at the same point**: for a JSON-Schema `required` list
//!   either order is right, for an ordered pipeline neither might be,
//!   and nothing in the file says which kind of array it is. The same
//!   insertion on both sides is taken once.
//!
//! # Formatting
//!
//! The result is ours' text, edited, never a re-serialisation (see
//! [`merge`](crate::merge::merge)): unchanged regions are copied byte
//! for byte and theirs' changes are spliced in as theirs wrote them,
//! shifted to ours' indentation. An item theirs adds takes the separator
//! ours uses in that container. For a file both sides wrote with the same
//! formatter, the result is that formatter's output.
//!
//! What it cannot promise is that the result equals what a *generator*
//! would produce from the merged sources. A schema generated in
//! declaration order puts two new fields of one model where the merged
//! source declares them, which no merge of the outputs can know. Where CI
//! regenerates the file and compares, such a difference turns the batch
//! red rather than landing silently: a safe failure, but a failure.

mod driver;
mod merge;
mod sequence;
mod value;

pub use driver::DriverOptions;
pub use driver::run;
pub use merge::Decline;
pub use merge::Reason;
pub use merge::Side;
pub use merge::merge;
