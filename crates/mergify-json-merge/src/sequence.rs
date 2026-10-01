//! Element-wise diff3 over three JSON arrays.
//!
//! Aligns ours and theirs against base with a longest common
//! subsequence each, then cuts the three arrays into chunks: a *stable*
//! chunk is one base element both sides kept, and an *unstable* chunk is
//! the stretch between two stable ones, where at least one side changed
//! something. This is diff3 with elements in place of lines.

use std::ops::Range;

use crate::value::Value;
use crate::value::same;

/// Above this many cells the alignment table (4 bytes a cell) is not
/// built and the merge declines. The common prefix and suffix are
/// trimmed first, so only arrays changed at both ends by thousands of
/// elements get here.
const MAX_TABLE_CELLS: usize = 4_000_000;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Chunk {
    /// A base element both sides kept; ours holds it at `o`.
    Stable { o: usize },
    Unstable {
        b: Range<usize>,
        o: Range<usize>,
        t: Range<usize>,
    },
}

pub(crate) struct TooLarge;

pub(crate) fn diff3(
    base: &[Value],
    ours: &[Value],
    theirs: &[Value],
) -> Result<Vec<Chunk>, TooLarge> {
    let to_ours = align(base, ours)?;
    let to_theirs = align(base, theirs)?;
    // Where the next chunk starts, in base, ours and theirs.
    let (mut at_b, mut at_o, mut at_t) = (0, 0, 0);
    let mut chunks = Vec::new();
    loop {
        let stable = (at_b..base.len()).find_map(|j| match (to_ours[j], to_theirs[j]) {
            (Some(in_ours), Some(in_theirs)) => Some((j, in_ours, in_theirs)),
            _ => None,
        });
        let (nb, no, nt) = stable.unwrap_or((base.len(), ours.len(), theirs.len()));
        if nb > at_b || no > at_o || nt > at_t {
            chunks.push(Chunk::Unstable {
                b: at_b..nb,
                o: at_o..no,
                t: at_t..nt,
            });
        }
        if stable.is_none() {
            return Ok(chunks);
        }
        chunks.push(Chunk::Stable { o: no });
        (at_b, at_o, at_t) = (nb + 1, no + 1, nt + 1);
    }
}

/// For each element of `a`, its index in `b` under one longest common
/// subsequence, or `None`. Matched indices increase monotonically.
fn align(a: &[Value], b: &[Value]) -> Result<Vec<Option<usize>>, TooLarge> {
    let mut out = vec![None; a.len()];
    let mut prefix = 0;
    while prefix < a.len() && prefix < b.len() && same(&a[prefix], &b[prefix]) {
        out[prefix] = Some(prefix);
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < a.len() - prefix
        && suffix < b.len() - prefix
        && same(&a[a.len() - 1 - suffix], &b[b.len() - 1 - suffix])
    {
        out[a.len() - 1 - suffix] = Some(b.len() - 1 - suffix);
        suffix += 1;
    }
    let (am, bm) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    if am.is_empty() || bm.is_empty() {
        return Ok(out);
    }
    let width = bm.len() + 1;
    if (am.len() + 1).saturating_mul(width) > MAX_TABLE_CELLS {
        return Err(TooLarge);
    }
    // `len[i * width + j]` is the LCS length of `am[i..]` and `bm[j..]`.
    let mut len = vec![0u32; (am.len() + 1) * width];
    for i in (0..am.len()).rev() {
        for j in (0..bm.len()).rev() {
            len[i * width + j] = if same(&am[i], &bm[j]) {
                len[(i + 1) * width + j + 1] + 1
            } else {
                len[(i + 1) * width + j].max(len[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < am.len() && j < bm.len() {
        if same(&am[i], &bm[j]) {
            out[prefix + i] = Some(prefix + j);
            i += 1;
            j += 1;
        } else if len[(i + 1) * width + j] >= len[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Kind;
    use crate::value::parse;

    fn items(text: &str) -> Vec<Value> {
        match parse(text).map(|v| v.kind) {
            Ok(Kind::Array(items)) => items,
            _ => panic!("not an array: {text}"),
        }
    }

    fn chunks(b: &str, o: &str, t: &str) -> Vec<Chunk> {
        diff3(&items(b), &items(o), &items(t)).unwrap_or_else(|_| panic!("too large"))
    }

    #[test]
    fn alignment_is_a_longest_common_subsequence() {
        let got =
            align(&items("[1,2,3,4,5]"), &items("[0,2,3,9,5,6]")).unwrap_or_else(|_| panic!());
        assert_eq!(got, vec![None, Some(1), Some(2), None, Some(4)]);
    }

    #[test]
    fn separate_edits_land_in_separate_chunks() {
        assert_eq!(
            chunks("[1,2,3]", "[0,1,2,3]", "[1,2,3,4]"),
            vec![
                Chunk::Unstable {
                    b: 0..0,
                    o: 0..1,
                    t: 0..0
                },
                Chunk::Stable { o: 1 },
                Chunk::Stable { o: 2 },
                Chunk::Stable { o: 3 },
                Chunk::Unstable {
                    b: 3..3,
                    o: 4..4,
                    t: 3..4
                },
            ]
        );
    }

    #[test]
    fn edits_with_no_kept_element_between_them_share_a_chunk() {
        assert_eq!(
            chunks("[1,2,3]", "[9,2,3]", "[1,8,3]"),
            vec![
                Chunk::Unstable {
                    b: 0..2,
                    o: 0..2,
                    t: 0..2
                },
                Chunk::Stable { o: 2 },
            ]
        );
    }

    #[test]
    fn a_huge_rewrite_is_refused() {
        let a = format!(
            "[{}]",
            (0..2100)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        let b = format!(
            "[{}]",
            (5000..7100)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(align(&items(&a), &items(&b)).is_err());
    }
}
