//! What an `Inherit` auth resolves to, where it comes from, and the credential preview.

use serde::{Deserialize, Serialize};

use super::url_sync::map_outside_tokens;
use crate::app::{AppState, command_error};
use crate::error::AppError;
use crate::workspace::models::{
    ApiKeyLocation, AuthConfig, Collection, Folder, Request, RequestId, WorkspaceId,
};
use crate::workspace::{effective_auth, folder_chain};

/// Masks a literal credential in a preview; the same bullets the executor's trace uses.
const MASKED: &str = "••••••";

/// The request whose auth to resolve: a saved request, or the context of an unsaved draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectiveAuthInput {
    pub workspace_id: String,
    /// A saved request: its stored collection, folder and auth are used and the three fields
    /// below are ignored.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Draft context: the collection the draft lives in (derived from `folder_id` when absent).
    #[serde(default)]
    pub collection_id: Option<String>,
    /// Draft context: the folder the draft lives in.
    #[serde(default)]
    pub folder_id: Option<String>,
    /// Draft context: the draft's own auth (`Inherit` when absent).
    #[serde(default)]
    pub auth: Option<AuthConfig>,
}

/// Where the effective auth was defined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthSource {
    /// The request itself chose its auth (anything but `Inherit`).
    Request,
    /// The nearest folder with a non-`Inherit` auth.
    Folder { id: String, name: String },
    /// The collection (its own `Inherit` means no auth).
    Collection { id: String, name: String },
    /// No parent defines auth: the request is sent without.
    None,
}

/// Where a previewed credential travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthPreviewLocation {
    Header,
    Query,
}

/// What the auth adds to the request, as shown next to the auth editor. Variable references
/// (`{{token}}`) are shown as written, literal secrets are masked, credentials computed at send
/// time (Basic, OAuth 2.0, Digest, SigV4) are described by a placeholder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthPreview {
    pub location: AuthPreviewLocation,
    /// Header name, or query parameter name.
    pub name: String,
    pub value: String,
}

/// The resolved auth of a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveAuth {
    /// The concrete config (never `Inherit`).
    pub config: AuthConfig,
    pub source: AuthSource,
    /// `None` for no auth.
    pub preview: Option<AuthPreview>,
}

fn not_found(input: &EffectiveAuthInput, id: &str) -> AppError {
    AppError::not_found(format!(
        "request {id} not found in workspace {}",
        input.workspace_id
    ))
}

/// Resolves a request's auth through its folder chain and collection.
///
/// A request id that is unknown (or in another workspace) is `NotFound`; for a draft, a missing
/// or foreign collection/folder is simply absent, so the answer is "no parent auth".
pub fn request_effective_auth(
    state: &AppState,
    input: EffectiveAuthInput,
) -> Result<EffectiveAuth, AppError> {
    let workspace_id = WorkspaceId::new(&input.workspace_id).map_err(command_error)?;
    let store = state.store();
    let request = match input.request_id.as_deref() {
        Some(id) => store
            .get_request(&workspace_id, &RequestId::new(id).map_err(command_error)?)
            .map_err(command_error)?
            .ok_or_else(|| not_found(&input, id))?,
        None => {
            let mut draft = Request::new("draft", workspace_id.as_str(), "draft", "http://draft")
                .map_err(command_error)?;
            if let Some(folder_id) = &input.folder_id {
                draft = draft.with_folder(folder_id).map_err(command_error)?;
            }
            if let Some(collection_id) = &input.collection_id {
                draft = draft
                    .with_collection(collection_id)
                    .map_err(command_error)?;
            }
            draft.auth = input.auth.clone().unwrap_or_default();
            draft
        }
    };

    let folder_collection = match &request.folder_id {
        Some(folder_id) => store
            .get_folder(&workspace_id, folder_id)
            .map_err(command_error)?
            .map(|folder| folder.collection_id),
        None => None,
    };
    let collection = match request.collection_id.clone().or(folder_collection) {
        Some(collection_id) => store
            .get_collection(&workspace_id, &collection_id)
            .map_err(command_error)?,
        None => None,
    };
    let folders = match &collection {
        Some(collection) => store
            .list_folders(&workspace_id, &collection.id)
            .map_err(command_error)?,
        None => Vec::new(),
    };
    Ok(resolve(&request, collection.as_ref(), &folders))
}

/// Config, source and preview from already loaded entities.
fn resolve(
    request: &Request,
    collection: Option<&Collection>,
    folders: &[Folder],
) -> EffectiveAuth {
    let config = effective_auth(request, collection, folders);
    let source = if !matches!(request.auth, AuthConfig::Inherit) {
        AuthSource::Request
    } else if let Some(folder) = folder_chain(request, folders)
        .folders
        .into_iter()
        .rev()
        .find(|folder| !matches!(folder.auth, AuthConfig::Inherit))
    {
        AuthSource::Folder {
            id: folder.id.as_str().to_owned(),
            name: folder.name,
        }
    } else if let Some(collection) = collection {
        AuthSource::Collection {
            id: collection.id.as_str().to_owned(),
            name: collection.name.clone(),
        }
    } else {
        AuthSource::None
    };
    let preview = auth_preview(&config);
    EffectiveAuth {
        config,
        source,
        preview,
    }
}

/// Trimmed `value`, `placeholder` when blank. Every literal stretch outside `{{variable}}`
/// tokens is masked (one mask per stretch, so `my-secret-{{env}}` shows `••••••{{env}}`); the
/// tokens stay readable and whitespace between them is not a secret.
fn shown(value: &str, placeholder: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return placeholder.to_owned();
    }
    map_outside_tokens(value, |literal, out| {
        if literal.trim().is_empty() {
            out.push_str(literal);
        } else {
            out.push_str(MASKED);
        }
    })
}

fn authorization(value: impl Into<String>) -> AuthPreview {
    AuthPreview {
        location: AuthPreviewLocation::Header,
        name: "Authorization".to_owned(),
        value: value.into(),
    }
}

/// The credential `auth` adds to a request, for display; `None` when it adds nothing.
fn auth_preview(auth: &AuthConfig) -> Option<AuthPreview> {
    Some(match auth {
        AuthConfig::Inherit | AuthConfig::None => return None,
        AuthConfig::Bearer { token_ref } => {
            authorization(format!("Bearer {}", shown(token_ref, "{{token}}")))
        }
        AuthConfig::Basic { .. } => authorization("Basic <base64(username:password)>"),
        AuthConfig::ApiKey {
            key,
            value_ref,
            location,
        } => {
            let (location, default_name) = match location {
                ApiKeyLocation::Header => (AuthPreviewLocation::Header, "X-API-Key"),
                ApiKeyLocation::Query => (AuthPreviewLocation::Query, "api_key"),
            };
            AuthPreview {
                location,
                name: Some(key.trim())
                    .filter(|key| !key.is_empty())
                    .unwrap_or(default_name)
                    .to_owned(),
                value: shown(value_ref, "{{apiKey}}"),
            }
        }
        AuthConfig::OAuth2 { .. } => authorization("Bearer <access token>"),
        AuthConfig::Digest { .. } => {
            authorization("Digest username=\"…\", realm=\"…\", nonce=\"…\", response=\"…\"")
        }
        AuthConfig::AwsSignatureV4 { .. } => authorization(
            "AWS4-HMAC-SHA256 Credential=<access key>/<date>/<region>/<service>/aws4_request",
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_mask_literals_and_keep_references() {
        let bearer = |token: &str| AuthConfig::Bearer {
            token_ref: token.to_owned(),
        };
        let value = |auth: AuthConfig| auth_preview(&auth).map(|preview| preview.value);
        assert_eq!(value(bearer("{{t}}")).as_deref(), Some("Bearer {{t}}"));
        assert_eq!(value(bearer("  ")).as_deref(), Some("Bearer {{token}}"));
        assert_eq!(value(bearer("abc")).as_deref(), Some("Bearer ••••••"));
        assert_eq!(value(AuthConfig::None), None);
    }

    #[test]
    fn literals_next_to_variables_are_masked_and_the_variables_stay_readable() {
        let preview = |token: &str| {
            auth_preview(&AuthConfig::Bearer {
                token_ref: token.to_owned(),
            })
            .map(|preview| preview.value)
        };
        assert_eq!(
            preview("my-secret-{{env}}").as_deref(),
            Some("Bearer ••••••{{env}}")
        );
        assert_eq!(
            preview("{{a}}hunter2{{b}}").as_deref(),
            Some("Bearer {{a}}••••••{{b}}")
        );
        assert_eq!(
            preview("{{a}} {{b}}").as_deref(),
            Some("Bearer {{a}} {{b}}")
        );
        // An unclosed or empty token is not a variable: everything is literal.
        assert_eq!(preview("secret{{").as_deref(), Some("Bearer ••••••"));
        assert_eq!(preview("pw{{}}").as_deref(), Some("Bearer ••••••"));
        assert!(
            !preview("my-secret-{{env}}")
                .unwrap_or_default()
                .contains("secret")
        );
    }
}
