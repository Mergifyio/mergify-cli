//! Glob-pattern matching: file path → matching scopes.
//!
//! Python uses `glob.translate` + `re.fullmatch`; we use `globset`.
//! The `**` recursive wildcard and the `?` / `[…]` character class
//! syntax match Python's `glob` semantics for the patterns Mergify
//! configs actually use (file globs anchored to repo root). One
//! intentional behavior: a pattern with an empty `include` list
//! follows Python and matches every path (the scope's exclude list
//! then decides) — the YAML deserializer fills the default
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
    // `literal_separator(false)` makes `*` and `**` cross `/`
    // boundaries, which is how Python's `glob.translate(...,
    // recursive=True)` behaves. `case_insensitive(false)` is the
    // default but stated for the record — file paths are case-
    // sensitive on the platforms Mergify cares about.
    GlobBuilder::new(pattern)
        .literal_separator(false)
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

    #[test]
    fn double_star_matches_across_path_separators() {
        // `mergify_cli/**` must catch nested files like
        // `mergify_cli/ci/scopes/cli.py`. This is the main
        // expectation of `glob.translate(..., recursive=True)`:
        // `**` traverses arbitrarily many directories.
        let ms = compile_one(&["mergify_cli/**"], &[]);
        let res = route(["mergify_cli/ci/scopes/cli.py"], &ms);
        assert!(res.hit.contains("s"), "expected hit, got {:?}", res.hit);
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
