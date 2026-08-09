//! Zero-config secrets bootstrap — Rust port of the env-bootstrap block in
//! `electron/main.js` (lines ~657–730) and `scripts/build/bootstrap-env.mjs`.
//!
//! On first launch, OmniRoute auto-generates three secrets if they aren't
//! already present in `<data_dir>/server.env` or the preferred `.env` file:
//!   - `JWT_SECRET`            — 64 random bytes hex-encoded
//!   - `STORAGE_ENCRYPTION_KEY` — 32 random bytes hex-encoded
//!     (refuses to auto-generate if `storage.sqlite` already contains
//!      `enc:v1:`-prefixed credentials and no key is configured — that
//!      would lock the user out of their stored provider connections)
//!   - `API_KEY_SECRET`         — 32 random bytes hex-encoded
//!
//! Persisted to `<data_dir>/server.env` so they survive restarts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rand::RngCore;
use tokio::fs;

/// Parse a flat `KEY=VALUE` env file into a map. Mirrors `parseEnvFile`
/// in `electron/main.js`. Comments (`#…`) and blank lines are skipped.
pub fn parse_env_file(content: &str) -> HashMap<String, String> {
    let mut env = HashMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(eq) = trimmed.find('=') {
            if eq == 0 {
                continue;
            }
            let key = trimmed[..eq].trim().to_string();
            let value = trimmed[eq + 1..].trim().to_string();
            env.insert(key, value);
        }
    }
    env
}

/// Resolve the preferred `.env` file path. Mirrors `getPreferredEnvFilePath()`
/// in `electron/main.js`:
///   1. `<DATA_DIR>/.env` (if `DATA_DIR` env var is set)
///   2. `<resolved_data_dir>/.env`
///   3. `<cwd>/.env`
/// Returns the first one that exists, or `None`.
pub fn preferred_env_path(data_dir: &Path) -> Option<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(dir) = std::env::var("DATA_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            candidates.push(PathBuf::from(trimmed).join(".env"));
        }
    }
    candidates.push(data_dir.join(".env"));
    candidates.push(std::env::current_dir().unwrap_or_default().join(".env"));

    candidates.into_iter().find(|p| p.exists())
}

/// Generate `n` random bytes as a lowercase hex string. Uses `rand::rngs::
/// OsRng` via `RngCore::fill_bytes` — same entropy source as Node's
/// `crypto.randomBytes()`.
fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}

/// Detect whether the SQLite database at `db_path` contains any
/// `enc:v1:`-prefixed credentials. Mirrors `hasEncryptedCredentials()` in
/// `electron/sqlite-inspection.js`.
///
/// The Electron build uses `better-sqlite3` (or `node:sqlite`); the Tauri
/// port reads the file directly with the `rusqlite` crate — but to avoid
/// adding a heavy native dependency for a one-shot first-launch check, we
/// do a very crude text scan of the SQLite file for the `enc:v1:` marker.
/// This is intentionally conservative: a false positive (refusing to
/// auto-generate) just means the operator must restore the key manually,
/// while a false negative could lock them out of their stored credentials.
pub fn has_encrypted_credentials(db_path: &Path) -> bool {
    if !db_path.exists() {
        return false;
    }
    // SQLite files embed cell payloads as raw bytes; scanning for the ASCII
    // marker `enc:v1:` is sufficient to detect prior encrypted writes.
    let bytes = match std::fs::read(db_path) {
        Ok(b) => b,
        Err(_) => return false,
    };
    bytes
        .windows(7)
        .any(|w| w == b"enc:v1:")
}

/// Bootstrap the server env. Reads `<data_dir>/server.env` and the preferred
/// `.env` file, generates any missing secrets, persists them back to
/// `server.env`, and returns the merged env map. The caller is expected to
/// add `PORT`, `NODE_ENV`, and `DATA_DIR` before spawning the server.
pub async fn bootstrap_server_env(data_dir: &Path) -> Result<HashMap<String, String>> {
    fs::create_dir_all(data_dir)
        .await
        .with_context(|| format!("create_dir_all({})", data_dir.display()))?;

    let server_env_path = data_dir.join("server.env");
    let preferred_path = preferred_env_path(data_dir);

    let mut persisted: HashMap<String, String> = if server_env_path.exists() {
        let content = fs::read_to_string(&server_env_path).await.unwrap_or_default();
        parse_env_file(&content)
    } else {
        HashMap::new()
    };
    let preferred: HashMap<String, String> = if let Some(p) = &preferred_path {
        let content = fs::read_to_string(p).await.unwrap_or_default();
        parse_env_file(&content)
    } else {
        HashMap::new()
    };

    // Merge precedence: persisted < preferred < process env
    let mut server_env = HashMap::new();
    for (k, v) in &persisted {
        server_env.insert(k.clone(), v.clone());
    }
    for (k, v) in &preferred {
        server_env.insert(k.clone(), v.clone());
    }
    for (k, v) in std::env::vars() {
        server_env.insert(k, v);
    }

    let mut changed = false;

    if !server_env.contains_key("JWT_SECRET") {
        let secret = random_hex(64);
        server_env.insert("JWT_SECRET".into(), secret.clone());
        persisted.insert("JWT_SECRET".into(), secret);
        changed = true;
        log::info!("[OmniRoute] ✨ JWT_SECRET auto-generated");
    }

    if !server_env.contains_key("STORAGE_ENCRYPTION_KEY") {
        let db_path = data_dir.join("storage.sqlite");
        if has_encrypted_credentials(&db_path) {
            anyhow::bail!(
                "Refusing to auto-generate STORAGE_ENCRYPTION_KEY: encrypted credentials already exist in {}. \
                 Restore the key via an appropriate .env file, {}, or process.env.",
                db_path.display(),
                server_env_path.display()
            );
        }
        let key = random_hex(32);
        server_env.insert("STORAGE_ENCRYPTION_KEY".into(), key.clone());
        persisted.insert("STORAGE_ENCRYPTION_KEY".into(), key);
        persisted.insert("STORAGE_ENCRYPTION_KEY_VERSION".into(), "v1".into());
        changed = true;
        log::info!("[OmniRoute] ✨ STORAGE_ENCRYPTION_KEY auto-generated");
    }

    if !server_env.contains_key("API_KEY_SECRET") {
        let secret = random_hex(32);
        server_env.insert("API_KEY_SECRET".into(), secret.clone());
        persisted.insert("API_KEY_SECRET".into(), secret);
        changed = true;
        log::info!("[OmniRoute] ✨ API_KEY_SECRET auto-generated");
    }

    if changed {
        server_env.insert("OMNIROUTE_BOOTSTRAPPED".into(), "true".into());
        let mut lines = vec!["# Auto-generated by OmniRoute bootstrap".to_string(), String::new()];
        let mut keys: Vec<&String> = persisted.keys().collect();
        keys.sort();
        for k in keys {
            if let Some(v) = persisted.get(k) {
                lines.push(format!("{k}={v}"));
            }
        }
        lines.push(String::new());
        fs::write(&server_env_path, lines.join("\n"))
            .await
            .with_context(|| format!("write {}", server_env_path.display()))?;
        log::info!("[OmniRoute] 📁 secrets persisted to {}", server_env_path.display());
    }

    Ok(server_env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_env_file() {
        let env = parse_env_file("# comment\nFOO=bar\nBAZ=qux\n\n");
        assert_eq!(env.get("FOO").map(|s| s.as_str()), Some("bar"));
        assert_eq!(env.get("BAZ").map(|s| s.as_str()), Some("qux"));
        assert!(!env.contains_key("# comment"));
    }

    #[test]
    fn ignores_blank_and_comment_lines() {
        let env = parse_env_file("\n# hi\n\nKEY=val\n");
        assert_eq!(env.len(), 1);
        assert_eq!(env.get("KEY").map(|s| s.as_str()), Some("val"));
    }

    #[test]
    fn random_hex_is_correct_length() {
        let h = random_hex(32);
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
