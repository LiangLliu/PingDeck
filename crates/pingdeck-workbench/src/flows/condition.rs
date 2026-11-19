//! Step conditions ("Run only if…"): a small, bounded expression language.
//!
//! Grammar (keywords are case-insensitive):
//!
//! ```text
//! expr    := and (("||" | "or") and)*
//! and     := unary (("&&" | "and") unary)*
//! unary   := ("!" | "not") unary | primary
//! primary := "(" expr ")" | operand (compare operand)?
//! compare := "==" | "!=" | ">" | ">=" | "<" | "<=" | "contains"
//! operand := '…' | "…" | bare word
//! ```
//!
//! A quoted operand may contain spaces, operators and keywords (`\` escapes the next character).
//! A bare word runs to whitespace, a parenthesis or an operator character and keeps every
//! `{{token}}` whole. The expression is parsed before any token is filled in, so a variable's
//! value never changes how it parses; an unresolved token becomes an empty string. `==` and `!=`
//! compare as numbers when both sides are numbers and as exact text otherwise; `>`, `>=`, `<` and
//! `<=` need numbers on both sides and fail the step otherwise. A lone operand is true unless it
//! is empty, `false`, `0`, `null` or `undefined`. `and`/`or` short-circuit.
//!
//! Bounds: at most [`MAX_CONDITION_BYTES`] bytes, [`MAX_TOKENS`] tokens and [`MAX_DEPTH`] levels of
//! parentheses and negation.
//!
//! Plain comparisons written before this grammar keep working: when the grammar rejects an
//! expression that holds exactly one `==`, `!=` or `contains` outside `{{tokens}}`, with text on
//! both sides, each side is the whole text around it (`{{s}} == Bad Request`,
//! `{{url}} contains https://x/a?b=c`), outer quotes are trimmed after the tokens are filled in,
//! and the sides compare as exact text.

use crate::workspace::{VariableScopes, resolve_template_lenient};

/// Longest condition, in bytes.
pub const MAX_CONDITION_BYTES: usize = 1024;
/// Most tokens (operands, operators, parentheses) one condition may have.
const MAX_TOKENS: usize = 128;
/// Deepest nesting of parentheses and negations.
const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compare {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Contains,
}

impl Compare {
    fn symbol(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Contains => "contains",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Open,
    Close,
    And,
    Or,
    Not,
    Compare(Compare),
    /// Operand text: a bare word as written, or a quoted string without its quotes.
    Operand(String),
}

impl Token {
    fn describe(&self) -> String {
        match self {
            Self::Open => "'('".into(),
            Self::Close => "')'".into(),
            Self::And => "'and'".into(),
            Self::Or => "'or'".into(),
            Self::Not => "'not'".into(),
            Self::Compare(compare) => format!("'{}'", compare.symbol()),
            Self::Operand(text) => format!("'{text}'"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Or(Vec<Node>),
    And(Vec<Node>),
    Not(Box<Node>),
    Compare(String, Compare, String),
    /// A plain comparison the grammar rejects, read as written before it (see the module docs).
    Loose(String, Compare, String),
    Truthy(String),
}

/// Why a condition cannot be parsed, or `None` when it can (an empty one always can). The
/// reason is the text flow save and a failing step show.
#[must_use]
pub fn check_condition(expression: &str) -> Option<String> {
    parse(expression).err()
}

/// Evaluates `expression` against `scopes`: `Ok(true)` runs the step (an empty expression always
/// does), `Ok(false)` skips it, `Err` is why it can be neither (a syntax error, or a number
/// comparison of a non-number), which fails the step.
pub(super) fn evaluate_condition(
    expression: &str,
    scopes: &VariableScopes,
) -> Result<bool, String> {
    match parse(expression)? {
        Some(node) => eval(&node, scopes),
        None => Ok(true),
    }
}

fn parse(expression: &str) -> Result<Option<Node>, String> {
    let expression = expression.trim();
    if expression.is_empty() {
        return Ok(None);
    }
    if expression.len() > MAX_CONDITION_BYTES {
        return Err(format!(
            "the condition is longer than {MAX_CONDITION_BYTES} characters"
        ));
    }
    match parse_strict(expression) {
        Ok(node) => Ok(Some(node)),
        Err(error) => loose(expression).map(Some).ok_or(error),
    }
}

fn parse_strict(expression: &str) -> Result<Node, String> {
    let tokens = tokenize(expression)?;
    let mut parser = Parser {
        tokens,
        position: 0,
        depth: 0,
    };
    let node = parser.or()?;
    if let Some(extra) = parser.peek() {
        return Err(format!(
            "unexpected {} after a complete condition (quote text that contains spaces)",
            extra.describe()
        ));
    }
    Ok(node)
}

/// The plain comparison `expression` holds when it has exactly one `==`, `!=` or `contains`
/// outside `{{tokens}}` and text on both sides of it, each side either wholly quoted or holding
/// no quote (an unclosed quote stays a syntax error).
fn loose(expression: &str) -> Option<Node> {
    let mut found = None;
    let mut index = 0;
    while index < expression.len() {
        let rest = &expression[index..];
        if rest.starts_with("{{")
            && let Some(end) = rest.find("}}")
        {
            index += end + 2;
            continue;
        }
        let operator = [
            ("==", Compare::Eq),
            ("!=", Compare::Ne),
            ("contains", Compare::Contains),
        ]
        .into_iter()
        .find(|(text, _)| rest.starts_with(text));
        if let Some((text, compare)) = operator {
            if found.is_some() {
                return None;
            }
            found = Some((index, text.len(), compare));
            index += text.len();
        } else {
            index += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    let (at, length, compare) = found?;
    let left = expression[..at].trim();
    let right = expression[at + length..].trim();
    let plain = |side: &str| {
        let wholly_quoted = ['"', '\''].into_iter().any(|quote| {
            side.len() >= 2
                && side.starts_with(quote)
                && side.ends_with(quote)
                && !side[1..side.len() - 1].contains(quote)
        });
        !side.is_empty() && (wholly_quoted || !side.contains(['"', '\'']))
    };
    (plain(left) && plain(right)).then(|| Node::Loose(left.to_owned(), compare, right.to_owned()))
}

/// Characters that end a bare word.
fn ends_word(character: char) -> bool {
    character.is_whitespace() || matches!(character, '(' | ')' | '=' | '!' | '<' | '>' | '&' | '|')
}

fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut index = 0;
    while let Some(character) = source[index..].chars().next() {
        if character.is_whitespace() {
            index += character.len_utf8();
            continue;
        }
        if tokens.len() >= MAX_TOKENS {
            return Err(format!("the condition has more than {MAX_TOKENS} parts"));
        }
        let rest = &source[index..];
        let symbol = [
            ("==", Token::Compare(Compare::Eq)),
            ("!=", Token::Compare(Compare::Ne)),
            (">=", Token::Compare(Compare::Ge)),
            ("<=", Token::Compare(Compare::Le)),
            ("&&", Token::And),
            ("||", Token::Or),
            (">", Token::Compare(Compare::Gt)),
            ("<", Token::Compare(Compare::Lt)),
            ("!", Token::Not),
            ("(", Token::Open),
            (")", Token::Close),
        ]
        .into_iter()
        .find(|(text, _)| rest.starts_with(text));
        if let Some((text, token)) = symbol {
            tokens.push(token);
            index += text.len();
            continue;
        }
        match character {
            '=' => return Err("use '==' to compare".into()),
            '&' => return Err("use '&&' or 'and' to combine conditions".into()),
            '|' => return Err("use '||' or 'or' to combine conditions".into()),
            '"' | '\'' => {
                let (text, length) = quoted(rest, character)?;
                tokens.push(Token::Operand(text));
                index += length;
            }
            _ => {
                let length = bare_word(rest);
                let word = &rest[..length];
                tokens.push(match word.to_ascii_lowercase().as_str() {
                    "and" => Token::And,
                    "or" => Token::Or,
                    "not" => Token::Not,
                    "contains" => Token::Compare(Compare::Contains),
                    _ => Token::Operand(word.to_owned()),
                });
                index += length;
            }
        }
    }
    Ok(tokens)
}

/// The content of the string `rest` opens with `quote`, and the bytes it spans.
fn quoted(rest: &str, quote: char) -> Result<(String, usize), String> {
    let mut text = String::new();
    let mut characters = rest.char_indices().skip(1);
    while let Some((offset, character)) = characters.next() {
        if character == '\\' {
            if let Some((_, escaped)) = characters.next() {
                text.push(escaped);
            }
        } else if character == quote {
            return Ok((text, offset + quote.len_utf8()));
        } else {
            text.push(character);
        }
    }
    Err(format!("a {quote} quote is not closed"))
}

/// Bytes of the bare word `rest` starts with; `{{token}}` spans are kept whole.
fn bare_word(rest: &str) -> usize {
    let mut index = 0;
    while index < rest.len() {
        let tail = &rest[index..];
        if tail.starts_with("{{")
            && let Some(end) = tail.find("}}")
        {
            index += end + 2;
            continue;
        }
        match tail.chars().next() {
            Some(character) if !ends_word(character) => index += character.len_utf8(),
            _ => break,
        }
    }
    index
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn eat(&mut self, token: &Token) -> bool {
        if self.peek() == Some(token) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn deeper(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(format!(
                "the condition nests deeper than {MAX_DEPTH} levels"
            ));
        }
        Ok(())
    }

    fn or(&mut self) -> Result<Node, String> {
        let mut items = vec![self.and()?];
        while self.eat(&Token::Or) {
            items.push(self.and()?);
        }
        Ok(if items.len() == 1 {
            items.remove(0)
        } else {
            Node::Or(items)
        })
    }

    fn and(&mut self) -> Result<Node, String> {
        let mut items = vec![self.unary()?];
        while self.eat(&Token::And) {
            items.push(self.unary()?);
        }
        Ok(if items.len() == 1 {
            items.remove(0)
        } else {
            Node::And(items)
        })
    }

    fn unary(&mut self) -> Result<Node, String> {
        if self.eat(&Token::Not) {
            self.deeper()?;
            let inner = self.unary()?;
            self.depth -= 1;
            return Ok(Node::Not(Box::new(inner)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Node, String> {
        let Some(token) = self.peek().cloned() else {
            return Err("the condition ends too early".into());
        };
        self.position += 1;
        match token {
            Token::Open => {
                self.deeper()?;
                let inner = self.or()?;
                if !self.eat(&Token::Close) {
                    return Err("a '(' is not closed".into());
                }
                self.depth -= 1;
                Ok(inner)
            }
            Token::Operand(left) => {
                let Some(Token::Compare(compare)) = self.peek().cloned() else {
                    return Ok(Node::Truthy(left));
                };
                self.position += 1;
                match self.peek().cloned() {
                    Some(Token::Operand(right)) => {
                        self.position += 1;
                        Ok(Node::Compare(left, compare, right))
                    }
                    _ => Err(format!("'{}' needs a value on its right", compare.symbol())),
                }
            }
            Token::Compare(compare) => {
                Err(format!("'{}' needs a value on its left", compare.symbol()))
            }
            other @ (Token::And | Token::Or | Token::Close | Token::Not) => Err(format!(
                "unexpected {} where a value was expected",
                other.describe()
            )),
        }
    }
}

fn eval(node: &Node, scopes: &VariableScopes) -> Result<bool, String> {
    match node {
        Node::Or(items) => {
            for item in items {
                if eval(item, scopes)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Node::And(items) => {
            for item in items {
                if !eval(item, scopes)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Node::Not(inner) => Ok(!eval(inner, scopes)?),
        Node::Truthy(operand) => Ok(truthy(&resolve_side(operand, scopes))),
        Node::Compare(left, compare, right) => compare_values(
            &resolve_side(left, scopes),
            *compare,
            &resolve_side(right, scopes),
        ),
        Node::Loose(left, compare, right) => {
            let (left, right) = (resolve_side(left, scopes), resolve_side(right, scopes));
            let (left, right) = (unquote(&left), unquote(&right));
            Ok(match compare {
                Compare::Contains => left.contains(right),
                Compare::Ne => left != right,
                _ => left == right,
            })
        }
    }
}

/// `value` trimmed of whitespace and outer quotes.
fn unquote(value: &str) -> &str {
    value.trim().trim_matches('"').trim_matches('\'').trim()
}

fn number(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
}

fn compare_values(left: &str, compare: Compare, right: &str) -> Result<bool, String> {
    let numbers = (number(left), number(right));
    let equal = || left == right || matches!(numbers, (Some(a), Some(b)) if a == b);
    match compare {
        Compare::Eq => Ok(equal()),
        Compare::Ne => Ok(!equal()),
        Compare::Contains => Ok(left.contains(right)),
        Compare::Gt | Compare::Ge | Compare::Lt | Compare::Le => {
            let (Some(a), Some(b)) = numbers else {
                let side = if numbers.0.is_none() { "left" } else { "right" };
                return Err(format!(
                    "'{}' compares numbers, but its {side} side is not a number",
                    compare.symbol()
                ));
            };
            Ok(match compare {
                Compare::Gt => a > b,
                Compare::Ge => a >= b,
                Compare::Lt => a < b,
                _ => a <= b,
            })
        }
    }
}

/// Interpolates `{{tokens}}` in one operand (or a whole expression); unresolved tokens become
/// empty.
pub(super) fn resolve_side(side: &str, scopes: &VariableScopes) -> String {
    let mut output = String::with_capacity(side.len());
    let mut rest = side;
    while let Some(start) = rest.find("{{") {
        let Some(length) = rest[start..].find("}}") else {
            break;
        };
        let token = &rest[start..start + length + 2];
        output.push_str(&rest[..start]);
        let value = resolve_template_lenient(token, scopes);
        if value != token {
            output.push_str(&value);
        }
        rest = &rest[start + token.len()..];
    }
    output.push_str(rest);
    output
}

fn truthy(value: &str) -> bool {
    let value = value.trim();
    !(value.is_empty()
        || value.eq_ignore_ascii_case("false")
        || value == "0"
        || value.eq_ignore_ascii_case("null")
        || value.eq_ignore_ascii_case("undefined"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::EnvironmentVariable;

    fn scopes() -> VariableScopes {
        VariableScopes::new().with_local_vars(vec![
            EnvironmentVariable::plain("status", "200").unwrap(),
            EnvironmentVariable::plain("count", "12").unwrap(),
            EnvironmentVariable::plain("token", "abc").unwrap(),
            EnvironmentVariable::plain("skip", "false").unwrap(),
            EnvironmentVariable::plain("eq", "a==b").unwrap(),
            EnvironmentVariable::plain("phrase", "x and y").unwrap(),
            EnvironmentVariable::plain("word", "abc").unwrap(),
        ])
    }

    #[test]
    fn comparisons_logic_quotes_and_variable_values_evaluate_as_written() {
        let scopes = scopes();
        for expression in [
            "",
            "{{status}} == 200",
            "{{status}} == 200.0",
            "{{status}} != 404",
            "{{count}} > 10",
            "{{count}} >= 12 && {{count}} <= 12",
            "{{count}} < 100",
            "{{token}} != ''",
            "{{token}} contains b",
            "{{token}}",
            "{{eq}} == \"a==b\"",
            "{{phrase}} == 'x and y'",
            "{{phrase}} contains \"and\"",
            "{{missing}} == \"\"",
            "not {{skip}}",
            "!{{missing}}",
            "{{skip}} or {{token}} == abc",
            "({{status}} == 404 || {{status}} == 200) AND {{token}}",
            "'it\\'s' == \"it's\"",
            "{{word}} == {{token}}",
            // Short-circuit: the number comparison of an empty value is never reached.
            "{{missing}} != '' and {{missing}} > 1 or {{token}}",
        ] {
            assert_eq!(
                evaluate_condition(expression, &scopes),
                Ok(true),
                "{expression}"
            );
        }
        for expression in [
            "{{status}} == 404",
            "{{count}} > 100",
            "{{skip}}",
            "{{missing}}",
            "{{eq}} != 'a==b'",
            "{{token}} and {{skip}}",
            "not ({{token}})",
        ] {
            assert_eq!(
                evaluate_condition(expression, &scopes),
                Ok(false),
                "{expression}"
            );
        }
    }

    #[test]
    fn malformed_conditions_are_rejected_before_and_while_running() {
        for (expression, reason) in [
            ("{{a}} = 1", "use '==' to compare"),
            ("{{a}} == ", "'==' needs a value on its right"),
            ("== 1", "'==' needs a value on its left"),
            (
                "{{a}} == x contains y",
                "unexpected 'contains' after a complete condition",
            ),
            ("({{a}}", "a '(' is not closed"),
            ("{{a}} == 'open", "a ' quote is not closed"),
            ("{{a}} and", "the condition ends too early"),
            ("{{a}} & {{b}}", "use '&&' or 'and' to combine conditions"),
        ] {
            let error = check_condition(expression).unwrap_or_default();
            assert!(error.starts_with(reason), "{expression}: {error}");
        }
        assert!(check_condition(&"x".repeat(MAX_CONDITION_BYTES + 1)).is_some());
        assert!(check_condition(&format!("{}x", "(".repeat(MAX_DEPTH + 1))).is_some());
        assert!(check_condition(&vec!["x"; MAX_TOKENS + 1].join(" or ")).is_some());
        assert_eq!(check_condition("  "), None);

        // Valid syntax, but a number comparison of text cannot decide: the step fails.
        assert_eq!(
            evaluate_condition("{{token}} > 10", &scopes()),
            Err("'>' compares numbers, but its left side is not a number".into())
        );
        assert!(evaluate_condition("{{missing}} <= 3", &scopes()).is_err());
    }

    #[test]
    fn plain_comparisons_the_grammar_rejects_still_evaluate_as_whole_sides() {
        let scopes = VariableScopes::new().with_local_vars(vec![
            EnvironmentVariable::plain("status_text", "Bad Request").unwrap(),
            EnvironmentVariable::plain("url", "https://x/a?b=c&d=e").unwrap(),
        ]);
        for (expression, expected) in [
            ("{{status_text}} == Bad Request", true),
            ("{{status_text}} != Bad Request", false),
            ("{{status_text}} == 'Bad Request'", true),
            ("{{url}} contains https://x/a?b=c", true),
            ("{{url}} contains https://y/?b=c", false),
        ] {
            assert_eq!(check_condition(expression), None, "{expression}");
            assert_eq!(
                evaluate_condition(expression, &scopes),
                Ok(expected),
                "{expression}"
            );
        }
        // Two comparisons, or a side left empty, stay syntax errors.
        for expression in [
            "{{a}} == x contains y z",
            "{{a}} == ",
            "== b c",
            "{{a}} == 'open text",
        ] {
            assert!(check_condition(expression).is_some(), "{expression}");
        }
    }
}
