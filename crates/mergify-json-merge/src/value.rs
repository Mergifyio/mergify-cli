//! A lossless JSON reader.
//!
//! Every value keeps the byte span it was read from, so the merge can
//! copy what did not change verbatim instead of re-serialising the
//! document. `serde_json` cannot do that: its `Value` keeps neither
//! spans nor (without the `preserve_order` feature, which would change
//! key order for every other crate in the binary) key order.
//!
//! The reader is strict RFC 8259 — no comments, no trailing commas — and
//! also refuses what JSON permits but a merge cannot reason about:
//! duplicate keys (which one wins is up to the reader) and lone UTF-16
//! surrogates (not representable as text).

use std::fmt;

/// Deeper input is refused rather than recursed into: a stack overflow
/// kills the process with a signal, and git aborts the WHOLE merge on a
/// driver that dies that way instead of recording one conflicted path.
pub(crate) const MAX_DEPTH: usize = 256;

#[derive(Debug)]
pub(crate) struct Value {
    /// Byte offset of the value's first character.
    pub start: usize,
    /// Byte offset just past the value's last character.
    pub end: usize,
    /// Structural hash: equal values have equal hashes. Lets the array
    /// alignment compare elements in constant time in the common case.
    pub hash: u64,
    pub kind: Kind,
}

#[derive(Debug)]
pub(crate) enum Kind {
    Null,
    Bool(bool),
    /// As written. `1` and `1.0` compare unequal, which can only make
    /// the merge decline more often, never merge wrongly.
    Number(String),
    /// Decoded, so `"A"` and `"A"` compare equal.
    String(String),
    Array(Vec<Value>),
    Object(Vec<Member>),
}

#[derive(Debug)]
pub(crate) struct Member {
    /// Byte offset of the key's opening quote.
    pub start: usize,
    pub key: String,
    pub value: Value,
}

impl Value {
    /// The `(start, end)` span of each item of a container: the whole
    /// `"key": value` for an object member, the value for an array
    /// element. Empty for a scalar.
    pub(crate) fn item_spans(&self) -> Vec<(usize, usize)> {
        match &self.kind {
            Kind::Array(items) => items.iter().map(|v| (v.start, v.end)).collect(),
            Kind::Object(members) => members.iter().map(|m| (m.start, m.value.end)).collect(),
            _ => Vec::new(),
        }
    }
}

/// Structural equality: same type, same content, and — for objects —
/// the same keys in the same order. Key order counts so that a side
/// whose only change was reordering keys is not mistaken for unchanged.
pub(crate) fn same(a: &Value, b: &Value) -> bool {
    if a.hash != b.hash {
        return false;
    }
    match (&a.kind, &b.kind) {
        (Kind::Null, Kind::Null) => true,
        (Kind::Bool(x), Kind::Bool(y)) => x == y,
        (Kind::Number(x), Kind::Number(y)) | (Kind::String(x), Kind::String(y)) => x == y,
        (Kind::Array(x), Kind::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Kind::Object(x), Kind::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .all(|(p, q)| p.key == q.key && same(&p.value, &q.value))
        }
        _ => false,
    }
}

#[derive(Debug)]
pub(crate) struct ParseError {
    pub offset: usize,
    pub message: &'static str,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

/// Parse a whole document. A leading byte-order mark is skipped (spans
/// stay relative to the full text, so the merge keeps ours' BOM).
pub(crate) fn parse(text: &str) -> Result<Value, ParseError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        pos: 0,
        depth: 0,
    };
    if text.starts_with('\u{feff}') {
        parser.pos = '\u{feff}'.len_utf8();
    }
    parser.skip_ws();
    let value = parser.value()?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(parser.error("trailing characters after the JSON value"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn error(&self, message: &'static str) -> ParseError {
        ParseError {
            offset: self.pos,
            message,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8, message: &'static str) -> Result<(), ParseError> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(message))
        }
    }

    fn value(&mut self) -> Result<Value, ParseError> {
        let start = self.pos;
        let kind = match self.peek() {
            Some(b'{') => self.object()?,
            Some(b'[') => self.array()?,
            Some(b'"') => Kind::String(self.string()?),
            Some(b't') => self.literal("true", Kind::Bool(true))?,
            Some(b'f') => self.literal("false", Kind::Bool(false))?,
            Some(b'n') => self.literal("null", Kind::Null)?,
            Some(b'-' | b'0'..=b'9') => self.number()?,
            Some(_) => return Err(self.error("unexpected character")),
            None => return Err(self.error("unexpected end of input")),
        };
        Ok(Value {
            start,
            end: self.pos,
            hash: hash_of(&kind),
            kind,
        })
    }

    fn literal(&mut self, word: &'static str, kind: Kind) -> Result<Kind, ParseError> {
        if self.bytes[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(kind)
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn digits(&mut self) -> usize {
        let from = self.pos;
        while let Some(b'0'..=b'9') = self.peek() {
            self.pos += 1;
        }
        self.pos - from
    }

    fn number(&mut self) -> Result<Kind, ParseError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.error("invalid number")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if self.digits() == 0 {
                return Err(self.error("invalid number"));
            }
        }
        if let Some(b'e' | b'E') = self.peek() {
            self.pos += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.pos += 1;
            }
            if self.digits() == 0 {
                return Err(self.error("invalid number"));
            }
        }
        Ok(Kind::Number(self.text[start..self.pos].to_owned()))
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let mut code = 0;
        for _ in 0..4 {
            let digit = match self.peek() {
                Some(b @ b'0'..=b'9') => b - b'0',
                Some(b @ b'a'..=b'f') => b - b'a' + 10,
                Some(b @ b'A'..=b'F') => b - b'A' + 10,
                _ => return Err(self.error("invalid \\u escape")),
            };
            code = code * 16 + u32::from(digit);
            self.pos += 1;
        }
        Ok(code)
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.expect(b'"', "expected a string")?;
        let mut out = String::new();
        loop {
            // Copy the run up to the next quote, backslash or control
            // character in one go. Those are all ASCII, so the slice
            // boundaries always fall on character boundaries.
            let run = self.pos;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            out.push_str(&self.text[run..self.pos]);
            match self.peek() {
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let escaped = match self.peek() {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        Some(b'u') => {
                            self.pos += 1;
                            out.push(self.unicode_escape()?);
                            continue;
                        }
                        _ => return Err(self.error("invalid escape")),
                    };
                    self.pos += 1;
                    out.push(escaped);
                }
                Some(_) => return Err(self.error("control character in string")),
                None => return Err(self.error("unterminated string")),
            }
        }
    }

    /// Decode what follows a `\u`, pairing surrogates.
    fn unicode_escape(&mut self) -> Result<char, ParseError> {
        let high = self.hex4()?;
        let code = if (0xD800..0xDC00).contains(&high) {
            if !self.bytes[self.pos..].starts_with(b"\\u") {
                return Err(self.error("lone surrogate in \\u escape"));
            }
            self.pos += 2;
            let low = self.hex4()?;
            if !(0xDC00..0xE000).contains(&low) {
                return Err(self.error("lone surrogate in \\u escape"));
            }
            0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
        } else {
            high
        };
        char::from_u32(code).ok_or_else(|| self.error("lone surrogate in \\u escape"))
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("nesting too deep"));
        }
        self.pos += 1;
        self.skip_ws();
        Ok(())
    }

    fn array(&mut self) -> Result<Kind, ParseError> {
        self.enter()?;
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Kind::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_ws();
                }
                Some(b']') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(Kind::Array(items));
                }
                _ => return Err(self.error("expected `,` or `]`")),
            }
        }
    }

    fn object(&mut self) -> Result<Kind, ParseError> {
        self.enter()?;
        let mut members: Vec<Member> = Vec::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Kind::Object(members));
        }
        loop {
            let start = self.pos;
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':', "expected `:`")?;
            self.skip_ws();
            let value = self.value()?;
            members.push(Member { start, key, value });
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_ws();
                }
                Some(b'}') => {
                    self.pos += 1;
                    self.depth -= 1;
                    reject_duplicate_keys(&members)?;
                    return Ok(Kind::Object(members));
                }
                _ => return Err(self.error("expected `,` or `}`")),
            }
        }
    }
}

/// Sorting borrowed keys rather than filling a set of owned ones: this
/// runs for every object of a two-megabyte schema, three times a merge.
fn reject_duplicate_keys(members: &[Member]) -> Result<(), ParseError> {
    let mut keys: Vec<(&str, usize)> = members.iter().map(|m| (m.key.as_str(), m.start)).collect();
    keys.sort_unstable();
    match keys.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        Some(pair) => Err(ParseError {
            offset: pair[0].1.max(pair[1].1),
            message: "duplicate key",
        }),
        None => Ok(()),
    }
}

/// 64-bit FNV-1a, written out because the std hasher is randomly seeded
/// per process and nothing here needs more than a fast pre-filter.
struct Fnv(u64);

impl Fnv {
    fn new(tag: u8) -> Self {
        let mut h = Self(0xcbf2_9ce4_8422_2325);
        h.write(&[tag]);
        h
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn write_str(&mut self, s: &str) {
        // Length-prefixed, so `["ab", "c"]` and `["a", "bc"]` differ.
        self.write(&s.len().to_le_bytes());
        self.write(s.as_bytes());
    }
}

fn hash_of(kind: &Kind) -> u64 {
    let mut h;
    match kind {
        Kind::Null => h = Fnv::new(0),
        Kind::Bool(b) => {
            h = Fnv::new(1);
            h.write(&[u8::from(*b)]);
        }
        Kind::Number(n) => {
            h = Fnv::new(2);
            h.write_str(n);
        }
        Kind::String(s) => {
            h = Fnv::new(3);
            h.write_str(s);
        }
        Kind::Array(items) => {
            h = Fnv::new(4);
            for item in items {
                h.write(&item.hash.to_le_bytes());
            }
        }
        Kind::Object(members) => {
            h = Fnv::new(5);
            for member in members {
                h.write_str(&member.key);
                h.write(&member.value.hash.to_le_bytes());
            }
        }
    }
    h.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> Value {
        parse(text).unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

    fn err(text: &str) -> &'static str {
        parse(text).expect_err(text).message
    }

    #[test]
    fn spans_point_at_the_source_text() {
        let text = "{\n  \"a\": [1, true],\n  \"b\": \"x\"\n}\n";
        let v = ok(text);
        let Kind::Object(members) = &v.kind else {
            panic!("not an object")
        };
        assert_eq!(
            &text[v.start..v.end],
            "{\n  \"a\": [1, true],\n  \"b\": \"x\"\n}"
        );
        assert_eq!(
            &text[members[0].start..members[0].value.end],
            "\"a\": [1, true]"
        );
        assert_eq!(&text[members[1].value.start..members[1].value.end], "\"x\"");
    }

    #[test]
    fn strings_are_decoded() {
        let v = ok(r#""aA\n😀\/""#);
        assert!(matches!(&v.kind, Kind::String(s) if s == "aA\n\u{1f600}/"));
        assert!(same(&ok(r#""A""#), &ok(r#""A""#)));
    }

    #[test]
    fn numbers_compare_as_written() {
        assert!(!same(&ok("1"), &ok("1.0")));
        assert!(same(&ok("-0.5e+3"), &ok("-0.5e+3")));
    }

    #[test]
    fn key_order_is_part_of_equality() {
        assert!(!same(&ok(r#"{"a":1,"b":2}"#), &ok(r#"{"b":2,"a":1}"#)));
        assert!(same(&ok(r#"{"a":1, "b":2}"#), &ok("{\"a\": 1,\n\"b\": 2}")));
    }

    #[test]
    fn a_bom_is_skipped() {
        let v = ok("\u{feff}{}");
        assert_eq!(v.start, 3);
    }

    #[test]
    fn rejects_what_a_merge_cannot_reason_about() {
        assert_eq!(err(r#"{"a":1,"a":2}"#), "duplicate key");
        assert_eq!(err(r#""\ud800""#), "lone surrogate in \\u escape");
        assert_eq!(err(r#""\udc00""#), "lone surrogate in \\u escape");
        assert_eq!(err("[1,]"), "unexpected character");
        assert_eq!(
            err("{} // comment"),
            "trailing characters after the JSON value"
        );
        assert_eq!(err("01"), "trailing characters after the JSON value");
        assert_eq!(err("1."), "invalid number");
        assert_eq!(err("\"a\tb\""), "control character in string");
        assert_eq!(err(""), "unexpected end of input");
        assert_eq!(err("truth"), "invalid literal");
    }

    #[test]
    fn nesting_is_bounded() {
        let deep = "[".repeat(MAX_DEPTH + 1) + &"]".repeat(MAX_DEPTH + 1);
        assert_eq!(err(&deep), "nesting too deep");
        let fine = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        ok(&fine);
    }
}
