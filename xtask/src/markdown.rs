//! Shared Markdown parsing helpers for `check-docs` and `docs-lint`.

use std::collections::{HashMap, HashSet};

use regex::Regex;

/// True when a line opens or closes a backtick code fence (` ``` `).
pub fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// Blanks every fenced line (including fence markers) while preserving line numbers.
pub fn strip_fences(text: &str) -> Vec<&str> {
    let mut in_fence = false;
    text.split('\n')
        .map(|line| {
            if is_fence(line) {
                in_fence = !in_fence;
                ""
            } else if in_fence {
                ""
            } else {
                line
            }
        })
        .collect()
}

fn is_unicode_mark(c: char) -> bool {
    // Non-spacing / combining marks used in GitHub slug preservation.
    matches!(c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F
    )
}

/// GitHub heading slugs of a Markdown document, adding `-1`, `-2` suffixes for duplicates.
pub fn slugs(text: &str) -> HashSet<String> {
    let Ok(head_re) = Regex::new(r"^#{1,6}\s+(.*?)\s*#*\s*$") else {
        return HashSet::new();
    };
    let Ok(link_re) = Regex::new(r"\[([^\]]*)\]\([^)]*\)") else {
        return HashSet::new();
    };
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out = HashSet::new();
    for line in strip_fences(text) {
        let Some(caps) = head_re.captures(line) else {
            continue;
        };
        let raw_heading = &caps[1];
        let no_links = link_re.replace_all(raw_heading, "$1");
        let plain: String = no_links.chars().filter(|&c| c != '`' && c != '*').collect();
        let base: String = plain
            .to_lowercase()
            .chars()
            .filter(|&c| {
                c.is_alphabetic()
                    || c.is_numeric()
                    || is_unicode_mark(c)
                    || c == '_'
                    || c == '-'
                    || c == ' '
            })
            .map(|c| if c == ' ' { '-' } else { c })
            .collect();
        let count = seen.entry(base.clone()).or_insert(0);
        let slug = if *count == 0 {
            base
        } else {
            format!("{base}-{}", *count)
        };
        *count += 1;
        out.insert(slug);
    }
    out
}

/// Numbered section identifiers from headings (`## 4.` / `### 4.5`).
pub fn sections(text: &str) -> HashSet<String> {
    let Ok(sec_re) = Regex::new(r"^#{1,6}\s+(\d+(?:\.\d+)*)[.\s]") else {
        return HashSet::new();
    };
    let mut out = HashSet::new();
    for line in strip_fences(text) {
        if let Some(caps) = sec_re.captures(line) {
            out.insert(caps[1].to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_github_slugs_and_sections() {
        let md = "# Title\n\n## 4. Front end\n\n### 4.5 Ports & API\n\n#### Run limits (normative)\n\n```md\n## 9. Fake\n```\n";
        let s = slugs(md);
        assert!(s.contains("4-front-end"));
        assert!(s.contains("45-ports--api"));
        assert!(s.contains("run-limits-normative"));
        assert!(!s.contains("9-fake"));

        let secs = sections(md);
        assert!(secs.contains("4"));
        assert!(secs.contains("4.5"));
        assert!(!secs.contains("9"));
    }
}
