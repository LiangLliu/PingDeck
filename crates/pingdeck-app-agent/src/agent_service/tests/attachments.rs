//! Attachment use-cases against an on-disk state in a temp folder.

use std::io::Write;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use flate2::Compression;
use flate2::write::ZlibEncoder;

use super::super::attachments::{
    agent_attachment_gc_impl, agent_attachment_get_bytes_impl, agent_attachment_get_impl,
    agent_attachment_put_bytes_impl, agent_attachment_put_path_impl,
    agent_attachment_set_thumb_impl, agent_blob_put_impl, agent_blob_read_impl,
    ensure_extracted_text, load_sendable, resolve_for_model, store_for,
};
use super::*;
use crate::agent::attachments::DocumentSupport;
use crate::agent::model::{AgentAttachment, AttachmentKind};
use crate::llm::hub::{HubDocument, HubDocumentSource, HubImage, HubMessage};

pub(super) const PNG_1X1: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0, 0,
    1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0, 0, 0, 0,
];

const THUMB: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAAAAAAAA";

pub(super) const MIB: usize = 1024 * 1024;

pub(super) fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = PNG_1X1[..16].to_vec();
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes
}

pub(super) fn disk_state(dir: &TempDir) -> AppState {
    let state = AppState::open(dir.path().join("pingdeck.db")).expect("state opens");
    state.on_start().expect("start");
    state
}

pub(super) fn put(state: &AppState, name: &str, media_type: &str, bytes: &[u8]) -> AgentAttachment {
    agent_attachment_put_bytes_impl(state, name, media_type, bytes, None).expect("attachment puts")
}

fn pdf_with_text(pages: &[&str]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    for (index, page) in pages.iter().enumerate() {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder
            .write_all(format!("BT /F1 12 Tf ({page}) Tj ET").as_bytes())
            .expect("deflate");
        let data = encoder.finish().expect("deflate finishes");
        bytes.extend_from_slice(
            format!(
                "{} 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
                index + 1,
                data.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
    }
    bytes.extend_from_slice(b"%%EOF\n");
    bytes
}

#[test]
fn put_bytes_and_path_sniff_dedupe_validate_limits_and_thumbnails() {
    let mem = state();
    assert!(matches!(
        agent_attachment_put_bytes_impl(&mem, "a.txt", "", b"x", None),
        Err(AppError::Unsupported { .. })
    ));

    let dir = TempDir::new("att-put");
    let state = disk_state(&dir);
    let first = put(&state, "pasted.txt", "text/plain", PNG_1X1);
    assert_eq!(first.kind, AttachmentKind::Image);
    assert_eq!(first.media_type, "image/png");
    assert_eq!((first.width, first.height), (Some(1), Some(1)));

    let with_thumb = agent_attachment_put_bytes_impl(
        &state,
        "other.png",
        "image/png",
        PNG_1X1,
        Some(THUMB.to_owned()),
    )
    .expect("dedupe + thumb");
    assert_eq!(with_thumb.id, first.id);
    assert_eq!(with_thumb.thumb_data_url.as_deref(), Some(THUMB));
    assert_eq!(
        agent_attachment_get_bytes_impl(&state, &first.id).expect("bytes"),
        PNG_1X1
    );

    let lazy = put(&state, "lazy.png", "image/png", &png(30, 20));
    let updated = agent_attachment_set_thumb_impl(&state, &lazy.id, THUMB).expect("set thumb");
    assert_eq!(updated.thumb_data_url.as_deref(), Some(THUMB));
    assert!(matches!(
        agent_attachment_set_thumb_impl(&state, &lazy.id, "data:text/html;base64,PGI+"),
        Err(AppError::Validation { .. })
    ));

    let file = dir.path().join("notes.md");
    fs::write(&file, "# notes").expect("write");
    let from_path =
        agent_attachment_put_path_impl(&state, &file.display().to_string(), None).expect("path");
    assert_eq!(
        (from_path.name.as_str(), from_path.kind),
        ("notes.md", AttachmentKind::Text)
    );
    assert!(matches!(
        agent_attachment_put_path_impl(&state, "relative/notes.md", None),
        Err(AppError::Validation { .. })
    ));

    let tricky = put(&state, "../../etc/pa\nsswd.txt", "", b"one");
    assert_eq!(tricky.name, "passwd.txt");

    settings_update_impl(
        &state,
        SettingsSection::Ai,
        &json!({"agent": {"max_image_mb": 1, "max_file_mb": 2}}),
    )
    .expect("limits");
    let mut big_img = png(100, 100);
    big_img.resize(MIB + 1, 0);
    let err = agent_attachment_put_bytes_impl(&state, "p.png", "image/png", &big_img, None)
        .expect_err("over image cap");
    assert!(err.message().contains("image limit is 1 MB"));
}

#[test]
fn pdf_extraction_sendable_validation_and_model_resolution() {
    let dir = TempDir::new("att-resolve");
    let state = disk_state(&dir);
    let image = put(&state, "p.png", "", PNG_1X1);
    let pdf_bytes = pdf_with_text(&["Quarterly numbers", "Appendix"]);
    let pdf = put(&state, "report.pdf", "", &pdf_bytes);
    let lost = put(&state, "lost.txt", "", b"will vanish");

    let ids = [image.id.clone(), pdf.id.clone()];
    assert_eq!(load_sendable(&state, &ids).expect("sendable").len(), 2);
    let eleven: Vec<String> = (0..11).map(|_| image.id.clone()).collect();
    assert!(load_sendable(&state, &eleven).is_err());

    let derived = ensure_extracted_text(&state, &pdf.id).expect("text layer");
    assert_eq!(
        agent_attachment_get_bytes_impl(&state, &derived.id).expect("bytes"),
        b"Quarterly numbers\n\nAppendix"
    );
    assert_eq!(ensure_extracted_text(&state, &pdf.id), Some(derived));

    store_for(&state)
        .expect("store")
        .remove(&lost.sha256)
        .expect("remove blob");
    let make_messages = || {
        vec![HubMessage {
            role: crate::llm::hub::HubRole::User,
            content: crate::agent::attachments::attachment_parts(
                &[image.clone(), pdf.clone(), lost.clone()],
                "look",
            ),
        }]
    };

    let mut native = make_messages();
    resolve_for_model(&state, DocumentSupport { pdf: true }, &mut native);
    assert!(matches!(
        &native[0].content[1],
        HubPart::Image { image: HubImage::Base64 { media_type, data } }
            if media_type == "image/png" && *data == STANDARD.encode(PNG_1X1)
    ));
    assert!(matches!(
        &native[0].content[2],
        HubPart::Document { document: HubDocument { media_type, source: HubDocumentSource::Base64 { .. }, .. } }
            if media_type == "application/pdf"
    ));
    assert_eq!(
        native[0].content[3],
        HubPart::text("[attachment removed: lost.txt]")
    );

    let mut lowered = make_messages();
    resolve_for_model(&state, DocumentSupport { pdf: false }, &mut lowered);
    assert!(matches!(
        &lowered[0].content[2],
        HubPart::Document { document: HubDocument { source: HubDocumentSource::Text { text }, .. } }
            if text == "Quarterly numbers\n\nAppendix"
    ));
}

#[test]
fn gc_keeps_linked_attachments_derived_text_and_journal_blobs() {
    let dir = TempDir::new("att-gc");
    let state = disk_state(&dir);
    let chat = session(&state, "chat");
    let pdf = put(&state, "r.pdf", "", &pdf_with_text(&["Words"]));
    let derived = ensure_extracted_text(&state, &pdf.id).expect("text");
    let unsent = put(&state, "unsent.txt", "", b"unsent");
    let stored_msg = append(&state, &message(&chat.id, "m1", AgentRole::User, "hi", 1));
    state
        .store()
        .link_message_attachments(&stored_msg.id, std::slice::from_ref(&pdf.id))
        .expect("link");

    let before = agent_blob_put_impl(&state, b"original").expect("blob");
    let unused = agent_blob_put_impl(&state, b"unused").expect("blob");
    assert_eq!(agent_blob_read_impl(&state, &before).unwrap(), b"original");
    assert!(matches!(
        agent_blob_read_impl(&state, "../../etc/passwd"),
        Err(AppError::Validation { .. })
    ));
    state
        .store()
        .insert_file_change(&crate::agent::model::FileChange {
            id: "c1".into(),
            session_id: chat.id.clone(),
            run_id: "r1".into(),
            call_id: "call1".into(),
            path: "/work/a.rs".into(),
            folder: None,
            kind: crate::agent::model::FileChangeKind::Edit,
            before_sha256: Some(before.clone()),
            after_sha256: None,
            added: 0,
            removed: 0,
            status: crate::agent::model::FileChangeStatus::Pending,
            created_at_ms: 1,
        })
        .expect("journal row");

    assert_eq!(agent_attachment_gc_impl(&state).expect("gc"), 2);
    assert!(agent_attachment_get_impl(&state, &pdf.id).is_ok());
    assert!(agent_attachment_get_impl(&state, &derived.id).is_ok());
    assert!(agent_attachment_get_impl(&state, &unsent.id).is_err());
    assert!(agent_blob_read_impl(&state, &before).is_ok());
    assert!(agent_blob_read_impl(&state, &unused).is_err());
}
