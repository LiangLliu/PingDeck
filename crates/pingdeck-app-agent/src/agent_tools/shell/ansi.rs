//! Terminal escape sequences in a command's output.
//!
//! Single responsibility: remove ANSI escape sequences (colours, cursor moves, window titles)
//! from the text the model reads, across read boundaries: a sequence split between two chunks of
//! output is still removed whole. The live view the user watches is not filtered.

/// Where the stripper is inside an escape sequence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum State {
    /// Plain text.
    #[default]
    Text,
    /// Just after `ESC`.
    Escape,
    /// Inside `ESC [ …` up to its final byte.
    Csi,
    /// Inside `ESC ] …` up to `BEL` or `ESC \`.
    Osc,
    /// Inside an OSC just after an `ESC` (a possible string terminator).
    OscEscape,
}

/// Strips escape sequences from a stream of text fragments.
#[derive(Debug, Default)]
pub(super) struct AnsiStripper {
    state: State,
}

impl AnsiStripper {
    /// `text` without escape sequences (the state carries over to the next call).
    pub(super) fn push(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            self.state = match (self.state, c) {
                (State::Text, '\u{1b}') => State::Escape,
                (State::Text, c) => {
                    out.push(c);
                    State::Text
                }
                (State::Escape, '[') => State::Csi,
                (State::Escape, ']') => State::Osc,
                // Any other `ESC x` is a two-character sequence.
                (State::Escape, _) => State::Text,
                (State::Csi, '\u{40}'..='\u{7e}') => State::Text,
                (State::Csi, _) => State::Csi,
                (State::Osc, '\u{7}') => State::Text,
                (State::Osc, '\u{1b}') => State::OscEscape,
                (State::Osc, _) => State::Osc,
                (State::OscEscape, '\\') => State::Text,
                (State::OscEscape, _) => State::Osc,
            };
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_cursor_moves_and_titles_are_removed() {
        let mut strip = AnsiStripper::default();
        assert_eq!(strip.push("\u{1b}[31mred\u{1b}[0m plain"), "red plain");
        assert_eq!(strip.push("a\u{1b}[2K\u{1b}[1Gb"), "ab");
        assert_eq!(strip.push("\u{1b}]0;title\u{7}after"), "after");
        assert_eq!(
            strip.push("\u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
        assert_eq!(strip.push("naïve ✓ text"), "naïve ✓ text");
    }

    #[test]
    fn a_sequence_split_between_fragments_is_still_removed() {
        let mut strip = AnsiStripper::default();
        let out = [
            strip.push("ok \u{1b}"),
            strip.push("[3"),
            strip.push("1;1mred"),
            strip.push("\u{1b}]0;ti"),
            strip.push("tle\u{7}!"),
        ]
        .concat();
        assert_eq!(out, "ok red!");
    }
}
