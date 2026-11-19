//! Unified diff and line-change counts for `write_file` and `edit_file`, delegated to [`pingdeck_base::diff`].

pub use pingdeck_base::diff::change_counts;

/// The unified diff from `old` (`None` for a file that does not exist yet) to `new`, for the file
/// `path`, cut off after about `cap` bytes with a note. Empty when nothing differs.
#[must_use]
pub fn unified_diff(path: &str, old: Option<&str>, new: &str, cap: usize) -> String {
    pingdeck_base::diff::unified_diff_capped(path, old, new, Some(cap))
}
