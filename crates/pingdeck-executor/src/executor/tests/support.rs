//! Shared fixtures for the executor tests: local servers, environments, cookie entries and temp files.

use pingdeck_model::{
    Environment, EnvironmentVariable, FormPart, HttpMethod, Request, RequestBody,
};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn environment_with_plain_values(values: Vec<(&str, &str)>) -> Environment {
    let mut environment =
        Environment::new("env_local", "ws_billing", "Local").expect("environment is valid");
    environment.variables = values
        .into_iter()
        .map(|(key, value)| {
            EnvironmentVariable::plain(key, value).expect("variable should be valid")
        })
        .collect();
    environment
}

pub(super) fn environment_with_secret(key: &str, value: &str) -> Environment {
    let mut environment =
        Environment::new("env_local", "ws_billing", "Local").expect("environment is valid");
    environment.variables =
        vec![EnvironmentVariable::masked(key, value).expect("secret variable should be valid")];
    environment
}

pub(super) fn write_temp_file(name: &str, contents: &[u8]) -> TestTempFile {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("pingdeck-test-{unique}"));
    fs::create_dir_all(&directory).expect("temp directory should be created");
    let path = directory.join(name);
    fs::write(&path, contents).expect("temp file should be written");

    TestTempFile { directory, path }
}

pub(super) fn multipart_file_request(
    base_url: &str,
    path: &std::path::Path,
    retry_count: u8,
) -> Request {
    let mut request = Request::new(
        "req_stream_upload",
        "ws_billing",
        "Stream upload",
        format!("{base_url}/upload"),
    )
    .expect("request should be valid");
    request.method = HttpMethod::Post;
    request.network_options.retry_count = retry_count;
    request.body = RequestBody::Multipart {
        parts: vec![FormPart {
            key: "file".into(),
            value: String::new(),
            file_path: Some(path.to_string_lossy().into_owned()),
            enabled: true,
        }],
    };
    request
}

pub(super) struct TestTempFile {
    directory: PathBuf,
    path: PathBuf,
}

impl TestTempFile {
    pub(super) fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TestTempFile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Returns a loopback URL bound to an ephemeral port and immediately released, so connecting to it
/// is refused promptly rather than waiting for a half-open socket timeout.
pub(super) fn unused_local_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral port should bind");
    let address = listener.local_addr().expect("listener address");
    drop(listener);
    format!("http://{address}")
}

pub(super) use crate::test_server::{TestServer, TestServerAction};
