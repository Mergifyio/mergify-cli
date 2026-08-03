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
//!
//! Parity is exact for every path git can report as changed. The
//! residual differences all need a path git never emits: a leading
//! `/`, a trailing `/`, an empty segment (`a//b`), or the empty
//! string. An unterminated `[` is the one place globset is stricter
//! — it rejects the pattern where the engine degrades it to a
//! literal — and erroring out on a malformed config is the side to
//! be on.
//!
//! One intentional behavior: a pattern with an empty `include` list
//! follows the engine and matches every path (the scope's exclude
//! list then decides) — the YAML deserializer fills the default
//! `["**/*"]` for us, so this fallthrough is mostly defensive.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use globset::GlobBuilder;
use globset::GlobMatcher;
use mergify_core::CliError;

use super::config::FileFilters;

/// Pre-built matchers for one scope.
#[derive(Debug)]
pub struct ScopeMatcher {
    pub name: String,
    include: Vec<GlobMatcher>,
    exclude: Vec<GlobMatcher>,
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

fn compile_list(scope: &str, patterns: &[String]) -> Result<Vec<GlobMatcher>, CliError> {
    patterns.iter().map(|pat| build_glob(scope, pat)).collect()
}

fn build_glob(scope: &str, pattern: &str) -> Result<GlobMatcher, CliError> {
    // `literal_separator(true)` is the whole parity story for
    // everything but `**`: it compiles `*` to `[^/]*` and `?` to
    // `[^/]`, which is what `glob.translate` emits and what the
    // data-types page documents. globset defaults it off, and that
    // default is what used to let `*.md` swallow every `.md` in the
    // tree. `case_insensitive(false)` is the default but stated for
    // the record — file paths are case-sensitive on the platforms
    // Mergify cares about.
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .case_insensitive(false)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| {
            CliError::Configuration(format!(
                "invalid glob {pattern:?} under scope {scope:?}: {e}"
            ))
        })
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
