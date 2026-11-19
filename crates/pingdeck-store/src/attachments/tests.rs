//! Sniffing and content-addressed blob store tests.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "pingdeck-attachments-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes
}

fn jpeg(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    bytes.extend_from_slice(b"JFIF\0");
    bytes.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0]);
    bytes.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x04, 0, 0, 0xFF, 0xC2, 0x00, 0x11, 8]);
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1, 0xFF, 0xD9]);
    bytes
}

fn gif(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 0x3B]);
    bytes
}

fn webp(chunk: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&u32::try_from(payload.len() + 12).unwrap().to_le_bytes());
    bytes.extend_from_slice(b"WEBP");
    bytes.extend_from_slice(chunk);
    bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn webp_lossy(width: u16, height: u16) -> Vec<u8> {
    let mut payload = vec![0x10, 0, 0, 0x9D, 0x01, 0x2A];
    payload.extend_from_slice(&width.to_le_bytes());
    payload.extend_from_slice(&height.to_le_bytes());
    webp(b"VP8 ", &payload)
}

fn webp_lossless(width: u32, height: u32) -> Vec<u8> {
    let word = (width - 1) | ((height - 1) << 14);
    let mut payload = vec![0x2F];
    payload.extend_from_slice(&word.to_le_bytes());
    webp(b"VP8L", &payload)
}

fn webp_extended(width: u32, height: u32) -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
    payload.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
    webp(b"VP8X", &payload)
}

fn sniffed(bytes: &[u8], name: &str, hint: &str) -> Sniffed {
    sniff(bytes, name, hint).expect("a known type")
}

fn described(s: Sniffed) -> (&'static str, AttachmentKind, Option<u32>, Option<u32>) {
    (s.media_type, s.kind, s.width, s.height)
}

#[test]
fn images_report_header_dimensions() {
    for (bytes, media_type, w, h) in [
        (png(640, 480), "image/png", Some(640), Some(480)),
        (jpeg(321, 123), "image/jpeg", Some(321), Some(123)),
        (gif(17, 9), "image/gif", Some(17), Some(9)),
        (webp_lossy(100, 50), "image/webp", Some(100), Some(50)),
        (webp_lossless(4000, 3), "image/webp", Some(4000), Some(3)),
        (
            webp_extended(1920, 1080),
            "image/webp",
            Some(1920),
            Some(1080),
        ),
        (png(0, 5), "image/png", None, None),
        (vec![0xFF, 0xD8, 0xFF], "image/jpeg", None, None),
        (b"GIF89a\x01".to_vec(), "image/gif", None, None),
        (b"RIFF\0\0\0\0WEBP".to_vec(), "image/webp", None, None),
    ] {
        assert_eq!(
            described(sniffed(&bytes, "pic", "")),
            (media_type, AttachmentKind::Image, w, h)
        );
    }
}

#[test]
fn magic_bytes_beat_claimed_type() {
    assert_eq!(
        described(sniffed(b"%PDF-1.7\n", "x.png", "image/png")),
        ("application/pdf", AttachmentKind::Pdf, None, None)
    );
    assert_eq!(
        described(sniffed(&png(2, 2), "notes.txt", "text/plain")),
        ("image/png", AttachmentKind::Image, Some(2), Some(2))
    );
}

#[test]
fn text_subtype_from_extension_then_hint_essence() {
    for (name, hint, media_type) in [
        ("README.md", "", "text/markdown"),
        ("c.json", "", "application/json"),
        ("lib.rs", "text/markdown", "text/x-rust"),
        ("Makefile", "", "text/plain"),
        ("notes", "text/markdown; charset=utf-8", "text/markdown"),
        ("notes", "application/x-evil", "text/plain"),
    ] {
        assert_eq!(
            described(sniffed("héllo\n".as_bytes(), name, hint)),
            (media_type, AttachmentKind::Text, None, None)
        );
    }
}

#[test]
fn svg_and_binary_are_refused() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#;
    assert!(sniff(svg, "logo.svg", "image/svg+xml").is_none());
    assert!(sniff(svg, "LOGO.SVG", "").is_none());
    assert_eq!(
        sniffed(svg, "logo", "image/svg+xml").kind,
        AttachmentKind::Text
    );

    for bad in [&[0xC3_u8, 0x28][..], b"text\0with nul", b"MZ\x90\x00\x03"] {
        assert!(sniff(bad, "x.txt", "text/plain").is_none());
    }
}

fn limits(image_bytes: u64, file_bytes: u64) -> AttachmentLimits {
    AttachmentLimits {
        image_bytes,
        file_bytes,
    }
}

fn store(dir: &TempDir, max_bytes: u64) -> AttachmentStore {
    AttachmentStore::new(dir.path().join("attachments"), limits(max_bytes, max_bytes))
}

#[test]
fn ingest_dedupes_identical_content() {
    let dir = TempDir::new();
    let store = store(&dir, MIB);
    let bytes = png(8, 6);
    let first = store.ingest("a.png", "image/png", &bytes).expect("ingest");
    assert_eq!(first, store.ingest("copy.bin", "", &bytes).expect("dedupe"));
    assert_eq!(first.sha256, sha256_hex(&bytes));
    assert_eq!((first.width, first.height), (Some(8), Some(6)));
    assert_eq!(store.read(&first.sha256).expect("read"), bytes);
    assert_eq!(
        store.read_prefix(&first.sha256, 4).expect("prefix"),
        bytes[..4]
    );
}

#[cfg(unix)]
#[test]
fn blobs_and_shards_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new();
    let store = store(&dir, MIB);
    let sha = store
        .ingest("a.png", "image/png", &png(8, 6))
        .expect("ingest")
        .sha256;
    let blob = dir.path().join("attachments").join(&sha[..2]).join(&sha);
    assert_eq!(
        fs::metadata(&blob).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(blob.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn malformed_hashes_are_refused_and_never_exist() {
    let dir = TempDir::new();
    let store = store(&dir, MIB);
    for bad in [
        "../../secret",
        "..",
        "",
        "ab",
        &"A".repeat(64),
        &"g".repeat(64),
    ] {
        assert!(matches!(store.read(bad), Err(AppError::Validation { .. })));
        assert!(matches!(
            store.remove(bad),
            Err(AppError::Validation { .. })
        ));
        assert!(!store.exists(bad));
    }
}

#[test]
fn gc_keeps_rows_and_extra_and_deletes_the_rest() {
    let dir = TempDir::new();
    let store = store(&dir, MIB);
    let kept = store
        .ingest("a.png", "image/png", &png(8, 6))
        .expect("ingest");
    let extra_sha = store
        .put_blob(&vec![0; 2 * MIB as usize])
        .expect("put_blob");
    let stray = store.ingest("s.txt", "", b"stray").expect("stray");
    let keep = HashSet::from([kept.sha256.clone()]);
    let extra = HashSet::from([extra_sha.clone()]);
    assert_eq!(store.gc(&keep, &extra).expect("gc"), 1);
    assert!(store.exists(&kept.sha256) && store.exists(&extra_sha) && !store.exists(&stray.sha256));
}

#[test]
fn remove_reports_whether_a_blob_existed_then_read_is_not_found() {
    let dir = TempDir::new();
    let store = store(&dir, MIB);
    let sha = store
        .ingest("a.png", "image/png", &png(8, 6))
        .expect("ingest")
        .sha256;
    assert!(store.remove(&sha).expect("remove"));
    assert!(!store.remove(&sha).expect("remove again"));
    assert!(matches!(store.read(&sha), Err(AppError::NotFound { .. })));
}

// Blob names are persisted references, so the content address must never change.
#[test]
fn blob_file_name_is_the_fixed_sha256_content_address() {
    let dir = TempDir::new();
    let store = store(&dir, MIB);
    let sha = "336d3e2df787c9dbfd8e9df1247afc807dbc6477f4c3c1bd05790a8aa18aca54";
    assert_eq!(store.put_blob(b"PingDeck attachment").expect("put"), sha);
    assert!(dir.path().join("attachments/33").join(sha).is_file());
}

#[test]
fn size_and_pixel_limits_are_enforced_per_kind() {
    let dir = TempDir::new();
    let store = AttachmentStore::new(dir.path().join("attachments"), limits(MIB, 3 * MIB));
    let mut big_png = png(8, 8);
    big_png.resize(MIB as usize + 1, 0);
    let err = store.ingest("big.png", "", &big_png).unwrap_err();
    assert!(err.message().contains("image limit is 1 MB"));
    assert!(
        store
            .ingest("big.txt", "", &vec![b'a'; MIB as usize + 1])
            .is_ok()
    );

    let err = store
        .ingest("huge.txt", "", &vec![b'a'; 3 * MIB as usize + 1])
        .unwrap_err();
    assert!(err.message().contains("file limit is 3 MB"));
    assert_eq!(store.max_bytes(), 3 * MIB);
    assert!(
        store
            .too_large(4 * MIB)
            .message()
            .contains("file limit is 3 MB")
    );

    for (bytes, w, h) in [
        (png(2251, 10), 2251, 10),
        (webp_extended(9000, 9000), 9000, 9000),
    ] {
        let err = store.ingest("p", "", &bytes).unwrap_err();
        assert!(
            err.message().contains(&format!("{w} x {h} px")) && err.message().contains("2250 px")
        );
    }
    assert!(store.ingest("p", "", &png(2250, 2250)).is_ok());
    let err = store.ingest("p", "", &png(5, 5)[..20]).unwrap_err();
    assert!(matches!(err, AppError::Validation { .. }));
    assert!(err.message().contains("the image header cannot be read"));
}

#[test]
fn count_lines_counts_newlines_plus_an_open_last_line() {
    for (bytes, lines) in [
        (&b""[..], 0),
        (b"a", 1),
        (b"a\n", 1),
        (b"a\nb", 2),
        (b"\n\n", 2),
    ] {
        assert_eq!(count_lines(bytes), lines, "{bytes:?}");
    }
    // `lines` is persisted on the attachment row: text only.
    let dir = TempDir::new();
    let store = store(&dir, MIB);
    assert_eq!(
        store.ingest("a.txt", "", b"a\nb").expect("text").lines,
        Some(2)
    );
    assert_eq!(
        store.ingest("a.png", "", &png(2, 2)).expect("png").lines,
        None
    );
}

#[test]
fn gc_leaves_foreign_files_and_drops_emptied_shards() {
    let dir = TempDir::new();
    let store = store(&dir, MIB);
    assert_eq!(
        store.gc(&HashSet::new(), &HashSet::new()).expect("no root"),
        0
    );

    let root = dir.path().join("attachments");
    let doomed = store.put_blob(b"doomed").expect("put");
    let neighbour = store.put_blob(b"neighbour").expect("put");
    assert_ne!(doomed[..2], neighbour[..2], "the two blobs share a shard");
    fs::write(root.join("README"), b"foreign").expect("readme");
    let notes = root.join(&neighbour[..2]).join("notes.txt");
    fs::write(&notes, b"foreign").expect("notes");

    assert_eq!(store.gc(&HashSet::new(), &HashSet::new()).expect("gc"), 2);
    assert!(root.join("README").is_file());
    assert!(notes.is_file());
    assert!(!root.join(&doomed[..2]).exists());
}
