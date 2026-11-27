//! Cookie jar persistence: upserts, imports, key-aware edits, and expiry cleanup.

use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, Params, Row, params};

use spanbay_model::models::{CookieJarEntry, CookieJarEntryId, RequestId, WorkspaceId};
use spanbay_model::validation::validate_id;

use crate::sqlite::i64_from_usize;
use crate::storage::{StorageError, WorkspaceStore};

/// Cookies kept per workspace; saving beyond it evicts the least recently updated (W-34).
pub const COOKIE_JAR_MAX_ROWS: usize = 3000;

pub use spanbay_model::models::COOKIE_MAX_BYTES;

/// The `cookie_jar` columns, in the order [`cookie_params`] binds and [`cookie_row`] reads them.
macro_rules! cookie_columns {
    () => {
        "id, workspace_id, source_request_id, name, value, domain, path, host_only, expires, \
         max_age, secure, http_only, same_site, created_at, updated_at"
    };
}

/// `$verb` (`INSERT` or `INSERT OR IGNORE`) of one whole row, followed by `$conflict`.
macro_rules! insert_cookie {
    ($verb:literal, $conflict:literal) => {
        concat!(
            $verb,
            " INTO cookie_jar (
                ",
            cookie_columns!(),
            "
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            $conflict
        )
    };
}

/// Restore: an id that exists again is skipped.
const RESTORE_SQL: &str = insert_cookie!("INSERT OR IGNORE", "");

/// Import: the imported row replaces the one with its key, timestamps included.
const IMPORT_SQL: &str = insert_cookie!(
    "INSERT",
    "
    ON CONFLICT(workspace_id, domain, path, name) DO UPDATE SET
        id = excluded.id,
        source_request_id = excluded.source_request_id,
        value = excluded.value,
        host_only = excluded.host_only,
        expires = excluded.expires,
        max_age = excluded.max_age,
        secure = excluded.secure,
        http_only = excluded.http_only,
        same_site = excluded.same_site,
        created_at = excluded.created_at,
        updated_at = excluded.updated_at"
);

/// Upsert from a response: like the import, but the row keeps its first `created_at`.
const UPSERT_SQL: &str = insert_cookie!(
    "INSERT",
    "
    ON CONFLICT(workspace_id, domain, path, name) DO UPDATE SET
        id = excluded.id,
        source_request_id = excluded.source_request_id,
        value = excluded.value,
        host_only = excluded.host_only,
        expires = excluded.expires,
        max_age = excluded.max_age,
        secure = excluded.secure,
        http_only = excluded.http_only,
        same_site = excluded.same_site,
        updated_at = excluded.updated_at"
);

const LIST_SQL: &str = concat!(
    "SELECT ",
    cookie_columns!(),
    " FROM cookie_jar WHERE workspace_id = ?1 ORDER BY domain ASC, path ASC, name ASC"
);

const LOAD_SQL: &str = concat!(
    "SELECT ",
    cookie_columns!(),
    " FROM cookie_jar WHERE workspace_id = ?1 AND id = ?2"
);

const DELETE_ONE_SQL: &str = "DELETE FROM cookie_jar WHERE workspace_id = ?1 AND id = ?2";

impl WorkspaceStore {
    /// Re-inserts previously deleted cookie-jar entries, skipping ids that already exist.
    pub fn restore_cookie_jar_entries(
        &self,
        entries: &[CookieJarEntry],
    ) -> Result<Vec<CookieJarEntryId>, StorageError> {
        self.atomic(|transaction| {
            let mut insert = transaction.prepare(RESTORE_SQL)?;
            let mut restored_ids = Vec::new();
            for entry in entries {
                if insert.execute(cookie_params(entry))? > 0 {
                    restored_ids.push(entry.id.clone());
                }
            }
            Ok(restored_ids)
        })
    }

    /// Upserts a cookie-jar entry keyed by workspace, domain, path, and name.
    #[cfg(any(test, feature = "test-support"))]
    pub fn save_cookie_jar_entry(&self, entry: &CookieJarEntry) -> Result<(), StorageError> {
        upsert_cookie(&self.connection, entry)
    }

    /// Upserts the cookies a response set into `workspace_id` in one transaction, then evicts
    /// the least recently updated rows beyond [`COOKIE_JAR_MAX_ROWS`]; the workspace's expired
    /// cookies are deleted afterwards, in a second transaction (a failure there keeps the upserts).
    ///
    /// A cookie whose name, value, domain and path exceed [`COOKIE_MAX_BYTES`] is dropped, as
    /// RFC 6265 allows; an entry of another workspace is ignored.
    pub fn save_cookie_jar_entries(
        &self,
        workspace_id: &str,
        entries: &[CookieJarEntry],
    ) -> Result<(), StorageError> {
        self.atomic(|transaction| {
            for entry in entries.iter().filter(|entry| {
                entry.workspace_id.as_str() == workspace_id
                    && entry.name.len() + entry.value.len() + entry.domain.len() + entry.path.len()
                        <= COOKIE_MAX_BYTES
            }) {
                upsert_cookie(transaction, entry)?;
            }
            transaction.execute(
                "
                DELETE FROM cookie_jar
                WHERE workspace_id = ?1
                  AND id NOT IN (
                      SELECT id FROM cookie_jar WHERE workspace_id = ?1
                      ORDER BY updated_at DESC, id DESC
                      LIMIT ?2
                  )
                ",
                params![workspace_id, i64_from_usize(COOKIE_JAR_MAX_ROWS)],
            )?;
            Ok(())
        })?;
        // A response can also expire cookies (`Max-Age=0`); drop what is dead now.
        self.delete_expired_cookie_jar_entries(workspace_id, SystemTime::now())?;

        Ok(())
    }

    /// Upserts imported cookie-jar entries in one transaction, preserving their timestamps.
    pub fn import_cookie_jar_entries(
        &self,
        entries: &[CookieJarEntry],
    ) -> Result<Vec<CookieJarEntryId>, StorageError> {
        self.atomic(|transaction| {
            let mut upsert = transaction.prepare(IMPORT_SQL)?;
            let mut imported_ids = Vec::new();
            for entry in entries {
                upsert.execute(cookie_params(entry))?;
                imported_ids.push(entry.id.clone());
            }
            Ok(imported_ids)
        })
    }

    /// Sets a cookie's value; returns the updated entry, or `None` if it does not exist in
    /// `workspace_id` (a cookie of another workspace is never touched).
    pub fn update_cookie_jar_entry_value(
        &self,
        workspace_id: &str,
        id: &CookieJarEntryId,
        value: &str,
        updated_at: &str,
    ) -> Result<Option<CookieJarEntry>, StorageError> {
        self.update_then_load(
            workspace_id,
            id,
            "UPDATE cookie_jar SET value = ?3, updated_at = ?4 WHERE workspace_id = ?1 AND id = ?2",
            params![workspace_id, id.as_str(), value, updated_at],
        )
    }

    /// Sets a cookie's Secure, HttpOnly, and SameSite flags; returns the updated entry.
    pub fn update_cookie_jar_entry_flags(
        &self,
        workspace_id: &str,
        id: &CookieJarEntryId,
        secure: bool,
        http_only: bool,
        same_site: Option<&str>,
        updated_at: &str,
    ) -> Result<Option<CookieJarEntry>, StorageError> {
        self.update_then_load(
            workspace_id,
            id,
            "
            UPDATE cookie_jar
            SET secure = ?3, http_only = ?4, same_site = ?5, updated_at = ?6
            WHERE workspace_id = ?1 AND id = ?2
            ",
            params![
                workspace_id,
                id.as_str(),
                secure,
                http_only,
                same_site,
                updated_at
            ],
        )
    }

    /// Sets a cookie's `Expires` and `Max-Age` values; returns the updated entry.
    pub fn update_cookie_jar_entry_expiry(
        &self,
        workspace_id: &str,
        id: &CookieJarEntryId,
        expires: Option<&str>,
        max_age: Option<&str>,
        updated_at: &str,
    ) -> Result<Option<CookieJarEntry>, StorageError> {
        self.update_then_load(
            workspace_id,
            id,
            "
            UPDATE cookie_jar
            SET expires = ?3, max_age = ?4, updated_at = ?5
            WHERE workspace_id = ?1 AND id = ?2
            ",
            params![workspace_id, id.as_str(), expires, max_age, updated_at],
        )
    }

    /// Sets whether a cookie is host-only; returns the updated entry.
    pub fn update_cookie_jar_entry_scope(
        &self,
        workspace_id: &str,
        id: &CookieJarEntryId,
        host_only: bool,
        updated_at: &str,
    ) -> Result<Option<CookieJarEntry>, StorageError> {
        self.update_then_load(
            workspace_id,
            id,
            "
            UPDATE cookie_jar
            SET host_only = ?3, updated_at = ?4
            WHERE workspace_id = ?1 AND id = ?2
            ",
            params![workspace_id, id.as_str(), host_only, updated_at],
        )
    }

    /// Renames a cookie's name, domain, and path, re-deriving its id; returns the updated entry.
    /// A key another cookie of the workspace has fails on the unique index and changes nothing.
    pub fn update_cookie_jar_entry_key(
        &self,
        workspace_id: &str,
        id: &CookieJarEntryId,
        name: &str,
        domain: &str,
        path: &str,
        updated_at: &str,
    ) -> Result<Option<CookieJarEntry>, StorageError> {
        let Some(existing) = self.load_cookie_jar_entry_by_id(workspace_id, id)? else {
            return Ok(None);
        };
        let (name, domain, path) = normalize_cookie_key_fields(name, domain, path)?;
        let new_id = cookie_jar_entry_id_for_key(&existing.workspace_id, &domain, &path, &name)?;

        self.connection.execute(
            "
            UPDATE cookie_jar
            SET id = ?3, name = ?4, domain = ?5, path = ?6, updated_at = ?7
            WHERE workspace_id = ?1 AND id = ?2
            ",
            params![
                workspace_id,
                id.as_str(),
                new_id.as_str(),
                name,
                domain,
                path,
                updated_at
            ],
        )?;

        self.load_cookie_jar_entry_by_id(workspace_id, &new_id)
    }

    /// Lists a workspace's cookie-jar entries ordered by domain, path, and name.
    pub fn list_cookie_jar(&self, workspace_id: &str) -> Result<Vec<CookieJarEntry>, StorageError> {
        let mut statement = self.connection.prepare_cached(LIST_SQL)?;
        let rows = statement.query_map(params![workspace_id], cookie_row)?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?.into_domain()?);
        }

        Ok(entries)
    }

    /// Runs one `UPDATE` of the cookie `id` of `workspace_id` and reloads it; `None` when no row
    /// matched.
    fn update_then_load(
        &self,
        workspace_id: &str,
        id: &CookieJarEntryId,
        sql: &'static str,
        params: impl Params,
    ) -> Result<Option<CookieJarEntry>, StorageError> {
        if self.connection.execute(sql, params)? == 0 {
            return Ok(None);
        }
        self.load_cookie_jar_entry_by_id(workspace_id, id)
    }

    fn load_cookie_jar_entry_by_id(
        &self,
        workspace_id: &str,
        id: &CookieJarEntryId,
    ) -> Result<Option<CookieJarEntry>, StorageError> {
        self.connection
            .query_row(LOAD_SQL, params![workspace_id, id.as_str()], cookie_row)
            .optional()?
            .map(StoredCookieJarEntry::into_domain)
            .transpose()
    }

    /// Deletes one cookie-jar entry of `workspace_id`; returns whether it existed there.
    #[cfg(test)]
    pub(crate) fn delete_cookie_jar_entry(
        &self,
        workspace_id: &str,
        id: &CookieJarEntryId,
    ) -> Result<bool, StorageError> {
        Ok(self
            .connection
            .execute(DELETE_ONE_SQL, params![workspace_id, id.as_str()])?
            > 0)
    }

    /// Deletes the given cookie-jar entries of `workspace_id` in one transaction and returns
    /// the ids removed; ids unknown or owned by another workspace are skipped and left intact.
    pub fn delete_cookie_jar_entries(
        &self,
        workspace_id: &str,
        ids: &[CookieJarEntryId],
    ) -> Result<Vec<CookieJarEntryId>, StorageError> {
        self.atomic(|transaction| {
            let mut delete = transaction.prepare(DELETE_ONE_SQL)?;
            let mut deleted_ids = Vec::new();
            for id in ids {
                if delete.execute(params![workspace_id, id.as_str()])? > 0 {
                    deleted_ids.push(id.clone());
                }
            }
            Ok(deleted_ids)
        })
    }

    /// Deletes a workspace's cookies that are expired at `now` and returns their ids. The read
    /// and the deletes share one transaction, so a cookie refreshed meanwhile is never deleted.
    pub fn delete_expired_cookie_jar_entries(
        &self,
        workspace_id: &str,
        now: SystemTime,
    ) -> Result<Vec<CookieJarEntryId>, StorageError> {
        self.atomic(|transaction| {
            let expired_ids = self
                .list_cookie_jar(workspace_id)?
                .into_iter()
                .filter(|entry| entry.is_expired_at(now))
                .map(|entry| entry.id)
                .collect::<Vec<_>>();
            let mut delete = transaction.prepare(DELETE_ONE_SQL)?;
            for id in &expired_ids {
                delete.execute(params![workspace_id, id.as_str()])?;
            }
            Ok(expired_ids)
        })
    }
}

/// Upserts one response cookie; cached, as every response cookie goes through it.
fn upsert_cookie(connection: &Connection, entry: &CookieJarEntry) -> Result<(), StorageError> {
    connection
        .prepare_cached(UPSERT_SQL)?
        .execute(cookie_params(entry))?;
    Ok(())
}

/// The bind values of a whole row, in `cookie_columns!` order (a tuple, as `Params` takes one).
type CookieParams<'a> = (
    &'a str,
    &'a str,
    Option<&'a str>,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    bool,
    Option<&'a str>,
    Option<&'a str>,
    bool,
    bool,
    Option<&'a str>,
    &'a str,
    &'a str,
);

fn cookie_params(entry: &CookieJarEntry) -> CookieParams<'_> {
    (
        entry.id.as_str(),
        entry.workspace_id.as_str(),
        entry.source_request_id.as_ref().map(RequestId::as_str),
        &entry.name,
        &entry.value,
        &entry.domain,
        &entry.path,
        entry.host_only,
        entry.expires.as_deref(),
        entry.max_age.as_deref(),
        entry.secure,
        entry.http_only,
        entry.same_site.as_deref(),
        &entry.created_at,
        &entry.updated_at,
    )
}

/// Reads a row selected with `cookie_columns!`.
fn cookie_row(row: &Row<'_>) -> rusqlite::Result<StoredCookieJarEntry> {
    Ok(StoredCookieJarEntry {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        source_request_id: row.get(2)?,
        name: row.get(3)?,
        value: row.get(4)?,
        domain: row.get(5)?,
        path: row.get(6)?,
        host_only: row.get(7)?,
        expires: row.get(8)?,
        max_age: row.get(9)?,
        secure: row.get(10)?,
        http_only: row.get(11)?,
        same_site: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

struct StoredCookieJarEntry {
    id: String,
    workspace_id: String,
    source_request_id: Option<String>,
    name: String,
    value: String,
    domain: String,
    path: String,
    host_only: bool,
    expires: Option<String>,
    max_age: Option<String>,
    secure: bool,
    http_only: bool,
    same_site: Option<String>,
    created_at: String,
    updated_at: String,
}

impl StoredCookieJarEntry {
    fn into_domain(self) -> Result<CookieJarEntry, StorageError> {
        Ok(CookieJarEntry {
            id: CookieJarEntryId::new(&self.id)?,
            workspace_id: WorkspaceId::new(&self.workspace_id)?,
            source_request_id: self.source_request_id.map(RequestId::new).transpose()?,
            name: self.name,
            value: self.value,
            domain: self.domain,
            path: self.path,
            host_only: self.host_only,
            expires: self.expires,
            max_age: self.max_age,
            secure: self.secure,
            http_only: self.http_only,
            same_site: self.same_site,
            created_at: self.created_at,
            updated_at: self.updated_at,
            expired: false,
        })
    }
}

fn normalize_cookie_key_fields(
    name: &str,
    domain: &str,
    path: &str,
) -> Result<(String, String, String), StorageError> {
    let name = validate_id("cookie name", name)?;
    let domain = validate_id(
        "cookie domain",
        &validate_id("cookie domain", domain)?
            .trim_start_matches('.')
            .to_ascii_lowercase(),
    )?;
    let path = validate_id("cookie path", path)?;
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };

    Ok((name, domain, path))
}

fn cookie_jar_entry_id_for_key(
    workspace_id: &WorkspaceId,
    domain: &str,
    path: &str,
    name: &str,
) -> Result<CookieJarEntryId, StorageError> {
    Ok(CookieJarEntryId::new(format!(
        "cookie:{}:{}:{}:{}",
        workspace_id.as_str(),
        domain,
        path,
        name
    ))?)
}
