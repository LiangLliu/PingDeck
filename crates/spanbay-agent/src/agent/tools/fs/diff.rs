//! Unified diff and line-change counts for `write_file` and `edit_file`, delegated to [`spanbay_base::diff`].

pub use spanbay_base::diff::change_counts;

/// The unified diff from `old` (`None` for a file that does not exist yet) to `new`, for the file
/// `path`, cut off after about `cap` bytes with a note. Empty when nothing differs.
#[must_use]
pub fn unified_diff(path: &str, old: Option<&str>, new: &str, cap: usize) -> String {
    spanbay_base::diff::unified_diff_capped(path, old, new, Some(cap))
}
