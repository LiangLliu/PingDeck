use std::borrow::Cow;
use std::cmp::Ordering;

use super::json_ast::{Json, decode_string, number_len};
use super::{INVALID_ESCAPE, MAX_JSON_PATH_NODES, UNSUPPORTED_FILTER};

/// One selector inside a `[…]` union.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Selector {
    /// A quoted member name.
    Key(String),
    /// An array index (negative counts from the end).
    Index(i64),
}

/// One parsed JSONPath segment.
#[derive(Debug, PartialEq)]
pub(super) enum Segment {
    /// `.name` — member lookup, fanning out over array elements.
    Key(String),
    /// `[n]` — array index.
    Index(i64),
    /// `[a,b]` — selectors applied in order.
    Union(Vec<Selector>),
    /// `[*]` or `.*` — every array element or object value.
    Wildcard,
    /// `[start:end]` — array slice with open bounds.
    Slice {
        /// Inclusive start, `None` for the beginning.
        start: Option<i64>,
        /// Exclusive end, `None` for the end of the array.
        end: Option<i64>,
    },
    /// `..name` — recursive descent in document order.
    Descend(String),
    /// `[?(@.path OP literal)]` — keeps the children for which the predicate holds.
    Filter(Filter),
}

/// A filter predicate: a member path with an optional comparison.
#[derive(Debug, PartialEq)]
pub(super) struct Filter {
    /// Member names after `@`; empty compares the child itself (`[?(@ OP literal)]`).
    path: Vec<String>,
    /// `None` is the existence form `[?(@.path)]`.
    test: Option<(Comparison, Literal)>,
}

/// Comparison operators allowed in a filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comparison {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Right-hand side of a filter comparison.
#[derive(Debug, PartialEq)]
enum Literal {
    Number(f64),
    String(String),
    Bool(bool),
    Null,
}

pub(super) fn parse_json_path(expression: &str) -> Result<Vec<Segment>, String> {
    let path = expression.trim();
    if path.is_empty() {
        return Err("Enter a JSONPath expression, for example $.items[*].id".to_owned());
    }
    if !path.starts_with('$') {
        return Err("JSONPath must start with \"$\"".to_owned());
    }

    let mut index = 1;
    let mut segments = Vec::new();

    while index < path.len() {
        let character = path[index..].chars().next().unwrap_or('\0');
        if character == '.' {
            if path[index + 1..].starts_with('.') {
                let (name, next) = read_name(path, index + 2)?;
                segments.push(Segment::Descend(name));
                index = next;
                continue;
            }
            index += 1;
            if path[index..].starts_with('*') {
                index += 1;
                segments.push(Segment::Wildcard);
                continue;
            }
            let (name, next) = read_name(path, index)?;
            segments.push(Segment::Key(name));
            index = next;
            continue;
        }
        if character == '[' {
            let (segment, next) = read_bracket(path, index + 1)?;
            segments.push(segment);
            index = next;
            continue;
        }
        return Err(format!(
            "Unexpected \"{character}\" at position {}",
            index + 1
        ));
    }

    Ok(segments)
}

/// Selects every match of `segments` in `root`, in document order.
pub(super) fn select_json_path<'a, N: PathNode>(
    root: &'a N,
    segments: &[Segment],
) -> Result<Vec<&'a N>, String> {
    let mut current = vec![root];

    for segment in segments {
        let mut next = Vec::new();
        for value in current {
            apply_segment(segment, value, &mut next)?;
            check_node_limit(next.len())?;
        }
        if next.is_empty() {
            return Ok(next);
        }
        current = next;
    }

    Ok(current)
}

/// Errors when a working set holds more than [`MAX_JSON_PATH_NODES`] nodes.
fn check_node_limit(count: usize) -> Result<(), String> {
    if count > MAX_JSON_PATH_NODES {
        return Err(format!(
            "JSONPath matched too many nodes (limit {MAX_JSON_PATH_NODES})."
        ));
    }
    Ok(())
}

fn is_name_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '$' | '-')
}

/// Byte offset just past the run of member-name characters starting at `start`.
fn name_end(text: &str, start: usize) -> usize {
    text[start..]
        .char_indices()
        .find(|&(_, character)| !is_name_char(character))
        .map_or(text.len(), |(offset, _)| start + offset)
}

fn read_name(path: &str, start: usize) -> Result<(String, usize), String> {
    let end = name_end(path, start);
    if end == start {
        return Err(format!(
            "Expected a property name at position {}",
            start + 1
        ));
    }
    Ok((path[start..end].to_owned(), end))
}

/// Reads a `[...]` selector; `start` points just past the opening bracket.
fn read_bracket(path: &str, start: usize) -> Result<(Segment, usize), String> {
    let mut items: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut index = start;

    while index < path.len() {
        let character = path[index..].chars().next().unwrap_or('\0');
        if let Some(active) = quote {
            current.push(character);
            index += character.len_utf8();
            if character == '\\' {
                if let Some(escaped) = path[index..].chars().next() {
                    current.push(escaped);
                    index += escaped.len_utf8();
                }
            } else if character == active {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => {
                quote = Some(character);
                current.push(character);
                index += 1;
            }
            // A filter owns its commas; only plain selectors split into a union.
            ',' if !current.trim_start().starts_with('?') => {
                items.push(std::mem::take(&mut current));
                index += 1;
            }
            ']' => {
                items.push(std::mem::take(&mut current));
                return Ok((bracket_segment(&items)?, index + 1));
            }
            _ => {
                current.push(character);
                index += character.len_utf8();
            }
        }
    }

    Err("Unclosed \"[\" in JSONPath".to_owned())
}

fn bracket_segment(raw: &[String]) -> Result<Segment, String> {
    let items: Vec<&str> = raw.iter().map(|item| item.trim()).collect();
    if let [only] = items.as_slice() {
        if *only == "*" {
            return Ok(Segment::Wildcard);
        }
        if only.starts_with('?') {
            return parse_filter(only).map(Segment::Filter);
        }
        if !only.starts_with(['\'', '"']) && only.contains(':') {
            return slice_segment(only);
        }
    }

    let mut selectors = items
        .iter()
        .map(|item| parse_selector(item))
        .collect::<Result<Vec<_>, _>>()?;

    if selectors.len() != 1 {
        return Ok(Segment::Union(selectors));
    }

    Ok(match selectors.remove(0) {
        Selector::Key(name) => Segment::Key(name),
        Selector::Index(index) => Segment::Index(index),
    })
}

fn slice_segment(raw: &str) -> Result<Segment, String> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() != 2 {
        return Err(format!("Unsupported slice \"{raw}\" · use [start:end]"));
    }

    let bound = |part: &str| -> Result<Option<i64>, String> {
        let text = part.trim();
        if text.is_empty() {
            return Ok(None);
        }
        text.parse::<i64>()
            .map(Some)
            .map_err(|_| format!("Slice bound \"{text}\" is not a number"))
    };

    Ok(Segment::Slice {
        start: bound(parts[0])?,
        end: bound(parts[1])?,
    })
}

fn parse_selector(raw: &str) -> Result<Selector, String> {
    if raw.is_empty() {
        return Err("Empty selector in JSONPath brackets".to_owned());
    }
    if raw.starts_with(['\'', '"']) {
        return unquote(raw)
            .map(Selector::Key)
            .map_err(|error| match error {
                QuoteError::Malformed => format!("Unterminated or malformed quoted key {raw}"),
                QuoteError::InvalidEscape => INVALID_ESCAPE.to_owned(),
            });
    }
    raw.parse::<i64>()
        .map(Selector::Index)
        .map_err(|_| format!("Unsupported selector \"{raw}\""))
}

/// Parses `?(@.a.b)`, `?(@.a.b OP literal)` or `?(@ OP literal)`; the bracket text is trimmed.
fn parse_filter(raw: &str) -> Result<Filter, String> {
    let unsupported = || UNSUPPORTED_FILTER.to_owned();
    let inner = raw
        .strip_prefix('?')
        .map(str::trim_start)
        .and_then(|text| text.strip_prefix('('))
        .and_then(|text| text.strip_suffix(')'))
        .map(str::trim)
        .ok_or_else(unsupported)?;
    let (path, rest) = inner
        .strip_prefix('@')
        .and_then(parse_member_chain)
        .ok_or_else(unsupported)?;
    let rest = rest.trim_start();
    if rest.is_empty() {
        // Bare `@` always exists, so only the comparison form is meaningful for it.
        return if path.is_empty() {
            Err(unsupported())
        } else {
            Ok(Filter { path, test: None })
        };
    }
    let (comparison, literal) = parse_comparison(rest)?;
    Ok(Filter {
        path,
        test: Some((comparison, literal)),
    })
}

/// Reads `.a.b` member names; returns them with the unread remainder.
fn parse_member_chain(text: &str) -> Option<(Vec<String>, &str)> {
    let mut path = Vec::new();
    let mut rest = text;
    while let Some(after_dot) = rest.strip_prefix('.') {
        let end = name_end(after_dot, 0);
        if end == 0 {
            return None;
        }
        path.push(after_dot[..end].to_owned());
        rest = &after_dot[end..];
    }
    Some((path, rest))
}

fn parse_comparison(text: &str) -> Result<(Comparison, Literal), String> {
    const OPERATORS: [(&str, Comparison); 6] = [
        ("==", Comparison::Eq),
        ("!=", Comparison::Ne),
        ("<=", Comparison::Le),
        (">=", Comparison::Ge),
        ("<", Comparison::Lt),
        (">", Comparison::Gt),
    ];
    let (rest, comparison) = OPERATORS
        .iter()
        .find_map(|(symbol, comparison)| text.strip_prefix(symbol).map(|rest| (rest, *comparison)))
        .ok_or_else(|| UNSUPPORTED_FILTER.to_owned())?;
    Ok((comparison, parse_literal(rest.trim())?))
}

fn parse_literal(text: &str) -> Result<Literal, String> {
    let unsupported = || UNSUPPORTED_FILTER.to_owned();
    match text {
        "true" => Ok(Literal::Bool(true)),
        "false" => Ok(Literal::Bool(false)),
        "null" => Ok(Literal::Null),
        _ if text.starts_with(['\'', '"']) => {
            unquote(text)
                .map(Literal::String)
                .map_err(|error| match error {
                    QuoteError::Malformed => unsupported(),
                    QuoteError::InvalidEscape => INVALID_ESCAPE.to_owned(),
                })
        }
        _ if number_len(text.as_bytes()) == text.len() => {
            text.parse().map(Literal::Number).map_err(|_| unsupported())
        }
        _ => Err(unsupported()),
    }
}

/// Why a quoted key or filter literal could not be unquoted.
enum QuoteError {
    /// Text after the closing quote, an unescaped inner quote, or a dangling `\`.
    Malformed,
    /// A `\` followed by something other than the supported escapes.
    InvalidEscape,
}

/// Unwraps exactly one quoted string. Escapes are `\\ \' \" \/ \b \f \n \r \t \uXXXX` (a high
/// surrogate must be followed by a `\u` low surrogate, the pair becoming one character); any other
/// escape is [`QuoteError::InvalidEscape`], and anything else wrong with the quoting is
/// [`QuoteError::Malformed`].
fn unquote(text: &str) -> Result<String, QuoteError> {
    let quote = text.chars().next().ok_or(QuoteError::Malformed)?;
    let body = text
        .get(1..)
        .and_then(|rest| rest.strip_suffix(quote))
        .ok_or(QuoteError::Malformed)?;
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(character) = chars.next() {
        match character {
            '\\' => {
                let escape = chars.next().ok_or(QuoteError::Malformed)?;
                match escape {
                    '\\' | '\'' | '"' | '/' => out.push(escape),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => out.push(unquote_unicode(&mut chars)?),
                    _ => return Err(QuoteError::InvalidEscape),
                }
            }
            _ if character == quote => return Err(QuoteError::Malformed),
            _ => out.push(character),
        }
    }
    Ok(out)
}

/// Reads the four hex digits after `\u`; a high surrogate must be followed immediately by a
/// `\uDC00..DFFF` low surrogate and the pair becomes one character. A lone surrogate is an
/// [`QuoteError::InvalidEscape`].
fn unquote_unicode(chars: &mut std::str::Chars<'_>) -> Result<char, QuoteError> {
    let unit = read_escape_hex4(chars)?;
    match unit {
        0xD800..0xDC00 => {
            if chars.next() != Some('\\') || chars.next() != Some('u') {
                return Err(QuoteError::InvalidEscape);
            }
            let low = read_escape_hex4(chars)?;
            if !(0xDC00..0xE000).contains(&low) {
                return Err(QuoteError::InvalidEscape);
            }
            char::from_u32(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00))
                .ok_or(QuoteError::InvalidEscape)
        }
        _ => char::from_u32(unit).ok_or(QuoteError::InvalidEscape),
    }
}

/// Reads exactly four hex digits of a `\u` escape.
fn read_escape_hex4(chars: &mut std::str::Chars<'_>) -> Result<u32, QuoteError> {
    let mut value = 0_u32;
    for _ in 0..4 {
        let digit = chars
            .next()
            .and_then(|character| character.to_digit(16))
            .ok_or(QuoteError::InvalidEscape)?;
        value = value * 16 + digit;
    }
    Ok(value)
}

/// What the selector needs from a JSON tree. Implemented for the lexical tree the viewer
/// filters and for `serde_json::Value`, so assertions, Flow extraction and Mock rules share
/// one dialect with the filter.
pub(super) trait PathNode: Sized {
    /// Value of the first member named `name`; `None` for non-objects.
    fn member(&self, name: &str) -> Option<&Self>;
    /// The elements of an array; `None` for non-arrays.
    fn elements(&self) -> Option<&[Self]>;
    /// Calls `visit` on each array element or object value; scalars have no children.
    fn for_each_child<'a>(&'a self, visit: impl FnMut(&'a Self));
    fn as_number(&self) -> Option<f64>;
    fn as_text(&self) -> Option<Cow<'_, str>>;
    fn as_bool(&self) -> Option<bool>;
    fn is_null(&self) -> bool;
}

impl PathNode for Json<'_> {
    fn member(&self, name: &str) -> Option<&Self> {
        self.get(name)
    }

    fn elements(&self) -> Option<&[Self]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    fn for_each_child<'a>(&'a self, mut visit: impl FnMut(&'a Self)) {
        match self {
            Json::Array(items) => items.iter().for_each(&mut visit),
            Json::Object(members) => members.iter().for_each(|(_, child)| visit(child)),
            _ => {}
        }
    }

    fn as_number(&self) -> Option<f64> {
        match self {
            Json::Number(lexeme) => lexeme.parse().ok(),
            _ => None,
        }
    }

    fn as_text(&self) -> Option<Cow<'_, str>> {
        match self {
            Json::String(lexeme) => Some(decode_string(lexeme)),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(flag) => Some(*flag),
            _ => None,
        }
    }

    fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }
}

impl PathNode for serde_json::Value {
    fn member(&self, name: &str) -> Option<&Self> {
        self.as_object().and_then(|members| members.get(name))
    }

    fn elements(&self) -> Option<&[Self]> {
        self.as_array().map(Vec::as_slice)
    }

    fn for_each_child<'a>(&'a self, mut visit: impl FnMut(&'a Self)) {
        match self {
            Self::Array(items) => items.iter().for_each(&mut visit),
            Self::Object(members) => members.values().for_each(&mut visit),
            _ => {}
        }
    }

    fn as_number(&self) -> Option<f64> {
        self.as_f64()
    }

    fn as_text(&self) -> Option<Cow<'_, str>> {
        self.as_str().map(Cow::Borrowed)
    }

    fn as_bool(&self) -> Option<bool> {
        self.as_bool()
    }

    fn is_null(&self) -> bool {
        self.is_null()
    }
}

/// Appends the nodes `segment` selects from `value` to `out`; errors past the node ceiling.
fn apply_segment<'a, N: PathNode>(
    segment: &Segment,
    value: &'a N,
    out: &mut Vec<&'a N>,
) -> Result<(), String> {
    match segment {
        Segment::Key(name) => select_key(name, value, out),
        Segment::Index(index) => select_index(*index, value, out),
        Segment::Union(selectors) => {
            for selector in selectors {
                match selector {
                    Selector::Key(name) => select_key(name, value, out),
                    Selector::Index(index) => select_index(*index, value, out),
                }
                check_node_limit(out.len())?;
            }
        }
        Segment::Wildcard => value.for_each_child(|child| out.push(child)),
        Segment::Slice { start, end } => select_slice(*start, *end, value, out),
        Segment::Descend(name) => collect_descendants(name, value, out)?,
        Segment::Filter(filter) => value.for_each_child(|child| {
            if filter.matches(child) {
                out.push(child);
            }
        }),
    }
    Ok(())
}

/// `.name` on an object is a member lookup. On an array an all-digit name is an index
/// (`items.0.id`, like JavaScript property access); any other name fans out over the elements.
fn select_key<'a, N: PathNode>(name: &str, value: &'a N, out: &mut Vec<&'a N>) {
    match value.elements() {
        Some(items) => match name.parse::<usize>() {
            Ok(position) if name.bytes().all(|byte| byte.is_ascii_digit()) => {
                out.extend(items.get(position));
            }
            _ => out.extend(items.iter().filter_map(|item| item.member(name))),
        },
        None => out.extend(value.member(name)),
    }
}

fn select_index<'a, N: PathNode>(index: i64, value: &'a N, out: &mut Vec<&'a N>) {
    let Some(items) = value.elements() else {
        return;
    };
    let length = i64::try_from(items.len()).unwrap_or(i64::MAX);
    let resolved = if index < 0 { length + index } else { index };
    if let Ok(position) = usize::try_from(resolved) {
        out.extend(items.get(position));
    }
}

fn select_slice<'a, N: PathNode>(
    start: Option<i64>,
    end: Option<i64>,
    value: &'a N,
    out: &mut Vec<&'a N>,
) {
    let Some(items) = value.elements() else {
        return;
    };
    let length = i64::try_from(items.len()).unwrap_or(i64::MAX);
    let normalize = |bound: Option<i64>, default: i64| -> usize {
        let raw = bound.unwrap_or(default);
        let resolved = if raw < 0 { length + raw } else { raw };
        usize::try_from(resolved.clamp(0, length)).unwrap_or(0)
    };
    let from = normalize(start, 0);
    let to = normalize(end, length);
    if from < to {
        out.extend(&items[from..to]);
    }
}

/// Every value under `name` at any depth, in document order; stops at the node ceiling.
fn collect_descendants<'a, N: PathNode>(
    name: &str,
    value: &'a N,
    out: &mut Vec<&'a N>,
) -> Result<(), String> {
    if let Some(found) = value.member(name) {
        out.push(found);
        check_node_limit(out.len())?;
    }
    let mut outcome = Ok(());
    value.for_each_child(|child| {
        if outcome.is_ok() {
            outcome = collect_descendants(name, child, out);
        }
    });
    outcome
}

impl Filter {
    /// Whether `child` passes: path present and, for the comparison form, the test holds.
    fn matches<N: PathNode>(&self, child: &N) -> bool {
        let found = self
            .path
            .iter()
            .try_fold(child, |node, name| node.member(name));
        match (found, &self.test) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(value), Some((comparison, literal))) => compare(value, *comparison, literal),
        }
    }
}

/// Evaluates `value OP literal`; mismatched types are only ever unequal.
fn compare<N: PathNode>(value: &N, comparison: Comparison, literal: &Literal) -> bool {
    let mismatch = comparison == Comparison::Ne;
    match literal {
        Literal::Number(expected) => value.as_number().map_or(mismatch, |number| {
            number
                .partial_cmp(expected)
                .is_some_and(|ordering| holds(comparison, ordering))
        }),
        Literal::String(expected) => value.as_text().map_or(mismatch, |text| {
            holds(comparison, text.encode_utf16().cmp(expected.encode_utf16()))
        }),
        Literal::Bool(expected) => value.as_bool().map_or(mismatch, |flag| {
            equality_only(comparison, flag == *expected)
        }),
        Literal::Null if value.is_null() => equality_only(comparison, true),
        Literal::Null => mismatch,
    }
}

fn holds(comparison: Comparison, ordering: Ordering) -> bool {
    match comparison {
        Comparison::Eq => ordering.is_eq(),
        Comparison::Ne => ordering.is_ne(),
        Comparison::Lt => ordering.is_lt(),
        Comparison::Le => ordering.is_le(),
        Comparison::Gt => ordering.is_gt(),
        Comparison::Ge => ordering.is_ge(),
    }
}

/// Booleans and null only support `==` and `!=`.
fn equality_only(comparison: Comparison, equal: bool) -> bool {
    match comparison {
        Comparison::Eq => equal,
        Comparison::Ne => !equal,
        _ => false,
    }
}
