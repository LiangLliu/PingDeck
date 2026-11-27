//! Response content codings: a body the server compressed with any coding a browser or an
//! imported cURL asks for (`Accept-Encoding: gzip, deflate, br, zstd`) reaches formatting,
//! JSONPath and assertions decoded.

use super::support::{TestServer, TestServerAction};
use crate::executor::send::execute_request;
use crate::executor::types::ExecutorOptions;
use spanbay_model::{KeyValue, Request};

/// `{"ok":true}` in each coding, as `brotli`, `zstd` and zlib write it.
const BROTLI: &[u8] = &[
    0x21, 0x28, 0x00, 0x04, 0x7b, 0x22, 0x6f, 0x6b, 0x22, 0x3a, 0x74, 0x72, 0x75, 0x65, 0x7d, 0x03,
];
const ZSTD: &[u8] = &[
    0x28, 0xb5, 0x2f, 0xfd, 0x24, 0x0b, 0x59, 0x00, 0x00, 0x7b, 0x22, 0x6f, 0x6b, 0x22, 0x3a, 0x74,
    0x72, 0x75, 0x65, 0x7d, 0x6a, 0xbe, 0x13, 0xc7,
];
const DEFLATE: &[u8] = &[
    0x78, 0x9c, 0xab, 0x56, 0xca, 0xcf, 0x56, 0xb2, 0x2a, 0x29, 0x2a, 0x4d, 0xad, 0x05, 0x00, 0x16,
    0xe3, 0x04, 0x11,
];

#[tokio::test]
async fn brotli_zstd_and_deflate_bodies_are_decoded_even_when_the_request_asks_for_them() {
    let encoded = |coding: &str, body: &[u8]| {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Encoding: {coding}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        TestServerAction::RespondBytes(response)
    };
    let server = TestServer::spawn_sequence(vec![
        encoded("br", BROTLI),
        encoded("zstd", ZSTD),
        encoded("deflate", DEFLATE),
    ]);
    let mut request = Request::new("req_coding", "ws_coding", "Coding", &server.base_url).unwrap();
    // As a cURL copied from Firefox carries it.
    request.headers.push(KeyValue {
        key: "Accept-Encoding".into(),
        value: "gzip, deflate, br, zstd".into(),
        enabled: true,
        description: None,
    });
    for coding in ["br", "zstd", "deflate"] {
        let result = execute_request(&request, None, ExecutorOptions::default())
            .await
            .unwrap();
        assert_eq!(result.body_text, r#"{"ok":true}"#, "{coding}");
    }
}
