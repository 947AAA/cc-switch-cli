//! Supplementary config / settings commands (`src/lib/api/settings.ts`,
//! `src/lib/api/deeplink.ts`).
//!
//! Follows the [`super::meta`] template.
//!
//! Still unwired (fall through to HTTP 501):
//!   - `get_app_config_dir_override` / `set_app_config_dir_override`: the desktop
//!     reads/writes a Tauri Store via `app_store`, which the CLI has no
//!     equivalent of (`config::get_app_config_dir` documents "CLI mode: no app
//!     store override"). Tracked as a follow-up issue.
//!   - `webdav_sync_fetch_remote_info`: needs to read+parse the remote sync
//!     manifest; deferred.
//!   - `merge_deeplink_config`: backing fn lives in the private `deeplink`
//!     module and is not re-exported.

use serde_json::{json, Value};

use super::common::{block_on, bool_arg, from_arg};
use crate::services::webdav;
use crate::web::error::WebError;
use crate::{config, settings, AppState, WebDavSyncSettings};

pub fn dispatch(_state: &AppState, command: &str, args: &Value) -> Option<Result<Value, WebError>> {
    Some(match command {
        // No args -> string. Mirrors the desktop command, which returns
        // `get_claude_settings_path()` as a lossy string.
        "get_claude_code_config_path" => Ok(Value::String(
            config::get_claude_settings_path()
                .to_string_lossy()
                .into_owned(),
        )),

        // Persist WebDAV settings. `{ settings, passwordTouched }`. When the
        // password wasn't re-entered (passwordTouched=false) we keep the stored
        // one so saving from the UI doesn't blank it. TS expects `{ success }`.
        "webdav_sync_save_settings" => (|| -> Result<Value, WebError> {
            let mut incoming: WebDavSyncSettings = from_arg(args, "settings")?;
            if !bool_arg(args, "passwordTouched", false) {
                if let Some(existing) = settings::get_settings().webdav_sync {
                    incoming.password = existing.password;
                }
            }
            let mut app_settings = settings::get_settings();
            app_settings.webdav_sync = Some(incoming);
            settings::update_settings(app_settings).map_err(WebError::Domain)?;
            Ok(json!({ "success": true }))
        })(),

        // Test a WebDAV connection with the settings being edited (not the saved
        // ones). `{ settings, preserveEmptyPassword }`; an empty password with
        // preserveEmptyPassword reuses the stored credential. TS: WebDavTestResult
        // `{ success }` on success; a thrown error message on failure.
        "webdav_test_connection" => (|| -> Result<Value, WebError> {
            let mut incoming: WebDavSyncSettings = from_arg(args, "settings")?;
            if bool_arg(args, "preserveEmptyPassword", true) && incoming.password.is_empty() {
                if let Some(existing) = settings::get_settings().webdav_sync {
                    incoming.password = existing.password;
                }
            }
            let auth = webdav::auth_from_credentials(&incoming.username, &incoming.password);
            block_on(webdav::test_connection(&incoming.base_url, &auth))
                .map_err(WebError::Domain)?;
            Ok(json!({ "success": true }))
        })(),

        _ => return None,
    })
}
