//! Glob-pattern matching: file path → matching scopes.
//!
//! The engine derives a pull request's scopes from the very same
//! `scopes:` block, and it — not this command — is what the merge
//! queue acts on. So a glob here compiles as the engine compiles it
//! (`engine_glob`, a port of `mergify_engine/rules/globs.py`, which
//! documents the one pattern shape it refuses instead), or one config
//! means two things and `ci scopes` picks CI jobs for a
//! scope set the engine never derives. `engine_glob_cases.json` is the
//! proof: patterns and paths with the engine's own verdicts, which
//! this module must reproduce.
//!
//! One intentional behavior: a pattern with an empty `include` list
//! follows the engine and matches every path (the scope's exclude
//! list then decides) — the YAML deserializer fills the default
//! `["**/*"]` for us, so this fallthrough is mostly defensive.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use mergify_core::CliError;
use regex::Regex;

use super::config::FileFilters;
use super::engine_glob;

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
    engine_glob::compile(pattern).map_err(|e| match e {
        engine_glob::CompileError::Invalid(reason) => CliError::Configuration(format!(
            "invalid glob {pattern:?} under scope {scope:?}: {reason}"
        )),
        // Not the config's fault: the translation is ours.
        engine_glob::CompileError::Regex(e) => {
            CliError::wrap(format!("compile glob {pattern:?} under scope {scope:?}"), e)
        }
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
        // A pattern the engine refuses (an unclosed brace group)
        // fails config validation rather than never matching.
        let mut m = BTreeMap::new();
        m.insert("s".to_string(), filters(&["src/{a,b"], &[]));
        let err = compile(&m).unwrap_err();
        assert!(matches!(err, CliError::Configuration(_)));
        assert!(err.to_string().contains("invalid glob"), "got {err}");
    }
}
