//! Sherlock bug bounties and audit contests, from the public API behind audits.sherlock.xyz.

mod detail;

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

pub use detail::render;

use super::common::{Candidate, Filters, JsonApi, Record, apply_filters, iso, lookup, started};
use crate::error::{Error, Result};
use crate::jv::{self, s};
use crate::opts::{Common, Kind};

pub const ID: &str = "sherlock";
pub const NAME: &str = "Sherlock";
const API: &str = "https://mainnet-contest.sherlock.xyz";
const SITE: &str = "https://audits.sherlock.xyz";

fn path(kind: Kind) -> &'static str {
    match kind {
        Kind::Program => "bug_bounties",
        Kind::Competition => "contests",
    }
}

fn page(kind: Kind) -> &'static str {
    match kind {
        Kind::Program => "bug-bounties",
        Kind::Competition => "contests",
    }
}

fn records(kind: Kind, opts: &Common, cache_dir: &Path) -> Result<Vec<Value>> {
    let api = JsonApi::new(API, cache_dir, opts);
    let mut items = Vec::new();
    for n in 1.. {
        let data =
            api.get(&format!("{}-{n}", path(kind)), &format!("/{}", path(kind)), &[("page", n.to_string()), ("per_page", "500".into())])?;
        items.extend(jv::arr(&data, "items").iter().cloned());
        if !jv::t(&data, "has_next") {
            break;
        }
    }
    Ok(items)
}

pub fn status(kind: Kind, record: &Value) -> String {
    if kind == Kind::Program {
        let ready = record.get("is_ready").is_none_or(jv::truthy);
        return if ready { "live" } else { "upcoming" }.into();
    }
    let raw = s(record, "status");
    let value = match raw {
        "CREATED" => "upcoming".to_string(),
        "RUNNING" => "live".to_string(),
        "JUDGING" | "ESCALATING" | "SHERLOCK_JUDGING" => "evaluating".to_string(),
        "FINISHED" => "finished".to_string(),
        other => other.to_lowercase(),
    };
    if value == "live" && !started(iso(jv::at(record, "starts_at")).as_deref()) { "upcoming".into() } else { value }
}

pub fn url(kind: Kind, uid: &str) -> String {
    format!("{SITE}/{}/{uid}", page(kind))
}

pub fn normalize(kind: Kind, record: &Value) -> Record {
    let uid = jv::text(jv::at(record, "id"));
    let (reward, currency, start, end) = match kind {
        Kind::Program => (jv::at(record, "payout"), jv::at(record, "display_currency"), jv::at(record, "live_since"), &Value::Null),
        Kind::Competition => (jv::at(record, "rewards"), jv::at(record, "token"), jv::at(record, "starts_at"), jv::at(record, "ends_at")),
    };
    Record {
        source: ID.into(),
        id: json!(uid),
        target: format!("{ID}:{uid}"),
        kind: kind.as_str().into(),
        name: jv::opt(record, "title").map(str::to_string),
        url: url(kind, &uid),
        status: status(kind, record),
        reward: json!(jv::num(reward).unwrap_or(0.0)),
        currency: Some(currency.as_str().filter(|c| !c.is_empty()).unwrap_or("USDC").to_string()),
        kyc: record.get("requires_kyc").and_then(Value::as_bool),
        start: iso(start),
        end: iso(end),
        updated: iso(jv::at(record, "last_updated")),
        raw: record.clone(),
    }
}

fn tags(row: &Record) -> HashMap<&'static str, Vec<String>> {
    let raw = &row.raw;
    let strings = |v: &[Value]| v.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>();
    HashMap::from([
        ("project", jv::arr(raw, "categories").iter().map(|c| s(c, "name").to_string()).filter(|n| !n.is_empty()).collect()),
        ("language", strings(jv::arr(raw, "languages"))),
        ("type", jv::opt(raw, "type_label").map(|t| vec![t.to_string()]).unwrap_or_default()),
    ])
}

pub fn listing(kind: Kind, f: &Filters, opts: &Common, cache_dir: &Path) -> Result<Vec<Record>> {
    let rows = records(kind, opts, cache_dir)?.iter().map(|r| normalize(kind, r)).collect();
    Ok(apply_filters(rows, f, kind, tags, NAME))
}

pub fn candidates(kind: Kind, opts: &Common, cache_dir: &Path) -> Result<Vec<Candidate>> {
    Ok(records(kind, opts, cache_dir)?
        .iter()
        .map(|r| {
            let id = jv::text(jv::at(r, "id"));
            let title = jv::opt(r, "title").map(str::to_string).unwrap_or_else(|| id.clone());
            (id.clone(), title, vec![id, s(r, "title").to_string()])
        })
        .collect())
}

static URL_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/(bug-bounties|contests)/(\d+)/?$").unwrap());

pub fn identify(kind: Kind, target: &str, opts: &Common, cache_dir: &Path) -> Result<String> {
    if target.contains("://") {
        let path = url::Url::parse(target).map(|u| u.path().to_string()).unwrap_or_default();
        let Some(m) = URL_PATH.captures(&path) else {
            return Err(Error::value(format!("Expected a {NAME} bug bounty or contest URL")));
        };
        let found = if &m[1] == "bug-bounties" { Kind::Program } else { Kind::Competition };
        if found != kind {
            return Err(Error::Lookup(format!("{target} is a {found}: bb {found} {ID}:{}", &m[2])));
        }
        return Ok(m[2].to_string());
    }
    lookup(ID, NAME, kind, target, |k| candidates(k, opts, cache_dir))
}

pub fn detail(kind: Kind, target: &str, opts: &Common, cache_dir: &Path) -> Result<Value> {
    let uid = identify(kind, target, opts, cache_dir)?;
    let record =
        JsonApi::new(API, cache_dir, opts).get(&format!("{kind}-{uid}"), &format!("/{}/{uid}", path(kind)), &[]).map_err(|e| match e {
            Error::NotFound(_) => Error::value(format!("No {NAME} {kind} with id {uid}")),
            other => other,
        })?;
    let mut out = record.as_object().cloned().unwrap_or_default();
    out.insert("kind".into(), json!(kind.as_str()));
    out.insert("url".into(), json!(url(kind, &uid)));
    Ok(Value::Object(out))
}

/// Deployed contracts in scope; each names its network, so no explorer link is needed.
pub fn scope_assets(record: &Value) -> Vec<Value> {
    jv::arr(record, "covered_scope")
        .iter()
        .filter(|c| jv::opt(c, "address").is_some() && c.get("selected").is_none_or(jv::truthy))
        .map(|c| {
            json!({"url": "", "type": "smart_contract", "address": s(c, "address"),
                   "chain": s(c, "network").to_lowercase(), "description": s(c, "display_name")})
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_contests_and_bounties() {
        let contest = json!({"id": 7, "title": "Alpha", "status": "JUDGING", "rewards": 50000, "token": "USDC",
                              "starts_at": 1700000000, "ends_at": 1700500000, "requires_kyc": false});
        let row = normalize(Kind::Competition, &contest);
        assert_eq!(row.status, "evaluating");
        assert_eq!(row.target, "sherlock:7");
        assert_eq!(row.start.as_deref(), Some("2023-11-14T22:13:20Z"));
        assert_eq!(row.url, "https://audits.sherlock.xyz/contests/7");
        let future = json!({"id": 8, "status": "RUNNING", "starts_at": 4102444800i64});
        assert_eq!(status(Kind::Competition, &future), "upcoming");
        let bounty = json!({"id": 56, "title": "Usual", "payout": "250000", "is_ready": true});
        let row = normalize(Kind::Program, &bounty);
        assert_eq!((row.status.as_str(), row.reward()), ("live", 250000.0));
        assert_eq!(row.currency.as_deref(), Some("USDC"));
    }

    #[test]
    fn scope_uses_selected_contracts() {
        let record = json!({"covered_scope": [
            {"address": "0xabc", "network": "Ethereum", "display_name": "Vault"},
            {"address": "0xdef", "network": "Base", "selected": false},
            {"repo_name": "org/repo", "file_path": "a.sol"}]});
        let assets = scope_assets(&record);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0]["chain"], "ethereum");
    }

    #[test]
    fn url_kind_mismatch_names_the_right_command() {
        let opts = Common::default();
        let err = identify(Kind::Program, "https://audits.sherlock.xyz/contests/12", &opts, Path::new("/nonexistent")).unwrap_err();
        assert_eq!(err.to_string(), "https://audits.sherlock.xyz/contests/12 is a competition: bb competition sherlock:12");
        assert_eq!(identify(Kind::Competition, "https://audits.sherlock.xyz/contests/12/", &opts, Path::new("/x")).unwrap(), "12");
    }
}
