//! Shared test scaffolding for the mergify CLI Rust port.
//!
//! Every ported command writes through a `&mut dyn Output`, so tests
//! need a way to feed it an in-memory sink and read back what the
//! command produced. Every test module used to re-roll the same
//! `SharedBytes` / `SharedWriter` / `Captured` / `make_output` trio
//! by hand — about 30 LOC × ~15 files of pure boilerplate, drifting
//! over time (some had `stderr`, some didn't; some named fields, some
//! used helper functions). This crate is the one canonical version.
//!
//! Typical usage:
//!
//! ```ignore
//! use mergify_test_support::Captured;
//!
//! let mut cap = Captured::human();
//! run(options, &mut cap.output).await.unwrap();
//! assert!(cap.stdout().contains("ok"));
//! ```
//!
//! Lives as a separate `dev-dependencies` crate (rather than a feature
//! on `mergify-core`) so the test-only types never leak into a
//! production build.

use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;

use mergify_core::OutputMode;
use mergify_core::StdioOutput;

/// Mutex-protected, `Arc`-shared byte buffer the captured output is
/// streamed into. Exposed so tests that need to take a snapshot
/// mid-run (or share the buffer across threads) can grab a clone
/// of the `Arc`.
pub type SharedBytes = Arc<Mutex<Vec<u8>>>;

/// `Write` adapter over a [`SharedBytes`] handle. Used as the
/// stdout/stderr sink for [`StdioOutput::with_sinks`].
pub struct SharedWriter(pub SharedBytes);

impl Write for SharedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// In-memory capture of an [`StdioOutput`] for tests.
///
/// Construct with [`Captured::human`] or [`Captured::new`], pass
/// `&mut cap.output` to the command under test, then assert on the
/// recorded bytes via [`Captured::stdout`] / [`Captured::stderr`].
pub struct Captured {
    pub output: StdioOutput,
    stdout: SharedBytes,
    stderr: SharedBytes,
}

impl Captured {
    /// Capture an [`OutputMode::Human`] output. The most common
    /// case — every command renders human output by default and
    /// only flips to JSON when the user passes `--json`.
    #[must_use]
    pub fn human() -> Self {
        Self::new(OutputMode::Human)
    }

    /// Capture with an explicit [`OutputMode`].
    #[must_use]
    pub fn new(mode: OutputMode) -> Self {
        let stdout: SharedBytes = Arc::new(Mutex::new(Vec::new()));
        let stderr: SharedBytes = Arc::new(Mutex::new(Vec::new()));
        let output = StdioOutput::with_sinks(
            mode,
            SharedWriter(Arc::clone(&stdout)),
            SharedWriter(Arc::clone(&stderr)),
        );
        Self {
            output,
            stdout,
            stderr,
        }
    }

    /// Captured stdout as a UTF-8 string. Panics if the captured
    /// bytes aren't valid UTF-8 — tests shouldn't be producing
    /// invalid UTF-8 in the first place, and panicking with a clear
    /// site beats silently lossy `String::from_utf8_lossy`.
    #[must_use]
    pub fn stdout(&self) -> String {
        String::from_utf8(self.stdout.lock().unwrap().clone()).expect("captured stdout is UTF-8")
    }

    /// Captured stderr as a UTF-8 string. See [`Self::stdout`] for
    /// the UTF-8 caveat.
    #[must_use]
    pub fn stderr(&self) -> String {
        String::from_utf8(self.stderr.lock().unwrap().clone()).expect("captured stderr is UTF-8")
    }

    /// Snapshot the captured stdout bytes (without UTF-8 validation).
    /// Useful when the command writes a binary or pre-encoded payload
    /// the test wants to compare byte-for-byte.
    #[must_use]
    pub fn stdout_bytes(&self) -> Vec<u8> {
        self.stdout.lock().unwrap().clone()
    }
}
