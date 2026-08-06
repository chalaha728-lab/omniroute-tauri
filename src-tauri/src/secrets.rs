//! Secret bootstrap — 1:1 port of the IIFE in `electron/main.js` (lines ~534-607)
//! and `scripts/build/bootstrap-env.mjs`.
//!
//! Responsibilities:
//!   1. Read persisted secrets from `<DATA_DIR>/server.env`
//!   2. Generate missing secrets (JWT_SECRET 64B hex, STORAGE_ENCRYPTION_KEY 32B hex,
//!      API_KEY_SECRET 32B hex) with `rand::thread_rng().fill_bytes()`
//!   3. Persist back to `<DATA_DIR>/server.env` for future restarts
//!   4. Refuse to auto-generate STORAGE_ENCRYPTION_KEY if encrypted credentials
//!      already exist in `<DATA_DIR>/storage.sqlite` (matches main.js behavior)
//!
//! Returns the merged env (persisted + user .env + process env) so the caller
//! can pass it to the spawned Node sidecar.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rand::RngCore;
use serde::Serialize;

#[derive(Default, Serialize)]
pub struct BootstrapResult {
    /// Merged env: persisted server.env + .env + process env (last wins).
    pub env: HashMap<String, String>,
    pub changed: bool,
}

/// Resolve the data directory. Mirrors `resolveDataDir()` in main.js:
///   1. `DATA_DIR` env (highest priority)
///   2. On Windows: `%APPDATA%/omniroute`
///   3. On Unix: `$XDG_CONFIG_HOME/omniroute` or `~/.omniroute`
pub fn resolve_data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("DATA_DIR") {
        let trimmed = d.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("omniroute");
        }
        return dirs::home_dir()
            .map(|h| h.join("AppData").join("Roaming").join("omniroute"))
            .unwrap_or_else(|| PathBuf::from("omniroute"));
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            let trimmed = xdg.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed).join("omniroute");
            }
        }
        return dirs::home_dir()
            .map(|h| h.join(".omniroute"))
            .unwrap_or_else(|| PathBuf::from("omniroute"));
    }
}

/// Resolve the preferred .env file location:
///   1. `<DATA_DIR>/.env` if DATA_DIR env is set
///   2. `<resolved data dir>/.env`
///   3. `<cwd>/.env`
/// Mirrors `getPreferredEnvFilePath()` in main.js.
fn preferred_env_path(data_dir: &Path) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(d) = std::env::var("DATA_DIR") {
        let trimmed = d.trim();
        if !trimmed.is_empty() {
            candidates.push(PathBuf::from(trimmed).join(".env"));
        }
    }
    candidates.push(data_dir.join(".env"));
    candidates.push(std::env::current_dir().ok()?.join(".env"));
    candidates.into_iter().find(|p| p.exists())
}

/// Parse a simple `KEY=VALUE` env file. Mirrors `parseEnvFile()` in main.js.
fn parse_env_file(path: &Path) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return env,
    };
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(eq) = trimmed.find('=') {
            if eq == 0 {
                continue;
            }
            let key = trimmed[..eq].trim().to_string();
            let val = trimmed[eq + 1..].trim().to_string();
            env.insert(key, val);
        }
    }
    env
}

/// Check whether `<DATA_DIR>/storage.sqlite` already contains rows with
/// `enc:v1:`-prefixed tokens. Mirrors `hasEncryptedCredentials()` in
/// `electron/sqlite-inspection.js`. Refuses to auto-generate a new
/// STORAGE_ENCRYPTION_KEY when true.
///
/// Implementation note: we read the SQLite file as bytes and grep for the
/// `enc:v1:` literal — this avoids pulling in a Rust SQLite binding. False
/// positives are theoretically possible but vanishingly unlikely (the string
/// would need to appear in user data).
fn has_encrypted_credentials(db_path: &Path) -> bool {
    let bytes = match std::fs::read(db_path) {
        Ok(b) => b,
        Err(_) => return false,
    };
    // SQLite stores text as UTF-8; a substring search is sufficient for the
    // `enc:v1:` prefix that the encryption layer prepends.
    bytes
        .windows(b"enc:v1:".len())
        .any(|w| w == b"enc:v1:")
}

/// Run the bootstrap. Returns the merged env + a `changed` flag.
pub fn bootstrap(data_dir: &Path) -> Result<BootstrapResult, String> {
    let mut persisted: HashMap<String, String> = HashMap::new();
    let server_env_path = data_dir.join("server.env");
    if server_env_path.exists() {
        persisted = parse_env_file(&server_env_path);
    }

    let preferred_env = preferred_env_path(data_dir);
    let preferred = preferred_env
        .as_ref()
        .map(|p| parse_env_file(p))
        .unwrap_or_default();

    // Build the merged env in the same precedence order as main.js:
    //   persisted < preferred (.env) < process env
    let mut env: HashMap<String, String> = HashMap::new();
    for (k, v) in &persisted {
        env.insert(k.clone(), v.clone());
    }
    for (k, v) in &preferred {
        env.insert(k.clone(), v.clone());
    }
    for (k, v) in std::env::vars() {
        env.insert(k, v);
    }

    let mut changed = false;
    let mut to_persist = persisted.clone();

    if !env.contains_key("JWT_SECRET") {
        let secret = random_hex(64);
        env.insert("JWT_SECRET".into(), secret.clone());
        to_persist.insert("JWT_SECRET".into(), secret);
        changed = true;
        log::info!("[Tauri] ✨ JWT_SECRET auto-generated");
    }
    if !env.contains_key("STORAGE_ENCRYPTION_KEY") {
        let db_path = data_dir.join("storage.sqlite");
        if has_encrypted_credentials(&db_path) {
            return Err(format!(
                "Refusing to auto-generate STORAGE_ENCRYPTION_KEY: encrypted credentials already exist in {}. \
                 Restore the key via an appropriate .env file, {}, or process.env.",
                db_path.display(),
                server_env_path.display()
            ));
        }
        let key = random_hex(32);
        env.insert("STORAGE_ENCRYPTION_KEY".into(), key.clone());
        to_persist.insert("STORAGE_ENCRYPTION_KEY".into(), key);
        env.insert("STORAGE_ENCRYPTION_KEY_VERSION".into(), "v1".into());
        to_persist.insert("STORAGE_ENCRYPTION_KEY_VERSION".into(), "v1".into());
        changed = true;
        log::info!("[Tauri] ✨ STORAGE_ENCRYPTION_KEY auto-generated");
    }
    if !env.contains_key("API_KEY_SECRET") {
        let secret = random_hex(32);
        env.insert("API_KEY_SECRET".into(), secret.clone());
        to_persist.insert("API_KEY_SECRET".into(), secret);
        changed = true;
        log::info!("[Tauri] ✨ API_KEY_SECRET auto-generated");
    }

    if changed {
        env.insert("OMNIROUTE_BOOTSTRAPPED".into(), "true".into());
        to_persist.insert("OMNIROUTE_BOOTSTRAPPED".into(), "true".into());

        if let Err(e) = std::fs::create_dir_all(data_dir) {
            log::warn!("[Tauri] Could not create data dir {}: {}", data_dir.display(), e);
        }
        let mut lines: Vec<String> = vec!["# Auto-generated by OmniRoute bootstrap".into(), String::new()];
        for (k, v) in &to_persist {
            lines.push(format!("{}={}", k, v));
        }
        lines.push(String::new());
        if let Err(e) = std::fs::write(&server_env_path, lines.join("\n")) {
            log::warn!("[Tauri] Could not persist secrets: {}", e);
        } else {
            log::info!("[Tauri] 📁 Secrets persisted to: {}", server_env_path.display());
        }
    }

    Ok(BootstrapResult { env, changed })
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}
