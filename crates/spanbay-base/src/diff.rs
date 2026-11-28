//! Line-based unified diff and line-change counting built on [`similar::TextDiff`].

use std::fmt::Write as _;

/// Context lines shown around every diff hunk.
const CONTEXT: usize = 3;

/// Renders a unified diff from `old` (`None` for a file that does not exist yet) to `new` for
/// `path`, optionally capped at `cap` bytes with a truncation note. Empty when nothing differs.
#[must_use]
pub fn unified_diff_capped(path: &str, old: Option<&str>, new: &str, cap: Option<usize>) -> String {
    let old_text = old.unwrap_or("");
    if old.is_some() && old_text == new {
        return String::new();
    }
    let diff = similar::TextDiff::from_lines(old_text, new);
    let groups = diff.grouped_ops(CONTEXT);
    if groups.is_empty() && old.is_some() {
        return String::new();
    }
    let mut out = match old {
        Some(_) => format!("--- a/{path}\n+++ b/{path}\n"),
        None => format!("--- /dev/null\n+++ b/{path}\n"),
    };
    for group in groups {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let old_count = last.old_range().end - first.old_range().start;
        let new_count = last.new_range().end - first.new_range().start;
        let old_start = first.old_range().start + usize::from(old_count > 0);
        let new_start = first.new_range().start + usize::from(new_count > 0);
        let _ = writeln!(
            out,
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@"
        );
        for op in &group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    similar::ChangeTag::Equal => ' ',
                    similar::ChangeTag::Delete => '-',
                    similar::ChangeTag::Insert => '+',
                };
                out.push(sign);
                if cap.is_some() {
                    out.push_str(change.value());
                    if change.missing_newline() {
                        out.push_str("\n\\ No newline at end of file\n");
                    }
                } else {
                    let line = change.value();
                    out.push_str(line.strip_suffix('\n').unwrap_or(line));
                    out.push('\n');
                }
                if let Some(limit) = cap
                    && out.len() > limit
                {
                    let mut end = limit.min(out.len());
                    while !out.is_char_boundary(end) {
                        end -= 1;
                    }
                    out.truncate(end);
                    out.push_str("\n… diff truncated …\n");
                    return out;
                }
            }
        }
    }
    out
}

/// Unified diff of two texts with 3 lines of context; empty when equal.
#[must_use]
pub fn unified_diff_text(path: &str, before: &str, after: &str) -> String {
    unified_diff_capped(path, Some(before), after, None)
}

/// How many lines `new` adds to and removes from `old` (`None`: the file did not exist).
#[must_use]
pub fn change_counts(old: Option<&str>, new: &str) -> (u32, u32) {
    let diff = similar::TextDiff::from_lines(old.unwrap_or(""), new);
    let (mut added, mut removed) = (0_u32, 0_u32);
    for op in diff.ops() {
        if matches!(
            op.tag(),
            similar::DiffTag::Insert | similar::DiffTag::Replace
        ) {
            added = added.saturating_add(u32::try_from(op.new_range().len()).unwrap_or(u32::MAX));
        }
        if matches!(
            op.tag(),
            similar::DiffTag::Delete | similar::DiffTag::Replace
        ) {
            removed =
                removed.saturating_add(u32::try_from(op.old_range().len()).unwrap_or(u32::MAX));
        }
    }
    (added, removed)
}

/// How many lines differ between the texts: the larger of the removed and the added lines.
#[must_use]
pub fn changed_lines(before: &str, after: &str) -> usize {
    let (added, removed) = change_counts(Some(before), after);
    (added.max(removed)) as usize
}

/// Reverts the `hunk_index`-th unified-diff hunk (0-based, with 3 lines of context) between `old`
/// (`None` for a file that did not exist before) and `new`, returning the updated `new` text.
/// Returns `None` when `hunk_index` is out of bounds or `old` and `new` do not differ.
#[must_use]
pub fn revert_diff_hunk(old: Option<&str>, new: &str, hunk_index: usize) -> Option<String> {
    let old_text = old.unwrap_or("");
    if old.is_some() && old_text == new {
        return None;
    }
    let diff = similar::TextDiff::from_lines(old_text, new);
    let groups = diff.grouped_ops(CONTEXT);
    let group = groups.get(hunk_index)?;
    let (first, last) = (group.first()?, group.last()?);
    let old_range = first.old_range().start..last.old_range().end;
    let new_range = first.new_range().start..last.new_range().end;
    let mut out = String::with_capacity(new.len());
    let mut i = 0;
    while let Some(s) = diff.new_slice(i) {
        if i >= new_range.start {
            break;
        }
        out.push_str(s);
        i += 1;
    }
    for idx in old_range {
        if let Some(s) = diff.old_slice(idx) {
            out.push_str(s);
        }
    }
    let mut j = new_range.end;
    while let Some(s) = diff.new_slice(j) {
        out.push_str(s);
        j += 1;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverts_single_hunk_of_multi_hunk_diff_and_rejects_out_of_bounds() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
        let new = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nK\nl\n";
        let after_first = revert_diff_hunk(Some(old), new, 0).expect("revert hunk 0");
        assert!(after_first.contains("a\nb\nc\n") && after_first.contains("j\nK\nl\n"));
        let after_second = revert_diff_hunk(Some(old), &after_first, 0).expect("revert hunk 1");
        assert_eq!(after_second, old);
        assert!(revert_diff_hunk(Some(old), &after_second, 0).is_none());
        assert!(revert_diff_hunk(Some(old), new, 5).is_none());
    }
}
