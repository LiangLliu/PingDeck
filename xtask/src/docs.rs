//! Docs structure gate: verifies links, heading anchors, section references, repo paths,
//! Rust test names, PRD run limits vs `limits.rs`, banned words, and `docs/design/` hygiene.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};

use regex::Regex;

use crate::markdown::{sections, slugs, strip_fences};
use crate::run_limits::{PRD, check_run_limits};
use crate::{err_line, out_line};

const USAGE: &str = "usage: cargo xtask check-docs [--list]";
const MAX_MD_LINE: usize = 1000;
const LINE_PREVIEW: usize = 40;
const BANNED_PREVIEW: usize = 60;

const BYTE_CAPS: &[(&str, usize)] = &[
    ("README.md", 7000),
    ("docs/product.md", 56_000),
    ("docs/architecture.md", 68_000),
    ("docs/design.md", 36_000),
    ("docs/decisions.md", 136_000),
    ("docs/development.md", 22_000),
];

pub trait RepoView {
    fn files(&self) -> &[String];
    fn read(&self, path: &str) -> Option<String>;
    fn exists(&self, path: &str) -> bool;
}

pub struct DiskRepo {
    root: std::path::PathBuf,
    files: Vec<String>,
    file_set: HashSet<String>,
}

impl DiskRepo {
    pub fn open(root: &Path) -> Result<Self, String> {
        let output = Command::new("git")
            .args(["ls-files", "-co", "--exclude-standard"])
            .current_dir(root)
            .output()
            .map_err(|e| format!("failed to run git ls-files: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "git ls-files failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let files: Vec<String> = stdout
            .lines()
            .filter(|f| !f.is_empty() && root.join(f).exists())
            .map(str::to_owned)
            .collect();
        let file_set: HashSet<String> = files.iter().cloned().collect();
        Ok(Self {
            root: root.to_path_buf(),
            files,
            file_set,
        })
    }
}

impl RepoView for DiskRepo {
    fn files(&self) -> &[String] {
        &self.files
    }

    fn read(&self, path: &str) -> Option<String> {
        fs::read_to_string(self.root.join(path)).ok()
    }

    fn exists(&self, path: &str) -> bool {
        self.file_set.contains(path) || self.root.join(path).exists()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub key: String,
    pub msg: String,
}

pub type FailFn<'a> = &'a mut dyn FnMut(&str, &str, String, Option<String>);

fn doc_path_for(name: &str) -> Option<&'static str> {
    match name {
        "architecture" => Some("docs/architecture.md"),
        "product" => Some("docs/product.md"),
        "design" => Some("docs/design.md"),
        "decisions" => Some("docs/decisions.md"),
        "development" => Some("docs/development.md"),
        "PRD" => Some(PRD),
        _ => None,
    }
}

fn module_chapter(area: &str) -> Option<u32> {
    match area {
        "requests" => Some(2),
        "mock" => Some(3),
        "flows" => Some(4),
        "history" => Some(5),
        "environments" => Some(6),
        "chat" => Some(7),
        "providers" => Some(8),
        "settings" => Some(9),
        "usage" => Some(10),
        _ => None,
    }
}

fn is_md(f: &str) -> bool {
    f.ends_with(".md")
        && (f.starts_with("docs/") || !f.contains('/'))
        && !f.starts_with("docs/design/prototype/")
}

fn is_code(f: &str) -> bool {
    let ext_ok = [".rs", ".ts", ".tsx", ".mjs", ".js", ".css", ".sh", ".toml"]
        .iter()
        .any(|ext| f.ends_with(ext));
    if !ext_ok {
        return false;
    }
    let skip = [
        "node_modules/",
        "target/",
        "dist/",
        "dist-demo/",
        "fixtures/",
        "src/api/generated/",
        ".overhaul/",
    ]
    .iter()
    .any(|p| f.starts_with(p));
    !skip && f != "xtask/src/docs.rs"
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let h1 = (bytes[i + 1] as char).to_digit(16)?;
            let h2 = (bytes[i + 2] as char).to_digit(16)?;
            out.push(((h1 << 4) | h2) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn normalize_rel(base_file: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if let Some(dir) = base_file.rsplit_once('/').map(|(d, _)| d) {
        for p in dir.split('/') {
            if !p.is_empty() && p != "." {
                parts.push(p);
            }
        }
    }
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn char_prefix(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Runs all documentation structural checks against `repo`.
pub fn check(repo: &dyn RepoView) -> Vec<Failure> {
    let mut fails = Vec::new();
    let mut push_fail = |kind: &str, file: &str, detail: String, where_str: Option<String>| {
        let loc = where_str.unwrap_or_else(|| file.to_owned());
        fails.push(Failure {
            key: format!("{kind}\t{file}\t{detail}"),
            msg: format!("{loc}  [{kind}] {detail}"),
        });
    };

    let md_files: Vec<String> = repo.files().iter().filter(|f| is_md(f)).cloned().collect();
    let code_files: Vec<String> = repo
        .files()
        .iter()
        .filter(|f| is_code(f))
        .cloned()
        .collect();

    let mut slug_cache: HashMap<String, HashSet<String>> = HashMap::new();
    let mut sec_cache: HashMap<String, HashSet<String>> = HashMap::new();

    let acceptance_texts: Vec<String> = repo
        .files()
        .iter()
        .filter(|p| p.starts_with("docs/product/acceptance/"))
        .filter_map(|p| repo.read(p))
        .collect();

    check_markdown_files(
        repo,
        &md_files,
        &acceptance_texts,
        &mut slug_cache,
        &mut push_fail,
    );
    check_design_dir(repo, &mut push_fail);
    check_section_refs(repo, &code_files, &md_files, &mut sec_cache, &mut push_fail);
    check_run_limits(repo, &mut push_fail);

    fails
}

fn check_markdown_files(
    repo: &dyn RepoView,
    md_files: &[String],
    acceptance_texts: &[String],
    slug_cache: &mut HashMap<String, HashSet<String>>,
    fail: FailFn<'_>,
) {
    let Ok(code_span_re) = Regex::new(r"`([^`\s]+)`") else {
        return;
    };
    let Ok(backtick_re) = Regex::new(r"`[^`]*`") else {
        return;
    };
    let Ok(link_re) = Regex::new(r"\]\(([^)\s]+)\)") else {
        return;
    };
    let Ok(scheme_re) = Regex::new(r"(?i)^[a-z]+:") else {
        return;
    };
    let Ok(repo_path_re) =
        Regex::new(r"^(crates|src|src-tauri|docs|scripts|fixtures|xtask|tokens|\.github)/")
    else {
        return;
    };
    let Ok(strip_suffix_re) = Regex::new(r"(?:::\S*|:\d+(?:-\d+)?)$") else {
        return;
    };
    let Ok(rust_test_re) = Regex::new(r"^(.+\.rs)::([a-z][a-z0-9_]*)$") else {
        return;
    };
    let Ok(ac_re) = Regex::new(r"\bAC-[A-Z]+-\d+\b") else {
        return;
    };

    let banned: Vec<(Regex, &str)> = [
        (r"STATUS\.md", "STATUS.md"),
        (r"\bWP-\d", "WP- id"),
        (r"本轮", "本轮"),
        (r"/tmp/", "/tmp/ path"),
        (
            r"-p pingdeck-app --test",
            "-p pingdeck-app --test (use -p pingdeck-core)",
        ),
        (r"\.(?:rs|ts|tsx|mjs|md|css):\d+", "file:line reference"),
    ]
    .iter()
    .filter_map(|(pat, label)| Regex::new(pat).ok().map(|re| (re, *label)))
    .collect();

    for f in md_files {
        let content = repo.read(f).unwrap_or_default();
        for (i, raw) in strip_fences(&content).into_iter().enumerate() {
            let where_str = format!("{f}:{}", i + 1);

            // 1. Links and heading anchors (ignore brackets inside code spans).
            let no_code_brackets = backtick_re.replace_all(raw, |caps: &regex::Captures<'_>| {
                caps[0]
                    .chars()
                    .map(|c| {
                        if matches!(c, '[' | ']' | '(' | ')') {
                            ' '
                        } else {
                            c
                        }
                    })
                    .collect::<String>()
            });
            for m in link_re.captures_iter(&no_code_brackets) {
                let target = &m[1];
                if scheme_re.is_match(target) {
                    continue;
                }
                let (path_part, anchor_part) = match target.split_once('#') {
                    Some((p, a)) => (p, Some(a)),
                    None => (target, None),
                };
                let resolved = if !path_part.is_empty() {
                    let Some(decoded) = percent_decode(path_part) else {
                        fail("link", f, target.to_owned(), Some(where_str.clone()));
                        continue;
                    };
                    let norm = normalize_rel(f, &decoded);
                    if !repo.exists(&norm) {
                        fail("link", f, target.to_owned(), Some(where_str.clone()));
                        continue;
                    }
                    norm
                } else {
                    f.clone()
                };
                if let Some(anchor) = anchor_part
                    && resolved.ends_with(".md")
                {
                    let want = percent_decode(anchor).map(|s| s.to_lowercase());
                    let file_slugs = slug_cache
                        .entry(resolved.clone())
                        .or_insert_with(|| slugs(&repo.read(&resolved).unwrap_or_default()));
                    let ok = want.as_deref().is_some_and(|w| file_slugs.contains(w));
                    if !ok {
                        fail("anchor", f, target.to_owned(), Some(where_str.clone()));
                    }
                }
            }

            // 2. Backticked repo paths and Rust test names.
            for m in code_span_re.captures_iter(raw) {
                let token = &m[1];
                let clean = strip_suffix_re.replace(token, "");
                if !repo_path_re.is_match(&clean)
                    || clean.contains('*')
                    || clean.contains('<')
                    || clean.contains('>')
                    || clean.contains('…')
                    || clean.contains('{')
                    || clean.contains('}')
                    || clean.contains("...")
                {
                    continue;
                }
                let trimmed = clean.trim_end_matches('/');
                if !repo.exists(trimmed) {
                    fail("path", f, clean.into_owned(), Some(where_str.clone()));
                }
                if let Some(tcaps) = rust_test_re.captures(token) {
                    let rs_path = &tcaps[1];
                    let fn_name = &tcaps[2];
                    if repo.exists(rs_path) {
                        let rs_text = repo.read(rs_path).unwrap_or_default();
                        let fn_pat = format!(r"\bfn {fn_name}\b");
                        if Regex::new(&fn_pat)
                            .ok()
                            .is_some_and(|re| !re.is_match(&rs_text))
                        {
                            fail("test-name", f, token.to_owned(), Some(where_str.clone()));
                        }
                    }
                }
            }

            // 3. Line length, banned words, and AC ids.
            if raw.chars().count() > MAX_MD_LINE {
                fail(
                    "line-length",
                    f,
                    format!("{}…", char_prefix(raw, LINE_PREVIEW)),
                    Some(where_str.clone()),
                );
            }
            for (re, label) in &banned {
                if re.is_match(raw) {
                    fail(
                        "banned",
                        f,
                        format!("{label}: {}", char_prefix(raw.trim(), BANNED_PREVIEW)),
                        Some(where_str.clone()),
                    );
                }
            }
            for m in ac_re.find_iter(raw) {
                let id = m.as_str();
                if !acceptance_texts.iter().any(|t| t.contains(id)) {
                    fail("ac-id", f, id.to_owned(), Some(where_str.clone()));
                }
            }
        }
    }
}

fn check_design_dir(repo: &dyn RepoView, fail: FailFn<'_>) {
    for f in repo.files() {
        if f.starts_with("docs/design/")
            && !f.starts_with("docs/design/prototype/")
            && f.ends_with(".md")
            && f != "docs/design/content-bible.md"
        {
            fail(
                "design-dir",
                f,
                "only content-bible.md may live in docs/design/".to_owned(),
                None,
            );
        }
    }
}

fn check_section_refs(
    repo: &dyn RepoView,
    code_files: &[String],
    md_files: &[String],
    sec_cache: &mut HashMap<String, HashSet<String>>,
    fail: FailFn<'_>,
) {
    let Ok(ref_re) = Regex::new(
        r"(?:docs/(?:product/)?)?(architecture|product|design|decisions|development|PRD)\.md`? §(\d+(?:\.\d+)*)",
    ) else {
        return;
    };
    let Ok(area_re) = Regex::new(r"^src/features/([^/]+)/") else {
        return;
    };

    for f in code_files.iter().chain(md_files.iter()) {
        let want = area_re.captures(f).and_then(|c| module_chapter(&c[1]));
        let content = repo.read(f).unwrap_or_default();
        for (i, line) in content.split('\n').enumerate() {
            for caps in ref_re.captures_iter(line) {
                let Some(full) = caps.get(0) else { continue };
                // Equivalent of `(?<![\w/-])` without requiring lookbehind.
                if let Some(prev) = line[..full.start()].chars().next_back()
                    && (prev.is_alphanumeric() || matches!(prev, '_' | '/' | '-'))
                {
                    continue;
                }
                let doc_key = &caps[1];
                let sec_num = &caps[2];
                let Some(doc_path) = doc_path_for(doc_key) else {
                    continue;
                };
                let where_str = format!("{f}:{}", i + 1);
                let doc_secs = sec_cache
                    .entry(doc_path.to_owned())
                    .or_insert_with(|| sections(&repo.read(doc_path).unwrap_or_default()));
                if !doc_secs.contains(sec_num) {
                    fail(
                        "section",
                        f,
                        format!("{doc_key}.md §{sec_num}"),
                        Some(where_str),
                    );
                    continue;
                }
                if doc_key == "product"
                    && let Some(want_ch) = want
                    && let Some(ch) = sec_num
                        .split('.')
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                    && ch != 1
                    && ch != want_ch
                {
                    fail(
                        "module-chapter",
                        f,
                        format!("product.md §{sec_num} (want §{want_ch})"),
                        Some(where_str),
                    );
                }
            }
        }
    }
}

pub fn run(root: &Path, args: &[String]) -> ExitCode {
    let mut list_mode = false;
    for arg in args {
        match arg.as_str() {
            "--list" => list_mode = true,
            other => {
                err_line(&format!("unexpected argument '{other}'\n{USAGE}"));
                return ExitCode::from(2);
            }
        }
    }
    let repo = match DiskRepo::open(root) {
        Ok(r) => r,
        Err(e) => {
            err_line(&format!("check-docs: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let fails = check(&repo);
    if list_mode {
        let keys: BTreeSet<&str> = fails.iter().map(|f| f.key.as_str()).collect();
        for k in keys {
            out_line(k);
        }
        return ExitCode::SUCCESS;
    }
    for (f, cap) in BYTE_CAPS {
        let size = repo.read(f).map_or(0, |s| s.len());
        if size > *cap {
            err_line(&format!("warn  {f}  {size} B over the {cap} B cap"));
        }
    }
    for f in &fails {
        err_line(&f.msg);
    }
    out_line(&format!("check-docs: {} failure(s)", fails.len()));
    if fails.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_limits::{LIMITS_RS, PRD_ANCHOR};

    struct MemRepo {
        files: Vec<String>,
        map: HashMap<String, String>,
    }

    impl MemRepo {
        fn new(entries: HashMap<String, Option<String>>) -> Self {
            let mut map = HashMap::new();
            for (k, v) in entries {
                if let Some(text) = v {
                    map.insert(k, text);
                }
            }
            let mut files: Vec<String> = map.keys().cloned().collect();
            files.sort();
            Self { files, map }
        }
    }

    impl RepoView for MemRepo {
        fn files(&self) -> &[String] {
            &self.files
        }
        fn read(&self, path: &str) -> Option<String> {
            self.map.get(path).cloned()
        }
        fn exists(&self, path: &str) -> bool {
            self.map.contains_key(path)
                || self
                    .files
                    .iter()
                    .any(|n| n.starts_with(&format!("{path}/")))
        }
    }

    fn base_fixture() -> HashMap<String, Option<String>> {
        let disk = DiskRepo::open(&crate::repo_root()).unwrap();
        let prd_full = disk.read(PRD).unwrap();
        let start = prd_full.find("\n## 6.").unwrap();
        let end = prd_full[start..].find("\n## 7.").unwrap() + start;
        let prd_ch6 = prd_full[start..end].to_owned();
        let limits = disk.read(LIMITS_RS).unwrap();

        let mut m = HashMap::new();
        m.insert(PRD.to_owned(), Some(prd_ch6));
        m.insert(LIMITS_RS.to_owned(), Some(limits));
        m.insert(
            "docs/product.md".to_owned(),
            Some("# P\n\n## 1. Shell\n\n## 7. Chat\n\n## 10. Usage\n".to_owned()),
        );
        m.insert(
            "docs/architecture.md".to_owned(),
            Some("# A\n\n## 4. Front end\n\n### 4.5 Ports\n".to_owned()),
        );
        m.insert(
            "docs/design/content-bible.md".to_owned(),
            Some("# Bible\n".to_owned()),
        );
        m.insert(
            "docs/README.md".to_owned(),
            Some("[limits](product/PRD.md#run-limits-normative) [arch](architecture.md#45-ports) `docs/product.md` `crates/x.rs`\n".to_owned()),
        );
        m.insert(
            "crates/x.rs".to_owned(),
            Some("fn real_test() {}\n".to_owned()),
        );
        m.insert(
            "src/features/usage/a.ts".to_owned(),
            Some("// docs/product.md §10, docs/architecture.md §4.5\n".to_owned()),
        );
        m
    }

    fn kinds_with(patch: Vec<(&str, Option<String>)>) -> Vec<String> {
        let mut m = base_fixture();
        for (k, v) in patch {
            m.insert(k.to_owned(), v);
        }
        check(&MemRepo::new(m))
            .into_iter()
            .map(|f| f.key.split('\t').next().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn mirrors_all_check_docs_cases() {
        let base = base_fixture();
        let prd = base[PRD].as_ref().unwrap();
        let limits = base[LIMITS_RS].as_ref().unwrap();

        assert_eq!(kinds_with(vec![]), Vec::<String>::new());
        assert_eq!(
            kinds_with(vec![(
                PRD,
                Some(prd.replace(
                    "| Concurrent top-level runs | 3 |",
                    "| Concurrent top-level runs | 4 |"
                ))
            )]),
            vec!["run-limits"]
        );
        assert_eq!(
            kinds_with(vec![(
                LIMITS_RS,
                Some(limits.replace("QUEUE_MESSAGES: usize = 5", "QUEUE_MESSAGES: usize = 6"))
            )]),
            vec!["run-limits"]
        );
        assert_eq!(
            kinds_with(vec![(
                LIMITS_RS,
                Some(limits.replace("from_secs(8 * 60 * 60)", "from_secs(7 * 60 * 60)"))
            )]),
            vec!["run-limits"]
        );
        assert_eq!(
            kinds_with(vec![(PRD, Some(prd.replace("| Queue |", "|  Queue |")))]),
            vec!["run-limits"]
        );
        assert_eq!(
            kinds_with(vec![(
                PRD,
                Some(format!("{prd}\n#### Run limits (normative)\n"))
            )]),
            vec!["run-limits"]
        );
        assert_eq!(
            kinds_with(vec![(PRD, Some(prd.replace(PRD_ANCHOR, "Runs wait.")))]),
            vec!["run-limits"]
        );
        assert_eq!(
            kinds_with(vec![("docs/README.md", Some("[x](gone.md)\n".to_owned()))]),
            vec!["link"]
        );
        assert_eq!(
            kinds_with(vec![(
                "docs/README.md",
                Some("[x](product/PRD.md#no-such-heading)\n".to_owned())
            )]),
            vec!["anchor"]
        );
        assert_eq!(
            kinds_with(vec![(
                "src/x.ts",
                Some("// docs/architecture.md §4.9\n".to_owned())
            )]),
            vec!["section"]
        );
        assert_eq!(
            kinds_with(vec![(
                "src/features/usage/a.ts",
                Some("// docs/product.md §7\n".to_owned())
            )]),
            vec!["module-chapter"]
        );
        assert_eq!(
            kinds_with(vec![("docs/README.md", Some("`src/nope.ts`\n".to_owned()))]),
            vec!["path"]
        );
        assert_eq!(
            kinds_with(vec![(
                "docs/README.md",
                Some("`crates/x.rs::gone_test`\n".to_owned())
            )]),
            vec!["test-name"]
        );
        assert_eq!(
            kinds_with(vec![(
                "docs/README.md",
                Some("see docs/design.md:12\n".to_owned())
            )]),
            vec!["banned"]
        );
        assert_eq!(
            kinds_with(vec![("docs/design/plan.md", Some("# Plan\n".to_owned()))]),
            vec!["design-dir"]
        );
        assert_eq!(
            kinds_with(vec![("docs/README.md", Some("AC-REQ-1\n".to_owned()))]),
            vec!["ac-id"]
        );
        assert_eq!(
            kinds_with(vec![(
                "docs/README.md",
                Some("[x](100%.md) [y](README.md#a%zz)\n".to_owned())
            )]),
            vec!["link", "anchor"]
        );
    }
}
