//! The one token estimate: the agent's context figures, the context breakdown, the
//! gateway's `count_tokens` and the request log all count with it.
//!
//! Rule: ASCII bytes count 4 to a token, other UTF-8 bytes 3 to a token (CJK text is about one
//! token per character); an image, a non-text document and an audio clip have a flat price. It
//! is deterministic and cheap; the agent corrects it with the provider's reported counts.

use std::io::Write;

use crate::hub::{HubDocumentSource, HubMessage, HubPart, HubRequest};

/// Tokens charged for one image, whatever its size.
pub const IMAGE_TOKENS: u64 = 1500;
/// Tokens charged for one document that is not already extracted text.
pub const DOCUMENT_TOKENS: u64 = 1000;
/// Tokens charged for one audio clip.
pub const AUDIO_TOKENS: u64 = 500;

/// A running estimate: ASCII and other bytes counted apart, plus flat charges.
#[derive(Debug, Default, Clone, Copy)]
pub struct Tally {
    ascii: u64,
    other: u64,
    flat: u64,
}

impl Tally {
    /// Adds the bytes of `text`.
    pub fn text(&mut self, text: &str) {
        self.bytes(text.as_bytes());
    }

    /// Adds raw bytes (invalid UTF-8 counts like any non-ASCII byte).
    pub fn bytes(&mut self, bytes: &[u8]) {
        let ascii = bytes.iter().filter(|byte| byte.is_ascii()).count() as u64;
        self.ascii += ascii;
        self.other += bytes.len() as u64 - ascii;
    }

    /// Adds the compact JSON size of `value` without building the string.
    pub fn json(&mut self, value: &serde_json::Value) {
        // Serialising a `Value` into a counting sink cannot fail.
        serde_json::to_writer(TallySink(self), value).ok();
    }

    /// Adds a flat charge.
    pub fn flat(&mut self, tokens: u64) {
        self.flat += tokens;
    }

    /// Adds every part (tool results recursively).
    pub fn parts(&mut self, parts: &[HubPart]) {
        for part in parts {
            match part {
                HubPart::Text { text, .. } | HubPart::Reasoning { text, .. } => self.text(text),
                HubPart::Image { .. } => self.flat += IMAGE_TOKENS,
                HubPart::Document { document } => match &document.source {
                    HubDocumentSource::Text { text } => self.text(text),
                    _ => self.flat += DOCUMENT_TOKENS,
                },
                HubPart::Audio { .. } => self.flat += AUDIO_TOKENS,
                HubPart::ToolCall {
                    name, arguments, ..
                } => {
                    self.text(name);
                    self.text(arguments);
                }
                HubPart::ToolResult { name, content, .. } => {
                    if let Some(name) = name {
                        self.text(name);
                    }
                    self.parts(content);
                }
                HubPart::ServerToolUse { name, input, .. } => {
                    self.text(name);
                    self.json(input);
                }
                HubPart::ServerToolResult { name, payload, .. } => {
                    self.text(name);
                    self.json(payload);
                }
            }
        }
    }

    /// Adds the parts of every message.
    pub fn messages(&mut self, messages: &[HubMessage]) {
        for message in messages {
            self.parts(&message.content);
        }
    }

    /// The estimate so far.
    #[must_use]
    pub fn tokens(&self) -> u64 {
        (self.ascii * 3 + self.other * 4).div_ceil(12) + self.flat
    }
}

/// An `io::Write` sink that tallies the bytes written to it.
struct TallySink<'a>(&'a mut Tally);

impl Write for TallySink<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.bytes(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Estimated tokens of `text`.
#[must_use]
pub fn text_tokens(text: &str) -> u64 {
    bytes_tokens(text.as_bytes())
}

/// Estimated tokens of raw bytes.
#[must_use]
pub fn bytes_tokens(bytes: &[u8]) -> u64 {
    let mut tally = Tally::default();
    tally.bytes(bytes);
    tally.tokens()
}

/// Estimated prompt tokens of `request`: system prompt, messages and tool definitions.
#[must_use]
pub fn request_tokens(request: &HubRequest) -> u64 {
    let mut tally = Tally::default();
    if let Some(system) = &request.system {
        tally.text(system);
    }
    tally.messages(&request.messages);
    for tool in &request.tools {
        tally.text(&tool.name);
        if let Some(description) = &tool.description {
            tally.text(description);
        }
        tally.json(&tool.parameters);
    }
    tally.tokens()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_counts_four_bytes_and_other_text_three_bytes_a_token() {
        assert_eq!(text_tokens(""), 0);
        assert_eq!(text_tokens("abcd"), 1);
        assert_eq!(text_tokens("abcde"), 2);
        // 9 UTF-8 bytes of CJK text: one token per character.
        assert_eq!(text_tokens("日本語"), 3);
        // 4 ASCII + 3 CJK bytes: 1 + 1.
        assert_eq!(text_tokens("abcd日"), 2);
        assert_eq!(bytes_tokens(&[0xFF; 5]), 2);
    }
}
