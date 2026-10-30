//! Fetches Immunefi data, going through the cache first.
//!
//! Primary source: each page's React Server Components stream (`RSC: 1`), which is the exact
//! data the site renders, including the Resources tab and vault. Fallback, when a page cannot be
//! fetched or decoded and nothing is cached: `/public-api/bounties.json`, Immunefi's JSON API.
//! It holds every program and competition but no codebase/documentation links or vault data.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::cache::{Cache, Entry};
use crate::error::{Error, Result};
use crate::http;
use crate::jv;
use crate::opts::Kind;
use crate::rsc::Payload;

pub const BASE_URL: &str = "https://immunefi.com";
pub const PUBLIC_API_URL: &str = "https://immunefi.com/public-api/bounties.json";
const RSC_CACHE_KEY: &str = "immunefi-cli";
const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36 immunefi-cli/",
    env!("CARGO_PKG_VERSION")
);

pub fn kind_path(kind: Kind) -> &'static str {
    match kind {
        Kind::Program => "bug-bounty",
        Kind::Competition => "audit-competition",
    }
}

static SLUG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]*$").unwrap());
static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(bug-bounty|audit-competition)/([^/?#]+)").unwrap());

/// Accept a bare slug or any immunefi.com program/competition URL.
pub fn parse_target(value: &str) -> Result<(Option<Kind>, String)> {
    let (kind, slug) = match URL.captures(value) {
        Some(m) => (Some(if &m[1] == "bug-bounty" { Kind::Program } else { Kind::Competition }), m[2].to_string()),
        None => (None, value.trim().trim_matches('/').to_string()),
    };
    if !SLUG.is_match(&slug) {
        return Err(Error::value(format!("invalid slug: '{value}'")));
    }
    Ok((kind, slug))
}

#[derive(Debug, Clone)]
pub struct Fetched {
    pub data: Value,
    pub entry: Entry,
    pub from_cache: bool,
    pub stale: bool,
    /// Served from the public API fallback, which lacks some fields.
    pub degraded: bool,
}

fn fetch_page(url: &str) -> Result<Payload> {
    // _rsc is the cache-busting parameter Next.js itself adds, so shared caches never hand
    // back the HTML variant for this URL.
    let body =
        http::get(&format!("{url}?_rsc={RSC_CACHE_KEY}"), &[("User-Agent", USER_AGENT), ("RSC", "1"), ("Accept", "text/x-component")])?;
    let payload = Payload::new(&body)?;
    if payload.not_found {
        return Err(Error::NotFound(format!("{url} returned 404")));
    }
    Ok(payload)
}

fn api_kind(item: &Value) -> Kind {
    if jv::t(item, "endDate") { Kind::Competition } else { Kind::Program }
}

/// Shape a public-api/bounties.json entry like the `bounty` object pages carry.
pub fn from_public_api(item: &Value) -> Value {
    let tags: serde_json::Map<String, Value> = ["language", "ecosystem", "productType", "programType", "projectType"]
        .iter()
        .map(|k| (k.to_string(), if jv::t(item, k) { item[*k].clone() } else { json!([]) }))
        .collect();
    jv::merge(item, &json!({"tags": tags, "vaults": [], "source": "public-api"}))
}

pub struct Client {
    pub cache: Cache,
    pub refresh: bool,
    pub offline: bool,
}

impl Client {
    pub fn new(cache: Cache, refresh: bool, offline: bool) -> Self {
        Client { cache, refresh, offline }
    }

    fn cached(
        &self,
        key: &str,
        url: &str,
        fetch: impl FnOnce(&str) -> Result<Value>,
        fallback: Option<&dyn Fn() -> Result<Fetched>>,
    ) -> Result<Fetched> {
        let entry = self.cache.read(key);
        if let Some(e) = &entry {
            if self.offline || (!self.refresh && self.cache.is_fresh(e)) {
                return Ok(Fetched { data: e.data.clone(), entry: e.clone(), from_cache: true, stale: false, degraded: false });
            }
        }
        let fetched = if self.offline { Err(Error::fetch(format!("no cached data for '{key}' (offline mode)"), None)) } else { fetch(url) };
        let err = match fetched {
            Ok(data) => {
                let entry = self.cache.write(key, data.clone(), url)?;
                return Ok(Fetched { data, entry, from_cache: false, stale: false, degraded: false });
            }
            Err(e @ Error::NotFound(_)) => return Err(e),
            Err(e) => e,
        };
        if let Some(e) = entry {
            eprintln!("warning: {err}; using stale cache");
            return Ok(Fetched { data: e.data.clone(), entry: e, from_cache: true, stale: true, degraded: false });
        }
        let Some(fallback) = fallback else { return Err(err) };
        match fallback() {
            Ok(result) => {
                eprintln!("warning: {err}; using {PUBLIC_API_URL} instead");
                Ok(result)
            }
            Err(e @ Error::NotFound(_)) => Err(e),
            Err(_) => Err(err),
        }
    }

    /// Every program and competition from Immunefi's public JSON API (one ~6 MB document).
    pub fn dataset(&self) -> Result<Fetched> {
        self.cached(
            "public-api",
            PUBLIC_API_URL,
            |url| {
                let body = http::get(url, &[("User-Agent", USER_AGENT), ("Accept", "application/json")])?;
                let data: Value = serde_json::from_str(&body).map_err(|_| Error::Payload(format!("invalid JSON from {url}")))?;
                if !data.is_array() {
                    return Err(Error::Payload(format!("unexpected JSON shape from {url}")));
                }
                Ok(data)
            },
            None,
        )
    }

    fn listing_from_api(&self, kind: Kind) -> Result<Fetched> {
        let res = self.dataset()?;
        let items: Vec<Value> = res
            .data
            .as_array()
            .into_iter()
            .flatten()
            // The site lists invite-only competitions but hides invite-only programs.
            .filter(|x| api_kind(x) == kind && (kind == Kind::Competition || !jv::t(x, "inviteOnly")))
            .map(from_public_api)
            .collect();
        Ok(Fetched { data: Value::Array(items), degraded: true, ..res })
    }

    fn detail_from_api(&self, kind: Kind, slug: &str) -> Result<Fetched> {
        let res = self.dataset()?;
        let found = res.data.as_array().into_iter().flatten().find(|x| jv::s(x, "slug") == slug && api_kind(x) == kind);
        match found {
            Some(x) => Ok(Fetched { data: from_public_api(x), degraded: true, ..res.clone() }),
            None => Err(Error::NotFound(format!("'{slug}' is not in {PUBLIC_API_URL}"))),
        }
    }

    pub fn listing(&self, kind: Kind) -> Result<Fetched> {
        self.cached(
            &format!("{kind}s"),
            &format!("{BASE_URL}/{}/", kind_path(kind)),
            |url| fetch_page(url)?.find("bounties", Value::is_array),
            Some(&|| self.listing_from_api(kind)),
        )
    }

    pub fn detail(&self, kind: Kind, target: &str) -> Result<Fetched> {
        let (url_kind, slug) = parse_target(target)?;
        let kind = url_kind.unwrap_or(kind);
        let wanted = slug.clone();
        self.cached(
            &format!("{kind}/{slug}"),
            &format!("{BASE_URL}/{}/{slug}/information/", kind_path(kind)),
            move |url| {
                let page = fetch_page(url)?;
                let mut bounty = page.find("bounty", |b| b.is_object() && jv::s(b, "slug") == wanted)?;
                let vaults = page.find("vault", Value::is_object).map(|v| vec![v]).unwrap_or_default();
                bounty["vaults"] = Value::Array(vaults);
                Ok(bounty)
            },
            Some(&|| self.detail_from_api(kind, &slug)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert_eq!(parse_target("ssvnetwork").unwrap(), (None, "ssvnetwork".into()));
        assert_eq!(
            parse_target("https://immunefi.com/audit-competition/foo/information/").unwrap(),
            (Some(Kind::Competition), "foo".into())
        );
        assert!(parse_target("../etc").is_err());
    }

    #[test]
    fn public_api_shape() {
        let item = json!({"slug": "a", "language": ["Solidity"], "ecosystem": null});
        let shaped = from_public_api(&item);
        assert_eq!(shaped["tags"]["language"], json!(["Solidity"]));
        assert_eq!(shaped["tags"]["ecosystem"], json!([]));
        assert_eq!(shaped["source"], "public-api");
        assert_eq!(api_kind(&json!({"endDate": "2025-01-01"})), Kind::Competition);
    }

    #[test]
    fn offline_without_cache_fails_and_stale_cache_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let client = Client::new(Cache::new(dir.path(), 12.0), false, true);
        assert!(client.listing(Kind::Program).is_err());
        client.cache.write("programs", json!([{"slug": "x"}]), "u").unwrap();
        let got = client.listing(Kind::Program).unwrap();
        assert!(got.from_cache);
        assert_eq!(got.data, json!([{"slug": "x"}]));
    }
}
