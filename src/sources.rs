//! Platform registry and the dispatch from generic commands to each platform.
//!
//! Everything platform-specific the router needs is declared here, so help text and error
//! messages stay generic and name a platform only when the user selected or targeted one.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::cache::Cache;
use crate::error::{Error, Result};
use crate::evm::contract_cli::{ScopeRecord, ScopeSource};
use crate::jv::{self, s};
use crate::opts::{Common, Kind};
use crate::providers::common::{Candidate, Filters, Record};
use crate::providers::{cantina, hackenproof, immunefi, sherlock};
use crate::render::Style;

pub struct Source {
    pub id: &'static str,
    pub name: &'static str,
    pub site: &'static str,
    pub hosts: &'static [&'static str],
    /// The platform's own pages listing programs and competitions.
    pub program_listing: &'static str,
    pub competition_listing: &'static str,
    /// The platform's own command line (`bb <id> ...`), when it has one.
    pub native: Option<fn(Vec<OsString>) -> i32>,
    /// Platform-only commands also reachable as `bb <command>`.
    pub commands: &'static [&'static str],
    /// Optional capabilities: 'private', 'session', 'vault'.
    pub features: &'static [&'static str],
}

impl Source {
    pub fn listing_url(&self, kind: Kind) -> &'static str {
        match kind {
            Kind::Program => self.program_listing,
            Kind::Competition => self.competition_listing,
        }
    }

    /// Served in-process from a public JSON API (no native command line).
    pub fn served(&self) -> bool {
        self.native.is_none()
    }

    pub fn has_feature(&self, feature: &str) -> bool {
        self.features.contains(&feature)
    }
}

pub static SOURCES: &[Source] = &[
    Source {
        id: "immunefi",
        name: "Immunefi",
        site: "https://immunefi.com",
        hosts: &["immunefi.com", "www.immunefi.com"],
        program_listing: "https://immunefi.com/bug-bounty/",
        competition_listing: "https://immunefi.com/audit-competition/",
        native: Some(immunefi::cli::main),
        commands: &[],
        features: &["vault"],
    },
    Source {
        id: "cantina",
        name: "Cantina",
        site: "https://cantina.xyz",
        hosts: &["cantina.xyz", "www.cantina.xyz"],
        program_listing: "https://cantina.xyz/bounties",
        competition_listing: "https://cantina.xyz/competitions",
        native: Some(cantina::cli::main),
        commands: &["auth", "leaderboard", "import-page", "opportunities"],
        features: &["private", "session"],
    },
    Source {
        id: "sherlock",
        name: "Sherlock",
        site: "https://audits.sherlock.xyz",
        hosts: &["audits.sherlock.xyz", "app.sherlock.xyz", "sherlock.xyz", "www.sherlock.xyz"],
        program_listing: "https://audits.sherlock.xyz/bug-bounties",
        competition_listing: "https://audits.sherlock.xyz/contests",
        native: None,
        commands: &[],
        features: &[],
    },
    Source {
        id: "hackenproof",
        name: "HackenProof",
        site: "https://hackenproof.com",
        hosts: &["hackenproof.com", "www.hackenproof.com", "dashboard.hackenproof.com"],
        program_listing: "https://hackenproof.com/programs",
        competition_listing: "https://hackenproof.com/audit-programs",
        native: None,
        commands: &[],
        features: &[],
    },
];

pub fn ids() -> Vec<&'static str> {
    SOURCES.iter().map(|s| s.id).collect()
}

pub fn get(id: &str) -> Option<&'static Source> {
    SOURCES.iter().find(|s| s.id == id)
}

pub fn source(id: &str) -> &'static Source {
    get(id).unwrap_or_else(|| panic!("unknown source {id}"))
}

pub fn for_host(host: &str) -> Option<&'static str> {
    SOURCES.iter().find(|s| s.hosts.contains(&host)).map(|s| s.id)
}

pub fn with_feature(feature: &str) -> Vec<&'static Source> {
    SOURCES.iter().filter(|s| s.has_feature(feature)).collect()
}

pub fn with_command(command: &str) -> Vec<&'static Source> {
    SOURCES.iter().filter(|s| s.commands.contains(&command)).collect()
}

fn immunefi_client(opts: &Common, cache_dir: &Path) -> immunefi::client::Client {
    immunefi::client::Client::new(Cache::new(cache_dir, opts.ttl_hours()), opts.refresh, opts.offline)
}

fn cantina_client(opts: &Common, cache_dir: &Path) -> Result<cantina::client::Client> {
    let cookie = match &opts.firefox_profile {
        Some(p) => Some(cantina::client::firefox_cookie(p)?),
        None => None,
    };
    Ok(cantina::client::Client::new(Some(cache_dir.to_path_buf()), opts.refresh, opts.offline, opts.ttl_hours(), cookie))
}

fn print_warnings(client: &cantina::client::Client) {
    for w in client.take_warnings() {
        eprintln!("Warning: {w}");
    }
}

/// Every opportunity of `kind` on `source` as (target id, display name, match keys).
pub fn candidates(source: &str, kind: Kind, opts: &Common, cache_dir: &Path) -> Result<Vec<Candidate>> {
    match source {
        "sherlock" => sherlock::candidates(kind, opts, cache_dir),
        "hackenproof" => hackenproof::candidates(kind, opts, cache_dir),
        "immunefi" => {
            let data = immunefi_client(opts, cache_dir).listing(kind)?.data;
            Ok(data
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| {
                    let slug = jv::opt(r, "slug")?.to_string();
                    let project = jv::opt(r, "project").unwrap_or(&slug).to_string();
                    Some((slug.clone(), project, vec![slug, s(r, "project").to_string()]))
                })
                .collect())
        }
        _ => {
            let client = cantina_client(opts, cache_dir)?;
            let contest = kind == Kind::Competition;
            Ok(client
                .catalog()?
                .iter()
                .filter(|r| s(r, "kind").contains("contest") == contest)
                .map(|r| {
                    let id = s(r, "id").to_string();
                    let name = jv::opt(r, "name").unwrap_or(&id).to_string();
                    (id.clone(), name, vec![id, s(r, "name").into(), s(jv::at(r, "company"), "handle").into()])
                })
                .collect())
        }
    }
}

pub fn normalize(source: &str, kind: Kind, record: &Value) -> Record {
    if source == "immunefi" {
        let status = match kind {
            Kind::Program => immunefi::render::program_status(record).to_string(),
            Kind::Competition => immunefi::render::competition_status(record),
        };
        let uid = s(record, "slug");
        let path = immunefi::client::kind_path(kind);
        let reward = jv::first(record, &["rewardsPool", "maxBounty"]);
        return Record {
            source: source.into(),
            id: jv::at(record, "slug").clone(),
            target: format!("{source}:{uid}"),
            kind: kind.as_str().into(),
            name: jv::opt(record, "project").map(str::to_string),
            url: jv::opt(record, "url").map(str::to_string).unwrap_or_else(|| format!("https://immunefi.com/{path}/{uid}/information/")),
            status: status.to_lowercase(),
            reward: if reward.is_null() { json!(0) } else { reward.clone() },
            currency: Some("USD".into()),
            kyc: jv::at(record, "kyc").as_bool(),
            start: jv::opt(record, "launchDate").map(str::to_string),
            end: jv::opt(record, "endDate").map(str::to_string),
            updated: jv::opt(record, "updatedDate").map(str::to_string),
            raw: record.clone(),
        };
    }
    let id = s(record, "id");
    let timeframe = jv::at(record, "timeframe");
    Record {
        source: source.into(),
        id: jv::at(record, "id").clone(),
        target: format!("{source}:{id}"),
        kind: kind.as_str().into(),
        name: jv::opt(record, "name").map(str::to_string),
        url: jv::opt(record, "url")
            .map(str::to_string)
            .unwrap_or_else(|| format!("https://cantina.xyz/{}{id}", if kind == Kind::Program { "bounties/" } else { "competitions/" })),
        status: cantina::discovery::status(record),
        reward: json!(jv::f(record, "totalRewardPot").unwrap_or(0.0)),
        currency: jv::opt(record, "currencyCode").map(str::to_string),
        kyc: jv::at(record, "kycRequired").as_bool(),
        start: jv::opt(timeframe, "start").map(str::to_string),
        end: jv::opt(timeframe, "end").map(str::to_string),
        updated: jv::opt(record, "updatedAt").map(str::to_string),
        raw: record.clone(),
    }
}

/// Sort keys the unified CLI accepts, mapped onto Immunefi's own.
fn immunefi_sort(sort: &str, kind: Kind) -> String {
    let program = kind == Kind::Program;
    match sort {
        "reward" | "pool" | "bounty" => if program { "bounty" } else { "pool" }.into(),
        "start" | "launched" => if program { "launched" } else { "start" }.into(),
        other => other.into(),
    }
}

/// One platform's listing after its own filters; the caller sorts and limits globally.
pub fn listing(source: &str, kind: Kind, f: &Filters, opts: &Common, cache_dir: &Path) -> Result<Vec<Record>> {
    match source {
        "sherlock" => return sherlock::listing(kind, f, opts, cache_dir),
        "hackenproof" => return hackenproof::listing(kind, f, opts, cache_dir),
        _ => {}
    }
    let sort = f.sort.clone().unwrap_or_else(|| if kind == Kind::Program { "bounty" } else { "start" }.into());
    if source == "immunefi" {
        let native_sort = immunefi_sort(&sort, kind);
        let sorts = if kind == Kind::Program { immunefi::cli::PROGRAM_SORTS } else { immunefi::cli::COMPETITION_SORTS };
        if !sorts.contains(&native_sort.as_str()) {
            return Err(Error::value(format!("'{native_sort}' sorting is not available for {kind}s")));
        }
        let status = match f.status.as_deref() {
            Some("all" | "ended") | None => None,
            Some(other) => Some(other.to_string()),
        };
        let native = immunefi::cli::NativeFilter {
            search: Some(f.search.clone()).filter(|s| !s.is_empty()),
            ecosystem: f.ecosystem.clone(),
            language: f.language.clone(),
            program_type: f.program_type.clone(),
            project_type: f.project_type.clone(),
            kyc: f.kyc,
            min_bounty: Some(f.min_bounty),
            include_paused: f.include_paused,
            status,
            sort: native_sort,
            reverse: false,
            limit: None,
        };
        let data = immunefi_client(opts, cache_dir).listing(kind)?.data;
        let records = immunefi::cli::filter_items(data.as_array().map(Vec::as_slice).unwrap_or(&[]), kind, &native);
        let mut rows: Vec<Record> = records.iter().map(|r| normalize(source, kind, r)).filter(|r| r.reward() >= f.min_bounty).collect();
        match f.status.as_deref() {
            Some("ended") => rows.retain(|r| matches!(r.status.as_str(), "evaluating" | "finished")),
            Some(wanted) if kind == Kind::Program && wanted != "all" => {
                rows.retain(|r| if wanted == "active" { matches!(r.status.as_str(), "live" | "upcoming") } else { r.status == wanted })
            }
            _ => {}
        }
        return Ok(rows);
    }
    let client = cantina_client(opts, cache_dir)?;
    let kinds: &[&str] = if kind == Kind::Program { &["public_bounty", "private_bounty"] } else { &["public_contest", "private_contest"] };
    let mut records = cantina::cli::fetch_listing(&client, Some(kinds), f.status.as_deref(), kind == Kind::Competition)?;
    if f.has_tag_filter() {
        records = cantina::cli::enrich(&client, records)?;
    }
    if sort == "updated" && !records.iter().any(|r| jv::t(r, "updatedAt")) {
        client.warn("Cantina has no update dates; its records sort last");
    }
    let options = cantina::discovery::Options {
        status: f.status.clone(),
        search: f.search.clone(),
        min_bounty: f.min_bounty,
        kyc: f.kyc,
        language: f.language.clone(),
        ecosystem: f.ecosystem.clone(),
        program_type: f.program_type.clone(),
        project_type: f.project_type.clone(),
        sort,
        reverse: false,
        limit: None,
    };
    let records = cantina::discovery::apply(records, &options)?;
    print_warnings(&client);
    Ok(records.iter().map(|r| normalize(source, kind, r)).collect())
}

/// A served platform's detail record, as JSON.
pub fn served_detail(source: &str, kind: Kind, target: &str, opts: &Common, cache_dir: &Path) -> Result<Value> {
    match source {
        "sherlock" => sherlock::detail(kind, target, opts, cache_dir),
        _ => hackenproof::detail(kind, target, opts, cache_dir),
    }
}

pub fn served_render(source: &str, record: &Value, sections: &[String], style: Style) -> String {
    match source {
        "sherlock" => sherlock::render(record, sections, style),
        _ => hackenproof::render(record, sections, style),
    }
}

/// A platform's in-scope assets, adapted to the EVM engine.
pub struct PlatformScope {
    source: &'static str,
    opts: Common,
    cache_dir: PathBuf,
}

impl PlatformScope {
    pub fn new(source: &str, opts: Common, cache_dir: PathBuf) -> Self {
        PlatformScope { source: crate::sources::source(source).id, opts, cache_dir }
    }
}

impl ScopeSource for PlatformScope {
    fn scope(&self, kind: Kind, target: &str) -> Result<ScopeRecord> {
        match self.source {
            "immunefi" => {
                let data = immunefi_client(&self.opts, &self.cache_dir).detail(kind, target)?.data;
                Ok(ScopeRecord {
                    slug: jv::at(&data, "slug").clone(),
                    project: jv::opt(&data, "project").map(str::to_string),
                    assets: jv::arr(&data, "assets").to_vec(),
                })
            }
            "sherlock" | "hackenproof" => {
                let record = served_detail(self.source, kind, target, &self.opts, &self.cache_dir)?;
                let assets = if self.source == "sherlock" { sherlock::scope_assets(&record) } else { hackenproof::scope_assets(&record) };
                let slug = jv::first(&record, &["slug", "id"]).clone();
                Ok(ScopeRecord { slug, project: jv::opt(&record, "title").map(str::to_string), assets })
            }
            _ => {
                let client = cantina_client(&self.opts, &self.cache_dir)?;
                let uid = cantina::cli::resolve_target(&client, target, Some(kind))?;
                let record = client.detail(&uid, false)?;
                let scoped = cantina::contracts::scope(&record);
                let mut assets: Vec<Value> = jv::arr(&scoped, "contracts")
                    .iter()
                    .map(|r| json!({"url": jv::at(r, "url"), "type": "smart_contract", "description": jv::at(r, "name")}))
                    .collect();
                assets.extend(
                    jv::arr(&scoped, "unresolved")
                        .iter()
                        .map(|r| json!({"url": jv::at(r, "reference"), "type": "smart_contract", "description": jv::at(r, "reason")})),
                );
                print_warnings(&client);
                Ok(ScopeRecord { slug: json!(uid), project: jv::opt(&record, "name").map(str::to_string), assets })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry() {
        assert_eq!(ids(), ["immunefi", "cantina", "sherlock", "hackenproof"]);
        assert_eq!(for_host("audits.sherlock.xyz"), Some("sherlock"));
        assert_eq!(for_host("example.com"), None);
        assert_eq!(with_feature("vault").len(), 1);
        assert_eq!(with_command("auth")[0].id, "cantina");
        assert!(source("sherlock").served() && !source("immunefi").served());
    }

    #[test]
    fn normalizes_both_native_platforms() {
        let imm = normalize("immunefi", Kind::Program, &json!({"slug": "same", "project": "Same", "maxBounty": 100, "kyc": false}));
        assert_eq!((imm.target.as_str(), imm.status.as_str(), imm.reward()), ("immunefi:same", "live", 100.0));
        assert_eq!(imm.url, "https://immunefi.com/bug-bounty/same/information/");
        let can = normalize(
            "cantina",
            Kind::Program,
            &json!({"id": "same", "name": "Same", "status": "live", "totalRewardPot": "200",
            "currencyCode": "USDC", "kycRequired": true}),
        );
        assert_eq!((can.reward(), can.kyc, can.currency.as_deref()), (200.0, Some(true), Some("USDC")));
        assert_eq!(can.url, "https://cantina.xyz/bounties/same");
    }

    #[test]
    fn immunefi_sort_mapping() {
        assert_eq!(immunefi_sort("reward", Kind::Competition), "pool");
        assert_eq!(immunefi_sort("start", Kind::Program), "launched");
        assert_eq!(immunefi_sort("end", Kind::Program), "end");
    }
}
