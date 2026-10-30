//! What every platform shares: the normalized record, listing filters, cached JSON GETs and
//! name lookup. Everything here is platform-neutral.

use std::path::Path;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::cache::Cache;
use crate::error::{Error, Result};
use crate::http;
use crate::opts::{Common, Kind};
use crate::util::close_matches;

pub const SECTIONS: &[&str] = &["overview", "information", "scope", "resources", "leaderboard", "all"];

/// One opportunity in the shape every platform is normalized to.
#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub source: String,
    pub id: Value,
    pub target: String,
    pub kind: String,
    pub name: Option<String>,
    pub url: String,
    pub status: String,
    pub reward: Value,
    pub currency: Option<String>,
    pub kyc: Option<bool>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub updated: Option<String>,
    pub raw: Value,
}

impl Record {
    pub fn reward(&self) -> f64 {
        crate::jv::num(&self.reward).unwrap_or(0.0)
    }

    pub fn id_text(&self) -> String {
        crate::jv::text(&self.id)
    }
}

/// The listing filters, as the user gave them.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub search: String,
    pub ecosystem: Option<String>,
    pub language: Option<String>,
    pub program_type: Option<String>,
    pub project_type: Option<String>,
    pub kyc: Option<bool>,
    pub min_bounty: f64,
    pub status: Option<String>,
    pub include_paused: bool,
    pub sort: Option<String>,
    pub reverse: bool,
}

impl Filters {
    pub fn has_tag_filter(&self) -> bool {
        self.ecosystem.is_some() || self.language.is_some() || self.program_type.is_some() || self.project_type.is_some()
    }

    /// (needle, tag category, label) for each tag filter given.
    pub fn tag_filters(&self) -> Vec<(&str, &'static str, &'static str)> {
        [
            (&self.ecosystem, "ecosystem", "ecosystem"),
            (&self.language, "language", "language"),
            (&self.program_type, "type", "asset type"),
            (&self.project_type, "project", "project type"),
        ]
        .into_iter()
        .filter_map(|(v, cat, label)| v.as_deref().map(|n| (n, cat, label)))
        .collect()
    }
}

/// Cached GETs against one JSON API; a stale copy is used when the API is unreachable.
pub struct JsonApi {
    base: String,
    cache: Cache,
    refresh: bool,
    offline: bool,
}

impl JsonApi {
    pub fn new(base: &str, cache_dir: &Path, opts: &Common) -> Self {
        JsonApi {
            base: base.trim_end_matches('/').to_string(),
            cache: Cache::new(cache_dir, opts.ttl_hours()),
            refresh: opts.refresh,
            offline: opts.offline,
        }
    }

    pub fn get(&self, key: &str, path: &str, params: &[(&str, String)]) -> Result<Value> {
        let mut url = format!("{}{}", self.base, path);
        if !params.is_empty() {
            let query: Vec<String> = params
                .iter()
                .map(|(k, v)| format!("{k}={}", url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()))
                .collect();
            url = format!("{url}?{}", query.join("&"));
        }
        let entry = self.cache.read(key);
        if let Some(e) = &entry {
            if self.offline || (!self.refresh && self.cache.is_fresh(e)) {
                return Ok(e.data.clone());
            }
        }
        if self.offline {
            return Err(Error::fetch(format!("No cached data for {key} (offline mode)"), None));
        }
        let fetched = http::get(&url, &[("Accept", "application/json")])
            .and_then(|body| serde_json::from_str::<Value>(&body).map_err(|e| Error::fetch(format!("invalid JSON from {url}: {e}"), None)));
        match fetched {
            Ok(data) => {
                self.cache.write(key, data.clone(), &url)?;
                Ok(data)
            }
            Err(e @ Error::NotFound(_)) => Err(e),
            Err(e) => match entry {
                Some(stale) => {
                    eprintln!("Warning: {url} unavailable ({e}); using a stale cached copy");
                    Ok(stale.data)
                }
                None => Err(Error::fetch(e.to_string(), e.status())),
            },
        }
    }
}

/// Epoch seconds or a `25 Oct 2026` style date as an ISO 8601 UTC string, so sources sort together.
pub fn iso(value: &Value) -> Option<String> {
    let dt: DateTime<Utc> = match value {
        Value::Number(n) => {
            let secs = n.as_f64()?;
            Utc.timestamp_opt(secs.trunc() as i64, ((secs.fract()) * 1e9) as u32).single()?
        }
        Value::String(s) if !s.is_empty() => {
            let day = NaiveDate::parse_from_str(s.trim(), "%d %b %Y").ok()?;
            Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0)?)
        }
        _ => return None,
    };
    Some(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

pub fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

pub fn started(start: Option<&str>) -> bool {
    start.is_none_or(|s| s.is_empty() || s <= now_iso().as_str())
}

/// Tag lists of a record by category: ecosystem, language, type, project.
pub type Tags = fn(&Record) -> std::collections::HashMap<&'static str, Vec<String>>;

/// The generic listing filters over normalized rows.
pub fn apply_filters(rows: Vec<Record>, f: &Filters, kind: Kind, tags: Tags, name: &str) -> Vec<Record> {
    let wanted = f.status.as_deref();
    let mut out: Vec<Record> = rows
        .into_iter()
        .filter(|row| {
            let status = row.status.as_str();
            let keep = match wanted {
                None => kind != Kind::Program || matches!(status, "live" | "upcoming") || (f.include_paused && status == "paused"),
                Some("active") => matches!(status, "live" | "upcoming"),
                Some("ended") => matches!(status, "evaluating" | "finished"),
                Some("all") => true,
                Some(w) => status == w,
            };
            if !keep {
                return false;
            }
            let hay = [row.id_text(), row.name.clone().unwrap_or_default(), crate::jv::s(&row.raw, "company_name").to_string()]
                .join(" ")
                .to_lowercase();
            if !f.search.is_empty() && !hay.contains(&f.search.to_lowercase()) {
                return false;
            }
            if row.reward() < f.min_bounty {
                return false;
            }
            if let Some(k) = f.kyc {
                if row.kyc != Some(k) {
                    return false;
                }
            }
            true
        })
        .collect();
    for (needle, category, label) in f.tag_filters() {
        if !out.iter().any(|r| tags(r).get(category).is_some_and(|v| !v.is_empty())) {
            eprintln!("Note: {name} has no {label} tags; its {kind}s are left out");
        }
        let needle = needle.to_lowercase();
        out.retain(|r| tags(r).get(category).is_some_and(|v| v.iter().any(|t| t.to_lowercase().contains(&needle))));
    }
    out
}

/// (target id, display name, match keys) for one opportunity.
pub type Candidate = (String, String, Vec<String>);

fn matches_key(wanted: &str, keys: &[String]) -> bool {
    keys.iter().any(|k| !k.is_empty() && k.to_lowercase() == wanted)
}

/// The id of the one candidate whose id, name or handle equals `value` (case-insensitively).
pub fn pick(value: &str, rows: &[Candidate], kind: Kind, name: &str) -> Result<String> {
    let wanted = value.to_lowercase();
    let matches = crate::util::dedupe(rows.iter().filter(|(_, _, keys)| matches_key(&wanted, keys)).map(|(t, _, _)| t.clone()));
    if matches.len() == 1 {
        return Ok(matches[0].clone());
    }
    let label = |target: &str| {
        rows.iter().find(|(t, _, _)| t == target).map(|(t, n, _)| format!("{n} ({t})")).unwrap_or_else(|| target.to_string())
    };
    if !matches.is_empty() {
        let list: Vec<String> = matches.iter().take(5).map(|m| label(m)).collect();
        return Err(Error::value(format!("'{value}' matches several {name} {kind}s: {}", list.join(", "))));
    }
    let names: Vec<String> = rows.iter().map(|(_, n, _)| n.to_lowercase()).collect();
    let hints: Vec<String> = close_matches(&wanted, names.iter().map(String::as_str), 3, 0.6)
        .into_iter()
        .filter_map(|n| rows.iter().find(|(_, rn, _)| rn.to_lowercase() == n).map(|(t, rn, _)| format!("{rn} ({t})")))
        .collect();
    let hint = if hints.is_empty() { String::new() } else { format!("; did you mean: {}", hints.join(", ")) };
    Err(Error::value(format!("No {name} {kind} named '{value}'{hint}")))
}

/// [`pick`] among `candidates(kind)`; a value that names the other kind gets the right command.
pub fn lookup(source: &str, name: &str, kind: Kind, value: &str, candidates: impl Fn(Kind) -> Result<Vec<Candidate>>) -> Result<String> {
    match pick(value, &candidates(kind)?, kind, name) {
        Ok(id) => Ok(id),
        Err(err) => {
            let other = kind.other();
            let wanted = value.to_lowercase();
            if let Some((target, _, _)) = candidates(other)?.into_iter().find(|(_, _, keys)| matches_key(&wanted, keys)) {
                return Err(Error::Lookup(format!("'{value}' is a {name} {other}: bb {other} {source}:{target}")));
            }
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(status: &str, reward: f64) -> Record {
        Record {
            source: "x".into(),
            id: json!("a"),
            target: "x:a".into(),
            kind: "program".into(),
            name: Some("Alpha".into()),
            url: String::new(),
            status: status.into(),
            reward: json!(reward),
            currency: None,
            kyc: Some(false),
            start: None,
            end: None,
            updated: None,
            raw: json!({}),
        }
    }

    fn no_tags(_: &Record) -> std::collections::HashMap<&'static str, Vec<String>> {
        Default::default()
    }

    #[test]
    fn iso_dates() {
        assert_eq!(iso(&json!(0)), Some("1970-01-01T00:00:00Z".into()));
        assert_eq!(iso(&json!("25 Oct 2026")), Some("2026-10-25T00:00:00Z".into()));
        assert_eq!(iso(&json!("bad")), None);
        assert_eq!(iso(&Value::Null), None);
    }

    #[test]
    fn default_program_filter_hides_finished_and_paused() {
        let rows = vec![row("live", 1.0), row("finished", 1.0), row("paused", 1.0)];
        let kept = apply_filters(rows.clone(), &Filters::default(), Kind::Program, no_tags, "X");
        assert_eq!(kept.len(), 1);
        let f = Filters { include_paused: true, ..Default::default() };
        assert_eq!(apply_filters(rows.clone(), &f, Kind::Program, no_tags, "X").len(), 2);
        let f = Filters { status: Some("ended".into()), ..Default::default() };
        assert_eq!(apply_filters(rows.clone(), &f, Kind::Program, no_tags, "X").len(), 1);
        let f = Filters { min_bounty: 5.0, status: Some("all".into()), ..Default::default() };
        assert!(apply_filters(rows.clone(), &f, Kind::Program, no_tags, "X").is_empty());
        let f = Filters { language: Some("rust".into()), ..Default::default() };
        assert!(apply_filters(rows, &f, Kind::Program, no_tags, "X").is_empty());
    }

    #[test]
    fn pick_and_lookup() {
        let rows: Vec<Candidate> = vec![("1".into(), "Alpha".into(), vec!["1".into(), "Alpha".into()])];
        assert_eq!(pick("alpha", &rows, Kind::Program, "X").unwrap(), "1");
        let err = pick("alpah", &rows, Kind::Program, "X").unwrap_err().to_string();
        assert!(err.contains("did you mean: Alpha (1)"), "{err}");
        let other: Vec<Candidate> = vec![("9".into(), "Beta".into(), vec!["9".into(), "Beta".into()])];
        let found = lookup("x", "X", Kind::Program, "beta", |k| Ok(if k == Kind::Program { rows.clone() } else { other.clone() }));
        assert_eq!(found.unwrap_err(), Error::Lookup("'beta' is a X competition: bb competition x:9".into()));
    }
}
