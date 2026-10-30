//! Cached fetching for explorer and chain metadata, and the network seam tests replace.
//!
//! No platform requests are made here; providers adapt their scope data to this client.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::cache::Cache;
use crate::error::{Error, Result};

type HttpFn = dyn Fn(&str, &[(&str, &str)], Option<&[u8]>) -> Result<String> + Send + Sync;
type SleepFn = dyn Fn(f64) + Send + Sync;

/// How the EVM layer reaches the network and waits: the real thing, or a test double.
#[derive(Clone)]
pub struct Net {
    http: Arc<HttpFn>,
    sleep: Arc<SleepFn>,
    /// Seconds between Etherscan calls (the free tier allows 3 per second).
    pub etherscan_interval: f64,
    /// Seconds between calls to keyless explorer APIs and Sourcify.
    pub open_interval: f64,
}

impl Default for Net {
    fn default() -> Self {
        Net {
            http: Arc::new(crate::http::request),
            sleep: Arc::new(|secs| std::thread::sleep(Duration::from_secs_f64(secs.max(0.0)))),
            etherscan_interval: 0.35,
            open_interval: 0.2,
        }
    }
}

impl Net {
    /// A network served by `http`, with no waiting at all.
    pub fn fake(http: impl Fn(&str, &[(&str, &str)], Option<&[u8]>) -> Result<String> + Send + Sync + 'static) -> Self {
        Net { http: Arc::new(http), sleep: Arc::new(|_| {}), etherscan_interval: 0.0, open_interval: 0.0 }
    }

    /// GET (or POST with `body`); errors carry the HTTP status as [`crate::http::request`] does.
    pub fn request(&self, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Result<String> {
        (self.http)(url, headers, body)
    }

    pub fn get_json(&self, url: &str) -> Result<Value> {
        let body = self.request(url, &[("Accept", "application/json")], None)?;
        serde_json::from_str(&body).map_err(|_| Error::Payload(format!("invalid JSON from {}", crate::http::redact(url))))
    }

    pub fn sleep(&self, secs: f64) {
        if secs > 0.0 {
            (self.sleep)(secs)
        }
    }
}

/// A fetched or cached value.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub data: Value,
    pub from_cache: bool,
    pub stale: bool,
}

pub struct Client {
    pub cache: Cache,
    pub refresh: bool,
    pub offline: bool,
    pub net: Net,
}

impl Client {
    pub fn new(cache: Cache, refresh: bool, offline: bool) -> Self {
        Client { cache, refresh, offline, net: Net::default() }
    }

    /// The cached value for `key` while fresh (or always, offline); otherwise `fetch` it. A failed
    /// refetch falls back to a stale copy; "not found" never does.
    pub fn cached(&self, key: &str, url: &str, fetch: impl FnOnce() -> Result<Value>) -> Result<Fetched> {
        let entry = self.cache.read(key);
        if let Some(e) = &entry {
            if self.offline || (!self.refresh && self.cache.is_fresh(e)) {
                return Ok(Fetched { data: e.data.clone(), from_cache: true, stale: false });
            }
        }
        if self.offline {
            return Err(Error::fetch(format!("No cached EVM data for {key} (offline mode)"), None));
        }
        let data = match fetch() {
            Ok(data) => data,
            Err(err @ Error::NotFound(_)) => return Err(err),
            Err(err @ (Error::Fetch { .. } | Error::Chain(_) | Error::RateLimited(_) | Error::Reverted(_))) => {
                return match entry {
                    Some(e) => {
                        eprintln!("Warning: provider unavailable; using stale EVM cache");
                        Ok(Fetched { data: e.data, from_cache: true, stale: true })
                    }
                    None => Err(err),
                };
            }
            Err(err) => return Err(err),
        };
        self.cache.write(key, data.clone(), url)?;
        Ok(Fetched { data, from_cache: false, stale: false })
    }
}
