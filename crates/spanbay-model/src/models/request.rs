use serde::{Deserialize, Serialize};

use crate::ai_request::{AiRequestConfig, RequestMode};
use crate::validation::{WorkspaceError, validate_name, validate_url};

use super::{
    Assertion, AuthConfig, CollectionId, FolderId, FormPart, KeyValue, NetworkOptions, RequestId,
    WorkspaceId,
};

/// A saved request with its method, URL, headers, auth, body, and network options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: RequestId,
    pub workspace_id: WorkspaceId,
    pub collection_id: Option<CollectionId>,
    pub folder_id: Option<FolderId>,
    pub name: String,
    pub protocol: ProtocolKind,
    pub method: HttpMethod,
    pub url: String,
    pub query_params: Vec<KeyValue>,
    #[serde(default)]
    pub path_variables: Vec<KeyValue>,
    pub headers: Vec<KeyValue>,
    pub auth: AuthConfig,
    pub body: RequestBody,
    pub network_options: NetworkOptions,
    #[serde(default)]
    pub assertions: Vec<Assertion>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub pre_request_script: String,
    #[serde(default)]
    pub test_script: String,
    #[serde(default)]
    pub position: i64,
    /// How the request is edited and run; `ai` is `Some` exactly in AI API mode.
    #[serde(default)]
    pub mode: RequestMode,
    /// AI API settings (`None` for HTTP requests).
    #[serde(default)]
    pub ai: Option<AiRequestConfig>,
    /// When the agent last created or changed this saved request, until the user sends it from
    /// the UI (`requests.agent_touched_ms`). Read-only: a save ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_touched_ms: Option<i64>,
    /// Saved response examples (imported from Postman or OpenAPI), at most
    /// [`MAX_REQUEST_EXAMPLES`]; stored inside `requests.network_options_json`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<RequestExample>,
    /// Opaque version of the saved request the reader got (filled by the use cases that answer
    /// the editor, never stored or exported). A save that echoes it is refused with `Conflict`
    /// (`stale_write`) when the stored request changed since; without it the save overwrites.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

/// Most saved examples one request keeps.
pub const MAX_REQUEST_EXAMPLES: usize = 20;
/// Largest example body kept, in bytes; a longer body is cut at a character boundary.
pub const MAX_REQUEST_EXAMPLE_BODY_BYTES: usize = 64 * 1024;

/// A saved response example of a request: what the endpoint returns for one case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestExample {
    pub name: String,
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub body: String,
}

impl RequestExample {
    /// The example with its body cut to [`MAX_REQUEST_EXAMPLE_BODY_BYTES`]; `true` when cut.
    #[must_use]
    pub fn capped(mut self) -> (Self, bool) {
        if self.body.len() <= MAX_REQUEST_EXAMPLE_BODY_BYTES {
            return (self, false);
        }
        let mut end = MAX_REQUEST_EXAMPLE_BODY_BYTES;
        while !self.body.is_char_boundary(end) {
            end -= 1;
        }
        self.body.truncate(end);
        (self, true)
    }
}

impl Request {
    /// Creates a GET HTTP request with a validated id, workspace id, name, and URL.
    pub fn new(
        id: impl AsRef<str>,
        workspace_id: impl AsRef<str>,
        name: impl AsRef<str>,
        url: impl AsRef<str>,
    ) -> Result<Self, WorkspaceError> {
        Ok(Self {
            id: RequestId::new(id)?,
            workspace_id: WorkspaceId::new(workspace_id)?,
            collection_id: None,
            folder_id: None,
            name: validate_name("request", name.as_ref())?,
            protocol: ProtocolKind::HttpRest,
            method: HttpMethod::Get,
            url: validate_url(url.as_ref())?,
            query_params: Vec::new(),
            path_variables: Vec::new(),
            headers: Vec::new(),
            auth: AuthConfig::default(),
            body: RequestBody::None,
            network_options: NetworkOptions::default(),
            assertions: Vec::new(),
            description: String::new(),
            pre_request_script: String::new(),
            test_script: String::new(),
            position: 0,
            mode: RequestMode::Http,
            ai: None,
            agent_touched_ms: None,
            examples: Vec::new(),
            revision: None,
        })
    }

    /// Validates every assertion attached to the request.
    pub fn validate_assertions(&self) -> Result<(), WorkspaceError> {
        self.assertions.iter().try_for_each(Assertion::validate)
    }

    /// Rejects more than [`MAX_REQUEST_EXAMPLES`] examples or an example body over
    /// [`MAX_REQUEST_EXAMPLE_BODY_BYTES`].
    pub fn validate_examples(&self) -> Result<(), WorkspaceError> {
        if self.examples.len() > MAX_REQUEST_EXAMPLES {
            return Err(WorkspaceError::InvalidExample {
                reason: format!("a request keeps at most {MAX_REQUEST_EXAMPLES} examples"),
            });
        }
        if self
            .examples
            .iter()
            .any(|example| example.body.len() > MAX_REQUEST_EXAMPLE_BODY_BYTES)
        {
            return Err(WorkspaceError::InvalidExample {
                reason: format!(
                    "an example body is larger than {MAX_REQUEST_EXAMPLE_BODY_BYTES} bytes"
                ),
            });
        }
        Ok(())
    }

    /// Attaches the request to a collection, validating the collection id.
    pub fn with_collection(
        mut self,
        collection_id: impl AsRef<str>,
    ) -> Result<Self, WorkspaceError> {
        self.collection_id = Some(CollectionId::new(collection_id)?);
        Ok(self)
    }

    /// Attaches the request to a folder, validating the folder id.
    pub fn with_folder(mut self, folder_id: impl AsRef<str>) -> Result<Self, WorkspaceError> {
        self.folder_id = Some(FolderId::new(folder_id)?);
        Ok(self)
    }
}

/// The protocol a request speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolKind {
    HttpRest,
    Graphql,
    WebSocket,
    Grpc,
}

/// HTTP method of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

/// Body payload of a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestBody {
    None,
    RawText {
        content: String,
        /// The raw language picked in the editor; it sets the implicit Content-Type when no
        /// header does. Absent means plain text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<RawLanguage>,
    },
    Json {
        content: String,
    },
    FormUrlEncoded {
        fields: Vec<KeyValue>,
    },
    Multipart {
        parts: Vec<FormPart>,
    },
    Graphql {
        query: String,
        variables_json: Option<String>,
        operation_name: Option<String>,
    },
    Binary {
        file_path: String,
        content_type: Option<String>,
    },
}

/// Raw body languages other than JSON (JSON is its own [`RequestBody::Json`] variant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RawLanguage {
    Text,
    Javascript,
    Html,
    Xml,
}

impl RawLanguage {
    /// The Content-Type sent when no enabled header sets one.
    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Text => "text/plain; charset=utf-8",
            Self::Javascript => "application/javascript",
            Self::Html => "text/html; charset=utf-8",
            Self::Xml => "application/xml",
        }
    }
}

/// Outcome of the file picker for a binary request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestBodyFilePickResult {
    pub opened: bool,
    pub path: Option<String>,
}

impl RequestBodyFilePickResult {
    /// Reports that the user selected the file at `path`.
    #[must_use]
    pub fn selected(path: impl Into<String>) -> Self {
        Self {
            opened: true,
            path: Some(path.into()),
        }
    }

    /// Reports that the picker was canceled.
    #[must_use]
    pub fn canceled() -> Self {
        Self {
            opened: false,
            path: None,
        }
    }
}

/// Readiness state of the file referenced by a binary request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestBodyFileValidationStatus {
    Ready,
    Missing,
    NotFile,
    Unreadable,
    Empty,
    UnresolvedVariable,
    // Larger than the request body limit; sending it fails the same way.
    TooLarge,
}

/// Validation status of a request body file together with a display message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestBodyFileValidationResult {
    pub status: RequestBodyFileValidationStatus,
    pub message: String,
}

impl RequestBodyFileValidationResult {
    /// Result for a file that exists and is readable.
    #[must_use]
    pub fn ready() -> Self {
        Self {
            status: RequestBodyFileValidationStatus::Ready,
            message: "File ready".to_string(),
        }
    }

    /// Result for a blank file path.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            status: RequestBodyFileValidationStatus::Empty,
            message: "No file path".to_string(),
        }
    }

    /// Result for a path that does not exist.
    #[must_use]
    pub fn missing() -> Self {
        Self {
            status: RequestBodyFileValidationStatus::Missing,
            message: "File missing".to_string(),
        }
    }

    /// Result for a path that exists but is not a regular file.
    #[must_use]
    pub fn not_file() -> Self {
        Self {
            status: RequestBodyFileValidationStatus::NotFile,
            message: "Not a file".to_string(),
        }
    }

    /// Result for a file that could not be read, embedding the underlying `message`.
    #[must_use]
    pub fn unreadable(message: impl Into<String>) -> Self {
        Self {
            status: RequestBodyFileValidationStatus::Unreadable,
            message: format!("File unreadable: {}", message.into()),
        }
    }

    /// Result for a file over the request body limit of `limit_bytes`.
    #[must_use]
    pub fn too_large(limit_bytes: u64) -> Self {
        Self {
            status: RequestBodyFileValidationStatus::TooLarge,
            message: format!(
                "File is larger than the {} MiB request body limit",
                limit_bytes / (1024 * 1024)
            ),
        }
    }

    /// Result for a path containing a variable `key` that could not be resolved.
    #[must_use]
    pub fn unresolved_variable(key: impl Into<String>) -> Self {
        Self {
            status: RequestBodyFileValidationStatus::UnresolvedVariable,
            message: format!("Unresolved variable · {}", key.into()),
        }
    }
}
