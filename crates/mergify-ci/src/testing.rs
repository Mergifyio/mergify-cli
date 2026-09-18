//! Test-only helpers shared across the CI-aware command modules
//! (`detector`, `scopes_send`, `tests_show`, `tests_quarantine`).

use std::path::Path;
use std::path::PathBuf;

/// Write `payload` as a GitHub Actions event file under `dir` and
/// return its path, ready to point `GITHUB_EVENT_PATH` at. The
/// payload-reading helpers across `detector` and `scopes_send` all
/// need one, and each spelling it out obscures which key the test is
/// actually about.
pub(crate) fn write_github_event(dir: &Path, payload: &serde_json::Value) -> PathBuf {
    let path = dir.join("event.json");
    std::fs::write(&path, payload.to_string()).expect("write event payload");
    path
}

/// A high-entropy alphanumeric string of `len` chars, seeded so
/// different callers produce different content. Used by the
/// `junit_process::split` tests to fill stack traces with data gzip
/// can't collapse — a repeating or low-entropy filler would compress
/// to almost nothing and never force a split, so the compressed-size
/// path would go untested. Alphanumeric output is XML-safe, so the
/// same helper works both for building `TestCase` values directly and
/// for embedding inside a `<failure>` body in an XML fixture.
pub(crate) fn incompressible(seed: u64, len: usize) -> String {
    // xorshift64 → one alphanumeric char per step.
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            // `state % ALPHABET.len()` is always < 62, so it fits
            // usize on every target and the conversion can't fail;
            // `unwrap_or(0)` keeps it panic-free and ties the range to
            // the alphabet (no magic number).
            let idx = usize::try_from(state % ALPHABET.len() as u64).unwrap_or(0);
            char::from(ALPHABET[idx])
        })
        .collect()
}
