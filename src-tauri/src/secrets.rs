//! Driver secrets (network passwords) live in the OS credential store, never in the
//! bundle. A source's stored config keeps a marker in place of each secret; the real
//! value is put back just before connecting.
//!
//! Android has no credential store reachable from native code; secrets go to a file in
//! the app's private data directory, which other apps cannot read. On platforms without
//! either, the secret stays in the config.

use serde_json::Value;
use vp_driver::{ConfigField, ConfigKind};

const SERVICE: &str = "ViewPort";
const MARKER: &str = "$keyring";

/// Credential-store account for one secret: the driver plus its non-secret settings,
/// so the same server and user share one stored password across bundles.
fn account(driver: &str, config: &Value, schema: &[ConfigField], key: &str) -> String {
    let mut parts: Vec<String> = schema
        .iter()
        .filter(|f| f.kind != ConfigKind::Secret)
        .filter_map(|f| config.get(&f.key).map(|v| format!("{}={}", f.key, v)))
        .collect();
    parts.sort();
    format!("{driver}:{}#{key}", parts.join(";"))
}

#[cfg(any(windows, target_os = "macos", target_os = "ios", target_os = "linux"))]
mod store {
    pub fn set(account: &str, secret: &str) -> bool {
        keyring::Entry::new(super::SERVICE, account).and_then(|e| e.set_password(secret)).is_ok()
    }

    pub fn get(account: &str) -> Option<String> {
        keyring::Entry::new(super::SERVICE, account).and_then(|e| e.get_password()).ok()
    }
}

#[cfg(target_os = "android")]
mod store {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    static FILE: OnceLock<PathBuf> = OnceLock::new();
    static LOCK: Mutex<()> = Mutex::new(());

    pub fn init(dir: PathBuf) {
        let _ = std::fs::create_dir_all(&dir);
        let _ = FILE.set(dir.join("secrets.json"));
    }

    fn load(file: &PathBuf) -> BTreeMap<String, String> {
        std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn set(account: &str, secret: &str) -> bool {
        let Some(file) = FILE.get() else { return false };
        let _guard = LOCK.lock().unwrap();
        let mut all = load(file);
        all.insert(account.to_string(), secret.to_string());
        serde_json::to_vec(&all).is_ok_and(|json| std::fs::write(file, json).is_ok())
    }

    pub fn get(account: &str) -> Option<String> {
        let _guard = LOCK.lock().unwrap();
        load(FILE.get()?).remove(account)
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "ios", target_os = "linux", target_os = "android")))]
mod store {
    pub fn set(_account: &str, _secret: &str) -> bool {
        false
    }

    pub fn get(_account: &str) -> Option<String> {
        None
    }
}

/// Where the private secret store lives on platforms that need one (Android).
pub fn init(_private_dir: std::path::PathBuf) {
    #[cfg(target_os = "android")]
    store::init(_private_dir);
}

/// Moves secrets out of `config` into the credential store where possible.
pub fn stash(driver: &str, config: &mut Value, schema: &[ConfigField]) {
    for f in schema.iter().filter(|f| f.kind == ConfigKind::Secret) {
        let Some(secret) = config.get(&f.key).and_then(Value::as_str).map(str::to_string) else { continue };
        if secret.is_empty() {
            continue;
        }
        let acct = account(driver, config, schema, &f.key);
        if store::set(&acct, &secret) {
            config[&f.key] = Value::String(MARKER.into());
        }
    }
}

/// Returns `config` with stashed secrets filled back in. A secret that cannot be
/// found becomes empty, which makes the connection fail with an auth error the UI can
/// answer by asking for it again.
pub fn reveal(driver: &str, config: &Value, schema: &[ConfigField]) -> Value {
    let mut out = config.clone();
    for f in schema.iter().filter(|f| f.kind == ConfigKind::Secret) {
        if config.get(&f.key).and_then(Value::as_str) == Some(MARKER) {
            let acct = account(driver, config, schema, &f.key);
            out[&f.key] = Value::String(store::get(&acct).unwrap_or_default());
        }
    }
    out
}

/// The config as shown to the UI: secrets are never sent back, only whether one is set.
pub fn redact(config: &Value, schema: &[ConfigField]) -> Value {
    let mut out = config.clone();
    for f in schema.iter().filter(|f| f.kind == ConfigKind::Secret) {
        if let Some(v) = out.get_mut(&f.key) {
            let set = v.as_str().is_some_and(|s| !s.is_empty());
            *v = Value::String(if set { "********".into() } else { String::new() });
        }
    }
    out
}

/// Merges an edited config over the stored one: a redacted placeholder keeps the old secret.
pub fn merge_edit(old: &Value, new: &Value, schema: &[ConfigField]) -> Value {
    let mut out = new.clone();
    for f in schema.iter().filter(|f| f.kind == ConfigKind::Secret) {
        if new.get(&f.key).and_then(Value::as_str) == Some("********") {
            out[&f.key] = old.get(&f.key).cloned().unwrap_or(Value::Null);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Vec<ConfigField> {
        vec![
            ConfigField::new("host", ConfigKind::Text, true),
            ConfigField::new("password", ConfigKind::Secret, false),
        ]
    }

    #[test]
    fn redact_and_merge() {
        let stored = serde_json::json!({ "host": "nas", "password": "$keyring" });
        let shown = redact(&stored, &schema());
        assert_eq!(shown["password"], "********");
        assert_eq!(shown["host"], "nas");
        let edited = serde_json::json!({ "host": "nas2", "password": "********" });
        assert_eq!(merge_edit(&stored, &edited, &schema())["password"], "$keyring");
        let changed = serde_json::json!({ "host": "nas2", "password": "new" });
        assert_eq!(merge_edit(&stored, &changed, &schema())["password"], "new");
    }

    #[test]
    fn account_ignores_secrets_and_order() {
        let a = account("smb", &serde_json::json!({ "host": "nas", "password": "x" }), &schema(), "password");
        let b = account("smb", &serde_json::json!({ "password": "y", "host": "nas" }), &schema(), "password");
        assert_eq!(a, b);
        assert!(!a.contains('x'));
    }
}
