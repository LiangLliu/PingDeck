use super::*;
use CatalogSyncErrorKind as K;

const SAMPLE: &str = include_str!("../../../../../fixtures/models-dev-sample.json");

fn sample() -> ParsedCatalog {
    parse(SAMPLE, 7).unwrap()
}

/// `family/model_id` of a row.
fn key(record: &ModelRecord) -> String {
    format!("{}/{}", record.family, record.model_id)
}

fn by<'a>(parsed: &'a ParsedCatalog, wanted: &str) -> &'a ModelRecord {
    parsed.records.iter().find(|r| key(r) == wanted).unwrap()
}

#[test]
fn the_sample_parses_to_its_chat_and_embedding_models_in_key_order() {
    let parsed = sample();
    assert_eq!(parsed.snapshot_at_ms, None);
    let keys: Vec<String> = parsed.records.iter().map(key).collect();
    assert_eq!(
        keys,
        [
            "alpha/alpha-bare",
            "alpha/alpha-embed",
            "alpha/alpha-free",
            "alpha/alpha-nocost",
            "alpha/alpha-old",
            "alpha/alpha-pro",
            "alpha/alpha-tiered",
            "alpha/alpha-weird",
            "beta/alpha-pro",
            "beta/beta-own",
        ]
    );
}

#[test]
fn a_full_model_entry_maps_every_field() {
    let parsed = sample();
    let pro = by(&parsed, "alpha/alpha-pro");
    assert_eq!(
        (pro.family_name.as_str(), pro.name.as_str(), pro.kind),
        ("Alpha Labs", "Alpha Pro", CatalogKind::Chat)
    );
    assert_eq!(
        (pro.context_tokens, pro.max_output_tokens),
        (Some(200_000), Some(64_000))
    );
    assert!(pro.reasoning && pro.vision && pro.tools && pro.prompt_caching);
    assert_eq!(
        (
            pro.input_per_mtok,
            pro.output_per_mtok,
            pro.cache_read_per_mtok,
            pro.cache_write_per_mtok,
            pro.reasoning_per_mtok
        ),
        (Some(3.0), Some(15.0), Some(0.3), Some(3.75), Some(15.0))
    );
    assert_eq!(
        (pro.currency.as_str(), pro.synced_at_ms, pro.source),
        ("USD", 7, CatalogSource::ModelsDev)
    );
    assert!(pro.is_first_party() && pro.canonical_id.is_empty());
    assert_eq!(pro.input_modalities, ["text", "image", "pdf"]);
    assert!(by(&parsed, "alpha/alpha-bare").input_modalities.is_empty());
}

#[test]
fn release_dates_keep_their_day_or_month_precision() {
    let parsed = sample();
    let dates: Vec<(&str, Option<&str>)> = parsed
        .records
        .iter()
        .map(|r| (r.model_id.as_str(), r.release_date.as_deref()))
        .collect();
    assert_eq!(
        dates[..8],
        [
            ("alpha-bare", None),
            ("alpha-embed", None),
            ("alpha-free", Some("2025-11-20")),
            ("alpha-nocost", Some("2026-03-01")),
            ("alpha-old", Some("2024-02")),
            ("alpha-pro", Some("2026-03-01")),
            ("alpha-tiered", Some("2026-03")),
            ("alpha-weird", None),
        ]
    );
}

#[test]
fn costs_and_limits_are_read_field_by_field() {
    let parsed = sample();
    let tiered = by(&parsed, "alpha/alpha-tiered");
    assert_eq!(
        (
            tiered.input_per_mtok,
            tiered.output_per_mtok,
            tiered.context_tokens,
            tiered.vision
        ),
        (Some(3.0), Some(15.0), Some(1_000_000), false)
    );
    let free = by(&parsed, "alpha/alpha-free");
    assert_eq!(
        (free.input_per_mtok, free.output_per_mtok),
        (Some(0.0), Some(0.0))
    );
    assert!(free.has_price());
    let nocost = by(&parsed, "alpha/alpha-nocost");
    assert!(!nocost.has_price() && nocost.reasoning);
    let embed = by(&parsed, "alpha/alpha-embed");
    assert_eq!(
        (embed.kind, embed.input_per_mtok),
        (CatalogKind::Embedding, Some(0.02))
    );
    let bare = by(&parsed, "alpha/alpha-bare");
    assert_eq!(
        (
            bare.name.as_str(),
            bare.context_tokens,
            bare.max_output_tokens
        ),
        ("Bare", None, None)
    );
    assert!(!bare.vision && !bare.tools && !bare.reasoning);
    assert_eq!(by(&parsed, "alpha/alpha-old").input_per_mtok, Some(10.0));
    let weird = by(&parsed, "alpha/alpha-weird");
    assert_eq!(
        (
            weird.input_per_mtok,
            weird.output_per_mtok,
            weird.context_tokens,
            weird.max_output_tokens
        ),
        (None, Some(1.0), None, Some(1024))
    );
}

#[test]
fn a_resold_model_keeps_its_canonical_id() {
    let parsed = sample();
    let resold = by(&parsed, "beta/alpha-pro");
    assert_eq!(resold.canonical_id, "alpha/alpha-pro");
    assert!(!resold.is_first_party() && by(&parsed, "beta/beta-own").prompt_caching);
}

#[test]
fn only_a_json_object_body_parses() {
    assert!(parse("[1]", 0).is_err() && parse("nope", 0).is_err());
    assert!(parse("{}", 0).unwrap().records.is_empty());
}

#[test]
fn input_modalities_are_filtered_and_ordered() {
    let mods = parse(
        r#"{"fam":{"name":"F","models":{"m":{"modalities":{"input":["pdf","video","smell","text","audio"]}}}}}"#,
        1,
    )
    .unwrap();
    assert_eq!(
        mods.records[0].input_modalities,
        ["text", "audio", "video", "pdf"]
    );
    assert!(!mods.records[0].vision);
}

#[test]
fn a_remote_body_needs_at_least_50_models() {
    let err = parse_remote(SAMPLE, 0).unwrap_err();
    assert!(matches!(err, AppError::Validation { .. }) && err.message().contains("10 models"));
    let fifty: Vec<String> = (0..50)
        .map(|n| format!("\"m{n}\":{{\"name\":\"M{n}\",\"cost\":{{\"input\":1,\"output\":2}}}}"))
        .collect();
    assert_eq!(
        parse_remote(
            &format!("{{\"p\":{{\"models\":{{{}}}}}}}", fifty.join(",")),
            1
        )
        .unwrap()
        .records
        .len(),
        50
    );
    assert!(
        parse_remote(
            &format!("{{\"p\":{{\"models\":{{{}}}}}}}", fifty[..49].join(",")),
            1
        )
        .is_err()
    );
}

#[test]
fn the_bundled_snapshot_is_small_sorted_and_covers_the_major_labs() {
    assert!(SNAPSHOT_JSON.len() < 400_000);
    let snap = bundled_snapshot().unwrap();
    let at = snap.snapshot_at_ms.unwrap();
    assert!(at > 1_700_000_000_000 && snap.records.len() > 500);
    let mut families: Vec<&str> = snap.records.iter().map(|r| r.family.as_str()).collect();
    families.dedup();
    for f in [
        "openai",
        "anthropic",
        "google",
        "deepseek",
        "alibaba",
        "zhipuai",
        "moonshotai",
        "xai",
        "mistral",
        "minimax",
    ] {
        assert!(families.contains(&f), "{f}");
    }
    assert!(
        snap.records
            .windows(2)
            .all(|w| (&w[0].family, &w[0].model_id) < (&w[1].family, &w[1].model_id))
    );
}

#[test]
fn error_kind_classifies_download_failures() {
    for (msg, want) in [
        (
            "Model catalog download failed: HTTP 503 Service Unavailable",
            (K::Http, Some(503)),
        ),
        (
            "Model catalog download failed: HTTP 301 Moved Permanently",
            (K::Http, Some(301)),
        ),
        (
            "Model catalog download failed: error sending request",
            (K::Network, None),
        ),
        ("Model catalog download timed out", (K::Timeout, None)),
        ("Model catalog is larger than 16 MiB", (K::TooLarge, None)),
        (
            "Model catalog download is not valid UTF-8",
            (K::Invalid, None),
        ),
        (
            "Invalid models.dev catalog: expected value at line 1",
            (K::Invalid, None),
        ),
        (
            "The models.dev catalog has only 49 models; expected at least 50",
            (K::TooFew, None),
        ),
        ("database is locked", (K::Other, None)),
    ] {
        assert_eq!(error_kind(msg), want, "{msg}");
    }
}

/// Serves `head` and then `body` once on a loopback port; returns the URL.
async fn serve_once(head: &'static str, body: Vec<u8>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0_u8; 4096];
        let _ = socket.read(&mut buffer).await;
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(&body).await;
        let _ = socket.shutdown().await;
    });
    format!("http://127.0.0.1:{port}/api.json")
}

#[tokio::test]
async fn download_failures_carry_the_prefix_their_kind_is_read_back_from() {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .unwrap();
    let kind_of = |result: Result<String, AppError>| error_kind(result.unwrap_err().message());

    let url = serve_once(
        "HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n",
        b"{}".to_vec(),
    )
    .await;
    assert_eq!(download(&client, &url, None).await.unwrap(), "{}");

    let url = serve_once(
        "HTTP/1.1 301 Moved Permanently\r\nlocation: /x\r\ncontent-length: 0\r\n\r\n",
        Vec::new(),
    )
    .await;
    let error = download(&client, &url, None).await.unwrap_err();
    assert!(
        error.message().ends_with("(redirects are not followed)"),
        "{error}"
    );
    assert_eq!(error_kind(error.message()), (K::Http, Some(301)));

    let url = serve_once(
        "HTTP/1.1 200 OK\r\ncontent-length: 16777217\r\n\r\n",
        Vec::new(),
    )
    .await;
    assert_eq!(
        kind_of(download(&client, &url, None).await),
        (K::TooLarge, None)
    );

    // No Content-Length: the running total stops the read.
    let mut chunked = Vec::new();
    let piece = vec![b'a'; 1024 * 1024];
    for _ in 0..17 {
        chunked.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
        chunked.extend_from_slice(&piece);
        chunked.extend_from_slice(b"\r\n");
    }
    chunked.extend_from_slice(b"0\r\n\r\n");
    let url = serve_once(
        "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n",
        chunked,
    )
    .await;
    assert_eq!(
        kind_of(download(&client, &url, None).await),
        (K::TooLarge, None)
    );

    let url = serve_once(
        "HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n",
        vec![0xff, 0xfe],
    )
    .await;
    assert_eq!(
        kind_of(download(&client, &url, None).await),
        (K::Invalid, None)
    );
}
