//! TLS certificate rows (CA and client); a key passphrase lives in the secret store under
//! `passphrase_ref`.

use rusqlite::params;

use spanbay_model::models::{TlsCertificate, TlsCertificateId, TlsCertificateKind, WorkspaceId};

use crate::storage::{StorageError, WorkspaceStore};

impl WorkspaceStore {
    /// Upserts a TLS client certificate, rejecting an id owned by another workspace.
    pub fn save_tls_certificate(&self, certificate: &TlsCertificate) -> Result<(), StorageError> {
        self.atomic(|transaction| {
            self.ensure_item_workspace(
                "tls_certificates",
                "certificate",
                certificate.id.as_str(),
                certificate.workspace_id.as_str(),
            )?;
            transaction.execute(
                "
                INSERT INTO tls_certificates (
                    id, workspace_id, kind, host_pattern, cert_path, key_path, passphrase_ref, enabled
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT(id) DO UPDATE SET
                    workspace_id = excluded.workspace_id,
                    kind = excluded.kind,
                    host_pattern = excluded.host_pattern,
                    cert_path = excluded.cert_path,
                    key_path = excluded.key_path,
                    passphrase_ref = excluded.passphrase_ref,
                    enabled = excluded.enabled
                ",
                params![
                    certificate.id.as_str(),
                    certificate.workspace_id.as_str(),
                    tls_certificate_kind_to_storage(certificate.kind),
                    certificate.host_pattern,
                    certificate.cert_path,
                    certificate.key_path,
                    certificate.passphrase_ref,
                    certificate.enabled,
                ],
            )?;
            Ok(())
        })
    }

    /// Lists a workspace's TLS certificates ordered by kind, host pattern, and id.
    pub fn list_tls_certificates(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<TlsCertificate>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "
            SELECT id, workspace_id, kind, host_pattern, cert_path, key_path, passphrase_ref, enabled
            FROM tls_certificates
            WHERE workspace_id = ?1
            ORDER BY kind ASC, host_pattern ASC, id ASC
            ",
        )?;
        let rows = statement.query_map(params![workspace_id], stored_tls_certificate_row)?;

        let mut certificates = Vec::new();
        for row in rows {
            certificates.push(row?.into_domain()?);
        }
        Ok(certificates)
    }

    /// Deletes a TLS certificate scoped to its workspace; returns whether a row was removed.
    pub fn delete_tls_certificate(
        &self,
        id: &str,
        workspace_id: &str,
    ) -> Result<bool, StorageError> {
        let deleted = self.connection.execute(
            "DELETE FROM tls_certificates WHERE id = ?1 AND workspace_id = ?2",
            params![id, workspace_id],
        )?;

        Ok(deleted > 0)
    }
}

struct StoredTlsCertificateRow {
    id: String,
    workspace_id: String,
    kind: String,
    host_pattern: String,
    cert_path: String,
    key_path: Option<String>,
    passphrase_ref: Option<String>,
    enabled: bool,
}

fn stored_tls_certificate_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<StoredTlsCertificateRow> {
    Ok(StoredTlsCertificateRow {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        kind: row.get(2)?,
        host_pattern: row.get(3)?,
        cert_path: row.get(4)?,
        key_path: row.get(5)?,
        passphrase_ref: row.get(6)?,
        enabled: row.get(7)?,
    })
}

impl StoredTlsCertificateRow {
    fn into_domain(self) -> Result<TlsCertificate, StorageError> {
        Ok(TlsCertificate {
            id: TlsCertificateId::new(self.id)?,
            workspace_id: WorkspaceId::new(self.workspace_id)?,
            kind: tls_certificate_kind_from_storage(&self.kind).ok_or_else(|| {
                StorageError::InvalidStoredValue {
                    table: "tls_certificates",
                    column: "kind",
                    value: self.kind,
                }
            })?,
            host_pattern: self.host_pattern,
            cert_path: self.cert_path,
            key_path: self.key_path,
            passphrase_ref: self.passphrase_ref,
            enabled: self.enabled,
        })
    }
}

fn tls_certificate_kind_to_storage(kind: TlsCertificateKind) -> &'static str {
    match kind {
        TlsCertificateKind::Ca => "ca",
        TlsCertificateKind::Client => "client",
    }
}

fn tls_certificate_kind_from_storage(value: &str) -> Option<TlsCertificateKind> {
    match value {
        "ca" => Some(TlsCertificateKind::Ca),
        "client" => Some(TlsCertificateKind::Client),
        _ => None,
    }
}
