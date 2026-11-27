//! Tab enums and public model re-exports for `RequestsView`.

use spanbay_desktop::core::model::{AuthConfig, HttpMethod, RequestBody};
pub use spanbay_desktop::core::response_viewer::ALL_RESPONSE_FORMATS;

pub use super::model_ai::*;
pub use super::model_tabs::*;

pub const ALL_METHODS: [HttpMethod; 7] = [
    HttpMethod::Get,
    HttpMethod::Post,
    HttpMethod::Put,
    HttpMethod::Patch,
    HttpMethod::Delete,
    HttpMethod::Head,
    HttpMethod::Options,
];

/// HTTP request editor sub-tabs (matching SpanBay prototype & `TabSession::EditorTab`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HttpEdTab {
    #[default]
    Params,
    Headers,
    Auth,
    Body,
    Assertions,
    Scripts,
    Network,
    Docs,
}

impl HttpEdTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Params => 0,
            Self::Headers => 1,
            Self::Auth => 2,
            Self::Body => 3,
            Self::Assertions => 4,
            Self::Scripts => 5,
            Self::Network => 6,
            Self::Docs => 7,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Headers,
            2 => Self::Auth,
            3 => Self::Body,
            4 => Self::Assertions,
            5 => Self::Scripts,
            6 => Self::Network,
            7 => Self::Docs,
            _ => Self::Params,
        }
    }
}

/// HTTP response inspector sub-tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HttpRespTab {
    #[default]
    Body,
    Headers,
    Cookies,
    Trace,
    Tests,
}

impl HttpRespTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Body => 0,
            Self::Headers => 1,
            Self::Cookies => 2,
            Self::Trace => 3,
            Self::Tests => 4,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Headers,
            2 => Self::Cookies,
            3 => Self::Trace,
            4 => Self::Tests,
            _ => Self::Body,
        }
    }
}

/// AI API request editor sub-tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AiEdTab {
    #[default]
    Messages,
    Params,
    Tools,
    RawJson,
    Assertions,
    Scripts,
}

impl AiEdTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Messages => 0,
            Self::Params => 1,
            Self::Tools => 2,
            Self::RawJson => 3,
            Self::Assertions => 4,
            Self::Scripts => 5,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Params,
            2 => Self::Tools,
            3 => Self::RawJson,
            4 => Self::Assertions,
            5 => Self::Scripts,
            _ => Self::Messages,
        }
    }
}

/// AI API response inspector sub-tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AiRespTab {
    #[default]
    Merged,
    Events,
    Raw,
    Headers,
    Timing,
    Tests,
    Usage,
    Compare,
}

impl AiRespTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Merged => 0,
            Self::Events => 1,
            Self::Raw => 2,
            Self::Headers => 3,
            Self::Timing => 4,
            Self::Tests => 5,
            Self::Usage => 6,
            Self::Compare => 7,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Events,
            2 => Self::Raw,
            3 => Self::Headers,
            4 => Self::Timing,
            5 => Self::Tests,
            6 => Self::Usage,
            7 => Self::Compare,
            _ => Self::Merged,
        }
    }
}

/// Body mode selector for HTTP requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BodyKind {
    #[default]
    None,
    Json,
    Raw,
    Form,
    Multipart,
    Graphql,
    Binary,
}

impl BodyKind {
    #[must_use]
    pub const fn from_body(body: &RequestBody) -> Self {
        match body {
            RequestBody::None => Self::None,
            RequestBody::Json { .. } => Self::Json,
            RequestBody::RawText { .. } => Self::Raw,
            RequestBody::FormUrlEncoded { .. } => Self::Form,
            RequestBody::Multipart { .. } => Self::Multipart,
            RequestBody::Graphql { .. } => Self::Graphql,
            RequestBody::Binary { .. } => Self::Binary,
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::None => 0,
            Self::Json => 1,
            Self::Raw => 2,
            Self::Form => 3,
            Self::Multipart => 4,
            Self::Graphql => 5,
            Self::Binary => 6,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Json,
            2 => Self::Raw,
            3 => Self::Form,
            4 => Self::Multipart,
            5 => Self::Graphql,
            6 => Self::Binary,
            _ => Self::None,
        }
    }
}

/// All 8 core authentication schemes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthKind {
    #[default]
    Inherit,
    None,
    Bearer,
    Basic,
    ApiKey,
    OAuth2,
    Digest,
    AwsSigV4,
}

impl AuthKind {
    #[must_use]
    pub const fn from_auth(auth: &AuthConfig) -> Self {
        match auth {
            AuthConfig::Inherit => Self::Inherit,
            AuthConfig::None => Self::None,
            AuthConfig::Bearer { .. } => Self::Bearer,
            AuthConfig::Basic { .. } => Self::Basic,
            AuthConfig::ApiKey { .. } => Self::ApiKey,
            AuthConfig::OAuth2 { .. } => Self::OAuth2,
            AuthConfig::Digest { .. } => Self::Digest,
            AuthConfig::AwsSignatureV4 { .. } => Self::AwsSigV4,
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Inherit => 0,
            Self::None => 1,
            Self::Bearer => 2,
            Self::Basic => 3,
            Self::ApiKey => 4,
            Self::OAuth2 => 5,
            Self::Digest => 6,
            Self::AwsSigV4 => 7,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::None,
            2 => Self::Bearer,
            3 => Self::Basic,
            4 => Self::ApiKey,
            5 => Self::OAuth2,
            6 => Self::Digest,
            7 => Self::AwsSigV4,
            _ => Self::Inherit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScriptSubTab {
    Pre,
    #[default]
    Post,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NewItemKind {
    #[default]
    HttpGet,
    HttpPost,
    Graphql,
    AiApi,
    Collection,
    Folder,
}

impl NewItemKind {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::HttpGet => 0,
            Self::HttpPost => 1,
            Self::Graphql => 2,
            Self::AiApi => 3,
            Self::Collection => 4,
            Self::Folder => 5,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::HttpPost,
            2 => Self::Graphql,
            3 => Self::AiApi,
            4 => Self::Collection,
            5 => Self::Folder,
            _ => Self::HttpGet,
        }
    }
}
