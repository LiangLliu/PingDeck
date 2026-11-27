//! `{{...}}` response templating: a bounded Handlebars subset with Mockoon's and SpanBay's
//! helpers.
//!
//! Single responsibility: parse a template into text, expressions and blocks (`{{#repeat}}`,
//! `{{#each}}`, `{{#if}}`, `{{#unless}}`, with `{{else}}`), then render it against one request.
//! Sub-expressions in parentheses and `key=value` options are understood. Rendering is bounded:
//! output stops at [`MAX_RENDER_BYTES`], blocks and sub-expressions nest at most
//! [`MAX_DEPTH`] deep, one block runs its body at most [`MAX_BLOCK_ITERATIONS`] times and one
//! render runs at most [`MAX_TOTAL_ITERATIONS`] block bodies. An expression that names no known
//! helper is sent as written, and [`unknown_helpers`] lists those names (the importers warn with
//! it). A template whose blocks do not nest is rendered without blocks: block tags are then sent
//! as written and the plain expressions still render.

use std::collections::{BTreeSet, HashMap};

use serde_json::Value;

use crate::workspace::items::{MockDataBucket, json_path_lookup, json_value_to_string};
use crate::workspace::variables::{dynamic_variable_names, next_random};
use crate::workspace::{VariableScopes, resolve_template_lenient};

use super::routing::{header_value, query_value};
use super::template_extras::{faker_value, format_date, parse_date, random_between};

/// Most bytes one render produces; output past it is dropped.
pub(super) const MAX_RENDER_BYTES: usize = 1024 * 1024;
/// Deepest nesting of blocks and of sub-expressions.
pub(super) const MAX_DEPTH: usize = 16;
/// Most times one `repeat` or `each` block runs its body.
pub(super) const MAX_BLOCK_ITERATIONS: usize = 1000;
/// Most block bodies one render runs in total, across every block.
pub(super) const MAX_TOTAL_ITERATIONS: usize = 10_000;

/// Inline helpers [`Renderer::helper`] answers.
const HELPERS: [&str; 36] = [
    "urlParam",
    "queryParam",
    "header",
    "cookie",
    "body",
    "bodyRaw",
    "method",
    "path",
    "hostname",
    "baseUrl",
    "ip",
    "now",
    "date",
    "int",
    "float",
    "boolean",
    "guid",
    "uuid",
    "faker",
    "data",
    "array",
    "oneOf",
    "eq",
    "gt",
    "gte",
    "lt",
    "lte",
    "and",
    "or",
    "not",
    "includes",
    "lowercase",
    "uppercase",
    "stringify",
    "len",
    "concat",
];

/// Block helpers [`Renderer::block`] answers.
const BLOCKS: [&str; 4] = ["repeat", "each", "if", "unless"];

/// Request data and live buckets visible to a template.
pub(super) struct TemplateContext<'a> {
    pub(super) method: &'a str,
    pub(super) path: &'a str,
    pub(super) query: &'a str,
    pub(super) headers: &'a [(String, String)],
    pub(super) cookies: &'a HashMap<String, String>,
    pub(super) params: HashMap<String, String>,
    pub(super) body: &'a str,
    pub(super) body_json: Option<&'a Value>,
    pub(super) peer_ip: &'a str,
    pub(super) base_url: &'a str,
    /// Live data buckets by id; empty unless a template references `{{data ...}}`.
    pub(super) buckets: &'a HashMap<String, Value>,
    /// Configured buckets, used to resolve `{{data 'name'}}` to a bucket id.
    pub(super) bucket_defs: &'a [MockDataBucket],
}

/// Renders `template` against `context`.
pub(super) fn render(template: &str, context: &TemplateContext<'_>) -> String {
    if !template.contains("{{") {
        return template.to_owned();
    }
    let nodes = parse(template).unwrap_or_else(|| flat(template));
    let mut renderer = Renderer {
        context,
        scopes: Vec::new(),
        out: String::with_capacity(template.len()),
        iterations: 0,
    };
    renderer.nodes(&nodes, 0);
    renderer.out
}

/// The helper names in `template` that [`render`] does not know (sent as written), sorted and
/// without duplicates: `name` for an expression, `#name` for a block, `faker 'x.y'` for an
/// unsupported faker method and `$name` for an unknown dynamic variable.
pub(super) fn unknown_helpers(template: &str) -> Vec<String> {
    if !template.contains("{{") {
        return Vec::new();
    }
    let nodes = parse(template).unwrap_or_else(|| flat(template));
    let mut unknown = BTreeSet::new();
    scan_nodes(&nodes, 0, &mut unknown);
    unknown.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// A parsed piece of a template.
enum Node<'t> {
    Text(&'t str),
    /// `raw` is the tag as written, sent when the expression is not understood.
    Expr {
        raw: &'t str,
        inner: &'t str,
    },
    Block {
        raw_open: &'t str,
        name: &'t str,
        args: &'t str,
        body: Vec<Node<'t>>,
        inverse: Vec<Node<'t>>,
    },
}

/// One `{{...}}` (or `{{{...}}}`) tag: the tag as written, its trimmed content and where it ends.
struct Tag<'t> {
    start: usize,
    end: usize,
    raw: &'t str,
    inner: &'t str,
}

/// The first tag at or after `from`; `None` when no tag opens there or the last one never closes.
fn next_tag(template: &str, from: usize) -> Option<Tag<'_>> {
    let start = from + template.get(from..)?.find("{{")?;
    let triple = template.get(start + 2..)?.starts_with('{');
    let (open, close) = if triple { (3, "}}}") } else { (2, "}}") };
    let inner_start = start + open;
    let inner_end = inner_start + template.get(inner_start..)?.find(close)?;
    let end = inner_end + close.len();
    Some(Tag {
        start,
        end,
        raw: template.get(start..end)?,
        inner: template.get(inner_start..inner_end)?.trim(),
    })
}

/// An open block while its body is parsed.
struct Frame<'t> {
    raw_open: &'t str,
    name: &'t str,
    args: &'t str,
    body: Vec<Node<'t>>,
    inverse: Option<Vec<Node<'t>>>,
}

/// Where the next node goes: the innermost open block (its `else` part once seen) or the root.
fn target<'a, 't>(
    root: &'a mut Vec<Node<'t>>,
    stack: &'a mut [Frame<'t>],
) -> &'a mut Vec<Node<'t>> {
    match stack.last_mut() {
        Some(Frame {
            inverse: Some(inverse),
            ..
        }) => inverse,
        Some(frame) => &mut frame.body,
        None => root,
    }
}

/// The template as nodes, or `None` when its blocks do not nest (a missing or mismatched close,
/// or deeper than [`MAX_DEPTH`]).
fn parse(template: &str) -> Option<Vec<Node<'_>>> {
    let mut root = Vec::new();
    let mut stack: Vec<Frame<'_>> = Vec::new();
    let mut cursor = 0;
    while let Some(tag) = next_tag(template, cursor) {
        if tag.start > cursor {
            target(&mut root, &mut stack).push(Node::Text(template.get(cursor..tag.start)?));
        }
        cursor = tag.end;
        if let Some(open) = tag.inner.strip_prefix('#') {
            if stack.len() >= MAX_DEPTH {
                return None;
            }
            let (name, args) = split_head(open.trim());
            stack.push(Frame {
                raw_open: tag.raw,
                name,
                args,
                body: Vec::new(),
                inverse: None,
            });
        } else if let Some(close) = tag.inner.strip_prefix('/') {
            let frame = stack.pop()?;
            if close.trim() != frame.name {
                return None;
            }
            let block = Node::Block {
                raw_open: frame.raw_open,
                name: frame.name,
                args: frame.args,
                body: frame.body,
                inverse: frame.inverse.unwrap_or_default(),
            };
            target(&mut root, &mut stack).push(block);
        } else if tag.inner == "else"
            && let Some(frame) = stack.last_mut()
            && frame.inverse.is_none()
        {
            frame.inverse = Some(Vec::new());
        } else if tag.inner.starts_with('!') {
            // A comment renders nothing.
        } else {
            target(&mut root, &mut stack).push(Node::Expr {
                raw: tag.raw,
                inner: tag.inner,
            });
        }
    }
    if !stack.is_empty() {
        return None;
    }
    if cursor < template.len() {
        root.push(Node::Text(template.get(cursor..)?));
    }
    Some(root)
}

/// Every tag as a plain expression (block tags then fall through as written).
fn flat(template: &str) -> Vec<Node<'_>> {
    let mut nodes = Vec::new();
    let mut cursor = 0;
    while let Some(tag) = next_tag(template, cursor) {
        if let Some(text) = template
            .get(cursor..tag.start)
            .filter(|text| !text.is_empty())
        {
            nodes.push(Node::Text(text));
        }
        nodes.push(Node::Expr {
            raw: tag.raw,
            inner: tag.inner,
        });
        cursor = tag.end;
    }
    if let Some(text) = template.get(cursor..).filter(|text| !text.is_empty()) {
        nodes.push(Node::Text(text));
    }
    nodes
}

/// `name rest` split at the first whitespace or `(`.
fn split_head(token: &str) -> (&str, &str) {
    match token.find(|c: char| c.is_whitespace() || c == '(') {
        Some(index) => (&token[..index], token[index..].trim()),
        None => (token, ""),
    }
}

/// One argument of a helper call.
enum Arg<'t> {
    /// A quoted string or a bare word that is not a path.
    Lit(String),
    /// The inside of `( ... )`.
    Sub(&'t str),
    /// `this`, `this.x`, `@index`, `@key`, `@first`, `@last` or `.`.
    Path(&'t str),
    /// `key=value`.
    Hash(&'t str, Box<Arg<'t>>),
}

/// Splits helper arguments: quoted strings (an unclosed quote runs to the end), balanced
/// parentheses, `key=value` options and bare words.
fn parse_args(input: &str) -> Vec<Arg<'_>> {
    let mut args = Vec::new();
    let mut rest = input.trim_start();
    while !rest.is_empty() {
        let (arg, tail) = parse_arg(rest, true);
        args.push(arg);
        rest = tail.trim_start();
    }
    args
}

/// One argument and the text after it. A hash value (`allow_hash` false) never opens another
/// `key=`, so `a=b=c` is `a=b` followed by a stray `=` and `c`, and parsing never recurses.
fn parse_arg(input: &str, allow_hash: bool) -> (Arg<'_>, &str) {
    let mut chars = input.char_indices();
    match chars.next() {
        Some((_, quote @ ('\'' | '"'))) => {
            let body = &input[1..];
            match body.find(quote) {
                Some(end) => (Arg::Lit(body[..end].to_owned()), &body[end + 1..]),
                None => (Arg::Lit(body.to_owned()), ""),
            }
        }
        Some((_, '(')) => {
            let end = closing_paren(input).unwrap_or(input.len());
            let inner = input.get(1..end).unwrap_or_default();
            (Arg::Sub(inner), input.get(end + 1..).unwrap_or_default())
        }
        _ => {
            let end = input
                .find(|c: char| c.is_whitespace() || c == '(' || c == '=')
                .unwrap_or(input.len());
            let word = &input[..end];
            let tail = &input[end..];
            if allow_hash
                && let Some(value) = tail.strip_prefix('=')
                && !word.is_empty()
            {
                let (value, tail) = parse_arg(value.trim_start(), false);
                return (Arg::Hash(word, Box::new(value)), tail);
            }
            if word.is_empty() {
                // A stray `=` or `)`: skip one character.
                let skip = input.chars().next().map_or(1, char::len_utf8);
                return (Arg::Lit(String::new()), &input[skip..]);
            }
            let arg = if is_scope_path(word) {
                Arg::Path(word)
            } else {
                Arg::Lit(word.to_owned())
            };
            (arg, tail)
        }
    }
}

/// Byte index of the `)` closing the `(` at index 0, skipping quoted text.
fn closing_paren(input: &str) -> Option<usize> {
    let mut depth = 0_usize;
    let mut quote = None;
    for (index, character) in input.char_indices() {
        match (quote, character) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(character),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_scope_path(word: &str) -> bool {
    word == "this" || word == "." || word.starts_with("this.") || word.starts_with('@')
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// A value an expression produced: request text or JSON (bucket contents, body paths, helper
/// results). JSON prints like the `body` helper: a string as itself, anything else as JSON.
enum Val {
    Text(String),
    Json(Value),
}

impl Val {
    fn into_text(self) -> String {
        match self {
            Self::Text(text) => text,
            Self::Json(value) => json_value_to_string(&value),
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Json(value) => json_value_to_string(value),
        }
    }

    /// Handlebars truthiness: empty text, `null`, `false`, `0`, `""` and `[]` are false.
    fn truthy(&self) -> bool {
        match self {
            Self::Text(text) => !text.is_empty(),
            Self::Json(Value::Null | Value::Bool(false)) => false,
            Self::Json(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
            Self::Json(Value::String(text)) => !text.is_empty(),
            Self::Json(Value::Array(items)) => !items.is_empty(),
            Self::Json(_) => true,
        }
    }

    /// The value as JSON; text that parses as a JSON array or object becomes it.
    fn into_json(self) -> Value {
        match self {
            Self::Json(value) => value,
            Self::Text(text) => match serde_json::from_str::<Value>(&text) {
                Ok(value @ (Value::Array(_) | Value::Object(_))) => value,
                _ => Value::String(text),
            },
        }
    }

    fn number(&self) -> Option<f64> {
        match self {
            Self::Json(Value::Number(number)) => number.as_f64(),
            other => other.text().trim().parse::<f64>().ok(),
        }
    }
}

/// The iteration an `each` or `repeat` block is in.
struct Scope {
    this: Value,
    index: usize,
    key: Option<String>,
    last: bool,
}

struct Renderer<'c, 'a> {
    context: &'c TemplateContext<'a>,
    scopes: Vec<Scope>,
    out: String,
    iterations: usize,
}

impl Renderer<'_, '_> {
    fn full(&self) -> bool {
        self.out.len() >= MAX_RENDER_BYTES
    }

    /// Appends `text`, cut on a character boundary at [`MAX_RENDER_BYTES`].
    fn push(&mut self, text: &str) {
        let room = MAX_RENDER_BYTES.saturating_sub(self.out.len());
        if text.len() <= room {
            self.out.push_str(text);
        } else {
            self.out.push_str(&text[..text.floor_char_boundary(room)]);
        }
    }

    fn nodes(&mut self, nodes: &[Node<'_>], depth: usize) {
        for node in nodes {
            if self.full() {
                return;
            }
            match node {
                Node::Text(text) => self.push(text),
                Node::Expr { raw, inner } => match self.expr(inner, depth) {
                    Some(value) => self.push(&value.into_text()),
                    None => self.push(raw),
                },
                Node::Block { .. } => self.block(node, depth + 1),
            }
        }
    }

    /// Whether another block body may run; counts it.
    fn take_iteration(&mut self) -> bool {
        if self.iterations >= MAX_TOTAL_ITERATIONS || self.full() {
            return false;
        }
        self.iterations += 1;
        true
    }

    fn block(&mut self, node: &Node<'_>, depth: usize) {
        let Node::Block {
            raw_open,
            name,
            args,
            body,
            inverse,
        } = node
        else {
            return;
        };
        let (name, body, inverse) = (*name, body.as_slice(), inverse.as_slice());
        let args = parse_args(args);
        match name {
            "if" | "unless" => {
                let truthy = positional(&args)
                    .next()
                    .and_then(|arg| self.arg(arg, depth))
                    .is_some_and(|value| value.truthy());
                let chosen = if truthy == (name == "if") {
                    body
                } else {
                    inverse
                };
                self.nodes(chosen, depth);
            }
            "repeat" => {
                let numbers: Vec<i64> = positional(&args)
                    .filter_map(|arg| self.arg(arg, depth))
                    .filter_map(|value| value.number())
                    .map(|number| number as i64)
                    .collect();
                let count = match numbers.as_slice() {
                    [] => 0,
                    [count] => *count,
                    [min, max, ..] => random_between(*min, *max),
                };
                let count = usize::try_from(count.max(0))
                    .unwrap_or(0)
                    .min(MAX_BLOCK_ITERATIONS);
                let comma = hash(&args, "comma")
                    .and_then(|arg| self.arg(arg, depth))
                    .is_none_or(|value| value.truthy() && value.text() != "false");
                let this = self
                    .scopes
                    .last()
                    .map_or(Value::Null, |scope| scope.this.clone());
                for index in 0..count {
                    if !self.take_iteration() {
                        break;
                    }
                    if index > 0 && comma {
                        self.push(",");
                    }
                    self.scopes.push(Scope {
                        this: this.clone(),
                        index,
                        key: None,
                        last: index + 1 == count,
                    });
                    self.nodes(body, depth);
                    self.scopes.pop();
                }
            }
            "each" => {
                let items: Vec<(Option<String>, Value)> = match positional(&args)
                    .next()
                    .and_then(|arg| self.arg(arg, depth))
                    .map(Val::into_json)
                {
                    Some(Value::Array(items)) => {
                        items.into_iter().map(|item| (None, item)).collect()
                    }
                    Some(Value::Object(map)) => map
                        .into_iter()
                        .map(|(key, item)| (Some(key), item))
                        .collect(),
                    _ => Vec::new(),
                };
                if items.is_empty() {
                    self.nodes(inverse, depth);
                    return;
                }
                let count = items.len().min(MAX_BLOCK_ITERATIONS);
                for (index, (key, item)) in items.into_iter().take(count).enumerate() {
                    if !self.take_iteration() {
                        break;
                    }
                    self.scopes.push(Scope {
                        this: item,
                        index,
                        key,
                        last: index + 1 == count,
                    });
                    self.nodes(body, depth);
                    self.scopes.pop();
                }
            }
            _ => {
                // An unknown block is sent as written around its rendered body.
                self.push(raw_open);
                self.nodes(body, depth);
                self.push(&format!("{{{{/{name}}}}}"));
            }
        }
    }

    fn arg(&mut self, arg: &Arg<'_>, depth: usize) -> Option<Val> {
        match arg {
            Arg::Lit(text) => Some(Val::Text(text.clone())),
            Arg::Sub(inner) => self.expr(inner, depth + 1),
            Arg::Path(path) => self.scope_path(path),
            Arg::Hash(..) => None,
        }
    }

    /// Positional arguments as text; a missing value (an unresolved path) is empty.
    fn texts(&mut self, args: &[Arg<'_>], depth: usize) -> Vec<String> {
        positional(args)
            .map(|arg| self.arg(arg, depth).map(Val::into_text).unwrap_or_default())
            .collect()
    }

    fn scope_path(&self, path: &str) -> Option<Val> {
        let scope = self.scopes.last()?;
        match path {
            "this" | "." => Some(Val::Json(scope.this.clone())),
            "@index" => Some(Val::Json(Value::from(scope.index))),
            "@first" => Some(Val::Json(Value::Bool(scope.index == 0))),
            "@last" => Some(Val::Json(Value::Bool(scope.last))),
            "@key" => Some(Val::Text(
                scope.key.clone().unwrap_or_else(|| scope.index.to_string()),
            )),
            _ => {
                let rest = path.strip_prefix("this.")?;
                Some(
                    json_path_lookup(&scope.this, rest)
                        .ok()
                        .flatten()
                        .map_or(Val::Text(String::new()), |value| {
                            Val::Json(value.into_owned())
                        }),
                )
            }
        }
    }

    fn expr(&mut self, inner: &str, depth: usize) -> Option<Val> {
        let inner = inner.trim();
        if inner.is_empty() || depth > MAX_DEPTH {
            return None;
        }
        if inner.starts_with('$') {
            let literal = format!("{{{{{inner}}}}}");
            let resolved = resolve_template_lenient(&literal, &VariableScopes::new());
            return (resolved != literal).then_some(Val::Text(resolved));
        }
        if is_scope_path(inner) {
            return self.scope_path(inner);
        }
        let (name, rest) = split_head(inner);
        let args = parse_args(rest);
        if let Some(value) = self.helper(name, &args, depth) {
            return Some(value);
        }
        if !rest.is_empty() {
            return None;
        }
        if let Some(value) = spanbay_token(inner, self.context) {
            return Some(Val::Text(value));
        }
        // Inside `each`, a bare name reads a field of the current item.
        self.scopes
            .last()
            .and_then(|scope| scope.this.get(inner))
            .map(|value| Val::Json(value.clone()))
    }

    fn helper(&mut self, name: &str, args: &[Arg<'_>], depth: usize) -> Option<Val> {
        let context = self.context;
        let text = |value: Option<String>| Some(Val::Text(value.unwrap_or_default()));
        match name {
            "urlParam" | "queryParam" | "header" | "cookie" => {
                let texts = self.texts(args, depth);
                let key = texts.first().map(String::as_str);
                let found = key.and_then(|key| match name {
                    "urlParam" => context.params.get(key).cloned(),
                    "queryParam" => query_value(context.query, key),
                    "header" => header_value(context.headers, key).map(str::to_owned),
                    _ => context.cookies.get(key).cloned(),
                });
                text(found.or_else(|| texts.get(1).cloned()))
            }
            "body" => {
                let texts = self.texts(args, depth);
                match texts.first().filter(|path| !path.is_empty()) {
                    Some(path) => Some(
                        context
                            .body_json
                            .and_then(|body| json_path_lookup(body, path).ok().flatten())
                            .map(|value| Val::Json(value.into_owned()))
                            .unwrap_or_else(|| {
                                Val::Text(texts.get(1).cloned().unwrap_or_default())
                            }),
                    ),
                    None => text(Some(context.body.to_owned())),
                }
            }
            "bodyRaw" => {
                let texts = self.texts(args, depth);
                match texts.first().filter(|path| !path.is_empty()) {
                    Some(path) => text(
                        context
                            .body_json
                            .and_then(|body| json_path_lookup(body, path).ok().flatten())
                            .map(|value| json_value_to_string(&value)),
                    ),
                    None => text(Some(context.body.to_owned())),
                }
            }
            "method" => text(Some(context.method.to_owned())),
            "path" => text(Some(context.path.to_owned())),
            "hostname" => text(Some("127.0.0.1".to_owned())),
            "baseUrl" => text(Some(context.base_url.to_owned())),
            "ip" => text(Some(context.peer_ip.to_owned())),
            "now" => {
                let texts = self.texts(args, depth);
                let now = chrono::Utc::now();
                text(Some(match texts.first() {
                    Some(pattern) => format_date(now, pattern),
                    None => now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                }))
            }
            "date" => {
                let texts = self.texts(args, depth);
                let start = texts.first().and_then(|value| parse_date(value))?;
                let end = texts.get(1).and_then(|value| parse_date(value))?;
                let (low, high) = (start.timestamp_millis(), end.timestamp_millis());
                let picked = chrono::DateTime::from_timestamp_millis(random_between(low, high))?;
                text(Some(match texts.get(2) {
                    Some(pattern) => format_date(picked, pattern),
                    None => picked.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                }))
            }
            "int" => {
                let numbers = self.numbers(args, depth);
                let min = numbers.first().map_or(0, |n| *n as i64);
                let max = numbers
                    .get(1)
                    .map_or(min.saturating_add(100), |n| *n as i64);
                Some(Val::Json(Value::from(random_between(min, max))))
            }
            "float" => {
                let numbers = self.numbers(args, depth);
                let min = numbers.first().copied().unwrap_or(0.0);
                let max = numbers.get(1).copied().unwrap_or(min + 100.0);
                let unit = (next_random() % 1_000_001) as f64 / 1_000_000.0;
                let value = ((min + (max - min) * unit) * 10_000.0).round() / 10_000.0;
                Some(Val::Json(
                    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number),
                ))
            }
            "boolean" => Some(Val::Json(Value::Bool(next_random().is_multiple_of(2)))),
            "guid" | "uuid" => text(Some(resolve_template_lenient(
                "{{$guid}}",
                &VariableScopes::new(),
            ))),
            "faker" => {
                let texts = self.texts(args, depth);
                let method = texts.first()?;
                faker_value(method, &texts[1..]).map(Val::Text)
            }
            "data" => {
                let texts = self.texts(args, depth);
                let value = data_bucket(context, texts.first()?)?;
                match texts.get(1).filter(|path| !path.is_empty()) {
                    Some(path) => Some(
                        json_path_lookup(value, path)
                            .ok()
                            .flatten()
                            .map_or(Val::Text(String::new()), |found| {
                                Val::Json(found.into_owned())
                            }),
                    ),
                    None => Some(Val::Json(value.clone())),
                }
            }
            "array" => {
                let items = positional(args)
                    .map(|arg| self.arg(arg, depth).map_or(Value::Null, Val::into_json))
                    .collect();
                Some(Val::Json(Value::Array(items)))
            }
            "oneOf" => {
                let choices = match positional(args).next().and_then(|arg| self.arg(arg, depth)) {
                    Some(value) => match value.into_json() {
                        Value::Array(items) => items,
                        other => vec![other],
                    },
                    None => Vec::new(),
                };
                if choices.is_empty() {
                    return text(None);
                }
                let index = usize::try_from(next_random() % choices.len() as u64).unwrap_or(0);
                Some(Val::Json(
                    choices.into_iter().nth(index).unwrap_or(Value::Null),
                ))
            }
            "eq" | "gt" | "gte" | "lt" | "lte" => {
                let values: Vec<Val> = positional(args)
                    .map(|arg| self.arg(arg, depth).unwrap_or(Val::Text(String::new())))
                    .collect();
                let [left, right, ..] = values.as_slice() else {
                    return Some(Val::Json(Value::Bool(false)));
                };
                let result = if name == "eq" {
                    left.text() == right.text()
                } else {
                    match (left.number(), right.number()) {
                        (Some(a), Some(b)) => match name {
                            "gt" => a > b,
                            "gte" => a >= b,
                            "lt" => a < b,
                            _ => a <= b,
                        },
                        _ => false,
                    }
                };
                Some(Val::Json(Value::Bool(result)))
            }
            "and" | "or" => {
                let values: Vec<bool> = positional(args)
                    .map(|arg| self.arg(arg, depth).is_some_and(|value| value.truthy()))
                    .collect();
                let result = if name == "and" {
                    !values.is_empty() && values.iter().all(|value| *value)
                } else {
                    values.iter().any(|value| *value)
                };
                Some(Val::Json(Value::Bool(result)))
            }
            "not" => Some(Val::Json(Value::Bool(
                !positional(args)
                    .next()
                    .and_then(|arg| self.arg(arg, depth))
                    .is_some_and(|value| value.truthy()),
            ))),
            "includes" => {
                let texts = self.texts(args, depth);
                let found = match (texts.first(), texts.get(1)) {
                    (Some(haystack), Some(needle)) => haystack.contains(needle.as_str()),
                    _ => false,
                };
                Some(Val::Json(Value::Bool(found)))
            }
            "lowercase" | "uppercase" => {
                let value = self
                    .texts(args, depth)
                    .into_iter()
                    .next()
                    .unwrap_or_default();
                text(Some(if name == "lowercase" {
                    value.to_lowercase()
                } else {
                    value.to_uppercase()
                }))
            }
            "stringify" => {
                let value = positional(args).next().and_then(|arg| self.arg(arg, depth));
                text(value.map(|value| value.into_json().to_string()))
            }
            "len" => {
                let length = match positional(args).next().and_then(|arg| self.arg(arg, depth)) {
                    Some(Val::Json(Value::Array(items))) => items.len(),
                    Some(Val::Json(Value::Object(map))) => map.len(),
                    Some(value) => value.into_text().chars().count(),
                    None => 0,
                };
                Some(Val::Json(Value::from(length)))
            }
            "concat" => text(Some(self.texts(args, depth).concat())),
            _ => None,
        }
    }

    fn numbers(&mut self, args: &[Arg<'_>], depth: usize) -> Vec<f64> {
        positional(args)
            .filter_map(|arg| self.arg(arg, depth))
            .filter_map(|value| value.number())
            .collect()
    }
}

fn positional<'a, 't>(args: &'a [Arg<'t>]) -> impl Iterator<Item = &'a Arg<'t>> {
    args.iter().filter(|arg| !matches!(arg, Arg::Hash(..)))
}

fn hash<'a, 't>(args: &'a [Arg<'t>], key: &str) -> Option<&'a Arg<'t>> {
    args.iter().find_map(|arg| match arg {
        Arg::Hash(name, value) if *name == key => Some(value.as_ref()),
        _ => None,
    })
}

/// `{{data 'name'}}` resolves a bucket by name first, then by id.
fn data_bucket<'c>(context: &TemplateContext<'c>, name: &str) -> Option<&'c Value> {
    context
        .bucket_defs
        .iter()
        .rev()
        .find(|bucket| bucket.name == name)
        .and_then(|bucket| context.buckets.get(&bucket.id))
        .or_else(|| context.buckets.get(name))
}

/// SpanBay's dotted tokens (`params.id`, `query.page`, `headers.x`, `cookies.x`, `body.a.b`,
/// `request.query.x`, `request.method`, `request.path`).
const SPANBAY_PREFIXES: [&str; 6] = [
    "params.",
    "query.",
    "headers.",
    "cookies.",
    "body.",
    "request.query.",
];

fn spanbay_token(token: &str, context: &TemplateContext<'_>) -> Option<String> {
    if let Some(key) = token.strip_prefix("params.") {
        return Some(context.params.get(key).cloned().unwrap_or_default());
    }
    if let Some(key) = token.strip_prefix("query.") {
        return Some(query_value(context.query, key).unwrap_or_default());
    }
    if let Some(name) = token.strip_prefix("headers.") {
        return Some(
            header_value(context.headers, name)
                .map(str::to_owned)
                .unwrap_or_default(),
        );
    }
    if let Some(name) = token.strip_prefix("cookies.") {
        return Some(context.cookies.get(name).cloned().unwrap_or_default());
    }
    if let Some(path) = token.strip_prefix("body.") {
        return Some(
            context
                .body_json
                .and_then(|body| json_path_lookup(body, path).ok().flatten())
                .map(|value| json_value_to_string(&value))
                .unwrap_or_default(),
        );
    }
    if let Some(path) = token.strip_prefix("request.query.") {
        return Some(query_value(context.query, path).unwrap_or_default());
    }
    match token {
        "request.method" => Some(context.method.to_owned()),
        "request.path" => Some(context.path.to_owned()),
        _ => None,
    }
}

fn is_spanbay_token(token: &str) -> bool {
    SPANBAY_PREFIXES
        .iter()
        .any(|prefix| token.starts_with(prefix))
        || matches!(token, "request.method" | "request.path")
}

// ---------------------------------------------------------------------------
// Unknown helper scan
// ---------------------------------------------------------------------------

/// `depth` counts the enclosing `each`/`repeat` blocks, where a bare name may read an item field.
fn scan_nodes(nodes: &[Node<'_>], depth: usize, unknown: &mut BTreeSet<String>) {
    for node in nodes {
        match node {
            Node::Text(_) => {}
            Node::Expr { inner, .. } => scan_expr(inner, depth, 0, unknown),
            Node::Block {
                name,
                args,
                body,
                inverse,
                ..
            } => {
                if BLOCKS.contains(name) {
                    scan_args(args, depth, 0, unknown);
                } else {
                    unknown.insert(format!("#{name}"));
                }
                let inner_depth = if matches!(*name, "each" | "repeat") {
                    depth + 1
                } else {
                    depth
                };
                scan_nodes(body, inner_depth, unknown);
                scan_nodes(inverse, depth, unknown);
            }
        }
    }
}

/// `nesting` counts the enclosing sub-expressions; past [`MAX_DEPTH`] the scan stops, as
/// rendering does.
fn scan_expr(inner: &str, depth: usize, nesting: usize, unknown: &mut BTreeSet<String>) {
    let inner = inner.trim();
    if inner.is_empty() || is_scope_path(inner) || nesting > MAX_DEPTH {
        return;
    }
    if let Some(name) = inner.strip_prefix('$') {
        let name = name.split_whitespace().next().unwrap_or_default();
        let known = dynamic_variable_names()
            .iter()
            .any(|known| known.strip_prefix('$') == Some(name));
        if !known {
            unknown.insert(format!("${name}"));
        }
        return;
    }
    let (name, rest) = split_head(inner);
    if HELPERS.contains(&name) {
        if name == "faker"
            && let Some(Arg::Lit(method)) = parse_args(rest).first()
            && faker_value(method, &[]).is_none()
        {
            unknown.insert(format!("faker '{method}'"));
        }
        scan_args(rest, depth, nesting, unknown);
        return;
    }
    let field_of_item = depth > 0 && rest.is_empty();
    if !(rest.is_empty() && is_spanbay_token(inner)) && !field_of_item {
        unknown.insert(name.to_owned());
    }
}

fn scan_args(args: &str, depth: usize, nesting: usize, unknown: &mut BTreeSet<String>) {
    for arg in parse_args(args) {
        let arg = match arg {
            Arg::Hash(_, value) => *value,
            other => other,
        };
        if let Arg::Sub(inner) = arg {
            scan_expr(inner, depth, nesting + 1, unknown);
        }
    }
}
