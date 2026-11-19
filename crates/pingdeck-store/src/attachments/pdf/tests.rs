//! PDF text extraction against hand-built PDFs (FlateDecode and plain streams) and hostile input.

use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;

use super::*;

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(data).expect("deflate");
    encoder.finish().expect("deflate finishes")
}

fn stream_object(dictionary: &str, data: &[u8]) -> Vec<u8> {
    let mut object = format!(
        "1 0 obj\n<< {dictionary} /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    object.extend_from_slice(data);
    object.extend_from_slice(b"\nendstream\nendobj\n");
    object
}

fn flate(content: &str) -> Vec<u8> {
    stream_object("/Filter /FlateDecode", &deflate(content.as_bytes()))
}

fn plain(content: &str) -> Vec<u8> {
    stream_object("", content.as_bytes())
}

fn pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    for object in objects {
        bytes.extend_from_slice(object);
    }
    bytes.extend_from_slice(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
    bytes
}

fn text_of(objects: &[Vec<u8>]) -> Option<String> {
    extract_pdf_text(&pdf(objects))
}

#[test]
fn flate_plain_and_multi_stream_extraction_skips_non_text_streams() {
    let image = stream_object(
        "/Type /XObject /Subtype /Image /Filter /FlateDecode",
        &deflate(b"BT (hidden in pixels) Tj ET"),
    );
    let metadata = stream_object("/Type /Metadata /Subtype /XML", b"BT (xmp) Tj ET");
    let text = text_of(&[
        flate("BT /F1 12 Tf 72 700 Td (Hello, world) Tj 0 -14 Td (Second line) Tj ET"),
        image,
        plain("BT (Page two, uncompressed) Tj ET"),
        metadata,
    ]);
    assert_eq!(
        text.as_deref(),
        Some("Hello, world\nSecond line\n\nPage two, uncompressed")
    );
}

#[test]
fn text_operators_escapes_hex_utf16_and_comments_are_decoded() {
    let tj =
        "BT [(Hel) 20 (lo) -300 (Wor) 5 (ld)] TJ 0 -12 Td (two) ' 1 2 (three) \" T* (four) Tj ET";
    assert_eq!(
        text_of(&[flate(tj)]).as_deref(),
        Some("Hello World\ntwo\nthree\nfour")
    );

    let escapes = concat!(
        "BT (a\\(b\\)c\\101\\\\) Tj 0 -1 Td (x(y)z) Tj 0 -1 Td <48656C6C6F> Tj ",
        "0 -1 Td <FEFF004800690020D83DDE00> Tj 0 -1 Td (caf\\351 \\223q\\224 \\2) Tj ",
        "0 -1 Td % (not shown) Tj\n/Span <</ActualText (alt)>> BDC (ab\\\ncd) Tj EMC ET"
    );
    assert_eq!(
        text_of(&[plain(escapes)]).as_deref(),
        Some("a(b)cA\\\nx(y)z\nHello\nHi \u{1F600}\ncafé “q”\nabcd")
    );
}

#[test]
fn unreadable_encrypted_composite_font_and_truncated_pdfs_return_none() {
    assert_eq!(extract_pdf_text(b""), None);
    assert_eq!(extract_pdf_text(b"%PDF-1.7"), None);
    assert_eq!(
        extract_pdf_text(b"%PDF-1.4\n1 0 obj\n<< >>\nstream\nBT (cut) Tj ET"),
        None
    );
    assert_eq!(text_of(&[plain("BT <00480065006C006C006F> Tj ET")]), None);
    assert_eq!(text_of(&[flate("BT /F1 12 Tf ET")]), None);

    let page = flate("BT (Quarterly report) Tj ET");
    let mut encrypted = pdf(std::slice::from_ref(&page));
    encrypted.extend_from_slice(b"<< /Encrypt 9 0 R >>\n");
    assert_eq!(extract_pdf_text(&encrypted), None);

    let mut composite = pdf(std::slice::from_ref(&page));
    composite.extend_from_slice(b"<< /Type /Font /Subtype /Type0 /Encoding /Identity-H >>\n");
    assert_eq!(extract_pdf_text(&composite), None);

    let garbage: Vec<u8> = (0..512_u32).map(|n| (n * 31 % 251) as u8).collect();
    assert_eq!(
        text_of(&[stream_object("/Filter /FlateDecode", &garbage)]),
        None
    );
    assert_eq!(
        text_of(&[stream_object(
            "/Filter [/ASCII85Decode /FlateDecode]",
            &deflate(b"BT (x) Tj ET")
        )]),
        None
    );

    let mut long = String::from("BT (first) Tj ET\n");
    for n in 0..500_u32 {
        long.push_str(&format!("BT ({}) Tj ET\n", n * 7919 % 10007));
    }
    let compressed = deflate(long.as_bytes());
    assert_eq!(
        text_of(&[stream_object(
            "/Filter /FlateDecode",
            &compressed[..compressed.len() / 2]
        )]),
        None
    );
}

#[test]
fn decompression_bombs_and_stream_caps_stay_bounded() {
    let line = "BT (aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa) Tj ET\n";
    let content = line.repeat(12 * 1024 * 1024 / line.len());
    let compressed = deflate(content.as_bytes());
    let text = text_of(&[stream_object("/Filter /FlateDecode", &compressed)]).expect("text");
    assert!(text.len() <= 1024 * 1024 && text.len() > 512 * 1024);

    let zero_bomb = deflate(&vec![0_u8; 10 * 1024 * 1024]);
    assert_eq!(
        text_of(&[stream_object("/Filter /FlateDecode", &zero_bomb)]),
        None
    );

    let objects_data = deflate(&vec![0_u8; 8 * 1024 * 1024]);
    let mut skipped = vec![stream_object("/Type /ObjStm /Filter /FlateDecode", &objects_data); 8];
    skipped.push(plain("BT (beyond inflation budget) Tj ET"));
    assert_eq!(text_of(&skipped), None);

    let mut objects: Vec<Vec<u8>> = (0..10_000).map(|_| plain("q Q")).collect();
    objects.push(plain("BT (too late) Tj ET"));
    assert_eq!(text_of(&objects), None);
    objects.remove(0);
    assert_eq!(text_of(&objects).as_deref(), Some("too late"));
}

#[test]
fn composite_font_inside_a_compressed_object_stream_is_unreadable() {
    let object_stream = |objects: &[u8]| {
        stream_object(
            "/Type /ObjStm /N 1 /First 4 /Filter /FlateDecode",
            &deflate(objects),
        )
    };
    let page = plain("BT (body) Tj ET");
    let composite = object_stream(b"5 0 << /Type /Font /Subtype /Type0 /Encoding /Identity-H >>");
    assert_eq!(text_of(&[composite, page.clone()]), None);
    let simple = object_stream(b"5 0 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    assert_eq!(text_of(&[simple, page]).as_deref(), Some("body"));
}

#[test]
fn crlf_after_stream_keyword_starts_the_data() {
    // Only a compressed stream tells `\r\n` from `\r`: a stray `\n` in front of zlib data breaks
    // the inflate, while in a plain stream it would pass as whitespace.
    let data = deflate(b"BT (crlf) Tj ET");
    let mut object = format!(
        "1 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\r\n",
        data.len()
    )
    .into_bytes();
    object.extend_from_slice(&data);
    object.extend_from_slice(b"\nendstream\nendobj\n");
    assert_eq!(text_of(&[object]).as_deref(), Some("crlf"));
}

#[test]
fn unknown_filter_streams_are_skipped_not_refused() {
    let unknown = stream_object("/Filter /FooDecode", b"BT (hidden) Tj ET");
    assert_eq!(
        text_of(&[unknown, plain("BT (shown) Tj ET")]).as_deref(),
        Some("shown")
    );
}

#[test]
fn stream_keyword_inside_a_word_is_ignored() {
    let glued =
        b"1 0 obj\n<< /Length 17 >>xstream\nBT (hidden) Tj ET\nendstream\nendobj\n".to_vec();
    assert_eq!(
        text_of(&[glued, plain("BT (shown) Tj ET")]).as_deref(),
        Some("shown")
    );
}

#[test]
fn literal_string_line_continuation_tab_and_octal_overflow() {
    // `\<CR><LF>` continues the line, `\t` becomes a space and `\777` keeps its low byte (0xFF).
    assert_eq!(
        text_of(&[plain("BT (a\\\r\nb\\tc\\777) Tj ET")]).as_deref(),
        Some("ab c\u{FF}")
    );
}
