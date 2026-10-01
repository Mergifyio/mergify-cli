//! The structural three-way merge, and the text it produces.
//!
//! The output is OURS' TEXT, edited. Whatever ours already holds is
//! copied byte for byte, and each change theirs made is spliced in as
//! theirs wrote it. Nothing is re-serialised, so key order, indentation,
//! blank lines and number spellings survive everywhere the merge did not
//! have to touch.
//!
//! Before claiming success, the merged text is parsed again and compared
//! with the value the merge decided on. A splice that came out wrong (a
//! stray comma, a lost member) is caught there and becomes a decline, so
//! a mistake in the text assembly can cost a conflict but never a wrong
//! merge.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;

use crate::sequence;
use crate::sequence::Chunk;
use crate::value;
use crate::value::Kind;
use crate::value::Member;
use crate::value::Value;
use crate::value::same;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Base,
    Ours,
    Theirs,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Base => "base",
            Self::Ours => "ours",
            Self::Theirs => "theirs",
        })
    }
}

/// Why the merge refused. Every variant is a case where more than one
/// result is defensible, or none is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    NotJson {
        side: Side,
        error: String,
    },
    /// Both sides changed the same scalar, or changed its type, to
    /// different values.
    BothChanged,
    /// Both sides added the same key (or the same file) with values
    /// that are not both objects and differ.
    BothAdded,
    DeletedAndChanged {
        deleted_by: Side,
    },
    /// Both sides changed the same stretch of an array, and not by
    /// editing the same elements in place.
    ArrayEditsOverlap,
    ArrayTooLarge,
    /// The merged text did not parse back as the merged value.
    RenderMismatch,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotJson { side, error } => write!(f, "{side} is not valid JSON: {error}"),
            Self::BothChanged => f.write_str("both sides changed this value differently"),
            Self::BothAdded => f.write_str("both sides added this value, differently"),
            Self::DeletedAndChanged { deleted_by } => {
                let other = if *deleted_by == Side::Ours {
                    Side::Theirs
                } else {
                    Side::Ours
                };
                write!(f, "{deleted_by} deleted this key and {other} changed it")
            }
            Self::ArrayEditsOverlap => {
                f.write_str("both sides changed the same part of this array")
            }
            Self::ArrayTooLarge => f.write_str("this array changed too much to align"),
            Self::RenderMismatch => {
                f.write_str("the merged text did not read back as the merged value")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decline {
    /// JSON Pointer (RFC 6901) to the value the merge stopped at; empty
    /// for the document root.
    pub pointer: String,
    pub reason: Reason,
}

impl fmt::Display for Decline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.reason {
            Reason::NotJson { .. } | Reason::RenderMismatch => write!(f, "{}", self.reason),
            reason if self.pointer.is_empty() => write!(f, "{reason} (at the document root)"),
            reason => write!(f, "{reason} (at `{}`)", self.pointer),
        }
    }
}

impl std::error::Error for Decline {}

/// Merge `theirs` into `ours`, both descended from `base`. Returns the
/// merged document, borrowing `ours` when the merge leaves it as is.
///
/// An empty (or blank) `base` means both sides added the file: git
/// hands an add/add to the driver that way.
pub fn merge<'a>(base: &str, ours: &'a str, theirs: &str) -> Result<Cow<'a, str>, Decline> {
    let parse = |text, side| {
        value::parse(text).map_err(|e| Decline {
            pointer: String::new(),
            reason: Reason::NotJson {
                side,
                error: e.to_string(),
            },
        })
    };
    let o = parse(ours, Side::Ours)?;
    let t = parse(theirs, Side::Theirs)?;
    let b = if base.trim_matches([' ', '\t', '\n', '\r']).is_empty() {
        None
    } else {
        Some(parse(base, Side::Base)?)
    };
    let mut merger = Merger {
        ours,
        theirs,
        path: Vec::new(),
    };
    let out = merger.merge_value(b.as_ref(), &o, &t)?;
    if matches!(out.expect, Expect::Ours(_)) {
        return Ok(Cow::Borrowed(ours));
    }
    let text = format!("{}{}{}", &ours[..o.start], out.text, &ours[o.end..]);
    if !value::parse(&text).is_ok_and(|merged| matches(&merged, &out.expect)) {
        return Err(Decline {
            pointer: String::new(),
            reason: Reason::RenderMismatch,
        });
    }
    Ok(Cow::Owned(text))
}

/// What a merged value must read back as, kept beside its text for the
/// final check.
enum Expect<'a> {
    /// Ours' value, and its text is ours' span verbatim.
    Ours(&'a Value),
    Theirs(&'a Value),
    Object(Vec<(&'a str, Expect<'a>)>),
    Array(Vec<Expect<'a>>),
}

fn matches(v: &Value, expect: &Expect<'_>) -> bool {
    match (expect, &v.kind) {
        (Expect::Ours(w) | Expect::Theirs(w), _) => same(v, w),
        (Expect::Object(want), Kind::Object(got)) => {
            got.len() == want.len()
                && got
                    .iter()
                    .zip(want)
                    .all(|(m, (key, e))| m.key == *key && matches(&m.value, e))
        }
        (Expect::Array(want), Kind::Array(got)) => {
            got.len() == want.len() && got.iter().zip(want).all(|(v, e)| matches(v, e))
        }
        _ => false,
    }
}

struct Out<'a> {
    text: Cow<'a, str>,
    expect: Expect<'a>,
}

/// One item of a container being rebuilt. `ours_index` is its position
/// in ours' container when it came from there: two items that were
/// neighbours in ours keep the separator that stood between them.
struct Item<'a> {
    ours_index: Option<usize>,
    text: Cow<'a, str>,
}

struct Merger<'a> {
    ours: &'a str,
    theirs: &'a str,
    path: Vec<String>,
}

impl<'a> Merger<'a> {
    fn decline(&self, reason: Reason) -> Decline {
        let mut pointer = String::new();
        for segment in &self.path {
            pointer.push('/');
            pointer.push_str(&segment.replace('~', "~0").replace('/', "~1"));
        }
        Decline { pointer, reason }
    }

    fn keep(&self, o: &'a Value) -> Out<'a> {
        Out {
            text: Cow::Borrowed(&self.ours[o.start..o.end]),
            expect: Expect::Ours(o),
        }
    }

    /// Theirs' value, written where ours' stands.
    fn take_theirs(&self, o: &'a Value, t: &'a Value) -> Out<'a> {
        Out {
            text: reindent(
                &self.theirs[t.start..t.end],
                line_indent(self.theirs, t.start),
                line_indent(self.ours, o.start),
            ),
            expect: Expect::Theirs(t),
        }
    }

    fn merge_value(
        &mut self,
        b: Option<&'a Value>,
        o: &'a Value,
        t: &'a Value,
    ) -> Result<Out<'a>, Decline> {
        if same(o, t) || b.is_some_and(|b| same(b, t)) {
            return Ok(self.keep(o));
        }
        if b.is_some_and(|b| same(b, o)) {
            return Ok(self.take_theirs(o, t));
        }
        match (b.map(|b| &b.kind), &o.kind, &t.kind) {
            (Some(Kind::Object(bm)), Kind::Object(om), Kind::Object(tm)) => {
                self.merge_object(Some(bm), o, om, t, tm)
            }
            (None, Kind::Object(om), Kind::Object(tm)) => self.merge_object(None, o, om, t, tm),
            (Some(Kind::Array(bi)), Kind::Array(oi), Kind::Array(ti)) => {
                self.merge_array(bi, o, oi, t, ti)
            }
            (None, _, _) => Err(self.decline(Reason::BothAdded)),
            _ => Err(self.decline(Reason::BothChanged)),
        }
    }

    /// Key by key. `base` is `None` when both sides added the object, so
    /// every key is new to both.
    fn merge_object(
        &mut self,
        base: Option<&'a [Member]>,
        o: &'a Value,
        om: &'a [Member],
        t: &'a Value,
        tm: &'a [Member],
    ) -> Result<Out<'a>, Decline> {
        let base = base.unwrap_or_default();
        let index = |members: &'a [Member]| -> HashMap<&'a str, &'a Member> {
            members.iter().map(|m| (m.key.as_str(), m)).collect()
        };
        let in_base = index(base);
        let in_theirs = index(tm);
        let in_ours: HashSet<&str> = om.iter().map(|m| m.key.as_str()).collect();

        for bm in base {
            if !in_ours.contains(bm.key.as_str())
                && let Some(tv) = in_theirs.get(bm.key.as_str())
                && !same(&bm.value, &tv.value)
            {
                self.path.push(bm.key.clone());
                return Err(self.decline(Reason::DeletedAndChanged {
                    deleted_by: Side::Ours,
                }));
            }
        }

        let Additions {
            in_order,
            at_front,
            after,
        } = additions(om, tm, &in_ours, &in_base);
        let (from, to) = (
            line_indent(self.theirs, t.start),
            line_indent(self.ours, o.start),
        );
        let theirs_member = |tv: &'a Member| Item {
            ours_index: None,
            text: reindent(&self.theirs[tv.start..tv.value.end], from, to),
        };
        let mut changed = !in_order.is_empty() || !at_front.is_empty() || !after.is_empty();
        let mut items = Vec::new();
        let mut expect = Vec::new();
        for tv in &at_front {
            items.push(theirs_member(tv));
            expect.push((tv.key.as_str(), Expect::Theirs(&tv.value)));
        }
        for (j, ov) in om.iter().enumerate() {
            let key = ov.key.as_str();
            self.path.push(ov.key.clone());
            let merged = match (in_base.get(key), in_theirs.get(key)) {
                (Some(bv), Some(tv)) => {
                    Some(self.merge_value(Some(&bv.value), &ov.value, &tv.value)?)
                }
                (None, Some(tv)) => Some(self.merge_value(None, &ov.value, &tv.value)?),
                (Some(bv), None) if same(&bv.value, &ov.value) => None,
                (Some(_), None) => {
                    return Err(self.decline(Reason::DeletedAndChanged {
                        deleted_by: Side::Theirs,
                    }));
                }
                (None, None) => Some(self.keep(&ov.value)),
            };
            self.path.pop();
            match merged {
                None => changed = true,
                Some(out) => {
                    let text = if let Expect::Ours(_) = out.expect {
                        Cow::Borrowed(&self.ours[ov.start..ov.value.end])
                    } else {
                        changed = true;
                        Cow::Owned(format!(
                            "{}{}",
                            &self.ours[ov.start..ov.value.start],
                            out.text
                        ))
                    };
                    items.push(Item {
                        ours_index: Some(j),
                        text,
                    });
                    expect.push((key, out.expect));
                }
            }
            for tv in after.get(key).into_iter().flatten() {
                items.push(theirs_member(tv));
                expect.push((tv.key.as_str(), Expect::Theirs(&tv.value)));
            }
        }
        for tv in in_order {
            let at = expect.partition_point(|(key, _)| *key < tv.key.as_str());
            items.insert(at, theirs_member(tv));
            expect.insert(at, (tv.key.as_str(), Expect::Theirs(&tv.value)));
        }
        if !changed {
            return Ok(self.keep(o));
        }
        Ok(Out {
            text: Cow::Owned(self.render_container(o, t, &items)),
            expect: Expect::Object(expect),
        })
    }

    /// Element-wise diff3. Where only one side changed a stretch, that
    /// side's version is taken. Where both did, the merge goes on only
    /// if they changed the same elements in place (same count, nothing
    /// moved), merging each element; anything else declines — two
    /// insertions at the same point included, since which goes first is
    /// a question only the array's meaning answers.
    fn merge_array(
        &mut self,
        bi: &'a [Value],
        o: &'a Value,
        oi: &'a [Value],
        t: &'a Value,
        ti: &'a [Value],
    ) -> Result<Out<'a>, Decline> {
        let chunks = sequence::diff3(bi, oi, ti)
            .map_err(|sequence::TooLarge| self.decline(Reason::ArrayTooLarge))?;
        let (from, to) = (
            line_indent(self.theirs, t.start),
            line_indent(self.ours, o.start),
        );
        let mut changed = false;
        let mut items = Vec::new();
        let mut expect = Vec::new();
        let keep = |items: &mut Vec<Item<'a>>, expect: &mut Vec<Expect<'a>>, j: usize| {
            items.push(Item {
                ours_index: Some(j),
                text: Cow::Borrowed(&self.ours[oi[j].start..oi[j].end]),
            });
            expect.push(Expect::Ours(&oi[j]));
        };
        for chunk in chunks {
            let (rb, ro, rt) = match chunk {
                Chunk::Stable { o: j } => {
                    keep(&mut items, &mut expect, j);
                    continue;
                }
                Chunk::Unstable { b, o, t } => (b, o, t),
            };
            let (bs, os, ts) = (&bi[rb], &oi[ro.clone()], &ti[rt]);
            if seq_same(bs, ts) || seq_same(os, ts) {
                ro.for_each(|j| keep(&mut items, &mut expect, j));
            } else if seq_same(bs, os) {
                changed = true;
                for tv in ts {
                    items.push(Item {
                        ours_index: None,
                        text: reindent(&self.theirs[tv.start..tv.end], from, to),
                    });
                    expect.push(Expect::Theirs(tv));
                }
            } else if bs.len() == os.len()
                && bs.len() == ts.len()
                && !moved(bs, os)
                && !moved(bs, ts)
            {
                for (n, j) in ro.enumerate() {
                    self.path.push(j.to_string());
                    let out = self.merge_value(Some(&bs[n]), &os[n], &ts[n])?;
                    self.path.pop();
                    changed |= !matches!(out.expect, Expect::Ours(_));
                    items.push(Item {
                        ours_index: Some(j),
                        text: out.text,
                    });
                    expect.push(out.expect);
                }
            } else {
                return Err(self.decline(Reason::ArrayEditsOverlap));
            }
        }
        if !changed {
            return Ok(self.keep(o));
        }
        Ok(Out {
            text: Cow::Owned(self.render_container(o, t, &items)),
            expect: Expect::Array(expect),
        })
    }

    /// Rebuild a container from `items`, in ours' formatting: ours'
    /// brackets and the whitespace inside them, and between two items
    /// that were neighbours in ours, the separator that stood there.
    /// Anywhere else the separator is one of ours' (or, when ours has
    /// fewer than two items to take one from, theirs').
    fn render_container(&self, o: &Value, t: &Value, items: &[Item<'_>]) -> String {
        let (ours, theirs) = (self.ours, self.theirs);
        let os = o.item_spans();
        let ts = t.item_spans();
        let (from, to) = (line_indent(theirs, t.start), line_indent(ours, o.start));
        let (open, close) = (&ours[o.start..=o.start], &ours[o.end - 1..o.end]);
        if items.is_empty() {
            return format!("{open}{close}");
        }
        let (leading, trailing) = match (os.first(), os.last(), ts.first(), ts.last()) {
            (Some(f), Some(l), _, _) => (
                Cow::Borrowed(&ours[o.start + 1..f.0]),
                Cow::Borrowed(&ours[l.1..o.end - 1]),
            ),
            (_, _, Some(f), Some(l)) => (
                reindent(&theirs[t.start + 1..f.0], from, to),
                reindent(&theirs[l.1..t.end - 1], from, to),
            ),
            _ => (Cow::Borrowed(""), Cow::Borrowed("")),
        };
        let separator = if os.len() >= 2 {
            Cow::Borrowed(&ours[os[0].1..os[1].0])
        } else if ts.len() >= 2 {
            reindent(&theirs[ts[0].1..ts[1].0], from, to)
        } else if !leading.is_empty() {
            Cow::Owned(format!(",{leading}"))
        } else if ours.contains(": ") {
            // A one-line container with nothing to copy a separator
            // from: follow the document's own spacing.
            Cow::Borrowed(", ")
        } else {
            Cow::Borrowed(",")
        };
        let mut text = String::from(open);
        text.push_str(&leading);
        for (n, item) in items.iter().enumerate() {
            if n > 0 {
                match (items[n - 1].ours_index, item.ours_index) {
                    (Some(a), Some(b)) if b == a + 1 => text.push_str(&ours[os[a].1..os[b].0]),
                    _ => text.push_str(&separator),
                }
            }
            text.push_str(&item.text);
        }
        text.push_str(&trailing);
        text.push_str(close);
        text
    }
}

/// Where the keys only theirs added go, by one of two rules.
struct Additions<'a> {
    /// Sorted rule: each goes to its sorted place among the result's keys.
    in_order: Vec<&'a Member>,
    /// Anchor rule: before ours' first key ...
    at_front: Vec<&'a Member>,
    /// ... or right after the ours key named here.
    after: HashMap<&'a str, Vec<&'a Member>>,
}

/// A key only theirs added goes where theirs put it. When both sides
/// keep the object sorted — a package.json's dependencies, a generated
/// schema's components — that is its sorted place among ours' keys, even
/// beside a key ours added at the same spot. Otherwise it goes right
/// after the key that precedes it in theirs and that ours also has.
fn additions<'a>(
    om: &[Member],
    tm: &'a [Member],
    in_ours: &HashSet<&str>,
    in_base: &HashMap<&str, &Member>,
) -> Additions<'a> {
    let sorted = is_sorted(om) && is_sorted(tm);
    let mut found = Additions {
        in_order: Vec::new(),
        at_front: Vec::new(),
        after: HashMap::new(),
    };
    let mut anchor = None;
    for tv in tm {
        let key = tv.key.as_str();
        if in_ours.contains(key) {
            anchor = Some(key);
        } else if in_base.contains_key(key) {
            // Ours deleted it and theirs left it as it was.
        } else if sorted {
            found.in_order.push(tv);
        } else {
            match anchor {
                Some(a) => found.after.entry(a).or_default().push(tv),
                None => found.at_front.push(tv),
            }
        }
    }
    found
}

/// Keys in strictly ascending byte order. Vacuously true for fewer than
/// two keys, which costs nothing: with one key or none, theirs' order is
/// the only evidence and sorted insertion follows it.
fn is_sorted(members: &[Member]) -> bool {
    members.windows(2).all(|pair| pair[0].key < pair[1].key)
}

fn seq_same(a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same(x, y))
}

/// Whether `side` holds, at some position, an element base holds at a
/// different one — a move, which an in-place merge would misread as two
/// unrelated edits.
fn moved(base: &[Value], side: &[Value]) -> bool {
    side.iter().enumerate().any(|(i, v)| {
        !same(v, &base[i]) && base.iter().enumerate().any(|(j, w)| j != i && same(v, w))
    })
}

/// The run of spaces and tabs that starts the line holding `pos`.
fn line_indent(text: &str, pos: usize) -> &str {
    let line = text[..pos].rfind('\n').map_or(0, |n| n + 1);
    let rest = &text[line..];
    let width = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    &rest[..width]
}

/// Move a block of text taken from a line indented `from` to a line
/// indented `to`: every line after the first that starts with `from`
/// starts with `to` instead. JSON only allows newlines between tokens,
/// so this never touches the inside of a string.
fn reindent<'t>(text: &'t str, from: &str, to: &str) -> Cow<'t, str> {
    if from == to || !text.contains('\n') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
            if let Some(rest) = line.strip_prefix(from) {
                out.push_str(to);
                out.push_str(rest);
                continue;
            }
        }
        out.push_str(line);
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn merged(base: &str, ours: &str, theirs: &str) -> String {
        match merge(base, ours, theirs) {
            Ok(text) => text.into_owned(),
            Err(e) => panic!("declined: {e}"),
        }
    }

    #[track_caller]
    fn declined(base: &str, ours: &str, theirs: &str) -> Decline {
        match merge(base, ours, theirs) {
            Ok(text) => panic!("merged:\n{text}"),
            Err(e) => e,
        }
    }

    const PACKAGE: &str = r#"{
  "name": "dashboard",
  "devDependencies": {
    "@types/lodash": "4.17.24",
    "@types/luxon": "3.7.1",
    "typescript": "5.9.2"
  }
}
"#;

    #[test]
    fn adjacent_bumps_both_land() {
        // The census's shape: two renovate bumps on neighbouring lines,
        // which git's line merge reports as a conflict.
        let ours = PACKAGE.replace("4.17.24", "4.17.25");
        let theirs = PACKAGE.replace("3.7.1", "3.7.4");
        assert_eq!(
            merged(PACKAGE, &ours, &theirs),
            PACKAGE
                .replace("4.17.24", "4.17.25")
                .replace("3.7.1", "3.7.4")
        );
    }

    #[test]
    fn a_key_theirs_added_lands_where_theirs_put_it() {
        let ours = PACKAGE.replace("4.17.24", "4.17.25");
        let theirs = PACKAGE.replace(
            "    \"typescript\"",
            "    \"@types/node\": \"24.0.0\",\n    \"typescript\"",
        );
        let want = PACKAGE.replace("4.17.24", "4.17.25").replace(
            "    \"typescript\"",
            "    \"@types/node\": \"24.0.0\",\n    \"typescript\"",
        );
        assert_eq!(merged(PACKAGE, &ours, &theirs), want);
    }

    #[test]
    fn keys_added_at_the_front_and_the_end_keep_theirs_order() {
        let base = "{\n  \"b\": 1\n}\n";
        let ours = "{\n  \"b\": 2\n}\n";
        let theirs = "{\n  \"a\": 0,\n  \"a2\": 0,\n  \"b\": 1,\n  \"c\": 3,\n  \"d\": 4\n}\n";
        assert_eq!(
            merged(base, ours, theirs),
            "{\n  \"a\": 0,\n  \"a2\": 0,\n  \"b\": 2,\n  \"c\": 3,\n  \"d\": 4\n}\n"
        );
    }

    #[test]
    fn keys_both_sides_added_at_one_spot_stay_sorted() {
        // Two new dependencies between the same two neighbours: the line
        // merge conflicts, and placing theirs right after its theirs-side
        // predecessor would put `b` after ours' `c`.
        let base = "{\n  \"a\": 1,\n  \"d\": 4\n}\n";
        let ours = "{\n  \"a\": 1,\n  \"c\": 3,\n  \"d\": 4\n}\n";
        let theirs = "{\n  \"a\": 1,\n  \"b\": 2,\n  \"d\": 4\n}\n";
        let want = "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3,\n  \"d\": 4\n}\n";
        assert_eq!(merged(base, ours, theirs), want);
        assert_eq!(merged(base, theirs, ours), want);
    }

    #[test]
    fn in_an_unsorted_object_theirs_keys_follow_their_predecessor() {
        let base = r#"{"z": 1, "a": 1, "q": 1}"#;
        let ours = r#"{"z": 1, "a": 1, "y": 0, "q": 1}"#;
        let theirs = r#"{"z": 1, "a": 1, "m": 2, "q": 1}"#;
        assert_eq!(
            merged(base, ours, theirs),
            r#"{"z": 1, "a": 1, "m": 2, "y": 0, "q": 1}"#
        );
    }

    #[test]
    fn deletions_take_their_separator_with_them() {
        let base = "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}";
        let bump = "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 4\n}";
        assert_eq!(
            merged(base, bump, "{\n  \"b\": 2,\n  \"c\": 3\n}"),
            "{\n  \"b\": 2,\n  \"c\": 4\n}"
        );
        let bump = "{\n  \"a\": 0,\n  \"b\": 2,\n  \"c\": 3\n}";
        assert_eq!(
            merged(base, bump, "{\n  \"a\": 1,\n  \"b\": 2\n}"),
            "{\n  \"a\": 0,\n  \"b\": 2\n}"
        );
        let base = "{\"a\": {\"x\": 1}, \"b\": 1}";
        assert_eq!(
            merged(
                base,
                "{\"a\": {}, \"b\": 1}",
                "{\"a\": {\"x\": 1}, \"b\": 2}"
            ),
            "{\"a\": {}, \"b\": 2}"
        );
    }

    #[test]
    fn emptying_an_object_and_filling_an_empty_one() {
        let base = "{\"a\": {\"x\": 1}, \"b\": {}, \"c\": 0}";
        let ours = "{\"a\": {}, \"b\": {}, \"c\": 1}";
        let theirs =
            "{\n  \"a\": {\"x\": 1},\n  \"b\": {\n    \"y\": 2,\n    \"z\": 3\n  },\n  \"c\": 0\n}";
        assert_eq!(
            merged(base, ours, theirs),
            "{\"a\": {}, \"b\": {\n  \"y\": 2,\n  \"z\": 3\n}, \"c\": 1}"
        );
        // Ours emptied it and theirs added to it: theirs' layout fills
        // the brackets.
        let theirs = "{\n  \"k\": 1,\n  \"n\": 2\n}";
        assert_eq!(merged("{\"k\": 1}", "{}", theirs), "{\n  \"n\": 2\n}");
    }

    #[test]
    fn the_same_change_on_both_sides_is_taken_once() {
        let ours = PACKAGE.replace("3.7.1", "3.7.4");
        assert_eq!(merged(PACKAGE, &ours, &ours.clone()), ours);
    }

    #[test]
    fn the_same_dependency_bumped_twice_declines() {
        let ours = PACKAGE.replace("3.7.1", "3.7.4");
        let theirs = PACKAGE.replace("3.7.1", "3.8.0");
        assert_eq!(
            declined(PACKAGE, &ours, &theirs),
            Decline {
                pointer: "/devDependencies/@types~1luxon".into(),
                reason: Reason::BothChanged,
            }
        );
    }

    #[test]
    fn a_change_to_what_the_other_side_deleted_declines() {
        let base = r#"{"a": {"x": 1}, "b": 1}"#;
        assert_eq!(
            declined(base, r#"{"b": 1}"#, r#"{"a": {"x": 2}, "b": 1}"#).reason,
            Reason::DeletedAndChanged {
                deleted_by: Side::Ours
            }
        );
        assert_eq!(
            declined(base, r#"{"a": {"x": 2}, "b": 1}"#, r#"{"b": 1}"#),
            Decline {
                pointer: "/a".into(),
                reason: Reason::DeletedAndChanged {
                    deleted_by: Side::Theirs
                },
            }
        );
        // Deleted on both sides, or deleted on one and untouched on the
        // other, is not a conflict.
        assert_eq!(merged(base, r#"{"b": 2}"#, r#"{"b": 1}"#), r#"{"b": 2}"#);
        assert_eq!(merged(base, r#"{"a": {"x": 1}}"#, r#"{"b": 1}"#), "{}");
    }

    #[test]
    fn objects_both_sides_added_are_merged_key_by_key() {
        let base = r#"{"a": 1}"#;
        let ours = r#"{"a": 1, "new": {"x": 1, "both": true}}"#;
        let theirs = r#"{"a": 1, "new": {"both": true, "y": 2}}"#;
        assert_eq!(
            merged(base, ours, theirs),
            r#"{"a": 1, "new": {"x": 1, "both": true, "y": 2}}"#
        );
        let theirs = r#"{"a": 1, "new": {"x": 2}}"#;
        assert_eq!(declined(base, ours, theirs).pointer, "/new/x");
        assert_eq!(
            declined(base, r#"{"a": 1, "n": 1}"#, r#"{"a": 1, "n": 2}"#).reason,
            Reason::BothAdded
        );
    }

    #[test]
    fn an_empty_base_is_an_add_add() {
        assert_eq!(
            merged("", r#"{"b": 1}"#, r#"{"a": 2}"#),
            r#"{"a": 2, "b": 1}"#
        );
        assert_eq!(merged("\n", "[1]\n", "[1]\n"), "[1]\n");
        assert_eq!(declined("", "[1]", "[2]").reason, Reason::BothAdded);
    }

    #[test]
    fn edits_to_different_parts_of_an_array_merge() {
        let base = "[\n  \"a\",\n  \"b\",\n  \"c\"\n]";
        let ours = "[\n  \"A\",\n  \"b\",\n  \"c\"\n]";
        let theirs = "[\n  \"a\",\n  \"b\",\n  \"c\",\n  \"d\"\n]";
        assert_eq!(
            merged(base, ours, theirs),
            "[\n  \"A\",\n  \"b\",\n  \"c\",\n  \"d\"\n]"
        );
        // Neighbouring elements edited in place, one per side.
        assert_eq!(merged("[1, 2, 3]", "[9, 2, 3]", "[1, 8, 3]"), "[9, 8, 3]");
        // One side removes, the other edits elsewhere.
        assert_eq!(
            merged("[1, 2, 3, 4]", "[1, 3, 4]", "[1, 2, 3, 5]"),
            "[1, 3, 5]"
        );
    }

    #[test]
    fn two_insertions_at_the_same_point_decline() {
        // A `required` list would want both; an ordered pipeline might
        // want either order or neither. Not ours to guess.
        assert_eq!(
            declined(
                r#"{"required": ["a"]}"#,
                r#"{"required": ["a", "b"]}"#,
                r#"{"required": ["a", "c"]}"#
            ),
            Decline {
                pointer: "/required".into(),
                reason: Reason::ArrayEditsOverlap,
            }
        );
        // The same insertion on both sides is one insertion.
        assert_eq!(
            merged(r#"["a"]"#, r#"["a", "b"]"#, r#"["a", "b"]"#),
            r#"["a", "b"]"#
        );
    }

    #[test]
    fn elements_edited_in_place_merge_recursively() {
        let base = r#"[{"name": "a", "in": "query"}, {"name": "b"}]"#;
        let ours = r#"[{"name": "a", "in": "path"}, {"name": "b"}]"#;
        let theirs = r#"[{"name": "a", "in": "query", "required": true}, {"name": "b"}]"#;
        assert_eq!(
            merged(base, ours, theirs),
            r#"[{"name": "a", "in": "path", "required": true}, {"name": "b"}]"#
        );
    }

    #[test]
    fn a_moved_element_is_not_merged_in_place() {
        let base = r#"[{"k": 1}, {"k": 2}]"#;
        let ours = r#"[{"k": 2}, {"k": 1, "x": 0}]"#;
        let theirs = r#"[{"k": 1}, {"k": 2, "y": 0}]"#;
        assert_eq!(
            declined(base, ours, theirs).reason,
            Reason::ArrayEditsOverlap
        );
    }

    #[test]
    fn a_type_change_against_an_edit_declines() {
        assert_eq!(
            declined(r#"{"a": [1]}"#, r#"{"a": {"x": 1}}"#, r#"{"a": [1, 2]}"#).reason,
            Reason::BothChanged
        );
    }

    #[test]
    fn invalid_json_declines() {
        let e = declined("{}", "{,}", "{}");
        assert!(
            matches!(
                e.reason,
                Reason::NotJson {
                    side: Side::Ours,
                    ..
                }
            ),
            "{e:?}"
        );
        assert_eq!(
            e.to_string(),
            "ours is not valid JSON: expected a string at byte 1"
        );
        let e = declined(r#"{"a":1,"a":2}"#, "{}", r#"{"b": 1}"#);
        assert!(
            matches!(
                e.reason,
                Reason::NotJson {
                    side: Side::Base,
                    ..
                }
            ),
            "{e:?}"
        );
    }

    #[test]
    fn untouched_regions_are_copied_byte_for_byte() {
        // Odd spacing, a blank line, number spellings and a key order
        // nothing sorts: all of it survives a change elsewhere.
        let base = "{ \"z\" :1.50 ,\n\n  \"a\":[ 1,2 ],\"m\": {\"q\":  true}}\r\n";
        let ours = base.replace("1.50", "1.5e0");
        let theirs = base.replace("true", "false");
        assert_eq!(
            merged(base, &ours, &theirs),
            "{ \"z\" :1.5e0 ,\n\n  \"a\":[ 1,2 ],\"m\": {\"q\":  false}}\r\n"
        );
    }

    #[test]
    fn a_reformatted_side_does_not_hide_the_other_sides_change() {
        let base = r#"{"a": 1, "b": 2}"#;
        let ours = "{\n    \"a\": 1,\n    \"b\": 2\n}";
        let theirs = r#"{"a": 1, "b": 3}"#;
        assert_eq!(merged(base, ours, theirs), r#"{"a": 1, "b": 3}"#);
    }

    #[test]
    fn theirs_text_is_shifted_to_ours_depth() {
        let base = "{\n  \"a\": {\n    \"x\": 1\n  }\n}";
        let ours = "{\n  \"a\": {\n    \"x\": 1\n  },\n  \"b\": 0\n}";
        // Theirs indents by four where ours indents by two. Theirs' block
        // is shifted to start at ours' depth; its inner steps stay
        // theirs'.
        let theirs = "{\n    \"a\": {\n        \"x\": 1,\n        \"y\": {\n            \"z\": 2\n        }\n    }\n}";
        assert_eq!(
            merged(base, ours, theirs),
            "{\n  \"a\": {\n      \"x\": 1,\n      \"y\": {\n          \"z\": 2\n      }\n  },\n  \"b\": 0\n}"
        );
    }

    #[test]
    fn the_read_back_check_compares_structure_not_text() {
        let v = value::parse(r#"{"a": [1, {"b": 2}]}"#).unwrap_or_else(|e| panic!("{e}"));
        let Kind::Object(m) = &v.kind else { panic!() };
        let Kind::Array(items) = &m[0].value.kind else {
            panic!()
        };
        let good = Expect::Object(vec![(
            "a",
            Expect::Array(vec![Expect::Ours(&items[0]), Expect::Theirs(&items[1])]),
        )]);
        assert!(matches(&v, &good));
        let wrong_key = Expect::Object(vec![("b", Expect::Ours(&m[0].value))]);
        assert!(!matches(&v, &wrong_key));
        let short = Expect::Object(vec![("a", Expect::Array(vec![Expect::Ours(&items[0])]))]);
        assert!(!matches(&v, &short));
    }

    #[test]
    fn indentation_helpers() {
        assert_eq!(line_indent("a\n  \t\"b\": 1", 6), "  \t");
        assert_eq!(line_indent("x", 0), "");
        assert_eq!(
            reindent("{\n    \"a\": 1\n  }", "  ", "\t"),
            "{\n\t  \"a\": 1\n\t}"
        );
        assert!(matches!(reindent("[1, 2]", "  ", ""), Cow::Borrowed(_)));
    }
}
