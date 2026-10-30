//! Cantina's web application API. Only GET requests are made.
//!
//! A signed-in session (Firefox cookies or `CANTINA_COOKIE`) is sent to Cantina hosts only,
//! never follows a redirect to another origin, and its responses are never cached.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cache::{now, read_entry, write_atomic};
use crate::error::{Error, Result};
use crate::http;
use crate::jv;
use crate::paths::provider_cache;
use crate::rsc::{Payload, extract_flight, parse_rows, resolve};

const API: &str = "https://api.cantina.xyz";
const SITE: &str = "https://cantina.xyz";
const USER_AGENT: &str = "cantina-cli/0.2.0";
const ENDED: &[&str] = &["complete", "published", "migrated", "judging", "escalations", "escalations_ended"];

fn denied(code: u16) -> Error {
    Error::fetch(format!("Cantina denied access (HTTP {code}); authenticate or check eligibility"), Some(code))
}

/// Query data the site embedded in a saved Cantina page, by query name.
pub fn page_queries(body: &str) -> Result<Value> {
    let rows = parse_rows(&extract_flight(body)?);
    let mut found = serde_json::Map::new();
    fn walk(v: &Value, found: &mut serde_json::Map<String, Value>) {
        match v {
            Value::Object(map) => {
                if let Some(Value::String(name)) = map.get("queryKey").and_then(|k| k.get(1)) {
                    let data = jv::path(v, &["state", "data"]);
                    if !data.is_null() {
                        found.insert(name.clone(), data.clone());
                    }
                }
                map.values().for_each(|c| walk(c, found));
            }
            Value::Array(items) => items.iter().for_each(|c| walk(c, found)),
            _ => {}
        }
    }
    for value in rows.values() {
        walk(&resolve(value, &rows), &mut found);
    }
    if found.is_empty() {
        return Err(Error::value("Page has no recognized Cantina query data"));
    }
    Ok(Value::Object(found))
}

fn firefox_root() -> PathBuf {
    let home = crate::paths::home();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Firefox")
    } else if cfg!(windows) {
        dirs::config_dir().unwrap_or(home).join("Mozilla").join("Firefox")
    } else {
        home.join(".mozilla/firefox")
    }
}

/// The default profile from profiles.ini: the install default, else the one marked default.
fn default_profile() -> Result<PathBuf> {
    let root = firefox_root();
    let ini = ini::Ini::load_from_file(root.join("profiles.ini")).unwrap_or_default();
    let mut paths: Vec<String> = ini
        .iter()
        .filter(|(sec, _)| sec.is_some_and(|s| s.starts_with("Install")))
        .filter_map(|(_, p)| p.get("Default").map(str::to_string))
        .collect();
    if paths.is_empty() {
        paths = ini
            .iter()
            .filter(|(sec, p)| sec.is_some_and(|s| s.starts_with("Profile")) && p.get("Default") == Some("1"))
            .filter_map(|(_, p)| p.get("Path").map(str::to_string))
            .collect();
    }
    paths.sort();
    paths.dedup();
    if paths.len() != 1 {
        return Err(Error::value("Cannot select a unique default Firefox profile; provide its path"));
    }
    Ok(root.join(&paths[0]))
}

/// Cantina cookies from a Firefox profile, read from a private copy of its cookie database so
/// Firefox's own (possibly locked) files are never touched. Kept in memory only.
pub fn firefox_cookie(profile: &str) -> Result<String> {
    let dir = if profile == "auto" { default_profile()? } else { crate::paths::expand(Path::new(profile)) };
    let source = dir.canonicalize().unwrap_or(dir).join("cookies.sqlite");
    if !source.is_file() {
        return Err(Error::value("Firefox profile has no cookies.sqlite"));
    }
    // The temporary directory is private (0700) and removed even if querying fails.
    let tmp = tempfile::Builder::new().prefix("cantina-session-").tempdir()?;
    let dest = tmp.path().join("cookies.sqlite");
    for suffix in ["", "-wal"] {
        let candidate = PathBuf::from(format!("{}{suffix}", source.display()));
        if candidate.exists() {
            let target = PathBuf::from(format!("{}{suffix}", dest.display()));
            std::fs::copy(&candidate, &target)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    let sql = |e: rusqlite::Error| Error::value(format!("cannot read Firefox cookies: {e}"));
    let db = rusqlite::Connection::open(&dest).map_err(sql)?;
    let mut columns = db.prepare("PRAGMA table_info(moz_cookies)").map_err(sql)?;
    let has_origin = columns.query_map([], |r| r.get::<_, String>(1)).map_err(sql)?.flatten().any(|c| c == "originAttributes");
    let filter = if has_origin { " AND originAttributes=''" } else { "" };
    let mut query = db
        .prepare(&format!("SELECT name,value FROM moz_cookies WHERE host IN (?1, ?2, ?3, ?4) AND (expiry=0 OR expiry>?5){filter}"))
        .map_err(sql)?;
    let rows = query
        .query_map(rusqlite::params!["cantina.xyz", ".cantina.xyz", "api.cantina.xyz", ".api.cantina.xyz", now() as i64], |r| {
            Ok(format!("{}={}", r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql)?;
    Ok(rows.flatten().collect::<Vec<_>>().join("; "))
}

static SITE_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/(bounties|competitions)/[a-fA-F0-9-]+(?:/leaderboard)?$").unwrap());

/// Python's `urlencode(query, doseq=True)`.
pub fn urlencode(query: &[(&str, String)]) -> String {
    query
        .iter()
        .map(|(k, v)| format!("{k}={}", url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()))
        .collect::<Vec<_>>()
        .join("&")
}

pub struct Client {
    pub root: PathBuf,
    pub refresh: bool,
    pub offline: bool,
    pub ttl_seconds: f64,
    pub cookie: String,
    pub warnings: RefCell<Vec<String>>,
}

impl Client {
    pub fn new(cache_dir: Option<PathBuf>, refresh: bool, offline: bool, ttl_hours: f64, cookie: Option<String>) -> Self {
        Client {
            root: cache_dir.map(|d| crate::paths::expand(&d)).unwrap_or_else(|| provider_cache("cantina")),
            refresh,
            offline,
            ttl_seconds: ttl_hours * 3600.0,
            cookie: cookie.filter(|c| !c.is_empty()).or_else(|| std::env::var("CANTINA_COOKIE").ok()).unwrap_or_default(),
            warnings: RefCell::new(Vec::new()),
        }
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.warnings.borrow_mut().push(message.into());
    }

    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut self.warnings.borrow_mut())
    }

    /// GET with same-origin redirects only, and up to three attempts for throttling and 5xx.
    fn fetch(&self, url: &str, site: bool) -> Result<String> {
        let accept = if site { "text/html" } else { "application/json" };
        let mut headers = vec![("Accept", accept), ("User-Agent", USER_AGENT)];
        if !self.cookie.is_empty() {
            headers.push(("Cookie", self.cookie.as_str()));
        }
        let origin = url::Url::parse(url).map_err(|_| Error::value("Invalid API path"))?;
        let mut current = url.to_string();
        let mut attempt = 0;
        let mut hops = 0;
        loop {
            let reply = http::send(&current, &headers, None, false)
                .map_err(|_| Error::fetch("Cannot read Cantina API; check network access and API availability", None))?;
            match reply.status {
                200..=299 => return Ok(reply.body),
                301 | 302 | 303 | 307 | 308 if hops < 5 => {
                    let next = reply.location.as_deref().and_then(|l| url::Url::parse(&current).ok()?.join(l).ok());
                    match next {
                        Some(n) if n.scheme() == "https" && n.host_str() == origin.host_str() => {
                            current = n.to_string();
                            hops += 1;
                        }
                        _ => return Err(Error::value("Refusing API redirect to another origin")),
                    }
                }
                429 | 500 | 502 | 503 | 504 if attempt < 2 => {
                    let delay = reply.retry_after.unwrap_or(2f64.powi(attempt)).clamp(0.0, 10.0);
                    if !cfg!(test) {
                        std::thread::sleep(Duration::from_secs_f64(delay));
                    }
                    attempt += 1;
                }
                401 | 403 => return Err(denied(reply.status)),
                404 => return Err(Error::fetch("Opportunity or endpoint not found", Some(404))),
                code => return Err(Error::fetch(format!("Cantina API returned HTTP {code}"), Some(code))),
            }
        }
    }

    pub fn get(&self, path: &str, query: &[(&str, String)], cache: bool, site: bool) -> Result<Value> {
        if site {
            if !SITE_PATH.is_match(path) {
                return Err(Error::value("Invalid site path"));
            }
        } else if !path.starts_with("/api/v0/") || path.contains("..") {
            return Err(Error::value("Invalid API path"));
        }
        let mut url = format!("{}{path}", if site { SITE } else { API });
        if !query.is_empty() {
            url = format!("{url}?{}", urlencode(query));
        }
        // Authenticated records are never persisted in the public cache.
        let cache = cache && self.cookie.is_empty();
        let dest = self.root.join(format!("{}.json", hex::encode(Sha256::digest(url.as_bytes()))));
        let entry = if cache { read_entry(&dest) } else { None };
        if let Some(e) = &entry {
            if self.offline || (!self.refresh && now() - e.fetched_at < self.ttl_seconds) {
                return Ok(e.data.clone());
            }
        }
        if self.offline {
            return Err(Error::value("No cached response available for this request"));
        }
        let parsed = self.fetch(&url, site).and_then(|body| {
            if site {
                Ok(Value::String(body))
            } else {
                serde_json::from_str(&body)
                    .map_err(|_| Error::fetch("Cannot read Cantina API; check network access and API availability", None))
            }
        });
        let data = match parsed {
            Ok(data) => data,
            Err(e) if e.status().is_none() && !matches!(e, Error::Value(_)) => match entry {
                Some(stale) => {
                    let when = chrono::DateTime::from_timestamp(stale.fetched_at as i64, 0)
                        .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                        .unwrap_or_default();
                    self.warn(format!("Network failed; using stale cache fetched at {when}"));
                    return Ok(stale.data);
                }
                None => return Err(e),
            },
            Err(e) => return Err(e),
        };
        if cache {
            write_atomic(&dest, &json!({"url": url, "fetched_at": now(), "data": data}))?;
        }
        Ok(data)
    }

    pub fn api(&self, path: &str) -> Result<Value> {
        self.get(path, &[], true, false)
    }

    pub fn opportunities(&self, kinds: Option<&[&str]>, status: &str) -> Result<Vec<Value>> {
        let mut result = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen: Vec<String> = Vec::new();
        loop {
            let mut query = vec![("limit", "50".to_string()), ("status", status.to_string())];
            if let Some(k) = kinds {
                query.push(("kind", k.join(",")));
            }
            if let Some(c) = &cursor {
                query.push(("next", c.clone()));
            }
            let page = self.get("/api/v0/opportunities", &query, true, false).and_then(|p| {
                if p.get("items").is_some_and(Value::is_array) {
                    Ok(p)
                } else {
                    Err(Error::value("Unexpected opportunity response schema"))
                }
            });
            let page = match page {
                Ok(p) => p,
                Err(e) if matches!(e.status(), Some(401 | 403)) => return Err(e),
                Err(e) if cursor.is_some() => return Err(e), // never mix a partial listing with a fallback
                Err(_) => {
                    self.warn("Opportunity API unavailable; using public repository catalog");
                    let rows = self.catalog()?;
                    return Ok(rows
                        .into_iter()
                        .filter(|r| kinds.is_none_or(|k| k.contains(&jv::s(r, "kind"))))
                        .filter(|r| {
                            let st = jv::s(r, "status");
                            if status == "ended" { ENDED.contains(&st) } else { matches!(st, "live" | "upcoming") }
                        })
                        .collect());
                }
            };
            result.extend(jv::arr(&page, "items").iter().cloned());
            match jv::opt(&page, "nextValue") {
                None => return Ok(result),
                Some(next) => {
                    if seen.iter().any(|s| s == next) {
                        return Err(Error::value("API repeated a pagination cursor"));
                    }
                    seen.push(next.to_string());
                    cursor = Some(next.to_string());
                }
            }
        }
    }

    pub fn catalog(&self) -> Result<Vec<Value>> {
        let data = self.api("/api/v0/repositories/public")?;
        match data.as_array() {
            Some(rows) if rows.iter().all(|r| r.get("id").is_some()) => Ok(rows.clone()),
            _ => Err(Error::value("Unexpected public repository catalog schema")),
        }
    }

    pub fn detail(&self, uid: &str, private: bool) -> Result<Value> {
        if private {
            return self.api(&format!("/api/v0/repositories/{uid}"));
        }
        let mut record: Option<Value> = None;
        match self.api(&format!("/api/v0/repositories/{uid}/public")) {
            Ok(r) if jv::s(&r, "id") == uid => record = Some(r),
            Ok(_) => self.warn("Detail API unavailable; trying public catalog and page data"),
            Err(e) if matches!(e.status(), Some(401 | 403)) => return Err(e),
            Err(_) => self.warn("Detail API unavailable; trying public catalog and page data"),
        }
        match self.catalog() {
            Ok(rows) => {
                if let Some(extra) = rows.iter().find(|r| jv::s(r, "id") == uid) {
                    record = Some(jv::merge(extra, record.as_ref().unwrap_or(&Value::Null)));
                }
            }
            Err(e) if matches!(e.status(), Some(401 | 403)) => return Err(e),
            Err(_) => self.warn("Public catalog enrichment unavailable"),
        }
        if record.is_none() {
            for (kind, key) in [("bounties", "bounty"), ("competitions", "competition")] {
                let Ok(Value::String(body)) = self.get(&format!("/{kind}/{uid}"), &[], true, true) else { continue };
                let Ok(found) = Payload::new(&body).and_then(|p| p.find(key, |r| jv::s(r, "id") == uid)) else { continue };
                self.warn("Using page fallback; repository metadata may be incomplete");
                record = Some(found);
                break;
            }
        }
        record.ok_or_else(|| Error::fetch("Opportunity not found in public API, catalog or page", Some(404)))
    }

    pub fn leaderboard(&self, uid: &str) -> Result<Value> {
        match self.api(&format!("/api/v0/repositories/{uid}/leaderboard/teams")) {
            Ok(data) => Ok(json!({"available": true, "data": data})),
            // Cantina returns 404 while scores are unpublished. Do not invent ranks.
            Err(e) if e.status() == Some(404) => {
                Ok(json!({"available": false, "reason": "Cantina has not published a leaderboard for this competition"}))
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_matches_python_layout() {
        let dir = tempfile::tempdir().unwrap();
        let client = Client::new(Some(dir.path().to_path_buf()), false, true, 12.0, Some(String::new()));
        let url = "https://api.cantina.xyz/api/v0/opportunities?limit=50&status=live&kind=public_bounty%2Cprivate_bounty";
        let file = dir.path().join(format!("{}.json", hex::encode(Sha256::digest(url.as_bytes()))));
        std::fs::write(&file, r#"{"url": "x", "fetched_at": 0, "data": {"items": [{"id": "1"}]}}"#).unwrap();
        let items = client.opportunities(Some(&["public_bounty", "private_bounty"]), "live").unwrap();
        assert_eq!(items, vec![json!({"id": "1"})]);
    }

    #[test]
    fn rejects_paths_outside_the_api() {
        let client = Client::new(None, false, true, 12.0, None);
        assert!(client.get("/etc/passwd", &[], true, false).is_err());
        assert!(client.get("/api/v0/../x", &[], true, false).is_err());
        assert!(client.get("/bounties/../x", &[], true, true).is_err());
    }

    #[test]
    fn authenticated_responses_skip_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let client = Client::new(Some(dir.path().to_path_buf()), false, true, 12.0, Some("session=1".into()));
        let url = format!("{API}/api/v0/repositories/public");
        let file = dir.path().join(format!("{}.json", hex::encode(Sha256::digest(url.as_bytes()))));
        std::fs::write(&file, r#"{"url": "x", "fetched_at": 0, "data": []}"#).unwrap();
        assert!(client.catalog().is_err(), "a session must never read the public cache");
    }

    #[test]
    fn urlencode_like_python() {
        assert_eq!(urlencode(&[("kind", "a,b".into()), ("q", "x y".into())]), "kind=a%2Cb&q=x+y");
    }

    #[test]
    fn page_query_extraction() {
        let html =
            r#"<script>self.__next_f.push([1,"0:{\"queryKey\":[\"x\",\"bounty\"],\"state\":{\"data\":{\"id\":\"1\"}}}\n"])</script>"#;
        assert_eq!(page_queries(html).unwrap(), json!({"bounty": {"id": "1"}}));
    }
}
