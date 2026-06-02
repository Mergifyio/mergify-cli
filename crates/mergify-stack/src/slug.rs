//! Branch-name slug derivation from a commit title + Change-Id.
//!
//! Mirrors `mergify_cli/stack/slug.py::slugify_title`. The output
//! shape is `<slug>--<hex8>` where:
//!
//! - `<slug>` is the title with the conventional-commit prefix
//!   stripped, stop-words removed, common nouns abbreviated, then
//!   lower-cased + ASCII-only + hyphenated, capped at 50 chars on
//!   a word boundary.
//! - `<hex8>` is the first 8 hex chars of the Change-Id (with the
//!   leading `I` stripped).
//!
//! Slug stability across commit-message rewrites matters: as long
//! as the Change-Id is preserved an amend keeps the same branch
//! name and the existing PR is reused. The truncation + fallback
//! exist so a degenerate title (e.g. `"feat: "`, all stop words)
//! still yields a usable branch name.

use std::sync::LazyLock;

use regex::Regex;

const MAX_SLUG_LENGTH: usize = 50;
const SHORT_HASH_LENGTH: usize = 8;

/// `type(scope)!:` conventional-commit prefix — case-insensitive.
static CONVENTIONAL_COMMIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z]+(?:\([^)]*\))?[!]?:\s*").unwrap());

/// Split a sub-token boundary at any non-ASCII-alphanumeric run.
/// Matches Python's `re.split(r"[^a-zA-Z0-9]+", token)` semantics:
/// the regex crate's default is Unicode-aware, so the `[^…]`
/// complement also includes accented letters — which is what we
/// want here (`modèle` → `["mod", "le"]`).
static NON_ALPHANUMERIC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9]+").unwrap());

/// Final pass to coalesce any leftover non-`[a-z0-9-]` chars into
/// hyphens after the slug joins. Belt-and-braces with
/// [`NON_ALPHANUMERIC_RE`] since the joining step adds `-` between
/// tokens and we want to dedupe consecutive `-`.
static NON_SLUG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-z0-9-]").unwrap());

static MULTIPLE_HYPHEN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-{2,}").unwrap());

/// Canonical singular/plural pairs the slug shortens. Mirrors the
/// Python table verbatim so old branches stay reachable when the
/// CLI is upgraded — changing this map would silently fork branch
/// names on existing stacks.
const ABBREVIATIONS: &[(&str, &str)] = &[
    ("application", "app"),
    ("applications", "apps"),
    ("authentification", "auth"),
    ("authentication", "auth"),
    ("authorization", "authz"),
    ("command", "cmd"),
    ("commands", "cmds"),
    ("configuration", "config"),
    ("connection", "conn"),
    ("connections", "conns"),
    ("dependency", "dep"),
    ("dependencies", "deps"),
    ("description", "desc"),
    ("development", "dev"),
    ("directory", "dir"),
    ("documentation", "docs"),
    ("environment", "env"),
    ("environments", "envs"),
    ("function", "func"),
    ("functions", "funcs"),
    ("generation", "gen"),
    ("implement", "impl"),
    ("implementation", "impl"),
    ("information", "info"),
    ("initialization", "init"),
    ("library", "lib"),
    ("libraries", "libs"),
    ("management", "mgmt"),
    ("message", "msg"),
    ("messages", "msgs"),
    ("middleware", "mw"),
    ("notification", "notif"),
    ("notifications", "notifs"),
    ("number", "num"),
    ("package", "pkg"),
    ("packages", "pkgs"),
    ("parameter", "param"),
    ("parameters", "params"),
    ("performance", "perf"),
    ("production", "prod"),
    ("property", "prop"),
    ("properties", "props"),
    ("reference", "ref"),
    ("references", "refs"),
    ("repository", "repo"),
    ("repositories", "repos"),
    ("request", "req"),
    ("requests", "reqs"),
    ("response", "resp"),
    ("responses", "resps"),
    ("specification", "spec"),
    ("specifications", "specs"),
    ("statistics", "stats"),
    ("subscription", "sub"),
    ("subscriptions", "subs"),
    ("synchronization", "sync"),
    ("temporary", "tmp"),
    ("transaction", "tx"),
    ("transactions", "txs"),
    ("utilities", "utils"),
    ("utility", "util"),
    ("validation", "val"),
    ("variable", "var"),
    ("variables", "vars"),
];

const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
    "from", "is", "are", "this", "that", "it", "its", "into", "as", "so", "be", "was", "were",
    "not", "no", "has", "have", "had", "will", "would", "can", "could", "should", "do", "does",
    "did", "just", "also", "when", "where", "how", "if", "then", "than", "more", "some", "all",
    "each", "every", "any", "both", "about", "between", "through", "during", "before", "after",
    "up", "out", "new",
];

fn abbreviate(token: &str) -> &str {
    for (full, abbr) in ABBREVIATIONS {
        if *full == token {
            return abbr;
        }
    }
    token
}

fn is_stop_word(token: &str) -> bool {
    STOP_WORDS.contains(&token)
}

/// Build the deterministic branch slug for `title` + `change_id`.
///
/// `change_id` is expected to be a full Change-Id (`I` + 40 hex);
/// only the first 8 hex chars after the `I` are used. Anything
/// shorter is taken as-is and may produce a slug suffix shorter
/// than 8 chars — the caller should validate the Change-Id shape
/// before calling.
#[must_use]
pub fn slugify_title(title: &str, change_id: &str) -> String {
    // 1. Strip conventional commit prefix (`feat:`, `fix(scope):`, …).
    let after_prefix = CONVENTIONAL_COMMIT_RE.replace(title, "");

    // 2. Tokenise. Whitespace splits first (preserving Python's
    //    `text.split()` semantics) then each token is split again
    //    on every non-ASCII-alphanumeric run.
    let mut words: Vec<String> = Vec::new();
    for ws_token in after_prefix.split_whitespace() {
        for sub in NON_ALPHANUMERIC_RE.split(ws_token) {
            if sub.is_empty() {
                continue;
            }
            let lower = sub.to_lowercase();
            let abbreviated = abbreviate(&lower);
            if !abbreviated.is_empty() && !is_stop_word(abbreviated) {
                words.push(abbreviated.to_string());
            }
        }
    }

    // 3. Join + normalise. The join is `-`; any stray non-slug char
    //    that survived (shouldn't happen — tokens are alphanumeric
    //    by construction) gets collapsed to `-`, then consecutive
    //    hyphens are deduplicated and edges trimmed.
    let mut slug = words.join("-");
    slug = NON_SLUG_RE.replace_all(&slug, "-").into_owned();
    slug = MULTIPLE_HYPHEN_RE.replace_all(&slug, "-").into_owned();
    slug = slug.trim_matches('-').to_string();

    // 4. Truncate at the last word boundary inside the cap so the
    //    slug never ends mid-word.
    if slug.len() > MAX_SLUG_LENGTH {
        let truncated = &slug[..MAX_SLUG_LENGTH];
        slug = match truncated.rfind('-') {
            Some(pos) if pos > 0 => truncated[..pos].to_string(),
            _ => truncated.to_string(),
        };
    }
    slug = slug.trim_matches('-').to_string();

    // 5. Fallback for degenerate inputs (all stop words, empty
    //    title after prefix strip, etc.).
    if slug.is_empty() {
        slug = "change".to_string();
    }

    // 6. Append the short Change-Id (strip leading `I`, take first
    //    8 hex chars).
    let hex_suffix: String = change_id
        .strip_prefix('I')
        .unwrap_or(change_id)
        .chars()
        .take(SHORT_HASH_LENGTH)
        .collect();

    format!("{slug}--{hex_suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CID: &str = "I29617d37762fd69809c255d7e7073cb11f8fbf50";

    #[test]
    fn basic_slugification() {
        assert_eq!(
            slugify_title("Add user model", CID),
            "add-user-model--29617d37"
        );
    }

    #[test]
    fn conventional_commit_prefix_is_stripped() {
        assert_eq!(
            slugify_title("feat(stack): improve comment design", CID),
            "improve-comment-design--29617d37",
        );
        assert_eq!(
            slugify_title("fix: handle missing change id", CID),
            "handle-missing-change-id--29617d37",
        );
    }

    #[test]
    fn stop_words_are_removed() {
        // "the", "in" → dropped.
        assert_eq!(
            slugify_title("Fix the bug in the parser", CID),
            "fix-bug-parser--29617d37",
        );
    }

    #[test]
    fn abbreviations_are_applied() {
        assert_eq!(
            slugify_title("Add user authentication model", CID),
            "add-user-auth-model--29617d37",
        );
        assert_eq!(
            slugify_title("Implement repository synchronization", CID),
            "impl-repo-sync--29617d37",
        );
    }

    #[test]
    fn non_ascii_chars_split_tokens() {
        // `l'authentification` → `l` + `authentification` (→ `auth`).
        // `modèle` → `mod` + `le` (the `è` is non-ASCII so it splits).
        assert_eq!(
            slugify_title("Fix l'authentification du modèle", CID),
            "fix-l-auth-du-mod-le--29617d37",
        );
    }

    #[test]
    fn special_chars_split_tokens() {
        assert_eq!(
            slugify_title("Add foo_bar & baz.qux", CID),
            "add-foo-bar-baz-qux--29617d37",
        );
    }

    #[test]
    fn short_title_passes_through() {
        assert_eq!(slugify_title("fix typo", CID), "fix-typo--29617d37");
    }

    #[test]
    fn all_stop_words_falls_back_to_change() {
        // Every token is a stop word → fallback to `"change"` so
        // the branch name is still well-formed.
        assert_eq!(slugify_title("It is the", CID), "change--29617d37");
    }

    #[test]
    fn empty_after_prefix_strip_falls_back_to_change() {
        // `"feat: "` → `""` after prefix strip → fallback.
        assert_eq!(slugify_title("feat: ", CID), "change--29617d37");
    }

    #[test]
    fn long_title_truncates_at_word_boundary() {
        // 20× "word " → joined slug is way over 50 chars; truncate
        // at the last `-` inside the cap so we don't end mid-word.
        let long_title = "word ".repeat(20);
        let result = slugify_title(&long_title, CID);
        let slug_part = result.rsplit_once("--").unwrap().0;
        assert!(slug_part.len() <= MAX_SLUG_LENGTH);
        assert!(!slug_part.ends_with('-'));
        assert!(result.ends_with("--29617d37"));
    }

    #[test]
    fn different_change_ids_produce_different_suffixes() {
        let title = "Add feature";
        assert_eq!(
            slugify_title(title, "Iaaaaaaa0762fd69809c255d7e7073cb11f8fbf50"),
            "add-feature--aaaaaaa0",
        );
        assert_eq!(
            slugify_title(title, "Ibbbbbbbb762fd69809c255d7e7073cb11f8fbf50"),
            "add-feature--bbbbbbbb",
        );
    }
}
