//! Recomputes the database schema fingerprint (`crates/spanbay-store/src/storage/schema.rs`)
//! and provides a zero-Node free TCP port probe for `scripts/smoke.sh`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::process::ExitCode;

use regex::Regex;

use crate::{err_line, out_line};

const FNV_OFFSET: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;
const POSITIVE_31: u32 = 0x7fff_ffff;

/// FNV-1a over UTF-8 SQL parts, kept to 31 positive bits and never 0.
/// Mirrors `crates/spanbay-store/src/storage/schema.rs::fingerprint` byte for byte.
pub fn fingerprint(parts: &[&str]) -> i64 {
    fn mix(hash: u32, byte: u8) -> u32 {
        (hash ^ u32::from(byte)).wrapping_mul(FNV_PRIME)
    }
    fn is_word(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'_'
    }

    let mut hash = FNV_OFFSET;
    let (mut in_quote, mut space, mut prev) = (false, false, 0u8);
    for part in parts {
        let bytes = part.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            index += 1;
            if !in_quote && byte == b'-' && index < bytes.len() && bytes[index] == b'-' {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                space = true;
                continue;
            }
            if !in_quote && byte.is_ascii_whitespace() {
                space = true;
                continue;
            }
            if space && is_word(prev) && is_word(byte) {
                hash = mix(hash, b' ');
            }
            space = false;
            prev = byte;
            if byte == b'\'' {
                in_quote = !in_quote;
            }
            hash = mix(hash, byte);
        }
    }
    let value = i64::from(hash & POSITIVE_31);
    if value == 0 { 1 } else { value }
}

fn sources_label(sources: &BTreeMap<String, String>) -> String {
    let dirs: BTreeSet<&str> = sources
        .keys()
        .map(|p| p.rfind('/').map_or("", |i| &p[..=i]))
        .collect();
    if dirs.len() == 1
        && let Some(&dir) = dirs.iter().next()
        && !dir.is_empty()
    {
        return dir.to_owned();
    }
    if sources.is_empty() {
        return "(no files)".to_owned();
    }
    sources.keys().cloned().collect::<Vec<_>>().join(", ")
}

fn extract_literal(
    sources: &BTreeMap<String, String>,
    name: &str,
    open: &str,
    close: &str,
) -> Result<(String, String, usize), String> {
    let pat = format!(
        r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?const[ \t]+{name}[ \t]*:[ \t]*&(?:'static[ \t]+)?str[ \t]*=[ \t]*"
    );
    let re = Regex::new(&pat).map_err(|e| format!("invalid regex for {name}: {e}"))?;
    let mut found: Vec<(&str, &str, usize, usize)> = Vec::new();
    for (path, text) in sources {
        for m in re.find_iter(text) {
            let line = text[..m.start()].bytes().filter(|&b| b == b'\n').count() + 1;
            found.push((path.as_str(), text.as_str(), line, m.end()));
        }
    }
    if found.is_empty() {
        return Err(format!(
            "{name} declaration not found in {}",
            sources_label(sources)
        ));
    }
    if found.len() > 1 {
        let places = found
            .iter()
            .map(|(p, _, line, _)| format!("{p}:{line}"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!("{name} is declared more than once: {places}"));
    }
    let (path, text, line, at) = found[0];
    if !text[at..].starts_with(open) {
        return Err(format!(
            "{name} at {path}:{line} is not a {open}...{close} literal"
        ));
    }
    let start = at + open.len();
    let Some(rel_end) = text[start..].find(close) else {
        return Err(format!(
            "{name} at {path}:{line}: unterminated {open} literal"
        ));
    };
    Ok((
        text[start..start + rel_end].to_owned(),
        path.to_owned(),
        line,
    ))
}

/// Extracts `[SCHEMA_DDL, SECRETS_COLUMNS]` from a map of `.rs` file paths to source texts.
pub fn extract_schema_parts(sources: &BTreeMap<String, String>) -> Result<[String; 2], String> {
    let (ddl, _, _) = extract_literal(sources, "SCHEMA_DDL", r##"r#""##, r##""#"##)?;
    let (columns, col_path, col_line) = extract_literal(sources, "SECRETS_COLUMNS", "\"", "\"")?;
    if columns.contains('\\') {
        return Err(format!(
            "SECRETS_COLUMNS at {col_path}:{col_line}: escape sequences are not supported"
        ));
    }
    Ok([ddl, columns])
}

pub fn run_fingerprint(root: &Path, args: &[String]) -> ExitCode {
    if args.len() > 1 || args.iter().any(|a| a.starts_with('-')) {
        err_line("usage: cargo xtask schema-fingerprint [storage-dir]");
        return ExitCode::from(2);
    }
    let dir = args.first().map_or_else(
        || root.join("crates/spanbay-store/src/storage"),
        |p| Path::new(p).to_path_buf(),
    );
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            err_line(&format!("schema-fingerprint: {}: {e}", dir.display()));
            return ExitCode::FAILURE;
        }
    };
    let mut sources = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("rs") {
            match fs::read_to_string(&path) {
                Ok(text) => {
                    sources.insert(path.to_string_lossy().replace('\\', "/"), text);
                }
                Err(e) => {
                    err_line(&format!("schema-fingerprint: {}: {e}", path.display()));
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    if sources.is_empty() {
        err_line(&format!(
            "schema-fingerprint: no .rs files in {}",
            dir.display()
        ));
        return ExitCode::FAILURE;
    }
    match extract_schema_parts(&sources) {
        Ok([ddl, cols]) => {
            out_line(&fingerprint(&[&ddl, &cols]).to_string());
            ExitCode::SUCCESS
        }
        Err(e) => {
            err_line(&format!("schema-fingerprint: {e}"));
            ExitCode::FAILURE
        }
    }
}

pub fn run_free_port(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        err_line("usage: cargo xtask free-port");
        return ExitCode::from(2);
    }
    match TcpListener::bind("127.0.0.1:0").and_then(|s| s.local_addr()) {
        Ok(addr) => {
            out_line(&addr.port().to_string());
            ExitCode::SUCCESS
        }
        Err(e) => {
            err_line(&format!("free-port: {e}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DDL: &str = "\nCREATE TABLE workspaces (\n    id TEXT PRIMARY KEY, -- stable id\n    name TEXT NOT NULL DEFAULT 'Untitled  space'\n);\n";
    const COLUMNS: &str = "secret_ref TEXT PRIMARY KEY,\n    value_text TEXT NOT NULL";

    fn sample(ddl: &str, prefix: &str) -> String {
        format!(
            "{prefix}pub(super) const SECRETS_COLUMNS: &str = \"{COLUMNS}\";\n\nconst SCHEMA_DDL: &str = r#\"{ddl}\"#;\n"
        )
    }

    #[test]
    fn extracts_real_storage_sources() {
        let dir = crate::repo_root().join("crates/spanbay-store/src/storage");
        let mut sources = BTreeMap::new();
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("rs") {
                sources.insert(
                    path.to_string_lossy().replace('\\', "/"),
                    fs::read_to_string(&path).unwrap(),
                );
            }
        }
        let [ddl, cols] = extract_schema_parts(&sources).unwrap();
        let value = fingerprint(&[&ddl, &cols]);
        assert!((1..=i64::from(POSITIVE_31)).contains(&value));
    }

    #[test]
    fn anchors_on_declaration_not_doc_comments() {
        let quoting =
            "/// smoke.sh finds `SCHEMA_DDL: &str = r#\"` and `SECRETS_COLUMNS: &str = \"` here.\n";
        let mut sources = BTreeMap::new();
        sources.insert("schema.rs".to_owned(), sample(DDL, quoting));
        assert_eq!(
            extract_schema_parts(&sources).unwrap(),
            [DDL.to_owned(), COLUMNS.to_owned()]
        );
    }

    #[test]
    fn rejects_missing_duplicate_or_escaped() {
        let mut missing = BTreeMap::new();
        missing.insert(
            "a.rs".to_owned(),
            r##"const OTHER: &str = r#"x"#;"##.to_owned(),
        );
        assert!(
            extract_schema_parts(&missing)
                .unwrap_err()
                .contains("SCHEMA_DDL declaration not found")
        );

        let mut dup = BTreeMap::new();
        dup.insert("a.rs".to_owned(), sample(DDL, ""));
        dup.insert("b.rs".to_owned(), sample(DDL, ""));
        assert!(
            extract_schema_parts(&dup)
                .unwrap_err()
                .contains("declared more than once")
        );

        let mut esc = BTreeMap::new();
        esc.insert(
            "a.rs".to_owned(),
            sample(DDL, "").replace("NOT NULL\"", "NOT NULL\\n\""),
        );
        assert!(
            extract_schema_parts(&esc)
                .unwrap_err()
                .contains("escape sequences")
        );
    }

    #[test]
    fn ignores_layout_and_comments_outside_quotes_only() {
        let mut s1 = BTreeMap::new();
        s1.insert("schema.rs".to_owned(), sample(DDL, ""));
        let [d1, c1] = extract_schema_parts(&s1).unwrap();
        let base = fingerprint(&[&d1, &c1]);

        let relaid = "\n\tCREATE   TABLE workspaces( -- reflowed\n id TEXT PRIMARY KEY,\n\n name TEXT NOT NULL DEFAULT 'Untitled  space' ) ;";
        let mut s2 = BTreeMap::new();
        s2.insert("schema.rs".to_owned(), sample(relaid, ""));
        let [d2, c2] = extract_schema_parts(&s2).unwrap();
        assert_eq!(fingerprint(&[&d2, &c2]), base);

        let mut s3 = BTreeMap::new();
        s3.insert(
            "schema.rs".to_owned(),
            sample(&DDL.replace("Untitled  space", "Untitled space"), ""),
        );
        let [d3, c3] = extract_schema_parts(&s3).unwrap();
        assert_ne!(fingerprint(&[&d3, &c3]), base);
    }

    #[test]
    fn carries_state_across_parts_and_ends_comment_at_part_end() {
        assert_eq!(fingerprint(&["a --c", "b"]), fingerprint(&["a b"]));
        assert_ne!(fingerprint(&["a", "b"]), fingerprint(&["a b"]));
    }
}
