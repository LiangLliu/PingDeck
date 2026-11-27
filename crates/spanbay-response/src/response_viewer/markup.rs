//! XML/HTML re-indentation for the response viewer.
//!
//! Single responsibility: split markup into tokens (tags, comments, CDATA, declarations,
//! processing instructions, raw-text elements and text) and write them back one per line,
//! two spaces per nesting level. It is a formatter, not a validating parser: malformed
//! input never fails, it just produces the nearest sensible layout.
//!
//! Every token scan only moves forward, and a quote or bracket only opens a span when a
//! closing partner exists later in the input, so the whole pass is O(N).

/// One level of indentation.
const INDENT: &str = "  ";

/// Deepest indentation written; deeper nesting is still tracked but not indented further, so
/// the output stays linear in the input instead of quadratic.
const MAX_INDENT_LEVELS: usize = 64;

/// HTML elements that never have content or a closing tag, so they do not nest.
const VOID_ELEMENTS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// HTML elements whose content is kept verbatim as part of one token.
const RAW_TEXT_ELEMENTS: [&str; 4] = ["script", "style", "pre", "textarea"];

/// What a token does to the nesting depth.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Character data; trimmed, never changes the depth.
    Text,
    /// A start tag that opens a nesting level.
    Open,
    /// An end tag that closes one.
    Close,
    /// Anything else that stands alone (self-closing or void tag, comment, CDATA, declaration,
    /// processing instruction, raw-text element with its content).
    Standalone,
}

/// One slice of the source with its role.
struct Token<'a> {
    text: &'a str,
    kind: Kind,
}

/// Splits `source` into [`Token`]s in document order.
struct Tokens<'a> {
    source: &'a str,
    cursor: usize,
    html: bool,
    /// Byte offsets of the last `"`, `'` and `]`; a delimiter after them can never be closed.
    last_double_quote: Option<usize>,
    last_single_quote: Option<usize>,
    last_bracket: Option<usize>,
}

impl<'a> Tokens<'a> {
    fn new(source: &'a str, html: bool) -> Self {
        Self {
            source,
            cursor: 0,
            html,
            last_double_quote: source.rfind('"'),
            last_single_quote: source.rfind('\''),
            last_bracket: source.rfind(']'),
        }
    }

    /// True when a markup construct (not plain text) starts at the `<` at `position`.
    fn at_markup(&self, position: usize) -> bool {
        let bytes = self.source.as_bytes();
        if bytes.get(position) != Some(&b'<') {
            return false;
        }
        match bytes.get(position + 1) {
            Some(b'!' | b'?') => true,
            Some(b'/') => self.name_starts_at(position + 2),
            _ => self.name_starts_at(position + 1),
        }
    }

    /// True when the character at `position` can begin a tag name: an ASCII letter, `_`, `:` or
    /// any non-ASCII character.
    fn name_starts_at(&self, position: usize) -> bool {
        self.source
            .get(position..)
            .and_then(|rest| rest.chars().next())
            .is_some_and(|character| {
                character.is_ascii_alphabetic()
                    || character == '_'
                    || character == ':'
                    || !character.is_ascii()
            })
    }

    /// End of the text run starting at `start` (a lone `<` belongs to the text).
    fn text_end(&self, start: usize) -> usize {
        let mut from = start;
        while let Some(offset) = self.source[from..].find('<') {
            let position = from + offset;
            if position > start && self.at_markup(position) {
                return position;
            }
            from = position + 1;
        }
        self.source.len()
    }

    /// Where a markup token starting at `start` ends, and what it is.
    fn markup(&self, start: usize) -> (usize, Kind) {
        let rest = &self.source[start..];
        if rest.starts_with("<!--") {
            return (self.until(start, 4, "-->"), Kind::Standalone);
        }
        if rest.starts_with("<![CDATA[") {
            return (self.until(start, 9, "]]>"), Kind::Standalone);
        }
        if rest.starts_with("<?") {
            return (self.until(start, 2, "?>"), Kind::Standalone);
        }
        if rest.starts_with("<!") {
            let end = self.tag_end(start + 2, false);
            return (
                end.unwrap_or_else(|| self.trimmed_len(start)),
                Kind::Standalone,
            );
        }
        if let Some(after_slash) = rest.strip_prefix("</") {
            let end = rest
                .find('>')
                .map_or_else(|| self.trimmed_len(start), |offset| start + offset + 1);
            // A stray `</br>` has no start tag to match, so it must not unnest.
            let void_end_tag = self.html && is_one_of(tag_name(after_slash), &VOID_ELEMENTS);
            return (
                end,
                if void_end_tag {
                    Kind::Standalone
                } else {
                    Kind::Close
                },
            );
        }

        let Some(end) = self.tag_end(start + 1, true) else {
            return (self.trimmed_len(start), Kind::Standalone);
        };
        let tag = &self.source[start..end];
        let name = tag_name(tag);
        if tag.ends_with("/>") {
            return (end, Kind::Standalone);
        }
        if self.html && is_one_of(name, &VOID_ELEMENTS) {
            return (end, Kind::Standalone);
        }
        if self.html && is_one_of(name, &RAW_TEXT_ELEMENTS) {
            return (self.raw_text_end(end, name), Kind::Standalone);
        }
        (end, Kind::Open)
    }

    /// End just after `terminator`, searched from `start + skip`; an unterminated construct
    /// runs to the end of the input without its trailing whitespace.
    fn until(&self, start: usize, skip: usize, terminator: &str) -> usize {
        let from = start + skip;
        self.source[from..].find(terminator).map_or_else(
            || self.trimmed_len(start),
            |offset| from + offset + terminator.len(),
        )
    }

    /// End offset of the input once trailing whitespace is dropped; used for unterminated tokens
    /// that start at `start`.
    fn trimmed_len(&self, start: usize) -> usize {
        start + self.source[start..].trim_end().len()
    }

    /// End just after the `>` that closes the tag or declaration whose body starts at `from`,
    /// or `None` when it is never closed. Quoted spans do not end it: attribute values for tags
    /// (a quote only opens after `=`), any quoted string for declarations, plus a `[...]`
    /// internal subset for declarations.
    fn tag_end(&self, from: usize, is_tag: bool) -> Option<usize> {
        let bytes = self.source.as_bytes();
        let mut position = from;
        let mut previous = 0_u8;
        while position < bytes.len() {
            let byte = bytes[position];
            let quote_limit = match byte {
                b'"' => self.last_double_quote,
                b'\'' => self.last_single_quote,
                _ => None,
            };
            let opens_quote =
                quote_limit.is_some_and(|last| last > position) && (!is_tag || previous == b'=');
            if opens_quote {
                // A closing quote exists after `position`, so this find cannot fail.
                let closing = self.source[position + 1..].find(byte as char);
                position = closing.map_or(bytes.len(), |offset| position + 1 + offset + 1);
                previous = byte;
                continue;
            }
            if !is_tag && byte == b'[' && self.last_bracket.is_some_and(|last| last > position) {
                let closing = self.source[position + 1..].find(']');
                position = closing.map_or(bytes.len(), |offset| position + 1 + offset + 1);
                previous = b']';
                continue;
            }
            if byte == b'>' {
                return Some(position + 1);
            }
            if !byte.is_ascii_whitespace() {
                previous = byte;
            }
            position += 1;
        }
        None
    }

    /// End just after the closing tag of the raw-text element `name` whose start tag ended at
    /// `content_start`; a missing closing tag keeps everything to the end of the input.
    fn raw_text_end(&self, content_start: usize, name: &str) -> usize {
        let bytes = self.source.as_bytes();
        let mut from = content_start;
        while let Some(offset) = self.source[from..].find("</") {
            let position = from + offset;
            let name_end = position + 2 + name.len();
            let matches_name = bytes
                .get(position + 2..name_end)
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name.as_bytes()));
            let boundary = bytes
                .get(name_end)
                .is_none_or(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>'));
            if matches_name && boundary {
                return self.source[position..]
                    .find('>')
                    .map_or_else(|| self.trimmed_len(position), |close| position + close + 1);
            }
            from = position + 2;
        }
        self.trimmed_len(content_start)
    }
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        while self.cursor < self.source.len() {
            let start = self.cursor;
            let (end, kind) = if self.at_markup(start) {
                self.markup(start)
            } else {
                (self.text_end(start), Kind::Text)
            };
            self.cursor = end.max(start + 1);
            let raw = &self.source[start..end];
            let text = if kind == Kind::Text { raw.trim() } else { raw };
            if !text.is_empty() {
                return Some(Token { text, kind });
            }
        }
        None
    }
}

/// The element name of a start tag such as `<div class="x">`.
fn tag_name(tag: &str) -> &str {
    let body = tag.strip_prefix('<').unwrap_or(tag);
    let end = body
        .find(|character: char| character.is_ascii_whitespace() || matches!(character, '/' | '>'))
        .unwrap_or(body.len());
    &body[..end]
}

/// ASCII case-insensitive membership test.
fn is_one_of(name: &str, names: &[&str]) -> bool {
    names
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// Indents markup: one token per line, two spaces per nesting level (the indentation stops
/// growing at [`MAX_INDENT_LEVELS`]). `html` additionally treats void elements as leaves and
/// keeps raw-text elements verbatim. Input with nothing but whitespace comes back unchanged.
#[must_use]
pub(super) fn pretty_markup(text: &str, html: bool) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    let mut depth: usize = 0;
    let mut first = true;
    for token in Tokens::new(text, html) {
        if token.kind == Kind::Close {
            depth = depth.saturating_sub(1);
        }
        if !first {
            out.push('\n');
        }
        first = false;
        for _ in 0..depth.min(MAX_INDENT_LEVELS) {
            out.push_str(INDENT);
        }
        out.push_str(token.text);
        if token.kind == Kind::Open {
            depth += 1;
        }
    }
    if first { text.to_owned() } else { out }
}

#[cfg(test)]
mod tests {
    use super::pretty_markup;

    #[test]
    fn pathological_input_stays_linear_and_never_panics() {
        for unit in [
            "<a b=\"",
            "<a b='",
            "<!x [",
            "<!-- ",
            "<![CDATA[",
            "<?p ",
            "<script>",
            "</",
        ] {
            let input = unit.repeat(50_000);
            for html in [false, true] {
                assert!(!pretty_markup(&input, html).is_empty(), "{unit}");
            }
        }
    }

    #[test]
    fn multibyte_text_around_tags_keeps_char_boundaries() {
        assert_eq!(
            pretty_markup("é<a b=\"é>é\">é</a>é<!--é-->", false),
            "é\n<a b=\"é>é\">\n  é\n</a>\né\n<!--é-->"
        );
    }
}
