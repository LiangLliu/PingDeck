//! Schema changes belong to `spanbay-store`: no other crate's product code may carry DDL.

use std::path::{Path, PathBuf};

const KINDS: [&str; 4] = ["TABLE", "INDEX", "TRIGGER", "VIEW"];
const MODIFIERS: [&str; 4] = ["TEMP", "TEMPORARY", "UNIQUE", "VIRTUAL"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(name.as_str(), "tests" | "testing" | "target") {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") {
            let stem = name.trim_end_matches(".rs");
            let test_file = stem == "tests"
                || stem.starts_with("test_")
                || stem.starts_with("tests_")
                || stem.ends_with("_tests")
                || stem.ends_with("_test");
            if !test_file {
                out.push(path);
            }
        }
    }
}

/// Drops inline `#[cfg(test)] mod name { … }` blocks by brace depth.
fn product_lines(source: &str) -> Vec<(usize, &str)> {
    let mut kept = Vec::new();
    let (mut pending, mut depth) = (false, 0usize);
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if depth > 0 {
            depth = (depth + line.matches('{').count()).saturating_sub(line.matches('}').count());
            continue;
        }
        if trimmed == "#[cfg(test)]" {
            pending = true;
            continue;
        }
        if pending && trimmed.starts_with("#[") {
            continue;
        }
        if std::mem::take(&mut pending) && trimmed.contains("mod ") && trimmed.ends_with('{') {
            depth = 1;
            continue;
        }
        kept.push((index + 1, line));
    }
    kept
}

/// `CREATE [TEMP|UNIQUE|VIRTUAL…] TABLE|INDEX|TRIGGER|VIEW`, `ALTER TABLE`, `DROP <kind>`.
fn has_ddl(line: &str) -> bool {
    let words: Vec<String> = line
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    words.iter().enumerate().any(|(at, word)| {
        let mut rest = words[at + 1..].iter().map(String::as_str);
        match word.as_str() {
            "CREATE" => rest
                .find(|next| !MODIFIERS.contains(next))
                .is_some_and(|kind| KINDS.contains(&kind)),
            "ALTER" => rest.next() == Some("TABLE"),
            "DROP" => rest.next().is_some_and(|kind| KINDS.contains(&kind)),
            _ => false,
        }
    })
}

#[test]
fn ddl_appears_only_in_spanbay_store_product_code() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root.join("crates")).unwrap() {
        let krate = entry.unwrap().path();
        // Skips stray files such as Finder's `.DS_Store`.
        if krate.is_dir()
            && krate
                .file_name()
                .is_some_and(|name| name != "spanbay-store")
        {
            rust_files(&krate.join("src"), &mut files);
        }
    }
    assert!(
        files.len() > 100,
        "the scan found only {} files",
        files.len()
    );

    let mut offenders = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file).unwrap();
        for (number, line) in product_lines(&source) {
            if has_ddl(line) {
                let shown = file.strip_prefix(&root).unwrap_or(file);
                offenders.push(format!("{}:{number}: {}", shown.display(), line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "DDL outside spanbay-store:\n{}",
        offenders.join("\n")
    );
}
