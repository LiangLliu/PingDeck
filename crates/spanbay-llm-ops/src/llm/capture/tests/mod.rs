//! Tests of the capture module, one file per concern, sharing the fixtures below.

mod bound;
mod curl;
mod cut_order;
mod diagnostics;
mod exchange;
mod policy;
mod redact;
mod replay;
mod reply;
mod split_frames;

use serde_json::{Value, json};

use crate::llm::capture::{
    BoundedBody, CapturedExchange, CapturedFrame, CapturedFrames, RawExchange, RowFacts,
    TimingJson, TranslationKind, TranslationNote, bound_body, redact_exchange,
};
use crate::llm::wire::Protocol;

/// Base62 alphabet for random secrets.
pub(super) const BASE62: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
/// A gateway token shaped like `pd_<tag>_<32 base62>`.
pub(super) const PD_TOKEN: &str = "pd_cc_AbCdEfGh01234567AbCdEfGh01234567";
/// A provider key shaped like an OpenAI one.
pub(super) const SK_KEY: &str = "sk-abcdefghijklmnopqrstuvwxyz012345";
/// The gateway base the curl tests use.
pub(super) const GATEWAY: &str = "http://127.0.0.1:4141";

pub(super) fn bounded(text: &str) -> BoundedBody {
    bound_body(text.as_bytes())
}

pub(super) fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

/// A translated Messages -> Chat exchange carrying a client token and a provider key.
pub(super) fn raw() -> RawExchange {
    RawExchange {
        request_log_id: 7,
        ts_ms: 1_000,
        client_protocol: Protocol::AnthropicMessages,
        client_req_headers: pairs(&[
            ("Authorization", &format!("Bearer {PD_TOKEN}")),
            ("content-type", "application/json"),
            ("anthropic-version", "2023-06-01"),
        ]),
        client_req_body: Some(bounded(
            r#"{"model":"claude-code","max_tokens":64,"top_k":5,"messages":[{"role":"user","content":"hi"}]}"#,
        )),
        hub_request: None,
        upstream_url: "https://api.deepseek.com/chat/completions".to_owned(),
        upstream_req_headers: pairs(&[
            ("Authorization", &format!("Bearer {SK_KEY}")),
            ("Content-Type", "application/json"),
            ("Content-Length", "123"),
        ]),
        upstream_req_body: Some(bounded(
            r#"{"model":"deepseek-v4-pro","max_tokens":64,"messages":[{"role":"user","content":"hi"}]}"#,
        )),
        upstream_status: Some(200),
        upstream_resp_headers: pairs(&[
            ("content-type", "application/json"),
            ("set-cookie", "session=abcdef123456"),
        ]),
        upstream_resp_body: Some(bounded(
            r#"{"id":"c1","model":"deepseek-v4-pro","choices":[{"index":0,"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#,
        )),
        client_resp_headers: Vec::new(),
        client_resp_body: None,
        upstream_frames: None,
        client_frames: None,
        translation_notes: vec![TranslationNote {
            field: "top_k".to_owned(),
            kind: TranslationKind::Dropped,
            reason: "no equivalent in Chat Completions".to_owned(),
        }],
        timing: TimingJson::default(),
    }
}

pub(super) fn captured() -> CapturedExchange {
    redact_exchange(raw(), &[]).0
}

pub(super) fn facts() -> RowFacts {
    RowFacts {
        protocol_out: Some(Protocol::OpenaiChat),
        requested_model: "claude-code".to_owned(),
        upstream_model: Some("deepseek-v4-pro".to_owned()),
        stream: false,
        provider_id: Some("p1".to_owned()),
        provider_exists: true,
    }
}

/// The JSON text of a Chat delta frame carrying `content`.
pub(super) fn chat_data(content: &str) -> String {
    json!({"choices": [{"index": 0, "delta": {"content": content}}]}).to_string()
}

pub(super) fn chat_frame(index: u32, content: &str) -> CapturedFrame {
    CapturedFrame {
        index,
        t_ms: u64::from(index),
        event: "message".to_owned(),
        data: chat_data(content),
    }
}

/// One Chat delta frame per piece.
pub(super) fn frames_of(pieces: &[&str]) -> CapturedFrames {
    let frames: Vec<CapturedFrame> = pieces
        .iter()
        .enumerate()
        .map(|(i, piece)| chat_frame(i as u32 + 1, piece))
        .collect();
    CapturedFrames {
        total_frames: frames.len() as u64,
        frames,
        truncated: false,
    }
}

/// `frames` stored as the client frames of [`raw`], redacted.
pub(super) fn redact_client_frames(frames: CapturedFrames, needles: &[String]) -> CapturedFrames {
    let mut exchange = raw();
    exchange.client_frames = Some(frames);
    redact_exchange(exchange, needles).0.client_frames.unwrap()
}

/// The data text of every frame.
pub(super) fn datas(frames: &CapturedFrames) -> Vec<String> {
    frames.frames.iter().map(|f| f.data.clone()).collect()
}

/// The delta content of every Chat frame.
pub(super) fn contents(frames: &CapturedFrames) -> Vec<String> {
    frames
        .frames
        .iter()
        .map(|f| {
            let value: Value = serde_json::from_str(&f.data).unwrap();
            value["choices"][0]["delta"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// xorshift64*: a fixed seed keeps the property test reproducible.
pub(super) struct Rng(pub(super) u64);

impl Rng {
    pub(super) fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub(super) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub(super) fn chars(&mut self, alphabet: &str, len: usize) -> String {
        let bytes = alphabet.as_bytes();
        (0..len)
            .map(|_| bytes[self.below(bytes.len())] as char)
            .collect()
    }

    /// `min..min + span` characters.
    pub(super) fn chars_in(&mut self, alphabet: &str, min: usize, span: usize) -> String {
        let len = min + self.below(span);
        self.chars(alphabet, len)
    }
}

/// A random secret of one of the shapes the text rules know, or `needle`. With `with_bearer` a
/// `Bearer` value is one of nine shapes; without it there are eight (picks 6 and 7 map to the JWT
/// and the needle). The seeded property tests depend on the exact draws, so keep the `below` calls
/// and their order unchanged.
pub(super) fn random_secret(rng: &mut Rng, needle: &str, with_bearer: bool) -> String {
    let pick = rng.below(if with_bearer { 9 } else { 8 });
    // Without the Bearer shape, picks 6 and 7 mean the JWT and the needle.
    let shape = if !with_bearer && pick >= 6 {
        pick + 1
    } else {
        pick
    };
    match shape {
        0 => format!("sk-{}", rng.chars_in(BASE62, 16, 30)),
        1 => format!("pd_cc_{}", rng.chars(BASE62, 32)),
        2 => format!("AIza{}", rng.chars(BASE62, 35)),
        3 => format!("gsk_{}", rng.chars_in(BASE62, 20, 20)),
        4 => format!("xai-{}", rng.chars_in(BASE62, 20, 20)),
        5 => format!("ghp_{}", rng.chars(BASE62, 36)),
        6 => format!("Bearer {}", rng.chars_in(BASE62, 20, 20)),
        7 => format!(
            "eyJ{}.eyJ{}.{}",
            rng.chars(BASE62, 12),
            rng.chars(BASE62, 12),
            rng.chars(BASE62, 12)
        ),
        _ => needle.to_owned(),
    }
}
