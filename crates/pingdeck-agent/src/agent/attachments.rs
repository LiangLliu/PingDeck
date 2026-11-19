//! Agent chat attachment bridge and window path grants.
//!
//! Re-exports the content-addressed blob store and format sniffers from `pingdeck_store::attachments`,
//! and provides agent-specific path grants plus model-message part construction and reference
//! resolution (`attachment_parts`, `resolve_attachment_refs`).

mod grants;
#[cfg(test)]
mod tests;

use base64::{Engine as _, engine::general_purpose::STANDARD};

use pingdeck_model::agent::{AgentAttachment, AttachmentKind};
use pingdeck_model::hub::{HubDocument, HubDocumentSource, HubImage, HubMessage, HubPart};

pub use grants::{GRANT_TTL, PathGrants};
pub use pingdeck_store::attachments::{
    AttachmentLimits, AttachmentStore, Ingested, Sniffed, blobs_lock, count_lines,
    extract_pdf_text, sniff,
};

const MIB: u64 = 1024 * 1024;

/// Most bytes of a text attachment sent inline with the message; a larger one is announced and
/// read in ranges by the `read_attachment` tool.
pub const INLINE_TEXT_LIMIT: u64 = 100 * 1024;

/// What a target can take as a document: PDFs without it are sent as extracted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentSupport {
    pub pdf: bool,
}

/// The content of a user message: `text` first (when not empty), then one part per attachment.
/// Images become image refs, PDFs and text files document refs.
#[must_use]
pub fn attachment_parts(attachments: &[AgentAttachment], text: &str) -> Vec<HubPart> {
    let mut parts = Vec::with_capacity(attachments.len() + 1);
    if !text.is_empty() {
        parts.push(HubPart::text(text));
    }
    for attachment in attachments {
        let attachment_id = attachment.id.clone();
        match attachment.kind {
            AttachmentKind::Image => parts.push(HubPart::Image {
                image: HubImage::AttachmentRef {
                    attachment_id,
                    media_type: attachment.media_type.clone(),
                },
            }),
            AttachmentKind::Pdf | AttachmentKind::Text => parts.push(HubPart::Document {
                document: HubDocument {
                    media_type: attachment.media_type.clone(),
                    name: Some(attachment.name.clone()),
                    source: HubDocumentSource::AttachmentRef { attachment_id },
                },
            }),
        }
    }
    parts
}

/// Replaces every attachment reference in `messages` (tool results included) with the real bytes,
/// for the request about to be encoded; the stored history keeps the references.
///
/// `lookup` finds an attachment row by id. `extracted_text` returns the derived text attachment of
/// a PDF (creating it on first use); it is called only for a PDF when `caps.pdf` is false, and the
/// PDF is then sent as that text. A reference whose row or blob is gone becomes a text notice
/// (`[attachment unavailable: <name>]`) so an old session stays usable; so does a PDF without
/// extractable text for a target that cannot read PDFs. Returns how many references were resolved
/// to content.
pub fn resolve_attachment_refs(
    store: &AttachmentStore,
    lookup: &dyn Fn(&str) -> Option<AgentAttachment>,
    extracted_text: &dyn Fn(&AgentAttachment) -> Option<AgentAttachment>,
    caps: DocumentSupport,
    messages: &mut [HubMessage],
) -> usize {
    let resolver = Resolver {
        store,
        lookup,
        extracted_text,
        caps,
    };
    messages
        .iter_mut()
        .map(|message| resolver.resolve_parts(&mut message.content))
        .sum()
}

struct Resolver<'a> {
    store: &'a AttachmentStore,
    lookup: &'a dyn Fn(&str) -> Option<AgentAttachment>,
    extracted_text: &'a dyn Fn(&AgentAttachment) -> Option<AgentAttachment>,
    caps: DocumentSupport,
}

impl Resolver<'_> {
    fn resolve_parts(&self, parts: &mut [HubPart]) -> usize {
        let mut resolved = 0;
        for part in parts {
            let id = match part {
                HubPart::Image {
                    image: HubImage::AttachmentRef { attachment_id, .. },
                }
                | HubPart::Document {
                    document:
                        HubDocument {
                            source: HubDocumentSource::AttachmentRef { attachment_id },
                            ..
                        },
                } => attachment_id.clone(),
                HubPart::ToolResult { content, .. } => {
                    resolved += self.resolve_parts(content);
                    continue;
                }
                _ => continue,
            };
            let attachment = (self.lookup)(&id);
            let loaded = match &attachment {
                Some(attachment) => self.load(attachment, part),
                None => Err(unavailable(&id)),
            };
            match loaded {
                Ok(replacement) => {
                    *part = replacement;
                    resolved += 1;
                }
                Err(notice) => *part = HubPart::text(notice),
            }
        }
        resolved
    }

    /// The inline form of `part` (an image or document reference) with the blob's content, or the
    /// notice that replaces it when the content cannot be had.
    fn load(&self, attachment: &AgentAttachment, part: &HubPart) -> Result<HubPart, String> {
        let lost = || unavailable(&attachment.name);
        if !self.store.exists(&attachment.sha256) {
            return Err(format!("[attachment removed: {}]", attachment.name));
        }
        match part {
            HubPart::Image {
                image: HubImage::AttachmentRef { media_type, .. },
            } => {
                let bytes = self.store.read(&attachment.sha256).map_err(|_| lost())?;
                Ok(HubPart::Image {
                    image: HubImage::Base64 {
                        media_type: media_type.clone(),
                        data: STANDARD.encode(bytes),
                    },
                })
            }
            HubPart::Document { document } => {
                let (media_type, source) = match attachment.kind {
                    AttachmentKind::Text if attachment.size_bytes > INLINE_TEXT_LIMIT => {
                        return Ok(HubPart::text(announcement(self.store, attachment)));
                    }
                    AttachmentKind::Text => (
                        document.media_type.clone(),
                        HubDocumentSource::Text {
                            text: inline_text(self.store, attachment).ok_or_else(lost)?,
                        },
                    ),
                    AttachmentKind::Pdf if !self.caps.pdf => {
                        // A lost PDF blob is a lost attachment; a readable one without text is not.
                        if !self.store.exists(&attachment.sha256) {
                            return Err(lost());
                        }
                        let derived = (self.extracted_text)(attachment).ok_or_else(|| {
                            format!(
                                "[attachment unavailable: {}: this model cannot read PDFs and the file has no text layer]",
                                attachment.name
                            )
                        })?;
                        if derived.size_bytes > INLINE_TEXT_LIMIT {
                            return Ok(HubPart::text(announcement(self.store, &derived)));
                        }
                        let text = inline_text(self.store, &derived).ok_or_else(lost)?;
                        ("text/plain".to_owned(), HubDocumentSource::Text { text })
                    }
                    _ => (
                        document.media_type.clone(),
                        HubDocumentSource::Base64 {
                            data: STANDARD
                                .encode(self.store.read(&attachment.sha256).map_err(|_| lost())?),
                        },
                    ),
                };
                Ok(HubPart::Document {
                    document: HubDocument {
                        media_type,
                        source,
                        ..document.clone()
                    },
                })
            }
            _ => Err(lost()),
        }
    }
}

fn unavailable(name: &str) -> String {
    format!("[attachment unavailable: {name}]")
}

/// The note that stands in for a text attachment too large to send inline; the model reads it in
/// ranges with `read_attachment`.
fn announcement(store: &AttachmentStore, attachment: &AgentAttachment) -> String {
    let lines = attachment
        .lines
        .or_else(|| {
            let bytes = store.read(&attachment.sha256).ok()?;
            Some(count_lines(&bytes))
        })
        .map_or_else(String::new, |lines| format!(", {lines} lines"));
    format!(
        "[attachment {}{lines}, {}: use read_attachment]",
        attachment.name,
        format_size(attachment.size_bytes)
    )
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{} KB", bytes.div_ceil(1024))
    } else {
        format!("{:.1} MB", bytes as f64 / MIB as f64)
    }
}

/// The text of a text attachment of at most [`INLINE_TEXT_LIMIT`] bytes.
fn inline_text(store: &AttachmentStore, attachment: &AgentAttachment) -> Option<String> {
    let bytes = store.read(&attachment.sha256).ok()?;
    String::from_utf8(bytes).ok()
}
