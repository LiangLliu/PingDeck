//! Percent-encoding shared by request building and secret masking.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// RFC 3986 unreserved characters stay literal in a path segment.
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Percent-encodes a path segment.
pub fn encode_path_segment(value: &str) -> String {
    utf8_percent_encode(value, PATH_SEGMENT).to_string()
}
