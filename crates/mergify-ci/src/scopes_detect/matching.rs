//! Glob-pattern matching: file path → matching scopes.
//!
//! The engine derives a pull request's scopes from the very same
//! `scopes:` block, in `mergify_engine/rules/globs.py` (brace
//! expansion, then `glob.translate`, matched with the third-party
//! `regex` module's `.match()` — `glob.translate` end-anchors its
//! output with `\z`, so that is a full match), and it — not this
//! command — is what the merge queue acts on. So the
//! semantics here have to track that module rather than globset's
//! defaults, or one config means two things and `ci scopes` picks
//! CI jobs for a scope set the engine never derives:
//!
//! - `*` and `?` stop at `/` (`literal_separator(true)`). `src/*.py`
//!   is `src`'s own Python files, not the whole subtree; only `**`
//!   crosses directories. This is also what
//!   `/configuration/data-types` documents.
//! - `{a,b}` alternation means the same on both sides — globset
//!   compiles it inline, the engine expands it into one pattern per
//!   branch (MRGFY-8359).
//! - `?` and `[...]` match one character, not one byte. globset
//!   parses the glob, but its regex is recompiled in Unicode mode
//!   (MRGFY-10066), since the engine matches `str`.
//!
//! Known differences that a real path or pattern can reach:
//!
//! - `\` is a path separator for the engine (`seps=["/", "\\"]`)
//!   and an escape here, so a changed file whose name contains a
//!   backslash (legal on Linux) can land in different scopes.
//! - `[^x]` negates here; `glob.translate` reads the `^` as a
//!   literal member (only `[!x]` negates there).
//! - A class spanning a `/` (`x[a/b]y`) is a class here and literal
//!   brackets for the engine, which splits on separators first.
//!
//! The rest need a path git never emits: a leading `/`, a trailing
//! `/`, an empty segment (`a//b`), or the empty string. An
//! unterminated `[` is the one place globset is stricter — it
//! rejects the pattern where the engine degrades it to a literal —
//! and erroring out on a malformed config is the side to be on.
//!
//! One intentional behavior: a pattern with an empty `include` list
//! follows the engine and matches every path (the scope's exclude
//! list then decides) — the YAML deserializer fills the default
//! `["**/*"]` for us, so this fallthrough is mostly defensive.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use globset::GlobBuilder;
use mergify_core::CliError;
use regex::Regex;
use regex::RegexBuilder;

use super::config::FileFilters;

/// Pre-built matchers for one scope.
#[derive(Debug)]
pub struct ScopeMatcher {
    pub name: String,
    include: Vec<Regex>,
    exclude: Vec<Regex>,
}

impl ScopeMatcher {
    fn matches(&self, path: &str) -> bool {
        // Mirrors the Python branch: if both lists are empty the
        // scope is inert. With the YAML default in place,
        // `include` is never actually empty here, but the guard is
        // kept so a programmatic caller with `FileFilters::default
        // ()` doesn't get every file classified into the scope.
        if self.include.is_empty() && self.exclude.is_empty() {
            return false;
        }
        let positive = if self.include.is_empty() {
            true
        } else {
            self.include.iter().any(|g| g.is_match(path))
        };
        if !positive {
            return false;
        }
        !self.exclude.iter().any(|g| g.is_match(path))
    }
}

/// Compile every scope's include/exclude lists once up front so
/// the per-file loop below isn't doing repeated glob construction.
pub fn compile(filters: &BTreeMap<String, FileFilters>) -> Result<Vec<ScopeMatcher>, CliError> {
    filters
        .iter()
        .map(|(name, f)| {
            Ok(ScopeMatcher {
                name: name.clone(),
                include: compile_list(name, &f.include)?,
                exclude: compile_list(name, &f.exclude)?,
            })
        })
        .collect()
}

fn compile_list(scope: &str, patterns: &[String]) -> Result<Vec<Regex>, CliError> {
    patterns.iter().map(|pat| build_glob(scope, pat)).collect()
}

fn build_glob(scope: &str, pattern: &str) -> Result<Regex, CliError> {
    // `literal_separator(true)` is the whole parity story for
    // everything but `**`: it compiles `*` to `[^/]*` and `?` to
    // `[^/]`, which is what `glob.translate` emits and what the
    // data-types page documents. globset defaults it off, and that
    // default is what used to let `*.md` swallow every `.md` in the
    // tree. `case_insensitive(false)` is the default but stated for
    // the record — file paths are case-sensitive on the platforms
    // Mergify cares about.
    let invalid = |reason: String| {
        CliError::Configuration(format!(
            "invalid glob {pattern:?} under scope {scope:?}: {reason}"
        ))
    };
    let glob = GlobBuilder::new(pattern)
        .literal_separator(true)
        .case_insensitive(false)
        .build()
        .map_err(|e| invalid(e.to_string()))?;
    // Not the config's fault: globset changed its output format.
    let unicode = unicode_regex(glob.regex()).ok_or_else(|| {
        CliError::Generic(format!(
            "cannot read globset's regex {:?} for glob {pattern:?}",
            glob.regex()
        ))
    })?;
    // globset builds its own matcher with `.` matching `\n`; keep it.
    RegexBuilder::new(&unicode)
        .dot_matches_new_line(true)
        .build()
        .map_err(|e| invalid(e.to_string()))
}

/// Rewrite globset's byte regex into the same regex over characters.
///
/// globset emits `(?-u)` up front and spells every non-ASCII glob
/// character as its UTF-8 bytes, so `?` (`[^/]`) matches one byte
/// and `[é]` is the byte class `[\xc3\xa9]`. Dropping `(?-u)` and
/// folding each `\xNN` run back into its character gives the
/// engine's semantics, where both match one character. `None` means
/// globset's output no longer has that shape.
fn unicode_regex(byte_regex: &str) -> Option<String> {
    let body = byte_regex.strip_prefix("(?-u)")?;
    let mut out = String::with_capacity(body.len());
    let mut bytes = Vec::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            flush_utf8(&mut bytes, &mut out)?;
            out.push(c);
            continue;
        }
        let escaped = chars.next()?;
        if escaped == 'x' {
            let hi = chars.next()?.to_digit(16)?;
            let lo = chars.next()?.to_digit(16)?;
            bytes.push(u8::try_from(hi << 4 | lo).ok()?);
        } else {
            flush_utf8(&mut bytes, &mut out)?;
            out.push('\\');
            out.push(escaped);
        }
    }
    flush_utf8(&mut bytes, &mut out)?;
    Some(out)
}

fn flush_utf8(bytes: &mut Vec<u8>, out: &mut String) -> Option<()> {
    out.push_str(std::str::from_utf8(bytes).ok()?);
    bytes.clear();
    Some(())
}

/// Result of routing a set of changed files through every scope
/// matcher. `hit` is the set of scope names with at least one
/// match; `by_scope` maps each hit scope to the files that hit
/// it (used for the verbose `ACTIONS_STEP_DEBUG=true` listing).
pub struct MatchResult {
    pub hit: BTreeSet<String>,
    pub by_scope: BTreeMap<String, Vec<String>>,
}

pub fn route<'a, I>(files: I, matchers: &[ScopeMatcher]) -> MatchResult
where
    I: IntoIterator<Item = &'a str>,
{
    let mut hit: BTreeSet<String> = BTreeSet::new();
    let mut by_scope: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in files {
        // globset's own matcher rewrote `\\` to `/` before matching
        // on non-Unix targets; the Windows binary keeps doing so.
        #[cfg(not(unix))]
        let file = &file.replace('\\', "/");
        for m in matchers {
            if m.matches(file) {
                hit.insert(m.name.clone());
                by_scope
                    .entry(m.name.clone())
                    .or_default()
                    .push(file.to_string());
            }
        }
    }
    MatchResult { hit, by_scope }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filters(include: &[&str], exclude: &[&str]) -> FileFilters {
        FileFilters {
            include: include.iter().map(|s| (*s).to_string()).collect(),
            exclude: exclude.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn compile_one(include: &[&str], exclude: &[&str]) -> Vec<ScopeMatcher> {
        let mut m = BTreeMap::new();
        m.insert("s".to_string(), filters(include, exclude));
        compile(&m).expect("globs compile")
    }

    /// Assert a single `include` pattern's verdict on one path.
    fn assert_matches(cases: &[(&str, &str, bool)]) {
        for &(pattern, path, expected) in cases {
            let ms = compile_one(&[pattern], &[]);
            let hit = route([path], &ms).hit.contains("s");
            assert_eq!(
                hit, expected,
                "{pattern:?} vs {path:?}: expected match={expected}",
            );
        }
    }

    #[test]
    fn single_star_and_question_mark_stop_at_a_separator() {
        // MRGFY-8392. globset's default (`literal_separator(false)`)
        // compiles `*` to `.*`, so `src/*.py` used to claim
        // `src/deep/nested.py` — a file the engine, which compiles
        // the same pattern to `src[/\\][^/\\]*\.py`, never puts in
        // the scope. Each pattern below is paired with the path the
        // two sides disagreed on and one they always agreed on, so
        // the bound is pinned without pinning `*` shut entirely.
        assert_matches(&[
            ("src/*.py", "src/deep/nested.py", false),
            ("src/*.py", "src/main.py", true),
            ("*.md", "docs/readme.md", false),
            ("*.md", "readme.md", true),
            (
                ".github/workflows/*",
                ".github/workflows/nested/ci.yml",
                false,
            ),
            (".github/workflows/*", ".github/workflows/ci.yml", true),
            ("package*.json", "packages/ui/tsconfig.json", false),
            ("package*.json", "package-lock.json", true),
            ("a?c", "a/c", false),
            ("a?c", "abc", true),
        ]);
    }

    #[test]
    fn separator_bounded_star_in_exclude_does_not_drop_a_scope() {
        // The direction that actually skipped CI. With `*` crossing
        // `/`, an `exclude: ['*.md']` swallowed every markdown file
        // in the tree, so `ci scopes` reported *fewer* scopes than
        // the engine and the matching CI job never ran for a pull
        // request the engine did consider in scope. Bounded, the
        // exclude only covers root-level markdown, as documented.
        let ms = compile_one(&["**/*"], &["*.md"]);
        let res = route(["docs/guide.md"], &ms);
        assert!(
            res.hit.contains("s"),
            "nested markdown must stay in scope, got {:?}",
            res.hit,
        );
        let res = route(["README.md"], &ms);
        assert!(res.hit.is_empty(), "unexpected hit: {:?}", res.hit);
    }

    #[test]
    fn double_star_is_the_only_wildcard_that_crosses_directories() {
        // `**` keeps its recursive meaning, including the "zero
        // segments" case (`**/x` matches a root-level `x`) that the
        // engine's `(?:.+[/\\])?` prefix also allows. Without this,
        // narrowing `*` could plausibly have been read as narrowing
        // `**` too.
        assert_matches(&[
            ("**/*.py", "a/b/c.py", true),
            ("**/*.py", "top.py", true),
            ("**/x", "x", true),
            ("**/x", "deep/nested/x", true),
            ("src/**", "src/a/b/c.py", true),
            ("src/**/*.py", "src/a/b.py", true),
            ("**/tests/**", "a/b/tests/c/d.py", true),
            // `**/*` is `FileFilters`' default `include`, so every
            // scope that only declares an `exclude` list rides on it.
            // Narrowing the trailing `*` must not stop it catching a
            // nested file, or those scopes would quietly go empty.
            ("**/*", "a/b/c/deep.py", true),
            ("**/*", "top.py", true),
        ]);
    }

    #[test]
    fn brace_alternation_expands_within_the_separator_bound() {
        // Braces are the other half of engine parity (MRGFY-8359),
        // and they compose with the bound rather than escaping it:
        // each branch is a separator-bounded `*` in its own right.
        assert_matches(&[
            ("*.{md,rst}", "readme.md", true),
            ("*.{md,rst}", "readme.rst", true),
            ("*.{md,rst}", "docs/readme.md", false),
        ]);
    }

    #[test]
    fn question_mark_and_class_match_one_character() {
        // MRGFY-10066. globset's byte regex made `?` one byte and
        // `[é]` a class of `é`'s two UTF-8 bytes, so every row below
        // but the plain-literal and `*` ones came out the other way
        // round from the engine, which matches characters. The U+FFFD row is how both GitHub and
        // `ci scopes` report a `café.txt` whose é is the Latin-1 byte.
        assert_matches(&[
            ("critical/caf?.txt", "critical/café.txt", true),
            ("critical/caf??.txt", "critical/café.txt", false),
            ("critical/caf[é].txt", "critical/café.txt", true),
            ("critical/caf?.txt", "critical/caf\u{FFFD}.txt", true),
            ("critical/caf[!é].txt", "critical/café.txt", false),
            ("critical/caf[!à].txt", "critical/café.txt", true),
            ("critical/caf[à-ê].txt", "critical/café.txt", true),
            ("critical/日本?.txt", "critical/日本語.txt", true),
            ("critical/*.txt", "critical/café.txt", true),
            ("critical/café.txt", "critical/café.txt", true),
            ("critical/café.txt", "critical/cafe.txt", false),
        ]);
    }

    #[test]
    fn double_star_crosses_a_newline_in_a_file_name() {
        // globset compiled its own regex with `.` matching `\n`, and
        // the engine's `glob.translate` output is `(?s:...)`. Now the
        // regex is built here, so the flag has to be kept by hand.
        assert_matches(&[("src/**", "src/a\nb.txt", true)]);
    }

    #[test]
    fn escaped_backslash_before_x_is_not_a_byte_escape() {
        // `\\x41` in globset's output is an escaped `\` followed by
        // the text `x41`: folding it into `A` would change the glob.
        assert_eq!(
            unicode_regex(r"(?-u)^a\\x41\xc3\xa9$").as_deref(),
            Some(r"^a\\x41é$"),
        );
    }

    #[test]
    fn exclude_takes_precedence_over_include() {
        // File matches include but also matches exclude — must
        // NOT be assigned the scope.
        let ms = compile_one(&["src/**"], &["src/vendor/**"]);
        let res = route(["src/vendor/legacy.py"], &ms);
        assert!(res.hit.is_empty(), "unexpected hit: {:?}", res.hit);
    }

    #[test]
    fn include_required_when_present() {
        // A file outside `src/**` must not slip in just because
        // the exclude list doesn't catch it. (Regression guard
        // for the "if include is non-empty, file must match it"
        // branch.)
        let ms = compile_one(&["src/**"], &["**/tests/**"]);
        let res = route(["docs/readme.md"], &ms);
        assert!(res.hit.is_empty(), "unexpected hit: {:?}", res.hit);
    }

    #[test]
    fn empty_filters_match_nothing() {
        // A scope with no include and no exclude is inert — same
        // as Python's `if not scope_config.include and not
        // scope_config.exclude: continue` branch. (FileFilters'
        // default fills include with `["**/*"]` so this case is
        // only reachable via direct construction.)
        let ms = compile_one(&[], &[]);
        let res = route(["anything.py"], &ms);
        assert!(res.hit.is_empty());
    }

    #[test]
    fn multiple_files_aggregate_per_scope() {
        // Two files matching the same scope both land in
        // `by_scope`; the scope name appears once in `hit`.
        let ms = compile_one(&["src/**"], &[]);
        let res = route(["src/a.py", "src/b.py"], &ms);
        assert_eq!(res.hit.len(), 1);
        assert_eq!(
            res.by_scope.get("s").map(Vec::as_slice),
            Some(["src/a.py".to_string(), "src/b.py".to_string()].as_slice()),
        );
    }

    #[test]
    fn invalid_glob_surfaces_configuration_error() {
        // An obviously-bad pattern (unterminated bracket
        // expression) should fail config validation rather than
        // crash at match time.
        let mut m = BTreeMap::new();
        m.insert("s".to_string(), filters(&["[unterminated"], &[]));
        let err = compile(&m).unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)));
        assert!(err.to_string().contains("invalid glob"), "got {err}");
    }
}
