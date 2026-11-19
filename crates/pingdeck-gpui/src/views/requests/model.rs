//! Tab, authentication mode, and open-tab state models for `RequestsView`.

use pingdeck_desktop::core::model::HttpMethod;

pub const ALL_METHODS: [HttpMethod; 7] = [
    HttpMethod::Get,
    HttpMethod::Post,
    HttpMethod::Put,
    HttpMethod::Patch,
    HttpMethod::Delete,
    HttpMethod::Head,
    HttpMethod::Options,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReqTab {
    Params,
    Headers,
    Auth,
    #[default]
    Body,
    Scripts,
    Snippets,
}

impl ReqTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Params => 0,
            Self::Headers => 1,
            Self::Auth => 2,
            Self::Body => 3,
            Self::Scripts => 4,
            Self::Snippets => 5,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            0 => Self::Params,
            1 => Self::Headers,
            2 => Self::Auth,
            4 => Self::Scripts,
            5 => Self::Snippets,
            _ => Self::Body,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RespTab {
    #[default]
    Body,
    Headers,
    Cookies,
    Summary,
}

impl RespTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Body => 0,
            Self::Headers => 1,
            Self::Cookies => 2,
            Self::Summary => 3,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Headers,
            2 => Self::Cookies,
            3 => Self::Summary,
            _ => Self::Body,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    #[default]
    None,
    Bearer,
    Basic,
    ApiKey,
}

impl AuthMode {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::None => 0,
            Self::Bearer => 1,
            Self::Basic => 2,
            Self::ApiKey => 3,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Bearer,
            2 => Self::Basic,
            3 => Self::ApiKey,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenReqTab {
    pub id: String,
    pub name: String,
    pub method: HttpMethod,
}
