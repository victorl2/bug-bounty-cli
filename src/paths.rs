//! One cache root, namespaced provider responses and shared explorer/EVM metadata.

use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// `~/foo` → `$HOME/foo`.
pub fn expand(path: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => home().join(rest),
        Err(_) => path.to_path_buf(),
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).filter(|v| !v.is_empty()).map(|v| expand(Path::new(&v)))
}

pub fn cache_root() -> PathBuf {
    env_path("BB_CACHE_DIR").unwrap_or_else(|| env_path("XDG_CACHE_HOME").unwrap_or_else(|| home().join(".cache")).join("bug-bounty-cli"))
}

/// A provider's response cache: `<SOURCE>_CACHE_DIR` (legacy) or `<root>/<source>`.
pub fn provider_cache(source: &str) -> PathBuf {
    env_path(&format!("{}_CACHE_DIR", source.to_uppercase())).unwrap_or_else(|| cache_root().join(source))
}

pub fn evm_cache() -> PathBuf {
    cache_root().join("evm")
}
