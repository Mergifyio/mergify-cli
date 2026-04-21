//! Command output abstraction.
//!
//! Every ported command writes its result through an [`Output`]
//! trait object rather than calling `println!` / `eprintln!`
//! directly. This keeps the JSON and human rendering paths
//! symmetric, lets commands stay test-friendly, and gives a single
//! place to enforce the "stdout must be a single JSON document
//! under `--json`" invariant (Phase 0.3).
//!
//! The trait is deliberately small. Commands emit one "result"
//! value that knows how to render itself both as JSON (via
//! [`serde::Serialize`]) and as human prose (via a closure). More
//! specialized affordances — progress bars, spinners, tables — are
//! added as ported commands need them.

use std::io::{self, Write};

use serde::Serialize;

/// Abstraction over the two rendering modes a command can emit
/// into. Commands take `&mut dyn Output` so tests can swap in a
/// fake sink — the stock approach is [`StdioOutput::with_sinks`]
/// plus a custom `Write` (see the test module for an example
/// using `Arc<Mutex<Vec<u8>>>`).
pub trait Output {
    /// Emit a value as the command's primary result.
    ///
    /// In JSON mode this serializes `value` and writes it to the
    /// stdout sink as a single JSON document. In human mode the
    /// provided `human` closure is invoked with the stdout sink
    /// instead — this is where the command renders tables, prose,
    /// colored output, etc.
    ///
    /// Commands must call this exactly once per invocation.
    fn emit(
        &mut self,
        value: &dyn ErasedSerialize,
        human: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>,
    ) -> io::Result<()>;

    /// Emit a progress or status message. In JSON mode this is a
    /// no-op to preserve stdout purity; in human mode it writes to
    /// stderr so piping stdout into a file is unaffected.
    fn status(&mut self, message: &str) -> io::Result<()>;
}

/// `dyn Serialize` cannot be constructed directly because
/// `Serialize` is not object-safe. This trait is — any
/// `&T: Serialize` can be passed as `&dyn ErasedSerialize`.
///
/// Returns a `Result` so serialization failures (custom `Serialize`
/// impls, unrepresentable values, etc.) are surfaced through
/// [`Output::emit`] rather than silently producing `null`.
pub trait ErasedSerialize {
    fn to_json_value(&self) -> serde_json::Result<serde_json::Value>;
}

impl<T: Serialize + ?Sized> ErasedSerialize for T {
    fn to_json_value(&self) -> serde_json::Result<serde_json::Value> {
        serde_json::to_value(self)
    }
}

/// Production [`Output`] that writes to the real streams
/// (stdout / stderr).
pub struct StdioOutput {
    mode: OutputMode,
    stdout: Box<dyn Write + Send>,
    stderr: Box<dyn Write + Send>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OutputMode {
    Human,
    Json,
}

impl StdioOutput {
    #[must_use]
    pub fn new(mode: OutputMode) -> Self {
        Self {
            mode,
            stdout: Box::new(io::stdout()),
            stderr: Box::new(io::stderr()),
        }
    }

    /// Construct with explicit sinks. Used by tests to capture
    /// output.
    pub fn with_sinks<O, E>(mode: OutputMode, stdout: O, stderr: E) -> Self
    where
        O: Write + Send + 'static,
        E: Write + Send + 'static,
    {
        Self {
            mode,
            stdout: Box::new(stdout),
            stderr: Box::new(stderr),
        }
    }
}

impl Output for StdioOutput {
    fn emit(
        &mut self,
        value: &dyn ErasedSerialize,
        human: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>,
    ) -> io::Result<()> {
        match self.mode {
            OutputMode::Json => {
                let json = value
                    .to_json_value()
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                let mut rendered = serde_json::to_string_pretty(&json)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                rendered.push('\n');
                self.stdout.write_all(rendered.as_bytes())
            }
            OutputMode::Human => human(&mut *self.stdout),
        }
    }

    fn status(&mut self, message: &str) -> io::Result<()> {
        match self.mode {
            OutputMode::Json => Ok(()),
            OutputMode::Human => writeln!(self.stderr, "{message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Result {
        valid: bool,
        errors: Vec<String>,
    }

    type SharedBytes = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

    struct Captured {
        output: StdioOutput,
        stdout: SharedBytes,
        stderr: SharedBytes,
    }

    fn captured(mode: OutputMode) -> Captured {
        let stdout: SharedBytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stderr: SharedBytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = StdioOutput::with_sinks(
            mode,
            SharedWriter(std::sync::Arc::clone(&stdout)),
            SharedWriter(std::sync::Arc::clone(&stderr)),
        );
        Captured {
            output,
            stdout,
            stderr,
        }
    }

    struct SharedWriter(SharedBytes);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn json_mode_writes_pretty_json_to_stdout_and_nothing_to_stderr() {
        let mut cap = captured(OutputMode::Json);
        let value = Result {
            valid: false,
            errors: vec!["bad thing".to_string()],
        };
        cap.output.status("fetching schema…").unwrap();
        cap.output
            .emit(&value, &mut |_| {
                unreachable!("human closure must not run in JSON mode")
            })
            .unwrap();

        let stdout_bytes = cap.stdout.lock().unwrap().clone();
        let stdout_str = String::from_utf8(stdout_bytes).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&stdout_str).unwrap();
        assert_eq!(parsed["valid"], false);
        assert_eq!(parsed["errors"][0], "bad thing");

        // `status` is a no-op in JSON mode.
        assert!(cap.stderr.lock().unwrap().is_empty());
    }

    #[test]
    fn human_mode_writes_closure_output_to_stdout_and_status_to_stderr() {
        let mut cap = captured(OutputMode::Human);
        let value = Result {
            valid: true,
            errors: vec![],
        };
        cap.output.status("fetching schema…").unwrap();
        cap.output
            .emit(&value, &mut |w| writeln!(w, "Configuration OK"))
            .unwrap();

        assert_eq!(
            String::from_utf8(cap.stdout.lock().unwrap().clone()).unwrap(),
            "Configuration OK\n",
        );
        assert_eq!(
            String::from_utf8(cap.stderr.lock().unwrap().clone()).unwrap(),
            "fetching schema…\n",
        );
    }
}
