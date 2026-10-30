//! HackenProof bug bounty programs and audit programs, from the public JSON API behind hackenproof.com.

mod detail;

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

pub use detail::render;

use super::common::{Candidate, Filters, JsonApi, Record, apply_filters, iso, lookup, started};
use crate::error::{Error, Result};
use crate::evm::explorers::first_address;
use crate::jv::{self, s};
use crate::opts::{Common, Kind};

pub const ID: &str = "hackenproof";
pub const NAME: &str = "HackenProof";
const SITE: &str = "https://hackenproof.com";
const PER_PAGE: u32 = 100; // the API's maximum
const MAX_PAGES: u32 = 50;

fn path(kind: Kind) -> &'static str {
    match kind {
        Kind::Program => "programs",
        Kind::Competition => "audits",
    }
}

fn page(kind: Kind) -> &'static str {
    match kind {
        Kind::Program => "programs",
        Kind::Competition => "audit-programs",
    }
}

fn api(cache_dir: &Path, opts: &Common) -> JsonApi {
    JsonApi::new(&format!("{SITE}/programs-api"), cache_dir, opts)
}

fn records(kind: Kind, opts: &Common, cache_dir: &Path) -> Result<Vec<Value>> {
    let api = api(cache_dir, opts);
    let mut items = Vec::new();
    for n in 1..=MAX_PAGES {
        let mut params = vec![("page", n.to_string()), ("per_page", PER_PAGE.to_string())];
        if kind == Kind::Program {
            params.insert(0, ("not_audits", "true".into()));
        }
        let data = api.get(&format!("{}-{n}", path(kind)), &format!("/{}", path(kind)), &params)?;
        items.extend(jv::arr(&data, path(kind)).iter().cloned());
        if !jv::t(&data, "next_page") {
            break;
        }
    }
    Ok(items)
}

pub fn status(record: &Value) -> String {
    if s(record, "state") == "paused" {
        return "paused".into();
    }
    let name = s(jv::at(record, "status"), "name").to_lowercase();
    match name.as_str() {
        "completed" => "finished".into(),
        "active" => if started(iso(jv::at(record, "start_date")).as_deref()) { "live" } else { "upcoming" }.into(),
        "" => s(record, "state").to_string(),
        _ => name,
    }
}

pub fn url(kind: Kind, slug: &str) -> String {
    format!("{SITE}/{}/{slug}", page(kind))
}

pub fn normalize(kind: Kind, record: &Value) -> Record {
    let slug = s(record, "slug");
    Record {
        source: ID.into(),
        id: json!(slug),
        target: format!("{ID}:{slug}"),
        kind: kind.as_str().into(),
        name: jv::opt(record, "title").map(str::to_string),
        url: url(kind, slug),
        status: status(record),
        reward: json!(jv::f(record, "max_bounty").unwrap_or(0.0)),
        currency: Some("USD".into()),
        kyc: record.get("kyc_required").and_then(Value::as_bool),
        start: iso(jv::at(record, "start_date")),
        end: if jv::t(record, "unending_program") { None } else { iso(jv::at(record, "end_date")) },
        updated: iso(jv::at(record, "updated_at")),
        raw: record.clone(),
    }
}

fn tags(row: &Record) -> HashMap<&'static str, Vec<String>> {
    let labels = jv::at(&row.raw, "labels");
    let list = |k: &str| jv::strs(labels, k).into_iter().map(str::to_string).collect();
    HashMap::from([
        ("ecosystem", list("platforms")),
        ("language", list("languages")),
        ("type", list("types")),
        ("project", list("project_types")),
    ])
}

pub fn listing(kind: Kind, f: &Filters, opts: &Common, cache_dir: &Path) -> Result<Vec<Record>> {
    let rows = records(kind, opts, cache_dir)?.iter().map(|r| normalize(kind, r)).collect();
    Ok(apply_filters(rows, f, kind, tags, NAME))
}

pub fn candidates(kind: Kind, opts: &Common, cache_dir: &Path) -> Result<Vec<Candidate>> {
    Ok(records(kind, opts, cache_dir)?
        .iter()
        .filter_map(|r| {
            let slug = jv::opt(r, "slug")?.to_string();
            let title = jv::opt(r, "title").map(str::to_string).unwrap_or_else(|| slug.clone());
            Some((slug.clone(), title, vec![slug, s(r, "title").to_string()]))
        })
        .collect())
}

static URL_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/(programs|audit-programs)/([\w-]+)/?$").unwrap());

pub fn identify(kind: Kind, target: &str, opts: &Common, cache_dir: &Path) -> Result<String> {
    if target.contains("://") {
        let path = url::Url::parse(target).map(|u| u.path().to_string()).unwrap_or_default();
        let Some(m) = URL_PATH.captures(&path) else {
            return Err(Error::value(format!("Expected a {NAME} program or audit program URL")));
        };
        let found = if &m[1] == "programs" { Kind::Program } else { Kind::Competition };
        if found != kind {
            return Err(Error::Lookup(format!("{target} is a {found}: bb {found} {ID}:{}", &m[2])));
        }
        return Ok(m[2].to_string());
    }
    lookup(ID, NAME, kind, target, |k| candidates(k, opts, cache_dir))
}

pub fn detail(kind: Kind, target: &str, opts: &Common, cache_dir: &Path) -> Result<Value> {
    let slug = identify(kind, target, opts, cache_dir)?;
    let record = api(cache_dir, opts).get(&format!("{kind}-{slug}"), &format!("/{}/{slug}", path(kind)), &[]).map_err(|e| match e {
        Error::NotFound(_) => Error::value(format!("No {NAME} {kind} named '{slug}'")),
        other => other,
    })?;
    let mut out = record.as_object().cloned().unwrap_or_default();
    out.insert("kind".into(), json!(kind.as_str()));
    out.insert("url".into(), json!(url(kind, &slug)));
    Ok(Value::Object(out))
}

/// In-scope targets that carry a contract address.
pub fn scope_assets(record: &Value) -> Vec<Value> {
    jv::arr(record, "scopes")
        .iter()
        .filter(|sc| !jv::t(sc, "out_of_scope") && first_address(s(sc, "target")).is_some())
        .map(|sc| json!({"url": s(sc, "target"), "type": "smart_contract", "description": s(sc, "title")}))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses() {
        assert_eq!(status(&json!({"state": "paused", "status": {"name": "Active"}})), "paused");
        assert_eq!(status(&json!({"status": {"name": "Completed"}})), "finished");
        assert_eq!(status(&json!({"status": {"name": "Active"}, "start_date": "01 Jan 2000"})), "live");
        assert_eq!(status(&json!({"status": {"name": "Active"}, "start_date": "01 Jan 2100"})), "upcoming");
    }

    #[test]
    fn normalize_and_scope() {
        let record = json!({"slug": "layer3", "title": "Layer3", "max_bounty": 50000, "kyc_required": true,
            "unending_program": true, "end_date": "01 Jan 2030", "status": {"name": "Active"},
            "scopes": [{"target": "https://etherscan.io/address/0xe1dee60c516a8350704ec24a6e856c9f533d1c1b", "title": "Token"},
                       {"target": "https://layer3.xyz", "title": "Site"},
                       {"target": "0xe1dee60c516a8350704ec24a6e856c9f533d1c1b", "out_of_scope": true}]});
        let row = normalize(Kind::Program, &record);
        assert_eq!((row.target.as_str(), row.end.clone(), row.kyc), ("hackenproof:layer3", None, Some(true)));
        assert_eq!(row.url, "https://hackenproof.com/programs/layer3");
        assert_eq!(scope_assets(&record).len(), 1);
        assert_eq!(url(Kind::Competition, "x"), "https://hackenproof.com/audit-programs/x");
    }
}
