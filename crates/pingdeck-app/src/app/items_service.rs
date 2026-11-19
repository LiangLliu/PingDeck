//! TLS certificate use-cases.
//!
//! Single responsibility: CRUD for the workspace's TLS client certificates. Mock servers and flows
//! have their own services ([`super::mock_service`], [`super::flow_service`]). A key passphrase
//! lives in the secret store (`tls.<workspace>.<certificate>.passphrase`); the webview only sees
//! whether one is set and can replace or clear it.

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::workspace::WorkspaceId;
use crate::workspace::models::{TlsCertificate, TlsCertificateId, TlsCertificateKind};

use super::{AppState, command_error, deleted_or_not_found, ensure_workspace_exists};

fn enabled_by_default() -> bool {
    true
}

/// A TLS certificate as the webview reads and saves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsCertificateDto {
    /// Certificate id.
    pub id: TlsCertificateId,
    /// Owning workspace.
    pub workspace_id: WorkspaceId,
    /// CA or client certificate.
    pub kind: TlsCertificateKind,
    /// Empty or `*` for a CA; `api.example.com` or `*.internal.corp:8443` for a client certificate.
    pub host_pattern: String,
    /// Path of the certificate file.
    pub cert_path: String,
    /// Path of the private key (client certificates).
    #[serde(default)]
    pub key_path: Option<String>,
    /// Whether a key passphrase is stored. Ignored on save: send `passphrase` to change it.
    #[serde(default)]
    pub has_passphrase: bool,
    /// Write-only: `None` keeps the stored passphrase, `Some("")` clears it, any other text
    /// replaces it. Never sent back.
    #[serde(default, skip_serializing)]
    pub passphrase: Option<String>,
    /// Whether the certificate is used.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

impl From<&TlsCertificate> for TlsCertificateDto {
    fn from(certificate: &TlsCertificate) -> Self {
        Self {
            id: certificate.id.clone(),
            workspace_id: certificate.workspace_id.clone(),
            kind: certificate.kind,
            host_pattern: certificate.host_pattern.clone(),
            cert_path: certificate.cert_path.clone(),
            key_path: certificate.key_path.clone(),
            has_passphrase: certificate.passphrase_ref.is_some(),
            passphrase: None,
            enabled: certificate.enabled,
        }
    }
}

/// The secret-store key of a certificate's key passphrase.
fn passphrase_secret_ref(workspace_id: &WorkspaceId, certificate_id: &TlsCertificateId) -> String {
    format!(
        "tls.{}.{}.passphrase",
        workspace_id.as_str(),
        certificate_id.as_str()
    )
}

/// Lists the TLS certificates of a workspace.
pub fn certificate_list_impl(
    state: &AppState,
    workspace_id: String,
) -> Result<Vec<TlsCertificateDto>, AppError> {
    let store = state.store();
    Ok(store
        .list_tls_certificates(&workspace_id)
        .map_err(command_error)?
        .iter()
        .map(TlsCertificateDto::from)
        .collect())
}

/// Validates and persists a TLS certificate in an existing workspace, applying the write-only
/// passphrase (see [`TlsCertificateDto::passphrase`]).
pub fn certificate_save_impl(
    state: &AppState,
    certificate: TlsCertificateDto,
) -> Result<TlsCertificateDto, AppError> {
    // serde skips the id constructors, so a blank or padded id must be rejected/trimmed here.
    let id = TlsCertificateId::new(certificate.id.as_str()).map_err(command_error)?;
    let workspace_id =
        WorkspaceId::new(certificate.workspace_id.as_str()).map_err(command_error)?;
    let mut model = TlsCertificate {
        id,
        workspace_id,
        kind: certificate.kind,
        host_pattern: certificate.host_pattern,
        cert_path: certificate.cert_path,
        key_path: certificate.key_path,
        passphrase_ref: None,
        enabled: certificate.enabled,
    };
    model.validate().map_err(command_error)?;
    let secret_ref = passphrase_secret_ref(&model.workspace_id, &model.id);
    let previous = {
        let store = state.store();
        ensure_workspace_exists(&store, model.workspace_id.as_str())?;
        store
            .list_tls_certificates(model.workspace_id.as_str())
            .map_err(command_error)?
            .into_iter()
            .find(|stored| stored.id == model.id)
    };
    let passphrase = certificate.passphrase.as_deref();
    model.passphrase_ref = match passphrase {
        None => previous
            .as_ref()
            .and_then(|stored| stored.passphrase_ref.clone()),
        Some("") => None,
        Some(_) => Some(secret_ref.clone()),
    };
    // The row goes first: a rejected save (a cross-workspace id clash) leaves no orphan secret.
    state
        .store()
        .save_tls_certificate(&model)
        .map_err(command_error)?;
    // The secret store shares the store lock: touch it only once the lock is released.
    let applied = match passphrase {
        None => Ok(()),
        Some("") => state.secrets().delete(&secret_ref).map(|_| ()),
        Some(passphrase) => state.secrets().put(&secret_ref, passphrase).map(|_| ()),
    };
    if let Err(error) = applied {
        // Undo the row so it never names a passphrase that was not stored.
        let store = state.store();
        let _ = match previous {
            Some(stored) => store.save_tls_certificate(&stored),
            None => store
                .delete_tls_certificate(model.id.as_str(), model.workspace_id.as_str())
                .map(|_| ()),
        };
        return Err(error);
    }

    Ok(TlsCertificateDto::from(&model))
}

/// Deletes a TLS certificate, failing with not-found if it is absent from the workspace.
pub fn certificate_delete_impl(
    state: &AppState,
    workspace_id: String,
    certificate_id: String,
) -> Result<(), AppError> {
    let certificate_id = TlsCertificateId::new(&certificate_id).map_err(command_error)?;
    let deleted = state
        .store()
        .delete_tls_certificate(certificate_id.as_str(), &workspace_id)
        .map_err(command_error)?;
    if deleted && let Ok(workspace) = WorkspaceId::new(&workspace_id) {
        state
            .secrets()
            .delete(&passphrase_secret_ref(&workspace, &certificate_id))?;
    }

    deleted_or_not_found(
        deleted,
        "certificate",
        certificate_id.as_str(),
        &workspace_id,
    )
}
