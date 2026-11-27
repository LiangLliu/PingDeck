//! A real local HTTP/1.1 mock server plus a Mockoon environment importer.
//!
//! Servers run on tokio with hyper (keep-alive, chunked request bodies, bounded
//! concurrency, asynchronous latency). Routes, responses, rules, headers,
//! latency and CORS live behind an `RwLock`, so saving a mock while it runs
//! hot-reloads every request without restarting the listener.
//!
//! Layout: [`server`] and [`http`] own the sockets and the hyper translation; the
//! other modules are request-to-reply logic ([`reply`], [`routing`], [`crud`],
//! [`proxy`], [`callbacks`], [`template`]) plus the Mockoon importer. [`logs`] and [`fanout`]
//! keep the request log and deliver it to subscribers in batches; [`history`] records every
//! served request as a history row on a background writer thread.

mod callbacks;
mod crud;
mod environment;
mod fanout;
mod history;
mod http;
mod live_data;
mod logs;
mod mockoon;
mod proxy;
mod recording;
mod reply;
mod routing;
mod server;
mod template;
mod template_extras;
#[cfg(test)]
mod tests;
mod transfer;

pub use environment::with_environment;
pub use history::{HISTORY_QUEUE_CAPACITY, MockHistorySink, MockHistoryWriter};
pub use live_data::{MAX_BUCKET_BYTES, MAX_TOTAL_BYTES, MockDataResetError, MockLiveBucket};
pub use logs::MOCK_LOG_CAPACITY;
pub use mockoon::import_mockoon_environment;
pub use recording::{MAX_RECORDING_BYTES, MAX_RECORDINGS, MockRecording};
pub use reply::{decode_base64_body, encode_base64_body};
pub use routing::{method_matches, normalized_prefix, route_answers, strip_prefix};
pub use server::{
    MockServerManager, MockServerStats, MockStartError, MockSubscribeError, RunningMockServer,
};
#[cfg(test)]
pub(crate) use tests::support::{response, route, server};
pub use transfer::{
    EXPORT_FORMAT, EXPORT_VERSION, TransferError, export_json, file_body_paths, parse_export,
    reissue_ids, strip_literal_credentials,
};
