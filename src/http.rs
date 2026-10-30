//! Blocking HTTP with one shared agent: gzip, timeouts and status codes mapped onto [`Error`].

use std::sync::OnceLock;
use std::time::Duration;

use ureq::Agent;

use crate::error::{Error, Result};

pub const USER_AGENT: &str = concat!("bug-bounty-cli/", env!("CARGO_PKG_VERSION"));
const TIMEOUT: Duration = Duration::from_secs(30);
/// Immunefi's public API document alone is ~6 MB.
const BODY_LIMIT: u64 = 256 * 1024 * 1024;

fn agent(follow_redirects: bool) -> &'static Agent {
    static FOLLOW: OnceLock<Agent> = OnceLock::new();
    static MANUAL: OnceLock<Agent> = OnceLock::new();
    let cell = if follow_redirects { &FOLLOW } else { &MANUAL };
    cell.get_or_init(|| {
        Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(if follow_redirects { 10 } else { 0 })
            .user_agent(USER_AGENT)
            .build()
            .into()
    })
}

/// A completed exchange: any status, with the body read.
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub location: Option<String>,
    pub retry_after: Option<f64>,
}

/// One request. `body` makes it a POST. Redirects are followed unless `follow` is false,
/// in which case the 3xx reply is returned for the caller to vet.
pub fn send(url: &str, headers: &[(&str, &str)], body: Option<&[u8]>, follow: bool) -> Result<Reply> {
    #[cfg(test)]
    if let Some(reply) = stub::answer(url, headers, body) {
        return reply;
    }
    let agent = agent(follow);
    let result = match body {
        Some(data) => {
            let mut req = agent.post(url);
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            req.send(data)
        }
        None => {
            let mut req = agent.get(url);
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            req.call()
        }
    };
    let mut resp = result.map_err(|e| Error::fetch(format!("could not reach {}: {}", redact(url), e), None))?;
    let status = resp.status().as_u16();
    let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let location = header("location");
    let retry_after = header("retry-after").and_then(|v| v.trim().parse().ok());
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(BODY_LIMIT)
        .read_to_vec()
        .map_err(|e| Error::fetch(format!("could not read {}: {}", redact(url), e), None))?;
    Ok(Reply { status, body: String::from_utf8_lossy(&bytes).into_owned(), location, retry_after })
}

/// GET (or POST with `body`) returning the body of a 2xx reply; 404 becomes [`Error::NotFound`].
pub fn request(url: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Result<String> {
    let reply = send(url, headers, body, true)?;
    match reply.status {
        200..=299 => Ok(reply.body),
        404 => Err(Error::NotFound(format!("{} returned 404", redact(url)))),
        code => Err(Error::fetch(format!("{} returned HTTP {code}", redact(url)), Some(code))),
    }
}

pub fn get(url: &str, headers: &[(&str, &str)]) -> Result<String> {
    request(url, headers, None)
}

pub fn get_json(url: &str) -> Result<serde_json::Value> {
    let body = get(url, &[("Accept", "application/json")])?;
    serde_json::from_str(&body).map_err(|_| Error::Payload(format!("invalid JSON from {}", redact(url))))
}

/// Never echo credentials that ride in a URL: query keys and long path tokens are masked.
pub fn redact(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else { return url.to_string() };
    let secret = |k: &str| matches!(k.to_ascii_lowercase().as_str(), "apikey" | "api_key" | "key" | "token");
    if parsed.query().is_some() {
        let pairs: Vec<(String, String)> =
            parsed.query_pairs().map(|(k, v)| (k.to_string(), if secret(&k) { "***".into() } else { v.to_string() })).collect();
        parsed.query_pairs_mut().clear().extend_pairs(pairs);
    }
    let path: Vec<String> = parsed
        .path()
        .split('/')
        .map(|p| if p.len() >= 20 && p.chars().all(|c| c.is_ascii_hexdigit()) { "***".to_string() } else { p.to_string() })
        .collect();
    parsed.set_path(&path.join("/"));
    parsed.to_string().replace("%2A%2A%2A", "***")
}

/// Unit tests answer requests on their own thread instead of reaching the network.
#[cfg(test)]
pub mod stub {
    use std::cell::RefCell;

    use super::Reply;
    use crate::error::Result;

    type Handler = Box<dyn FnMut(&str, &[(&str, &str)], Option<&[u8]>) -> Result<Reply>>;

    thread_local! {
        static HANDLER: RefCell<Option<Handler>> = RefCell::new(None);
    }

    pub(super) fn answer(url: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Option<Result<Reply>> {
        HANDLER.with(|h| h.borrow_mut().as_mut().map(|f| f(url, headers, body)))
    }

    /// Serve this thread's requests with `handler` until the guard is dropped.
    pub fn serve(handler: impl FnMut(&str, &[(&str, &str)], Option<&[u8]>) -> Result<Reply> + 'static) -> Guard {
        HANDLER.with(|h| *h.borrow_mut() = Some(Box::new(handler)));
        Guard
    }

    pub struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            HANDLER.with(|h| *h.borrow_mut() = None);
        }
    }

    pub fn reply(status: u16, body: &str) -> Result<Reply> {
        Ok(Reply { status, body: body.to_string(), location: None, retry_after: Some(0.0) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_keys_and_tokens() {
        let url = "https://api.etherscan.io/v2/api?chainid=1&apikey=SECRET";
        assert!(!redact(url).contains("SECRET"));
        let node = "https://name.quiknode.pro/0123456789abcdef0123456789abcdef/";
        assert!(!redact(node).contains("0123456789abcdef0123"));
        assert_eq!(redact("https://example.com/a"), "https://example.com/a");
    }
}
