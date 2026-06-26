//! Database-backup and Codex unified-history-backup commands
//! (`src/lib/api/settings.ts`: `backupsApi.*`, `settingsApi.*HistoryBackup`).
//!
//! Follows the [`super::meta`] template.
//!   - `create_db_backup` -> `Database::backup_database_file`
//!   - `list_db_backups` / `restore_db_backup` / `rename_db_backup` /
//!     `delete_db_backup` -> the `Database` backup-CRUD methods. Restore copies
//!     the snapshot back into the live connection (SQLite backup API) and then
//!     refreshes the in-memory config snapshot.
//!   - `has_codex_unify_history_backup` / `restore_codex_unified_history` ->
//!     `codex_history_migration::*`.

use serde_json::{json, Value};

use super::common::{str_arg, to_value};
use crate::web::error::WebError;
use crate::AppState;

pub fn dispatch(state: &AppState, command: &str, args: &Value) -> Option<Result<Value, WebError>> {
    Some(match command {
        // No args -> string (backup filename). `backup_database_file` returns
        // `Result<Option<PathBuf>, AppError>`; None means the DB file is missing.
        "create_db_backup" => match state.db.backup_database_file() {
            Ok(Some(path)) => Ok(Value::String(
                path.file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            )),
            Ok(None) => Err(WebError::Domain(crate::AppError::Config(
                "Database file not found, backup skipped".to_string(),
            ))),
            Err(e) => Err(WebError::Domain(e)),
        },

        // No args -> BackupEntry[] { filename, sizeBytes, createdAt }.
        "list_db_backups" => state
            .db
            .list_db_backups()
            .map_err(WebError::Domain)
            .and_then(to_value),

        // { filename } -> string. Restores in place then refreshes the snapshot.
        "restore_db_backup" => match str_arg(args, "filename") {
            Ok(filename) => state
                .db
                .restore_db_backup(filename)
                .and_then(|()| state.refresh_config_from_db())
                .map(|()| Value::String(filename.to_string()))
                .map_err(WebError::Domain),
            Err(e) => Err(e),
        },

        // { oldFilename, newName } -> string (the new filename).
        "rename_db_backup" => match (str_arg(args, "oldFilename"), str_arg(args, "newName")) {
            (Ok(old), Ok(new_name)) => state
                .db
                .rename_db_backup(old, new_name)
                .map(Value::String)
                .map_err(WebError::Domain),
            (Err(e), _) | (_, Err(e)) => Err(e),
        },

        // { filename } -> void.
        "delete_db_backup" => match str_arg(args, "filename") {
            Ok(filename) => state
                .db
                .delete_db_backup(filename)
                .map(|()| Value::Null)
                .map_err(WebError::Domain),
            Err(e) => Err(e),
        },

        // No args -> bool. Infallible (returns bool directly).
        "has_codex_unify_history_backup" => Ok(Value::Bool(
            crate::codex_history_migration::has_codex_official_history_unify_backup(),
        )),

        // No args -> CodexUnifyHistoryRestoreResult (snake_case struct without
        // Serialize -> build the camelCase TS shape by hand).
        "restore_codex_unified_history" => {
            match crate::codex_history_migration::restore_codex_official_history_from_backups() {
                Ok(outcome) => {
                    let mut value = json!({
                        "restoredJsonlFiles": outcome.restored_jsonl_files,
                        "restoredStateRows": outcome.restored_state_rows,
                    });
                    if let Some(reason) = outcome.skipped_reason {
                        value["skippedReason"] = Value::String(reason);
                    }
                    Ok(value)
                }
                Err(e) => Err(WebError::Domain(e)),
            }
        }

        _ => return None,
    })
}
