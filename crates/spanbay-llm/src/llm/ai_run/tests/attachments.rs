//! Attachment placeholders against a temp store.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use base64::{Engine as _, engine::general_purpose::STANDARD};

use super::super::attachments::{has_placeholder, resolve};

static COUNTER: AtomicU32 = AtomicU32::new(0);

const SHA: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00112233445566778899aabbccddeeff0";
const SHA2: &str = "0f1e2d3c4b5a6978ffffffffffffffffffffffffffffffffffffffffffffffff";

struct Store(PathBuf);

impl Store {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pd-att-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn put(&self, sha: &str, bytes: &[u8]) {
        let folder = self.0.join(&sha[..2]);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(sha), bytes).unwrap();
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn png(extra: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.resize(8 + extra, 7);
    bytes
}

fn text(result: Result<Vec<u8>, super::super::attachments::AttachmentError>) -> String {
    String::from_utf8(result.unwrap()).unwrap()
}

#[test]
fn a_full_hash_resolves_to_the_blobs_base64() {
    let store = Store::new();
    store.put(SHA, &png(100));
    let body = format!("{{\"url\":\"data:image/png;base64,{{{{attachment:{SHA}}}}}\"}}");
    let out = text(resolve(&body, Some(&store.0)));
    assert_eq!(
        out,
        format!(
            "{{\"url\":\"data:image/png;base64,{}\"}}",
            STANDARD.encode(png(100))
        )
    );
}

#[test]
fn a_twelve_digit_prefix_with_a_size_hint_resolves() {
    let store = Store::new();
    store.put(SHA, &png(10));
    let out = text(resolve(
        &format!("<{{{{attachment:{} 214 KB}}}}>", &SHA[..12]),
        Some(&store.0),
    ));
    assert_eq!(out, format!("<{}>", STANDARD.encode(png(10))));
}

#[test]
fn several_placeholders_and_the_text_between_them_are_kept() {
    let store = Store::new();
    store.put(SHA, &png(1));
    store.put(SHA2, b"%PDF-1.4 data");
    let body = format!("a {{{{attachment:{SHA}}}}} b {{{{attachment:{SHA2}}}}} c");
    let out = text(resolve(&body, Some(&store.0)));
    assert_eq!(
        out,
        format!(
            "a {} b {} c",
            STANDARD.encode(png(1)),
            STANDARD.encode(b"%PDF-1.4 data")
        )
    );
}

#[test]
fn a_body_without_placeholders_is_unchanged_and_needs_no_store() {
    assert!(!has_placeholder("{\"a\": 1}"));
    assert!(has_placeholder("x {{attachment:abc"));
    assert_eq!(text(resolve("{\"a\": 1}", None)), "{\"a\": 1}");
}

#[test]
fn a_missing_blob_or_store_is_reported_by_its_short_reference() {
    let store = Store::new();
    let body = format!("{{{{attachment:{SHA}}}}}");
    let error = resolve(&body, Some(&store.0)).unwrap_err();
    assert_eq!(error.0, format!("Attachment {} is missing", &SHA[..12]));
    assert!(resolve(&body, None).is_err());
}

#[test]
fn paths_cannot_be_spelled_in_a_reference() {
    let store = Store::new();
    for reference in [
        "../../../../etc/passwd",
        "..%2f..%2fetc%2fpasswd",
        "0f1e2d3c4b5a/../../x",
        "0f1e2d3c4b5a\\..\\x",
        "/etc/passwd",
        "0f1e2d3c4b5",
        "zzzzzzzzzzzzzzzz",
        &format!("{SHA}0"),
        "",
    ] {
        let error =
            resolve(&format!("{{{{attachment:{reference}}}}}"), Some(&store.0)).unwrap_err();
        assert!(
            error.0.contains("not an attachment reference"),
            "{reference}: {}",
            error.0
        );
    }
}

#[test]
fn an_unclosed_placeholder_is_refused() {
    let error = resolve("{{attachment:0f1e2d3c4b5a", None).unwrap_err();
    assert!(error.0.contains("not closed"));
}

#[test]
fn an_ambiguous_prefix_is_refused() {
    let store = Store::new();
    store.put(SHA, &png(1));
    store.put(SHA2, &png(2));
    let error = resolve("{{attachment:0f1e2d3c4b5a}}", Some(&store.0)).unwrap_err();
    assert!(error.0.contains("more than one"), "{}", error.0);
}

#[test]
fn only_images_and_pdfs_are_read() {
    let store = Store::new();
    store.put(SHA, b"just some text, not an image");
    let error = resolve(&format!("{{{{attachment:{SHA}}}}}"), Some(&store.0)).unwrap_err();
    assert!(
        error.0.contains("png, jpeg, webp, gif or pdf"),
        "{}",
        error.0
    );
    for (sha, bytes) in [
        (
            "1111111111111111111111111111111111111111111111111111111111111111",
            b"\xFF\xD8\xFFrest".to_vec(),
        ),
        (
            "2222222222222222222222222222222222222222222222222222222222222222",
            b"GIF89a....".to_vec(),
        ),
        (
            "3333333333333333333333333333333333333333333333333333333333333333",
            b"RIFF\0\0\0\0WEBPVP8 ".to_vec(),
        ),
    ] {
        store.put(sha, &bytes);
        assert!(resolve(&format!("{{{{attachment:{sha}}}}}"), Some(&store.0)).is_ok());
    }
}

#[test]
fn images_stop_at_10_mb_and_pdfs_at_20() {
    let store = Store::new();
    let mut image = png(0);
    image.resize(10 * 1024 * 1024, 0);
    store.put(SHA, &image);
    assert!(resolve(&format!("{{{{attachment:{SHA}}}}}"), Some(&store.0)).is_ok());
    image.push(0);
    store.put(SHA, &image);
    let error = resolve(&format!("{{{{attachment:{SHA}}}}}"), Some(&store.0)).unwrap_err();
    assert!(error.0.contains("10 MB"), "{}", error.0);

    let mut pdf = b"%PDF-1.7\n".to_vec();
    pdf.resize(11 * 1024 * 1024, b' ');
    store.put(SHA2, &pdf);
    assert!(
        resolve(&format!("{{{{attachment:{SHA2}}}}}"), Some(&store.0)).is_ok(),
        "a pdf may be over 10 MB"
    );
    pdf.resize(20 * 1024 * 1024 + 1, b' ');
    store.put(SHA2, &pdf);
    let error = resolve(&format!("{{{{attachment:{SHA2}}}}}"), Some(&store.0)).unwrap_err();
    assert!(error.0.contains("20 MB"), "{}", error.0);
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_in_the_store_is_not_followed() {
    let store = Store::new();
    let outside = store.0.join("outside.png");
    std::fs::write(&outside, png(10)).unwrap();
    let folder = store.0.join(&SHA[..2]);
    std::fs::create_dir_all(&folder).unwrap();
    std::os::unix::fs::symlink(&outside, folder.join(SHA)).unwrap();
    let error = resolve(&format!("{{{{attachment:{SHA}}}}}"), Some(&store.0)).unwrap_err();
    assert!(error.0.contains("missing"), "{}", error.0);
}
