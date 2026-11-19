//! Tests for agent attachment message-part bridging and reference resolution.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose::STANDARD};

use super::*;
use crate::agent::test_dir::TempDir;

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes
}

fn store(dir: &TempDir, max_bytes: u64) -> AttachmentStore {
    AttachmentStore::new(
        dir.path().join("attachments"),
        AttachmentLimits {
            image_bytes: max_bytes,
            file_bytes: max_bytes,
        },
    )
}

fn row(id: &str, name: &str, ingested: &Ingested) -> AgentAttachment {
    AgentAttachment {
        id: id.to_owned(),
        sha256: ingested.sha256.clone(),
        name: name.to_owned(),
        media_type: ingested.media_type.clone(),
        size_bytes: ingested.size_bytes,
        kind: ingested.kind,
        width: ingested.width,
        height: ingested.height,
        thumb_data_url: None,
        lines: None,
        removed: false,
        created_at_ms: 0,
    }
}

fn ref_image(id: &str) -> HubPart {
    HubPart::Image {
        image: HubImage::AttachmentRef {
            attachment_id: id.to_owned(),
            media_type: "image/png".to_owned(),
        },
    }
}

fn ref_doc(id: &str, media_type: &str) -> HubPart {
    HubPart::Document {
        document: HubDocument {
            media_type: media_type.to_owned(),
            name: Some(id.to_owned()),
            source: HubDocumentSource::AttachmentRef {
                attachment_id: id.to_owned(),
            },
        },
    }
}

fn user(content: Vec<HubPart>) -> HubMessage {
    HubMessage {
        role: pingdeck_model::hub::HubRole::User,
        content,
    }
}

fn lookup_in(rows: Vec<AgentAttachment>) -> impl Fn(&str) -> Option<AgentAttachment> {
    let rows: HashMap<String, AgentAttachment> =
        rows.into_iter().map(|row| (row.id.clone(), row)).collect();
    move |id| rows.get(id).cloned()
}

#[test]
fn attachment_parts_and_ref_resolution_including_missing_and_100kb_text_cap() {
    let dir = TempDir::new("att-parts");
    let store = store(&dir, 4 * MIB);
    let picture = png(2, 2);
    let image = row("i", "p.png", &store.ingest("p.png", "", &picture).unwrap());
    let pdf = row(
        "d",
        "r.pdf",
        &store.ingest("r.pdf", "", b"%PDF-1.7").unwrap(),
    );
    let text = row("t", "n.md", &store.ingest("n.md", "", b"# hi").unwrap());
    assert_eq!(attachment_parts(&[], ""), Vec::<HubPart>::new());
    assert_eq!(
        attachment_parts(&[image.clone(), pdf.clone(), text.clone()], "look").len(),
        4
    );

    let big = "€".repeat(500_000);
    let big_row = row(
        "b",
        "big.txt",
        &store.ingest("big.txt", "", big.as_bytes()).unwrap(),
    );
    let lost_ingested = store.ingest("gone.png", "", &png(1, 1)).unwrap();
    let lost = row("lost", "gone.png", &lost_ingested);
    store.remove(&lost_ingested.sha256).unwrap();

    let lookup = lookup_in(vec![image, pdf, text, big_row, lost]);
    let mut messages = vec![
        user(vec![
            HubPart::text("see"),
            ref_image("i"),
            ref_doc("d", "application/pdf"),
            ref_doc("b", "text/plain"),
            ref_image("lost"),
            ref_doc("unknown-row", "text/plain"),
        ]),
        HubMessage {
            role: pingdeck_model::hub::HubRole::Tool,
            content: vec![HubPart::ToolResult {
                call_id: "c".to_owned(),
                name: None,
                content: vec![ref_doc("t", "text/markdown"), ref_image("lost")],
                is_error: false,
            }],
        },
    ];
    let resolved = resolve_attachment_refs(
        &store,
        &lookup,
        &|_| None,
        DocumentSupport { pdf: true },
        &mut messages,
    );
    assert_eq!(resolved, 4);
    assert_eq!(
        messages[0].content[1],
        HubPart::Image {
            image: HubImage::Base64 {
                media_type: "image/png".to_owned(),
                data: STANDARD.encode(&picture),
            },
        }
    );
    assert_eq!(
        messages[0].content[3],
        HubPart::text("[attachment big.txt, 1 lines, 1.4 MB: use read_attachment]")
    );
    assert_eq!(
        messages[0].content[4],
        HubPart::text("[attachment removed: gone.png]")
    );
    assert_eq!(
        messages[0].content[5],
        HubPart::text("[attachment unavailable: unknown-row]")
    );
}

#[test]
fn pdf_resolves_natively_or_via_extracted_text_or_visible_notice() {
    let dir = TempDir::new("att-pdf");
    let store = store(&dir, MIB);
    let pdf = row(
        "d",
        "r.pdf",
        &store
            .ingest("r.pdf", "", b"%PDF-1.7 body of r.pdf")
            .unwrap(),
    );
    let derived = row(
        "d2",
        "r.pdf.txt",
        &store.ingest("r.pdf.txt", "", b"page text").unwrap(),
    );
    let lookup = lookup_in(vec![pdf]);

    let mut native = vec![user(vec![ref_doc("d", "application/pdf")])];
    assert_eq!(
        resolve_attachment_refs(
            &store,
            &lookup,
            &|_| panic!("unused"),
            DocumentSupport { pdf: true },
            &mut native
        ),
        1
    );

    let mut lowered = vec![user(vec![ref_doc("d", "application/pdf")])];
    assert_eq!(
        resolve_attachment_refs(
            &store,
            &lookup,
            &|_| Some(derived.clone()),
            DocumentSupport { pdf: false },
            &mut lowered
        ),
        1
    );
    assert_eq!(
        lowered[0].content[0],
        HubPart::Document {
            document: HubDocument {
                media_type: "text/plain".to_owned(),
                name: Some("d".to_owned()),
                source: HubDocumentSource::Text {
                    text: "page text".to_owned(),
                },
            },
        }
    );

    let mut no_text = vec![user(vec![ref_doc("d", "application/pdf")])];
    assert_eq!(
        resolve_attachment_refs(
            &store,
            &lookup,
            &|_| None,
            DocumentSupport { pdf: false },
            &mut no_text
        ),
        0
    );
    assert_eq!(
        no_text[0].content[0],
        HubPart::text(
            "[attachment unavailable: r.pdf: this model cannot read PDFs and the file has no text layer]"
        )
    );
}
