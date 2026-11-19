//! Environment, environment-variable, and global-variable persistence.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::OnceLock;

use rusqlite::{Connection, OptionalExtension, params};

use pingdeck_base::time::now_ms;
use pingdeck_model::models::{
    Collection, Environment, EnvironmentId, EnvironmentVariable, WorkspaceId,
};

use crate::sqlite::i64_from_usize;
use crate::storage::{StorageError, WorkspaceStore, secrets};

/// Opaque version of saved variable rows plus `head` (the owner's other saved fields).
///
/// Keyed with a per-process random key, so the value says nothing about secret values to the
/// webview; it only has to be equal for equal rows within one run. Covers every field a save
/// writes: id, key, value, flags and note, in order.
fn revision_of(head: impl Hash, variables: &[EnvironmentVariable]) -> String {
    static KEY: OnceLock<RandomState> = OnceLock::new();
    let mut hasher = KEY.get_or_init(RandomState::new).build_hasher();
    head.hash(&mut hasher);
    variables.len().hash(&mut hasher);
    for variable in variables {
        (
            &variable.id,
            &variable.key,
            &variable.value,
            variable.is_secret,
            variable.enabled,
            &variable.note,
        )
            .hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// The revision of a saved environment: its name, guard flags, declared hosts and variables.
#[must_use]
pub fn environment_revision(environment: &Environment) -> String {
    revision_of(
        (
            &environment.name,
            environment.confirm_before_send,
            environment.confirm_safe_methods,
            &environment.declared_hosts,
        ),
        &environment.variables,
    )
}

/// The revision of a saved collection: every field a save writes except its position, so a
/// reorder does not make an open editor stale. Keyed per process like the environment's (its
/// variables hold secret values).
#[must_use]
pub fn collection_revision(collection: &Collection) -> String {
    revision_of(
        (
            &collection.name,
            &collection.description,
            serde_json::to_string(&collection.auth).unwrap_or_default(),
            &collection.pre_request_script,
            &collection.test_script,
        ),
        &collection.variables,
    )
}

/// The revision of a workspace's saved global variables.
#[must_use]
pub fn globals_revision(variables: &[EnvironmentVariable]) -> String {
    revision_of((), variables)
}

/// SQL for a variable's value as the domain sees it: a secret variable's column is blank and its
/// value is the `secrets` row joined as `s` (blank when the row is gone); a NULL stays NULL.
const ENV_VALUE: &str =
    "CASE WHEN v.is_secret = 1 AND v.value = '' THEN COALESCE(s.value_text, '') ELSE v.value END";

/// The `environments` columns [`environment_row`] reads; callers append `WHERE`/`ORDER BY`.
const SELECT_ENVIRONMENT: &str = "SELECT id, workspace_id, name, confirm_before_send, confirm_safe_methods, declared_hosts_json, guard_changed_at_ms FROM environments";

/// Inserts one global variable: workspace, position, key, value, flags, note, id.
const INSERT_GLOBAL_VARIABLE: &str = "
    INSERT INTO global_variables (
        workspace_id, position, key, value, is_secret, enabled, note, variable_id
    )
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
";

/// Inserts one environment variable: environment, position, key, value, flags, note, id.
const INSERT_ENVIRONMENT_VARIABLE: &str = "
    INSERT INTO environment_variables (
        environment_id, position, key, value, is_secret, enabled, note, variable_id
    )
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
";

/// Secret-store key of a secret environment variable. The SQL here builds the same key as
/// `'env.' || environment_id || '.' || key`; the two must stay in step.
fn env_secret_ref(environment_id: &str, key: &str) -> String {
    format!("env.{environment_id}.{key}")
}

/// Secret-store key of a secret global variable; the SQL builds it as
/// `'global.' || workspace_id || '.' || key`.
fn global_secret_ref(workspace_id: &str, key: &str) -> String {
    format!("global.{workspace_id}.{key}")
}

/// What goes into the `value` column of `variable`: a non-empty secret value is moved into the
/// secret store under `secret_ref` and the column stays blank; anything else is stored as is.
fn column_value<'a>(
    connection: &Connection,
    secret_ref: &str,
    variable: &'a EnvironmentVariable,
    now_ms: i64,
) -> Result<Option<&'a str>, StorageError> {
    match variable.value.as_deref() {
        Some(value) if variable.is_secret && !value.is_empty() => {
            secrets::put(connection, secret_ref, value, now_ms)?;
            Ok(Some(""))
        }
        other => Ok(other),
    }
}

impl WorkspaceStore {
    /// Replaces all of a workspace's global variables, preserving their order.
    pub fn save_global_variables(
        &self,
        workspace_id: &str,
        variables: &[EnvironmentVariable],
    ) -> Result<(), StorageError> {
        self.atomic(|transaction| {
            Self::save_global_variables_on(transaction, workspace_id, variables)
        })
    }

    /// Replaces a workspace's global variables on the given connection or transaction.
    pub(super) fn save_global_variables_on(
        connection: &Connection,
        workspace_id: &str,
        variables: &[EnvironmentVariable],
    ) -> Result<(), StorageError> {
        connection.execute(
            "
            DELETE FROM secrets WHERE secret_ref IN (
                SELECT 'global.' || workspace_id || '.' || key
                FROM global_variables WHERE workspace_id = ?1 AND is_secret = 1
            )
            ",
            params![workspace_id],
        )?;
        connection.execute(
            "DELETE FROM global_variables WHERE workspace_id = ?1",
            params![workspace_id],
        )?;

        insert_variables(
            connection,
            INSERT_GLOBAL_VARIABLE,
            workspace_id,
            |key| global_secret_ref(workspace_id, key),
            variables,
        )
    }

    /// Lists a workspace's global variables in stored order.
    pub fn list_global_variables(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<EnvironmentVariable>, StorageError> {
        self.load_variables(
            &format!(
                "
            SELECT v.key, {ENV_VALUE}, v.is_secret, v.enabled, v.note, v.variable_id
            FROM global_variables v
            LEFT JOIN secrets s
              ON s.secret_ref = 'global.' || v.workspace_id || '.' || v.key
            WHERE v.workspace_id = ?1
            ORDER BY v.position ASC, v.key ASC
            "
            ),
            workspace_id,
            "global",
        )
    }

    /// Upserts an environment and replaces its variables in one transaction.
    pub fn save_environment(&self, environment: &Environment) -> Result<(), StorageError> {
        self.atomic(|transaction| Self::save_environment_on(transaction, environment))
    }

    /// Upserts an environment and its variables on the given connection or transaction.
    pub(super) fn save_environment_on(
        connection: &Connection,
        environment: &Environment,
    ) -> Result<(), StorageError> {
        // The conflict clause only updates a row the workspace already owns, so an id owned by
        // another workspace changes nothing and is reported instead of being taken over. A new
        // row keeps the caller's `guard_changed_at` (an undone delete); an update stamps it
        // whenever either guard flag changes, whoever wrote it (editor, sync, import).
        let changed = connection.execute(
            "
            INSERT INTO environments (
                id, workspace_id, name, confirm_before_send, confirm_safe_methods,
                declared_hosts_json, updated_at, guard_changed_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?7)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                confirm_before_send = excluded.confirm_before_send,
                confirm_safe_methods = excluded.confirm_safe_methods,
                declared_hosts_json = excluded.declared_hosts_json,
                updated_at = excluded.updated_at,
                guard_changed_at_ms = CASE
                    WHEN environments.confirm_before_send IS NOT excluded.confirm_before_send
                      OR environments.confirm_safe_methods IS NOT excluded.confirm_safe_methods
                    THEN ?8 ELSE environments.guard_changed_at_ms END
            WHERE environments.workspace_id = excluded.workspace_id
            ",
            params![
                environment.id.as_str(),
                environment.workspace_id.as_str(),
                environment.name,
                environment.confirm_before_send,
                environment.confirm_safe_methods,
                serde_json::to_string(&environment.declared_hosts)?,
                environment.guard_changed_at,
                now_ms(),
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::ItemWorkspaceMismatch {
                kind: "environment",
                id: environment.id.as_str().to_owned(),
            });
        }

        connection.execute(
            "
            DELETE FROM secrets WHERE secret_ref IN (
                SELECT 'env.' || environment_id || '.' || key
                FROM environment_variables WHERE environment_id = ?1 AND is_secret = 1
            )
            ",
            params![environment.id.as_str()],
        )?;
        connection.execute(
            "DELETE FROM environment_variables WHERE environment_id = ?1",
            params![environment.id.as_str()],
        )?;

        insert_variables(
            connection,
            INSERT_ENVIRONMENT_VARIABLE,
            environment.id.as_str(),
            |key| env_secret_ref(environment.id.as_str(), key),
            &environment.variables,
        )
    }

    /// Loads one environment of a workspace with its variables, or `None` when it does not exist there.
    pub fn get_environment(
        &self,
        workspace_id: &WorkspaceId,
        id: &EnvironmentId,
    ) -> Result<Option<Environment>, StorageError> {
        let environment_row = self
            .connection
            .query_row(
                &format!("{SELECT_ENVIRONMENT} WHERE workspace_id = ?1 AND id = ?2"),
                params![workspace_id.as_str(), id.as_str()],
                environment_row,
            )
            .optional()?;

        environment_row
            .map(|row| self.environment_from_row(row))
            .transpose()
    }

    /// Whether any workspace already owns an environment with this id.
    ///
    /// Row ids are global, so an importer picking a fresh id must see every workspace.
    pub fn environment_id_occupied(&self, id: &EnvironmentId) -> Result<bool, StorageError> {
        self.row_id_exists("environments", id.as_str())
    }

    /// Whether a workspace environment other than `except_id` already uses `name`, compared
    /// case-insensitively like the `uq_environments_workspace_name` index.
    pub fn environment_name_taken(
        &self,
        workspace_id: &WorkspaceId,
        name: &str,
        except_id: &EnvironmentId,
    ) -> Result<bool, StorageError> {
        Ok(self.connection.query_row(
            "
            SELECT EXISTS(
                SELECT 1 FROM environments
                WHERE workspace_id = ?1 AND name = ?2 COLLATE NOCASE AND id <> ?3
            )
            ",
            params![workspace_id.as_str(), name, except_id.as_str()],
            |row| row.get(0),
        )?)
    }

    /// Lists a workspace's environments, with variables, ordered by name and id.
    pub fn list_environments(&self, workspace_id: &str) -> Result<Vec<Environment>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "{SELECT_ENVIRONMENT} WHERE workspace_id = ?1 ORDER BY name ASC, id ASC"
        ))?;
        let rows = statement
            .query_map(params![workspace_id], environment_row)?
            .collect::<Result<Vec<_>, _>>()?;

        rows.into_iter()
            .map(|row| self.environment_from_row(row))
            .collect()
    }

    /// Builds the domain environment of a stored row, loading its variables.
    fn environment_from_row(&self, row: StoredEnvironment) -> Result<Environment, StorageError> {
        let mut environment = Environment::new(&row.id, &row.workspace_id, &row.name)?;
        environment.confirm_before_send = row.confirm_before_send;
        environment.confirm_safe_methods = row.confirm_safe_methods;
        environment.declared_hosts = serde_json::from_str(&row.declared_hosts_json)?;
        environment.guard_changed_at = row.guard_changed_at;
        environment.variables = self.environment_variables(&row.id)?;
        Ok(environment)
    }

    /// Deletes a workspace's environment by id, with the secrets of its variables; returns
    /// whether a row was removed.
    pub fn delete_environment(
        &self,
        workspace_id: &WorkspaceId,
        id: &EnvironmentId,
    ) -> Result<bool, StorageError> {
        self.atomic(|transaction| {
            // Only secrets of an environment this workspace owns; another workspace's are kept.
            transaction.execute(
                "
                DELETE FROM secrets WHERE secret_ref IN (
                    SELECT 'env.' || v.environment_id || '.' || v.key
                    FROM environment_variables v
                    JOIN environments e ON e.id = v.environment_id
                    WHERE e.workspace_id = ?1 AND e.id = ?2 AND v.is_secret = 1
                )
                ",
                params![workspace_id.as_str(), id.as_str()],
            )?;
            let affected_rows = transaction.execute(
                "DELETE FROM environments WHERE workspace_id = ?1 AND id = ?2",
                params![workspace_id.as_str(), id.as_str()],
            )?;
            Ok(affected_rows > 0)
        })
    }

    /// Reads a workspace environment variable's stored value (a secret one comes from the secret
    /// store), or `None` when absent or unset.
    #[cfg(test)]
    pub(crate) fn stored_environment_variable_value(
        &self,
        workspace_id: &WorkspaceId,
        environment_id: &str,
        key: &str,
    ) -> Result<Option<String>, StorageError> {
        let stored_value = self
            .connection
            .query_row(
                &format!(
                    "
                    SELECT {ENV_VALUE}
                    FROM environment_variables v
                    LEFT JOIN secrets s
                      ON s.secret_ref = 'env.' || v.environment_id || '.' || v.key
                    WHERE v.environment_id = ?2 AND v.key = ?3
                      AND v.environment_id IN (SELECT id FROM environments WHERE workspace_id = ?1)
                    "
                ),
                params![workspace_id.as_str(), environment_id, key],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;

        Ok(stored_value.flatten())
    }

    fn environment_variables(
        &self,
        environment_id: &str,
    ) -> Result<Vec<EnvironmentVariable>, StorageError> {
        self.load_variables(
            &format!(
                "
            SELECT v.key, {ENV_VALUE}, v.is_secret, v.enabled, v.note, v.variable_id
            FROM environment_variables v
            LEFT JOIN secrets s
              ON s.secret_ref = 'env.' || v.environment_id || '.' || v.key
            WHERE v.environment_id = ?1
            ORDER BY v.position ASC, v.key ASC
            "
            ),
            environment_id,
            environment_id,
        )
    }

    /// The variables `sql` selects for `owner_id` (its `?1`), in [`variable_row`] order; `label`
    /// names the owner in a stored-value error.
    fn load_variables(
        &self,
        sql: &str,
        owner_id: &str,
        label: &str,
    ) -> Result<Vec<EnvironmentVariable>, StorageError> {
        let mut statement = self.connection.prepare_cached(sql)?;
        let rows = statement.query_map(params![owner_id], variable_row)?;
        let mut variables = Vec::new();
        for row in rows {
            variables.push(row?.into_domain(label)?);
        }
        Ok(variables)
    }
}

/// Inserts `variables` of `owner_id` in order with `insert_sql` (binding owner, position, key,
/// value, flags, note and id). A secret value moves into the secret store under
/// `secret_ref(key)` first.
fn insert_variables(
    connection: &Connection,
    insert_sql: &'static str,
    owner_id: &str,
    secret_ref: impl Fn(&str) -> String,
    variables: &[EnvironmentVariable],
) -> Result<(), StorageError> {
    let now_ms = now_ms();
    let mut insert = connection.prepare(insert_sql)?;
    for (position, variable) in variables.iter().enumerate() {
        let stored_value = column_value(connection, &secret_ref(&variable.key), variable, now_ms)?;
        insert.execute(params![
            owner_id,
            i64_from_usize(position),
            variable.key,
            stored_value,
            variable.is_secret,
            variable.enabled,
            variable.note,
            variable.id,
        ])?;
    }
    Ok(())
}

struct StoredEnvironment {
    id: String,
    workspace_id: String,
    name: String,
    confirm_before_send: bool,
    confirm_safe_methods: bool,
    declared_hosts_json: String,
    guard_changed_at: Option<i64>,
}

fn environment_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEnvironment> {
    Ok(StoredEnvironment {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        name: row.get(2)?,
        confirm_before_send: row.get(3)?,
        confirm_safe_methods: row.get(4)?,
        declared_hosts_json: row.get(5)?,
        guard_changed_at: row.get(6)?,
    })
}

fn variable_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEnvironmentVariable> {
    Ok(StoredEnvironmentVariable {
        key: row.get(0)?,
        value: row.get(1)?,
        is_secret: row.get(2)?,
        enabled: row.get(3)?,
        note: row.get(4)?,
        id: row.get(5)?,
    })
}

struct StoredEnvironmentVariable {
    key: String,
    value: Option<String>,
    is_secret: bool,
    enabled: bool,
    note: String,
    id: String,
}

impl StoredEnvironmentVariable {
    fn into_domain(self, environment_id: &str) -> Result<EnvironmentVariable, StorageError> {
        let value = self
            .value
            .ok_or_else(|| StorageError::InvalidStoredVariable {
                environment_id: environment_id.to_owned(),
                key: self.key.clone(),
                reason: if self.is_secret {
                    "secret variable is missing value"
                } else {
                    "plain variable is missing value"
                },
            })?;

        let mut variable = if self.is_secret {
            EnvironmentVariable::masked(&self.key, value)?
        } else {
            EnvironmentVariable::plain(&self.key, value)?
        };

        variable.enabled = self.enabled;
        variable.note = self.note;
        variable.id = self.id;
        Ok(variable)
    }
}
