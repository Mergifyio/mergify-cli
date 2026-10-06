//! A port of the engine's scope-glob compiler.
//!
//! The engine compiles a scope glob in `mergify_engine/rules/globs.py`:
//! its own brace expansion, then Python's `glob.translate(recursive=True,
//! include_hidden=True, seps=["/", "\\"])` on every expansion, matched
//! from the start with the `regex` module and end-anchored with `\z`.
//! This module is that pipeline, step for step, emitting the regex for
//! Rust's `regex` crate instead of Python's. Each function names the
//! Python one it ports; keep them in step, and regenerate
//! `engine_glob_cases.json` against the engine when either side moves.
//!
//! What falls out of following the engine rather than a glob library:
//!
//! - `/` and `\` are both separators, in the pattern and in the path.
//!   `\` escapes nothing but `{`, `}` and `,`.
//! - The pattern is split on separators before anything else, so a
//!   `[...]` class never spans one: `x[a/b]y` is the literal `x[a`,
//!   a separator, then the literal `b]y`.
//! - `[!x]` negates, `[^x]` is a class holding `^` and `x`, and an
//!   unterminated `[` is a literal.
//! - `*` and `?` stay inside a segment; `**` crosses segments only when
//!   it is a whole segment, and a lone `*` segment is never empty.
//! - `?` and classes match a character, not a byte.
//!
//! One thing is refused rather than ported. The `regex` module reads
//! `[:name:]` inside a set as a POSIX class, where `glob.translate`
//! meant literal characters. A glob class ending that way swallows its
//! own closing bracket, and the engine either rejects the pattern (an
//! unknown name) or matches something no documentation describes
//! (`[a[:alpha:]]/b` matches one character of a set holding every
//! letter, `]`, `[`, `/` and `\`, then `b`). Such a pattern is a
//! configuration error here, which is the useful answer for whoever
//! wrote it. The `cli` rows of `engine_glob_cases.json` pin it.

use regex::Regex;
use regex::RegexBuilder;

const SEPARATORS: [char; 2] = ['/', '\\'];

/// What `\` escapes in brace expansion. Nothing else: `\` is also a
/// separator, and `src\**\*.py` has to keep meaning what it means.
const ESCAPABLE: [char; 3] = ['{', '}', ','];

/// The engine's `MAX_BRACE_EXPANSIONS`.
const MAX_BRACE_EXPANSIONS: usize = 128;

/// The engine's `MAX_BRACE_NESTING`.
const MAX_BRACE_NESTING: usize = 32;

const ANY_SEP: &str = r"[/\\]";
const NOT_SEP: &str = r"[^/\\]";

/// Why a glob did not compile.
#[derive(Debug)]
pub enum CompileError {
    /// The engine refuses the pattern too (or, for a POSIX class, reads
    /// it as something else): the configuration's fault.
    Invalid(String),
    /// The translation did not compile: a bug in this port.
    Regex(regex::Error),
}

/// The `regex` module has no size limit and the crate's default (10
/// MB) trips on a glob the engine accepts: 128 brace expansions of a
/// few dozen `?` each, every one a Unicode `[^/\\]` class.
const REGEX_SIZE_LIMIT: usize = 256 * 1024 * 1024;

/// Compile `pattern` into a regex matching the paths the engine puts in
/// a scope for it.
pub fn compile(pattern: &str) -> Result<Regex, CompileError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut translated = Vec::new();
    for expansion in expand_braces(&chars).map_err(CompileError::Invalid)? {
        translated.push(translate(&expansion).map_err(CompileError::Invalid)?);
    }
    // `glob.translate` wraps each expansion in `(?s:...)\z` and the
    // engine joins them with `|` and matches from the start, so this
    // is the same language. No translation holds a top-level `|`.
    RegexBuilder::new(&format!(r"\A(?s:{})\z", translated.join("|")))
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(CompileError::Regex)
}

/// `globs.expand_braces`.
fn expand_braces(pattern: &[char]) -> Result<Vec<String>, String> {
    // Outside a group the expansion only stops at the end of the
    // pattern, so the returned index carries nothing.
    let (expansions, _) = expand_from(pattern, 0, 0, false)?;
    Ok(expansions)
}

/// `globs._expand_from`.
fn expand_from(
    pattern: &[char],
    mut index: usize,
    depth: usize,
    inside_group: bool,
) -> Result<(Vec<String>, usize), String> {
    let mut expansions = vec![String::new()];
    let mut literal = String::new();

    while let Some(&c) = pattern.get(index) {
        if c == '\\'
            && let Some(&next) = pattern.get(index + 1)
            && ESCAPABLE.contains(&next)
        {
            literal.push(next);
            index += 2;
            continue;
        }
        match c {
            '[' => {
                if let Some(end) = scan_character_class(pattern, index) {
                    literal.extend(&pattern[index..end]);
                    index = end;
                    continue;
                }
            }
            '{' => {
                if depth >= MAX_BRACE_NESTING {
                    return Err(format!(
                        "nests brace groups more than {MAX_BRACE_NESTING} deep"
                    ));
                }
                let (branches, next) = expand_group(pattern, index + 1, depth + 1)?;
                expansions = combine(&expansions, &literal, &branches)?;
                literal.clear();
                index = next;
                continue;
            }
            '}' | ',' if inside_group => break,
            '}' => return Err("unbalanced `}`: use `\\}` to match a literal brace".into()),
            _ => {}
        }
        literal.push(c);
        index += 1;
    }

    Ok((combine(&expansions, &literal, &[String::new()])?, index))
}

/// `globs._expand_group`: `index` is just past the `{`.
fn expand_group(
    pattern: &[char],
    mut index: usize,
    depth: usize,
) -> Result<(Vec<String>, usize), String> {
    let mut branches = Vec::new();
    loop {
        let (branch, next) = expand_from(pattern, index, depth, true)?;
        branches.extend(branch);
        if branches.len() > MAX_BRACE_EXPANSIONS {
            return Err(too_many_expansions());
        }
        match pattern.get(next) {
            None => return Err("unclosed `{`: use `\\{` to match a literal brace".into()),
            Some('}') => {
                // `_drop_empty_branches`: `foo{,.txt}` matches
                // `foo.txt` but not `foo`, while `{}` and `{,}` keep
                // one empty branch.
                branches.retain(|b: &String| !b.is_empty());
                if branches.is_empty() {
                    branches.push(String::new());
                }
                return Ok((branches, next + 1));
            }
            // The `,` that ends this branch.
            Some(_) => index = next + 1,
        }
    }
}

/// `globs._scan_character_class`: the index just past the `[...]`
/// starting at `start`, or `None` when that `[` is a literal.
fn scan_character_class(pattern: &[char], start: usize) -> Option<usize> {
    let mut index = start + 1;
    if pattern.get(index) == Some(&'!') {
        index += 1;
    }
    if pattern.get(index) == Some(&']') {
        index += 1;
    }
    while let Some(&c) = pattern.get(index) {
        if c == ']' {
            return Some(index + 1);
        }
        if SEPARATORS.contains(&c) {
            return None;
        }
        index += 1;
    }
    None
}

/// `globs._combine`.
fn combine(prefixes: &[String], literal: &str, suffixes: &[String]) -> Result<Vec<String>, String> {
    if prefixes.len() * suffixes.len() > MAX_BRACE_EXPANSIONS {
        return Err(too_many_expansions());
    }
    Ok(prefixes
        .iter()
        .flat_map(|prefix| {
            suffixes
                .iter()
                .map(move |suffix| format!("{prefix}{literal}{suffix}"))
        })
        .collect())
}

fn too_many_expansions() -> String {
    format!("expands to more than {MAX_BRACE_EXPANSIONS} alternatives")
}

/// `glob.translate(pattern, recursive=True, include_hidden=True,
/// seps=["/", "\\"])`, without the `(?s:...)\z` wrapper.
fn translate(pattern: &str) -> Result<String, String> {
    let parts: Vec<&str> = pattern.split(SEPARATORS).collect();
    let last = parts.len() - 1;
    let mut out = String::new();
    for (index, &part) in parts.iter().enumerate() {
        match part {
            "*" if index < last => {
                out.push_str(NOT_SEP);
                out.push('+');
                out.push_str(ANY_SEP);
            }
            "*" => {
                out.push_str(NOT_SEP);
                out.push('+');
            }
            "**" if index < last => {
                // Consecutive `**` segments collapse into the last one.
                if parts[index + 1] != "**" {
                    out.push_str("(?:.+");
                    out.push_str(ANY_SEP);
                    out.push_str(")?");
                }
            }
            "**" => out.push_str(".*"),
            _ => {
                translate_segment(part, &mut out)?;
                if index < last {
                    out.push_str(ANY_SEP);
                }
            }
        }
    }
    Ok(out)
}

/// `fnmatch._translate(segment, star="[^/\\]*", question_mark="[^/\\]")`.
fn translate_segment(segment: &str, out: &mut String) -> Result<(), String> {
    let pattern: Vec<char> = segment.chars().collect();
    let n = pattern.len();
    let mut i = 0;
    while i < n {
        let c = pattern[i];
        i += 1;
        match c {
            '*' => {
                out.push_str(NOT_SEP);
                out.push('*');
                while i < n && pattern[i] == '*' {
                    i += 1;
                }
            }
            '?' => out.push_str(NOT_SEP),
            '[' => {
                let mut j = i;
                if j < n && pattern[j] == '!' {
                    j += 1;
                }
                if j < n && pattern[j] == ']' {
                    j += 1;
                }
                while j < n && pattern[j] != ']' {
                    j += 1;
                }
                if j >= n {
                    push_literal(out, '[');
                } else {
                    push_class(out, &class_chunks(&pattern[i..j]))?;
                    i = j + 1;
                }
            }
            c => push_literal(out, c),
        }
    }
    Ok(())
}

/// The class body `fnmatch._translate` builds from `stuff`, the
/// characters between `[` and `]`, as chunks of literal characters
/// where each boundary between two chunks is a range operator: the
/// last character of one chunk to the first of the next.
fn class_chunks(stuff: &[char]) -> Vec<Vec<char>> {
    if !stuff.contains(&'-') {
        return vec![stuff.to_vec()];
    }
    let find_hyphen = |from: usize| (from..stuff.len()).find(|&k| stuff[k] == '-');
    let mut chunks: Vec<Vec<char>> = Vec::new();
    let mut i = 0;
    let mut k = if stuff[0] == '!' { 2 } else { 1 };
    while let Some(hyphen) = find_hyphen(k) {
        chunks.push(stuff[i..hyphen].to_vec());
        i = hyphen + 1;
        k = hyphen + 3;
    }
    let chunk = &stuff[i..];
    if !chunk.is_empty() {
        chunks.push(chunk.to_vec());
    } else if let Some(last) = chunks.last_mut() {
        last.push('-');
    }
    // "Remove empty ranges -- invalid in RE.": a reversed range is
    // dropped along with its two bounds.
    for k in (1..chunks.len()).rev() {
        if let (Some(&low), Some(&high)) = (chunks[k - 1].last(), chunks[k].first())
            && low > high
        {
            let tail = chunks.remove(k);
            chunks[k - 1].pop();
            chunks[k - 1].extend(&tail[1..]);
        }
    }
    chunks
}

fn push_class(out: &mut String, chunks: &[Vec<char>]) -> Result<(), String> {
    let body: Vec<char> = chunks.join(&'-');
    match body.as_slice() {
        // An empty range never matches.
        [] => {
            out.push_str("[a&&b]");
            return Ok(());
        }
        // A negated empty range matches any character, `/` included.
        ['!'] => {
            out.push('.');
            return Ok(());
        }
        _ => {}
    }
    let negated = body[0] == '!';
    reject_posix_class(chunks, negated)?;
    out.push('[');
    if negated {
        out.push('^');
    }
    let mut first = true;
    for (index, chunk) in chunks.iter().enumerate() {
        if index > 0 {
            out.push('-');
        }
        for &c in chunk {
            if first && negated {
                // The `!` itself.
                first = false;
                continue;
            }
            first = false;
            push_literal(out, c);
        }
    }
    out.push(']');
    Ok(())
}

/// Refuse a class in which the `regex` module would read a POSIX
/// class (see the module doc).
fn reject_posix_class(chunks: &[Vec<char>], negated: bool) -> Result<(), String> {
    let body = python_class_body(chunks, negated);
    let mut index = 0;
    while index < body.len() {
        match body[index] {
            // An escape is one item, whatever it escapes.
            '\\' => index += 2,
            '[' if body.get(index + 1) == Some(&':') && is_posix_class(&body[index + 2..]) => {
                return Err(
                    "`[:` inside a character class is read by the engine as a POSIX class, \
                     not as literal characters"
                        .into(),
                );
            }
            _ => index += 1,
        }
    }
    Ok(())
}

/// The set body `fnmatch._translate` emits for the class, between the
/// `[` and the `]`, escapes included: what the `regex` module parses.
fn python_class_body(chunks: &[Vec<char>], negated: bool) -> Vec<char> {
    let mut body = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        if index > 0 {
            body.push('-');
        }
        for &c in chunk {
            // A segment holds no `\\`, so these are all the escapes:
            // a literal `-` and the set operators.
            if matches!(c, '-' | '&' | '~' | '|') {
                body.push('\\');
            }
            body.push(c);
        }
    }
    if negated {
        body[0] = '^';
    } else if matches!(body.first(), Some('^' | '[')) {
        body.insert(0, '\\');
    }
    body
}

/// Whether `rest`, the set body after a `[:` up to the set's closing
/// `]`, parses as a POSIX class name: `regex`'s `parse_posix_class`
/// and `parse_property_name`. The name must run to the end of the body,
/// since the only `]` that can close it is the set's own.
fn is_posix_class(rest: &[char]) -> bool {
    let name_part = |c: char| c.is_ascii_alphanumeric() || " &_-.".contains(c);
    let value_part = |c: char| c.is_ascii_alphanumeric() || " &_-./".contains(c);
    let mut index = usize::from(rest.first() == Some(&'^'));
    while index < rest.len() && name_part(rest[index]) {
        index += 1;
    }
    if matches!(rest.get(index), Some(':' | '=')) {
        let start = index + 1;
        let mut end = start;
        while end < rest.len() && value_part(rest[end]) {
            end += 1;
        }
        // A qualified name (`[:script=latin:]`) only when the value
        // is not blank; otherwise the `:` or `=` is not consumed.
        if rest[start..end].iter().any(|&c| c != ' ') {
            index = end;
        }
    }
    rest.get(index) == Some(&':') && index + 1 == rest.len()
}

fn push_literal(out: &mut String, c: char) {
    let mut buf = [0; 4];
    out.push_str(&regex::escape(c.encode_utf8(&mut buf)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct Case {
        pattern: String,
        path: String,
        engine: String,
        /// Set where this module answers differently on purpose: only
        /// `invalid`, for the POSIX classes the module doc describes.
        cli: Option<String>,
    }

    #[test]
    fn every_case_gets_the_engines_verdict() {
        let cases: Vec<Case> = serde_json::from_str(include_str!("engine_glob_cases.json"))
            .expect("engine_glob_cases.json parses");
        let failures: Vec<String> = cases
            .iter()
            .filter_map(|case| {
                let got = match compile(&case.pattern) {
                    Err(CompileError::Invalid(_)) => "invalid",
                    Err(CompileError::Regex(e)) => {
                        panic!("{:?} does not compile: {e}", case.pattern)
                    }
                    Ok(regex) if regex.is_match(&case.path) => "match",
                    Ok(_) => "miss",
                };
                let want = case.cli.as_deref().unwrap_or(&case.engine);
                (got != want).then(|| {
                    format!(
                        "{:?} vs {:?}: got {got}, want {want}",
                        case.pattern, case.path
                    )
                })
            })
            .collect();
        assert!(
            failures.is_empty(),
            "{} of {} cases differ:\n{}",
            failures.len(),
            cases.len(),
            failures.join("\n")
        );
    }
}
