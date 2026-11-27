use std::borrow::Cow;
use std::fmt::Write as _;

use super::{INDENT, MAX_JSON_DEPTH};

/// A parsed JSON value that borrows its lexemes from the source text.
#[derive(Debug)]
pub(super) enum Json<'a> {
    Null,
    Bool(bool),
    /// Number lexeme exactly as written.
    Number(&'a str),
    /// String lexeme including its quotes, escape sequences untouched.
    String(&'a str),
    Array(Vec<Json<'a>>),
    /// Members in document order; keys are string lexemes, duplicates kept.
    Object(Vec<(&'a str, Json<'a>)>),
}

impl Json<'_> {
    /// Value of the first member named `key`; `None` for non-objects.
    pub(super) fn get(&self, key: &str) -> Option<&Self> {
        let Self::Object(members) = self else {
            return None;
        };
        members
            .iter()
            .find(|(raw, _)| decode_string(raw) == key)
            .map(|(_, value)| value)
    }

    fn is_container(&self) -> bool {
        matches!(self, Self::Array(_) | Self::Object(_))
    }
}

/// Parses `text` as one complete JSON document; `None` when it is not valid JSON.
pub(super) fn parse_document(text: &str) -> Option<Json<'_>> {
    let mut parser = Parser { text, pos: 0 };
    parser.skip_whitespace();
    let value = parser.value(0)?;
    parser.skip_whitespace();
    (parser.pos == text.len()).then_some(value)
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn byte(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.byte(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, expected: u8) -> bool {
        let found = self.byte() == Some(expected);
        if found {
            self.pos += 1;
        }
        found
    }

    fn keyword(&mut self, word: &str, value: Json<'a>) -> Option<Json<'a>> {
        let found = self.text.as_bytes()[self.pos..].starts_with(word.as_bytes());
        if found {
            self.pos += word.len();
        }
        found.then_some(value)
    }

    fn value(&mut self, depth: usize) -> Option<Json<'a>> {
        match self.byte()? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Json::String),
            b't' => self.keyword("true", Json::Bool(true)),
            b'f' => self.keyword("false", Json::Bool(false)),
            b'n' => self.keyword("null", Json::Null),
            _ => self.number().map(Json::Number),
        }
    }

    fn array(&mut self, depth: usize) -> Option<Json<'a>> {
        if depth >= MAX_JSON_DEPTH {
            return None;
        }
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.eat(b']') {
            return Some(Json::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            if !self.eat(b',') {
                return self.eat(b']').then_some(Json::Array(items));
            }
        }
    }

    fn object(&mut self, depth: usize) -> Option<Json<'a>> {
        if depth >= MAX_JSON_DEPTH {
            return None;
        }
        self.pos += 1;
        let mut members = Vec::new();
        self.skip_whitespace();
        if self.eat(b'}') {
            return Some(Json::Object(members));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            if !self.eat(b':') {
                return None;
            }
            self.skip_whitespace();
            members.push((key, self.value(depth + 1)?));
            self.skip_whitespace();
            if !self.eat(b',') {
                return self.eat(b'}').then_some(Json::Object(members));
            }
        }
    }

    /// Reads a string lexeme (quotes included), validating escapes and control characters.
    fn string(&mut self) -> Option<&'a str> {
        let text = self.text;
        let bytes = text.as_bytes();
        let start = self.pos;
        if bytes.get(start) != Some(&b'"') {
            return None;
        }
        let mut at = start + 1;
        loop {
            match *bytes.get(at)? {
                b'"' => {
                    self.pos = at + 1;
                    return text.get(start..self.pos);
                }
                b'\\' => match *bytes.get(at + 1)? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => at += 2,
                    b'u' => {
                        if !bytes.get(at + 2..at + 6)?.iter().all(u8::is_ascii_hexdigit) {
                            return None;
                        }
                        at += 6;
                    }
                    _ => return None,
                },
                0..=0x1f => return None,
                _ => at += 1,
            }
        }
    }

    fn number(&mut self) -> Option<&'a str> {
        let len = number_len(self.text.as_bytes().get(self.pos..)?);
        if len == 0 {
            return None;
        }
        let start = self.pos;
        self.pos += len;
        self.text.get(start..self.pos)
    }
}

/// Length of the RFC 8259 number at the start of `bytes`, or 0 when there is none.
pub(super) fn number_len(bytes: &[u8]) -> usize {
    let digits = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count()
    };
    let mut end = usize::from(bytes.first() == Some(&b'-'));
    match bytes.get(end) {
        Some(b'0') => end += 1,
        Some(b'1'..=b'9') => end += digits(end),
        _ => return 0,
    }
    if bytes.get(end) == Some(&b'.') {
        let fraction = digits(end + 1);
        if fraction == 0 {
            return 0;
        }
        end += 1 + fraction;
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let mut at = end + 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        let exponent = digits(at);
        if exponent == 0 {
            return 0;
        }
        end = at + exponent;
    }
    end
}

/// One decoded element of a JSON string body.
enum Unit {
    Char(char),
    /// A `\uXXXX` surrogate with no partner; JSON allows it, `char` cannot hold it.
    LoneSurrogate(u32),
}

/// The text between the quotes of a string lexeme.
fn lexeme_body(lexeme: &str) -> &str {
    lexeme
        .get(1..lexeme.len().saturating_sub(1))
        .unwrap_or_default()
}

fn read_hex4(chars: &mut std::str::Chars<'_>) -> u32 {
    chars
        .by_ref()
        .take(4)
        .fold(0, |acc, digit| acc * 16 + digit.to_digit(16).unwrap_or(0))
}

/// Decodes the escapes of a validated string body, joining surrogate pairs.
fn for_each_unit(body: &str, mut emit: impl FnMut(Unit)) {
    let mut chars = body.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            emit(Unit::Char(character));
            continue;
        }
        match chars.next() {
            Some('b') => emit(Unit::Char('\u{8}')),
            Some('f') => emit(Unit::Char('\u{c}')),
            Some('n') => emit(Unit::Char('\n')),
            Some('r') => emit(Unit::Char('\r')),
            Some('t') => emit(Unit::Char('\t')),
            Some('u') => {
                let unit = read_hex4(&mut chars);
                if (0xD800..0xDC00).contains(&unit) {
                    let mut ahead = chars.clone();
                    if ahead.next() == Some('\\') && ahead.next() == Some('u') {
                        let low = read_hex4(&mut ahead);
                        if (0xDC00..0xE000).contains(&low) {
                            chars = ahead;
                            let code = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
                            emit(Unit::Char(char::from_u32(code).unwrap_or('\u{FFFD}')));
                            continue;
                        }
                    }
                    emit(Unit::LoneSurrogate(unit));
                } else if (0xDC00..0xE000).contains(&unit) {
                    emit(Unit::LoneSurrogate(unit));
                } else {
                    emit(Unit::Char(char::from_u32(unit).unwrap_or('\u{FFFD}')));
                }
            }
            Some(other) => emit(Unit::Char(other)),
            None => {}
        }
    }
}

/// Decoded text of a string lexeme (lone surrogates become U+FFFD); borrows when unescaped.
pub(super) fn decode_string(lexeme: &str) -> Cow<'_, str> {
    let body = lexeme_body(lexeme);
    if memchr::memchr(b'\\', body.as_bytes()).is_none() {
        return Cow::Borrowed(body);
    }
    let mut out = String::with_capacity(body.len());
    for_each_unit(body, |unit| match unit {
        Unit::Char(character) => out.push(character),
        Unit::LoneSurrogate(_) => out.push('\u{FFFD}'),
    });
    Cow::Owned(out)
}

/* ------------------------------------------------------------------ *
 * Writers: lexical pretty-printer and JSON-to-YAML.
 * ------------------------------------------------------------------ */

fn push_indent(out: &mut String, levels: usize) {
    out.extend(std::iter::repeat_n(INDENT, levels));
}

/// Writes `items` as a 2-space block between `delimiters`; empty stays compact.
pub(super) fn write_block<T>(
    out: &mut String,
    depth: usize,
    delimiters: (char, char),
    items: &[T],
    mut write_item: impl FnMut(&T, &mut String),
) {
    out.push(delimiters.0);
    if items.is_empty() {
        out.push(delimiters.1);
        return;
    }
    for (index, item) in items.iter().enumerate() {
        out.push_str(if index == 0 { "\n" } else { ",\n" });
        push_indent(out, depth + 1);
        write_item(item, out);
    }
    out.push('\n');
    push_indent(out, depth);
    out.push(delimiters.1);
}

/// Pretty-prints `value` with lexemes verbatim (same layout as `serde_json::to_string_pretty`).
pub(super) fn write_pretty(value: &Json<'_>, depth: usize, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Json::Number(lexeme) | Json::String(lexeme) => out.push_str(lexeme),
        Json::Array(items) => write_block(out, depth, ('[', ']'), items, |item, out| {
            write_pretty(item, depth + 1, out);
        }),
        Json::Object(members) => write_block(out, depth, ('{', '}'), members, |member, out| {
            let (key, child) = member;
            out.push_str(key);
            out.push_str(": ");
            write_pretty(child, depth + 1, out);
        }),
    }
}

/// Writes a string body as a JSON string literal (the escaping `JSON.stringify` uses).
fn write_quoted(body: &str, out: &mut String) {
    out.push('"');
    for_each_unit(body, |unit| match unit {
        Unit::Char('"') => out.push_str("\\\""),
        Unit::Char('\\') => out.push_str("\\\\"),
        Unit::Char('\u{8}') => out.push_str("\\b"),
        Unit::Char('\u{c}') => out.push_str("\\f"),
        Unit::Char('\n') => out.push_str("\\n"),
        Unit::Char('\r') => out.push_str("\\r"),
        Unit::Char('\t') => out.push_str("\\t"),
        Unit::Char(character) if character < ' ' => {
            let _ = write!(out, "\\u{:04x}", u32::from(character));
        }
        Unit::Char(character) => out.push(character),
        Unit::LoneSurrogate(unit) => {
            let _ = write!(out, "\\u{unit:04x}");
        }
    });
    out.push('"');
}

fn write_yaml_string(lexeme: &str, out: &mut String) {
    if lexeme.contains('\\') {
        write_quoted(lexeme_body(lexeme), out);
    } else {
        out.push_str(lexeme);
    }
}

/// Bare words YAML 1.1 and 1.2 readers retype as booleans or null (compared
/// case-insensitively). `~` and numerics never match the identifier shape, so
/// they are quoted by that rule alone.
const YAML_KEYWORDS: [&str; 9] = ["true", "false", "yes", "no", "on", "off", "y", "n", "null"];

/// Mapping keys stay bare only when they look like identifiers
/// (`^[A-Za-z_][A-Za-z0-9_-]*$`) and are not a YAML keyword; every other key
/// is written as a double-quoted, JSON-escaped string. String *values* are
/// always double-quoted, so no reader can retype them.
fn write_yaml_key(lexeme: &str, out: &mut String) {
    let key = decode_string(lexeme);
    let mut chars = key.chars();
    let identifier = chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let keyword = YAML_KEYWORDS
        .iter()
        .any(|word| key.eq_ignore_ascii_case(word));
    if identifier && !keyword {
        out.push_str(&key);
    } else {
        write_quoted(lexeme_body(lexeme), out);
    }
}

/// Writes a scalar or an empty container.
fn write_yaml_leaf(value: &Json<'_>, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Json::Number(lexeme) => out.push_str(lexeme),
        Json::String(lexeme) => write_yaml_string(lexeme, out),
        Json::Array(_) => out.push_str("[]"),
        Json::Object(_) => out.push_str("{}"),
    }
}

pub(super) fn write_yaml(value: &Json<'_>, indent: usize, out: &mut String, inline_first: bool) {
    match value {
        Json::Array(items) if !items.is_empty() => {
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                write_padded(out, indent, inline_first && index == 0);
                out.push_str("- ");
                write_yaml(item, indent + 2, out, true);
            }
        }
        Json::Object(members) if !members.is_empty() => {
            for (index, (key, child)) in members.iter().enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                write_padded(out, indent, inline_first && index == 0);
                write_yaml_key(key, out);
                out.push(':');
                if child.is_container() {
                    out.push('\n');
                    write_yaml(child, indent + 2, out, false);
                } else {
                    out.push(' ');
                    write_yaml_leaf(child, out);
                }
            }
        }
        leaf => {
            write_padded(out, indent, inline_first);
            write_yaml_leaf(leaf, out);
        }
    }
}

fn write_padded(out: &mut String, indent: usize, inline_first: bool) {
    if !inline_first {
        out.extend(std::iter::repeat_n(' ', indent));
    }
}

/* ------------------------------------------------------------------ *
 * JSONPath: `$`, `.name`, `['name']`, `[n]` (negative too), `[*]`, `.*`,
 * `..name`, `[start:end]`, `[a,b]` unions and `[?(@.path OP literal)]`
 * filters. Anything richer is rejected with a readable error.
 * ------------------------------------------------------------------ */
