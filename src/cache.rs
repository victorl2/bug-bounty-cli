//! On-disk JSON cache. Each entry is `{"fetched_at": <epoch>, "source": <url>, "data": <payload>}`,
//! the same files the Python releases wrote, so existing caches keep working.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::error::Result;

pub const MIN_TTL_HOURS: f64 = 10.0;
pub const DEFAULT_TTL_HOURS: f64 = 12.0;

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// The TTL from `BB_CACHE_TTL_HOURS` (or a legacy variable), else the default.
pub fn env_ttl_hours(legacy: &[&str]) -> f64 {
    std::iter::once("BB_CACHE_TTL_HOURS")
        .chain(legacy.iter().copied())
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_TTL_HOURS)
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub data: Value,
    pub fetched_at: f64,
    pub source: String,
}

impl Entry {
    pub fn age_seconds(&self) -> f64 {
        (now() - self.fetched_at).max(0.0)
    }
}

#[derive(Debug, Clone)]
pub struct Cache {
    pub root: PathBuf,
    pub ttl_seconds: f64,
}

impl Cache {
    pub fn new(root: impl Into<PathBuf>, ttl_hours: f64) -> Self {
        Cache { root: root.into(), ttl_seconds: ttl_hours * 3600.0 }
    }

    fn path(&self, key: &str) -> PathBuf {
        self.root.join(format!("{key}.json"))
    }

    pub fn read(&self, key: &str) -> Option<Entry> {
        read_entry(&self.path(key))
    }

    pub fn is_fresh(&self, entry: &Entry) -> bool {
        entry.age_seconds() < self.ttl_seconds
    }

    pub fn write(&self, key: &str, data: Value, source: &str) -> Result<Entry> {
        let fetched_at = now();
        write_atomic(&self.path(key), &json!({"fetched_at": fetched_at, "source": source, "data": data}))?;
        Ok(Entry { data, fetched_at, source: source.to_string() })
    }

    pub fn entries(&self) -> Vec<(String, Entry)> {
        let mut out: Vec<(String, Entry)> = json_files(&self.root)
            .into_iter()
            .filter_map(|p| {
                let key = p.strip_prefix(&self.root).ok()?.with_extension("").to_string_lossy().replace('\\', "/");
                Some((key, read_entry(&p)?))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn clear(&self) -> Result<()> {
        if self.root.is_dir() {
            fs::remove_dir_all(&self.root)?;
        }
        Ok(())
    }
}

/// An entry in either layout: `source` (this cache) or `url` (Cantina's hashed keys).
pub fn read_entry(path: &Path) -> Option<Entry> {
    let raw: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    let fetched_at = raw.get("fetched_at")?.as_f64()?;
    let source = raw.get("source").or_else(|| raw.get("url")).and_then(Value::as_str).unwrap_or("").to_string();
    Some(Entry { data: raw.get("data")?.clone(), fetched_at, source })
}

/// Write via a temporary file in the same directory, so readers never see half a file.
pub fn write_atomic(path: &Path, value: &Value) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let mut tmp = tempfile::Builder::new().prefix(".tmp-").suffix(".json").tempfile_in(dir)?;
    tmp.write_all(serde_json::to_string(value)?.as_bytes())?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Every cached `*.json` below `root`, skipping in-flight temporary files.
pub fn json_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(items) = fs::read_dir(&dir) else { continue };
        for item in items.flatten() {
            let path = item.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "json")
                && !path.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".tmp-"))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_freshness() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path(), 12.0);
        assert!(cache.read("a/b").is_none());
        cache.write("a/b", json!({"x": 1}), "https://example.com").unwrap();
        let entry = cache.read("a/b").unwrap();
        assert_eq!(entry.data, json!({"x": 1}));
        assert!(cache.is_fresh(&entry));
        assert_eq!(cache.entries()[0].0, "a/b");
        assert!(!Cache::new(dir.path(), 0.0).is_fresh(&entry));
    }

    #[test]
    fn reads_python_cantina_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        fs::write(&path, r#"{"url": "https://api.cantina.xyz/x", "fetched_at": 1.5, "data": [1]}"#).unwrap();
        let entry = read_entry(&path).unwrap();
        assert_eq!(entry.source, "https://api.cantina.xyz/x");
        assert_eq!(entry.data, json!([1]));
    }
}
