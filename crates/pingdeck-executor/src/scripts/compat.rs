//! Static compatibility scan of imported scripts: which host APIs a script uses that the
//! sandbox does not provide, so an import preview can list them before anything runs.

use serde::Serialize;

/// One API a script uses that the sandbox lacks, at its first occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompatIssue {
    /// The call or member as written, e.g. `require`, `CryptoJS.AES`, `setTimeout`.
    pub api: String,
    /// 1-based line of the first use.
    pub line: u32,
}

/// Globals that do not exist in the sandbox.
const MISSING_GLOBALS: &[&str] = &[
    "require",
    "setTimeout",
    "setInterval",
    "clearTimeout",
    "clearInterval",
    "xml2Json",
    "cheerio",
    "_",
];
/// The legacy `postman.*` members the sandbox provides.
const POSTMAN_PROVIDED: &[&str] = &[
    "setEnvironmentVariable",
    "getEnvironmentVariable",
    "clearEnvironmentVariable",
    "setGlobalVariable",
    "getGlobalVariable",
    "clearGlobalVariable",
    "setNextRequest",
];
/// `pm.*` members that do not exist in the sandbox.
const MISSING_PM: &[&str] = &["visualizer", "vault", "require"];
/// Every word a `pm.expect(…)` chain may use (the prelude's `Assertion`). Any other word throws
/// when the script runs, so the scan reports it up front.
const ASSERTION_WORDS: &[&str] = &[
    // Chains and flags.
    "to",
    "be",
    "been",
    "is",
    "that",
    "which",
    "and",
    "has",
    "have",
    "with",
    "at",
    "of",
    "same",
    "but",
    "does",
    "still",
    "also",
    "itself",
    "not",
    "deep",
    "own",
    "nested",
    "ordered",
    "any",
    "all",
    "a",
    "an",
    "include",
    "includes",
    "contain",
    "contains",
    "length",
    "lengthOf",
    // Value properties.
    "ok",
    "true",
    "false",
    "null",
    "undefined",
    "NaN",
    "exist",
    "exists",
    "finite",
    "extensible",
    "sealed",
    "frozen",
    "empty",
    // Value methods.
    "equal",
    "equals",
    "eq",
    "eql",
    "eqls",
    "property",
    "ownProperty",
    "haveOwnProperty",
    "nestedProperty",
    "above",
    "gt",
    "greaterThan",
    "below",
    "lt",
    "lessThan",
    "least",
    "gte",
    "greaterThanOrEqual",
    "most",
    "lte",
    "lessThanOrEqual",
    "within",
    "closeTo",
    "approximately",
    "match",
    "matches",
    "string",
    "oneOf",
    "satisfy",
    "satisfies",
    "instanceof",
    "instanceOf",
    "keys",
    "key",
    "members",
    "respondTo",
    "throw",
    "throws",
    "Throw",
    "jsonSchema",
];
/// Every word a `pm.response.to` chain may use (the prelude's `ResponseAssertion`); like
/// [`ASSERTION_WORDS`], any other word throws when the script runs.
const RESPONSE_WORDS: &[&str] = &[
    // Chains and flags.
    "to",
    "be",
    "have",
    "and",
    "is",
    "that",
    "which",
    "with",
    "been",
    "not",
    // Status, header and body assertions.
    "status",
    "ok",
    "success",
    "info",
    "redirection",
    "clientError",
    "serverError",
    "error",
    "accepted",
    "badRequest",
    "unauthorized",
    "forbidden",
    "notFound",
    "rateLimited",
    "header",
    "body",
    "withBody",
    "json",
    "jsonBody",
    "jsonSchema",
    "responseTime",
];
/// The `CryptoJS` members the sandbox provides.
const CRYPTOJS_PROVIDED: &[&str] = &[
    "MD5",
    "SHA1",
    "SHA256",
    "SHA512",
    "HmacSHA1",
    "HmacSHA256",
    "HmacSHA512",
    "enc",
];
const CRYPTOJS_ENC_PROVIDED: &[&str] = &["Hex", "Base64", "Utf8"];

/// Every API `source` uses that the sandbox lacks, in order of first use. A heuristic token
/// scan: line comments are skipped, string contents are not told apart from code.
#[must_use]
pub fn script_compat_issues(source: &str) -> Vec<CompatIssue> {
    let mut issues: Vec<CompatIssue> = Vec::new();
    let mut report = |api: String, line: usize| {
        if !issues.iter().any(|issue| issue.api == api) {
            let line = u32::try_from(line + 1).unwrap_or(u32::MAX);
            issues.push(CompatIssue { api, line });
        }
    };
    for (line, text) in source.lines().enumerate() {
        let code = text.split("//").next().unwrap_or_default();
        for (word, response) in unsupported_assertion_words(code) {
            if response {
                report(format!("pm.response.to.{word}"), line);
            } else {
                report(format!("pm.expect(…).{word}"), line);
            }
        }
        if sends_a_file(code) {
            report("pm.sendRequest file body".to_owned(), line);
        }
        let chains = member_chains(code);
        for chain in &chains {
            let parts: Vec<&str> = chain.split('.').collect();
            match parts.as_slice() {
                ["pm", "response", "to", words @ ..] => {
                    for word in words {
                        if !RESPONSE_WORDS.contains(word) {
                            report(format!("pm.response.to.{word}"), line);
                        }
                    }
                }
                [global, ..] if MISSING_GLOBALS.contains(global) => {
                    report((*global).to_owned(), line)
                }
                ["pm", member, ..] if MISSING_PM.contains(member) => {
                    report(format!("pm.{member}"), line);
                }
                ["postman", member, ..] if !POSTMAN_PROVIDED.contains(member) => {
                    report(format!("postman.{member}"), line);
                }
                ["CryptoJS", "enc", encoder, ..] if !CRYPTOJS_ENC_PROVIDED.contains(encoder) => {
                    report(format!("CryptoJS.enc.{encoder}"), line);
                }
                ["CryptoJS", member, ..] if !CRYPTOJS_PROVIDED.contains(member) => {
                    report(format!("CryptoJS.{member}"), line);
                }
                _ => {}
            }
        }
    }
    issues
}

/// The words of assertion chains that follow a call (`pm.expect(x).to.be.sucess`, and the chain
/// after a matcher call: `.equal(1).and.foo`) that no assertion provides, each with whether it
/// continues a response assertion (`pm.response.to.have.status(200).and.foo`), which has its own
/// words. Only chains that start with an assertion chain word (`to`, `not`, `and`, `but`, `that`,
/// `which`) are read, so other call chains (`items.map(f).filter(g)`) are left alone.
fn unsupported_assertion_words(code: &str) -> Vec<(&str, bool)> {
    let is_ident = |character: char| character.is_alphanumeric() || matches!(character, '_' | '$');
    let mut found = Vec::new();
    let mut rest = code;
    while let Some(index) = rest.find(").") {
        let response = continues_response_assertion(&code[..code.len() - rest.len() + index]);
        let known = if response {
            RESPONSE_WORDS
        } else {
            ASSERTION_WORDS
        };
        rest = &rest[index + 2..];
        let end = rest
            .find(|character: char| !is_ident(character) && character != '.')
            .unwrap_or(rest.len());
        let words: Vec<&str> = rest[..end]
            .split('.')
            .filter(|word| !word.is_empty())
            .collect();
        if words
            .first()
            .is_some_and(|first| ["to", "not", "and", "but", "that", "which"].contains(first))
        {
            found.extend(
                words
                    .into_iter()
                    .filter(|word| !known.contains(word))
                    .map(|word| (word, response)),
            );
        }
    }
    found
}

/// Whether the call that ends `before` belongs to a response assertion: the last assertion start
/// in it is a response's `.to` (`pm.response.to`, `res.to`, a member of a name) rather than an
/// `expect(` call, whose chain reads `expect(x).to`.
fn continues_response_assertion(before: &str) -> bool {
    let expect = before.rfind("expect(");
    let response = before
        .rmatch_indices(".to.")
        .map(|(index, _)| index)
        .find(|index| {
            before[..*index]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_alphanumeric() || character == '_')
        });
    match (expect, response) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(expect), Some(response)) => response > expect,
    }
}

/// Whether `code` builds a request body with a file (`mode: "file"`, a `type: "file"` form row,
/// or a `src:` path key in a `formdata` body on the same line), which a script cannot send. A
/// `src:` key anywhere else (`{ src: img.url }`) is ordinary code.
fn sends_a_file(code: &str) -> bool {
    let compact: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    let file_mode = [
        "mode:\"file\"",
        "mode:'file'",
        "type:\"file\"",
        "type:'file'",
    ]
    .iter()
    .any(|pattern| compact.contains(pattern));
    // `src` as an object key: after `{`, `,` or a quote, followed by `:` (or a closing quote).
    let src_key = compact.match_indices("src").any(|(index, _)| {
        let after = &compact[index + 3..];
        compact[..index]
            .chars()
            .next_back()
            .is_none_or(|character| matches!(character, '{' | ',' | '"' | '\''))
            && (after.starts_with(':') || after.starts_with("\":") || after.starts_with("':"))
    });
    file_mode || (src_key && compact.contains("formdata"))
}

/// Dotted identifier chains in `code` (`a.b.c`), skipping ones that are a member of something
/// else (`x.require` is not the global `require`).
fn member_chains(code: &str) -> Vec<String> {
    let is_ident = |character: char| character.is_alphanumeric() || matches!(character, '_' | '$');
    let mut chains = Vec::new();
    let mut current = String::new();
    let mut previous = ' ';
    for character in code.chars().chain(std::iter::once(' ')) {
        if is_ident(character) || (character == '.' && !current.is_empty()) {
            if current.is_empty() && previous == '.' {
                // A member of an expression the scan does not follow.
                previous = character;
                current.push('\0');
                continue;
            }
            current.push(character);
        } else {
            if !current.is_empty() && !current.starts_with('\0') {
                chains.push(current.trim_end_matches('.').to_owned());
            }
            current.clear();
        }
        previous = character;
    }
    chains
}
