//! What a message row puts into the search index (`agent_messages.search_text`) and which
//! attachments its parts link.

use pingdeck_model::agent::{AgentMessage, AgentRole};
use pingdeck_model::hub::{HubDocument, HubDocumentSource, HubImage, HubPart};

/// Longest searchable text kept per message, in bytes.
const SEARCH_TEXT_LIMIT: usize = 1024 * 1024;

/// Most bytes of one tool result that reach the search index: its head and its tail.
const TOOL_RESULT_SEARCH_BYTES: usize = 4096;

/// Text of the placeholder result stored with a tool turn until its tool finishes; it is what
/// the model sees if the application dies first.
pub(super) const PENDING_RESULT_TEXT: &str = "[interrupted before tool finished]";

/// The first and last half of `cap` bytes of `text` when it is longer than `cap`, joined by an
/// ellipsis line, each cut on a character boundary.
fn head_and_tail(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let head = text.floor_char_boundary(cap / 2);
    let tail = text.ceil_char_boundary(text.len() - cap / 2);
    format!("{}\n…\n{}", &text[..head], &text[tail..])
}

/// The searchable text of a message: its text parts in full and each tool result capped at
/// [`TOOL_RESULT_SEARCH_BYTES`] (head and tail), the whole cut on a character boundary. The
/// placeholder of a tool turn still running is not text anyone wrote, and output spilled to disk
/// never gets here.
pub(super) fn search_text(parts: &[HubPart]) -> String {
    let mut text = String::new();
    let mut push = |piece: &str| {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(piece);
    };
    for part in parts {
        match part {
            HubPart::Text { text: piece, .. } => push(piece),
            HubPart::ToolResult { content, .. } => {
                for piece in content {
                    if let HubPart::Text { text: piece, .. } = piece
                        && piece != PENDING_RESULT_TEXT
                    {
                        push(&head_and_tail(piece, TOOL_RESULT_SEARCH_BYTES));
                    }
                }
            }
            _ => {}
        }
    }
    text.truncate(text.floor_char_boundary(SEARCH_TEXT_LIMIT));
    text
}

/// The `search_text` a stored row of `message` gets. A summary is not conversation and an
/// unfinished stream is not final text: neither may be found by a search or become the
/// session's preview.
pub(super) fn indexed_text(message: &AgentMessage) -> String {
    if message.role == AgentRole::Summary || message.partial {
        String::new()
    } else {
        search_text(&message.parts)
    }
}

/// The attachment ids a message's top-level parts reference, in order.
pub(super) fn attachment_ids(parts: &[HubPart]) -> Vec<&str> {
    parts
        .iter()
        .filter_map(|part| match part {
            HubPart::Image {
                image: HubImage::AttachmentRef { attachment_id, .. },
            }
            | HubPart::Document {
                document:
                    HubDocument {
                        source: HubDocumentSource::AttachmentRef { attachment_id },
                        ..
                    },
            } => Some(attachment_id.as_str()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_result(text: &str) -> HubPart {
        HubPart::ToolResult {
            call_id: "c1".to_owned(),
            name: None,
            content: vec![HubPart::text(text)],
            is_error: false,
        }
    }

    #[test]
    fn a_text_within_the_cap_is_kept_whole() {
        assert_eq!(head_and_tail("abcdef", 6), "abcdef");
        assert_eq!(head_and_tail("", 0), "");
    }

    #[test]
    fn head_and_tail_cut_on_character_boundaries() {
        assert_eq!(head_and_tail("abcdefgh", 4), "ab\n…\ngh");
        // `é` is 2 bytes: the head cut at byte 3 moves down to 2 and the tail cut at byte 7
        // (10 - 3) moves up to 8.
        assert_eq!(head_and_tail("ééééé", 6), "é\n…\né");
        // `中` is 3 bytes: the head cut at byte 4 moves down to 3 and the tail cut at byte 8
        // (12 - 4) moves up to 9.
        assert_eq!(head_and_tail("中中中中", 8), "中\n…\n中");
    }

    #[test]
    fn the_running_placeholder_is_not_indexed() {
        let parts = [
            HubPart::text("question"),
            tool_result(PENDING_RESULT_TEXT),
            tool_result("output"),
        ];
        assert_eq!(search_text(&parts), "question\noutput");
    }

    #[test]
    fn search_text_is_cut_on_a_character_boundary_at_the_limit() {
        let text = format!("{}é", "a".repeat(SEARCH_TEXT_LIMIT - 1));
        let indexed = search_text(&[HubPart::text(text)]);
        assert_eq!(indexed.len(), SEARCH_TEXT_LIMIT - 1);
        assert!(indexed.bytes().all(|byte| byte == b'a'));
    }

    #[test]
    fn attachment_ids_are_the_top_level_references_in_order() {
        let image = |id: &str| HubPart::Image {
            image: HubImage::AttachmentRef {
                attachment_id: id.to_owned(),
                media_type: "image/png".to_owned(),
            },
        };
        let document = |id: &str| HubPart::Document {
            document: HubDocument {
                media_type: "application/pdf".to_owned(),
                name: None,
                source: HubDocumentSource::AttachmentRef {
                    attachment_id: id.to_owned(),
                },
            },
        };
        let nested = HubPart::ToolResult {
            call_id: "c1".to_owned(),
            name: None,
            content: vec![image("inner")],
            is_error: false,
        };
        let parts = [document("d1"), HubPart::text("look"), nested, image("i1")];
        assert_eq!(attachment_ids(&parts), ["d1", "i1"]);
    }
}
