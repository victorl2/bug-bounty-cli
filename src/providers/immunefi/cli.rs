//! Immunefi's own command line: `bb immunefi <command> ...`.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};

use super::client::{Client, Fetched};
use super::render;
use crate::cache::{Cache, MIN_TTL_HOURS, env_ttl_hours};
use crate::error::{Error, Result};
use crate::evm::contract_cli::{self, ContractCmd};
use crate::jv::{self, s, t};
use crate::opts::{Kind, print_json};
use crate::paths::provider_cache;
use crate::render::{Style, duration, table};
use crate::util::{close_matches, dedupe};

#[derive(Parser, Debug)]
#[command(
    name = "bb immunefi",
    version,
    about = "Browse Immunefi bug-bounty programs and audit competitions. Data is cached locally for at least 10 hours."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Debug, Clone, Default)]
#[command(next_help_heading = "cache and output")]
pub struct Native {
    /// print raw JSON instead of formatted text
    #[arg(long)]
    pub json: bool,
    /// ignore the cache and re-fetch from immunefi.com
    #[arg(long)]
    pub refresh: bool,
    /// only use cached data, never hit the network
    #[arg(long)]
    pub offline: bool,
    /// cache lifetime in hours (default 12, env IMMUNEFI_CACHE_TTL_HOURS, minimum 10)
    #[arg(long, value_name = "HOURS")]
    pub ttl: Option<f64>,
    /// cache directory (default ~/.cache/bug-bounty-cli/immunefi, env IMMUNEFI_CACHE_DIR)
    #[arg(long)]
    pub cache_dir: Option<PathBuf>,
    /// disable ANSI colors (also honours NO_COLOR)
    #[arg(long)]
    pub no_color: bool,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "filters")]
struct ListFilters {
    /// substring match on slug or project name
    #[arg(short, long)]
    search: Option<String>,
    /// ecosystem tag, e.g. ETH, Solana, Base
    #[arg(short, long)]
    ecosystem: Option<String>,
    /// language tag, e.g. Solidity, Rust, Move
    #[arg(short, long)]
    language: Option<String>,
    /// program type, e.g. 'Smart Contract', 'Blockchain/DLT', 'Websites'
    #[arg(short = 't', long = "type")]
    program_type: Option<String>,
    /// only programs that do / do not require KYC
    #[arg(long, value_parser = ["yes", "no"])]
    kyc: Option<String>,
    /// show at most N rows
    #[arg(short = 'n', long)]
    limit: Option<usize>,
    /// reverse the sort order
    #[arg(short, long)]
    reverse: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// list bug-bounty programs
    #[command(visible_aliases = ["bounties", "ls"])]
    Programs {
        #[command(flatten)]
        filters: ListFilters,
        /// minimum max-bounty in USD
        #[arg(long, value_name = "USD")]
        min_bounty: Option<f64>,
        /// project type tag, e.g. Defi, Infrastructure
        #[arg(long)]
        project_type: Option<String>,
        /// also list paused programs
        #[arg(long)]
        include_paused: bool,
        /// sort key
        #[arg(long, default_value = "bounty", value_parser = ["bounty", "vault", "launched", "updated", "name"])]
        sort: String,
        #[command(flatten)]
        native: Native,
    },
    /// list audit competitions
    #[command(visible_aliases = ["comps"])]
    Competitions {
        #[command(flatten)]
        filters: ListFilters,
        /// filter by status ('active' = upcoming or live)
        #[arg(long, value_parser = ["upcoming", "live", "evaluating", "finished", "active"])]
        status: Option<String>,
        /// sort key
        #[arg(long, default_value = "start", value_parser = ["start", "end", "pool", "name"])]
        sort: String,
        #[command(flatten)]
        native: Native,
    },
    /// show details of a bug-bounty program
    #[command(visible_aliases = ["bounty"])]
    Program(DetailArgs),
    /// show details of an audit competition
    #[command(visible_aliases = ["comp"])]
    Competition(DetailArgs),
    /// inspect and read smart contracts (any EVM chain)
    #[command(visible_aliases = ["c"])]
    Contract {
        #[command(subcommand)]
        action: ContractCmd,
    },
    /// show or clear the local cache
    Cache {
        #[arg(default_value = "info", value_parser = ["info", "clear"])]
        action: String,
        #[command(flatten)]
        native: Native,
    },
}

#[derive(Args, Debug)]
struct DetailArgs {
    /// slug (e.g. ssvnetwork) or an immunefi.com URL
    target: String,
    /// one or more of: overview, information, scope, resources, leaderboard (competitions), all (default: overview)
    #[arg(value_name = "section")]
    sections: Vec<String>,
    #[command(flatten)]
    native: Native,
}

/// Filters for an Immunefi listing; the unified CLI fills the same struct.
#[derive(Debug, Clone, Default)]
pub struct NativeFilter {
    pub search: Option<String>,
    pub ecosystem: Option<String>,
    pub language: Option<String>,
    pub program_type: Option<String>,
    pub project_type: Option<String>,
    pub kyc: Option<bool>,
    pub min_bounty: Option<f64>,
    pub include_paused: bool,
    pub status: Option<String>,
    pub sort: String,
    pub reverse: bool,
    pub limit: Option<usize>,
}

pub const PROGRAM_SORTS: &[&str] = &["bounty", "vault", "launched", "updated", "name"];
pub const COMPETITION_SORTS: &[&str] = &["start", "end", "pool", "name"];

pub fn make_client(native: &Native) -> Client {
    let ttl = native.ttl.unwrap_or_else(|| env_ttl_hours(&["IMMUNEFI_CACHE_TTL_HOURS"])).max(MIN_TTL_HOURS);
    Client::new(Cache::new(native.cache_dir.clone().unwrap_or_else(|| provider_cache("immunefi")), ttl), native.refresh, native.offline)
}

fn note_source(result: &Fetched) {
    if result.degraded {
        eprintln!("(public API fallback: codebase/documentation links, vault and some rule fields are unavailable)");
    }
}

fn tag_match(values: &Value, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    values.as_array().into_iter().flatten().any(|v| jv::text(v).to_lowercase().contains(&needle))
}

/// Sort key of one record: numbers sort as numbers, dates and names as text.
enum Key {
    Num(f64),
    Text(String),
}

fn sort_key(item: &Value, sort: &str) -> Key {
    let num = |k: &str| jv::at(item, k).as_f64().unwrap_or(0.0);
    match sort {
        "bounty" => Key::Num(num("maxBounty")),
        "vault" => Key::Num(num("vaultBalance")),
        "pool" => Key::Num(jv::first(item, &["rewardsPool", "maxBounty"]).as_f64().unwrap_or(0.0)),
        "launched" | "start" => Key::Text(s(item, "launchDate").into()),
        "updated" => Key::Text(s(item, "updatedDate").into()),
        "end" => Key::Text(s(item, "endDate").into()),
        _ => Key::Text(s(item, "project").to_lowercase()),
    }
}

pub fn filter_items(items: &[Value], kind: Kind, f: &NativeFilter) -> Vec<Value> {
    let mut out: Vec<Value> = items
        .iter()
        .filter(|it| {
            let tags = jv::at(it, "tags");
            if let Some(search) = &f.search {
                let hay = format!("{} {}", s(it, "slug"), s(it, "project")).to_lowercase();
                if !hay.contains(&search.to_lowercase()) {
                    return false;
                }
            }
            let tag_checks = [(&f.ecosystem, "ecosystem"), (&f.language, "language"), (&f.program_type, "programType")];
            if tag_checks.iter().any(|(needle, key)| needle.as_ref().is_some_and(|n| !tag_match(jv::at(tags, key), n))) {
                return false;
            }
            if f.kyc.is_some_and(|k| t(it, "kyc") != k) {
                return false;
            }
            if kind == Kind::Program {
                if f.min_bounty.is_some_and(|m| m != 0.0 && jv::at(it, "maxBounty").as_f64().unwrap_or(0.0) < m) {
                    return false;
                }
                if f.project_type.as_ref().is_some_and(|n| !tag_match(jv::at(tags, "projectType"), n)) {
                    return false;
                }
                if !f.include_paused && render::program_status(it) == "PAUSED" {
                    return false;
                }
            } else if let Some(wanted) = &f.status {
                let status = render::competition_status(it);
                let ok = if wanted == "active" { matches!(status.as_str(), "UPCOMING" | "LIVE") } else { status == wanted.to_uppercase() };
                if !ok {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect();
    let descending = f.sort != "name";
    out.sort_by(|a, b| {
        let ord = match (sort_key(a, &f.sort), sort_key(b, &f.sort)) {
            (Key::Num(x), Key::Num(y)) => x.total_cmp(&y),
            (Key::Text(x), Key::Text(y)) => x.cmp(&y),
            _ => std::cmp::Ordering::Equal,
        };
        if descending != f.reverse { ord.reverse() } else { ord }
    });
    if let Some(n) = f.limit.filter(|n| *n > 0) {
        out.truncate(n);
    }
    out
}

fn list(kind: Kind, f: &NativeFilter, native: &Native) -> Result<i32> {
    let result = make_client(native).listing(kind)?;
    let all = result.data.as_array().cloned().unwrap_or_default();
    let items = filter_items(&all, kind, f);
    if native.json {
        return print_json(&items);
    }
    note_source(&result);
    if items.is_empty() {
        println!("No {kind}s match the given filters.");
        return Ok(0);
    }
    let style = Style::for_stdout(native.no_color);
    let text = match kind {
        Kind::Program => render::program_table(&items, &style),
        Kind::Competition => render::competition_table(&items, &style),
    };
    println!("{text}");
    println!("{}", style.dim(&format!("\n{} of {} {kind}s", items.len(), all.len())));
    Ok(0)
}

/// Point at the right command or close slugs when a detail page is not found.
fn suggest(client: &Client, kind: Kind, slug: &str) -> String {
    let other = kind.other();
    let mut hints: Vec<String> = Vec::new();
    for k in [other, kind] {
        let Ok(res) = client.listing(k) else { continue };
        let items = res.data.as_array().cloned().unwrap_or_default();
        let slugs: Vec<String> = items.iter().map(|it| s(it, "slug").to_string()).collect();
        if k == other && slugs.iter().any(|x| x == slug) {
            let what = if k == Kind::Competition { "n audit" } else { " bug-bounty" };
            return format!("'{slug}' is a{what} {k}: try `bb immunefi {k} {slug}`");
        }
        hints.extend(close_matches(slug, slugs.iter().map(String::as_str), 3, 0.6).into_iter().map(str::to_string));
        let lower = slug.to_lowercase();
        hints.extend(items.iter().filter(|it| s(it, "project").to_lowercase().contains(&lower)).map(|it| s(it, "slug").to_string()));
    }
    let hints: Vec<String> = dedupe(hints).into_iter().take(5).collect();
    if hints.is_empty() {
        format!("run `bb immunefi {kind}s --search {slug}` to find it")
    } else {
        format!("did you mean: {}", hints.join(", "))
    }
}

pub fn valid_sections(kind: Kind) -> &'static [&'static str] {
    if kind == Kind::Competition { render::COMPETITION_SECTIONS } else { render::PROGRAM_SECTIONS }
}

fn detail(kind: Kind, args: &DetailArgs) -> Result<i32> {
    show_detail(kind, &args.target, &args.sections, &args.native, None)
}

/// A detail page. With `wrap` (the unified target), JSON is wrapped with its provenance.
pub fn show_detail(kind: Kind, target: &str, sections: &[String], native: &Native, wrap: Option<&str>) -> Result<i32> {
    let (url_kind, slug) = super::client::parse_target(target)?;
    let kind = url_kind.unwrap_or(kind);
    let valid = valid_sections(kind);
    let sections: Vec<&str> = if sections.is_empty() { vec!["overview"] } else { sections.iter().map(String::as_str).collect() };
    if sections.iter().any(|x| *x != "all" && !valid.contains(x)) {
        return Err(Error::value("unknown detail section"));
    }
    let chosen: Vec<&str> =
        if sections.contains(&"all") { valid.to_vec() } else { valid.iter().copied().filter(|v| sections.contains(v)).collect() };
    let client = make_client(native);
    let result = match client.detail(kind, &slug) {
        Ok(r) => r,
        Err(Error::NotFound(_)) => {
            eprintln!("error: no {kind} named '{slug}' on immunefi.com");
            eprintln!("{}", suggest(&client, kind, &slug));
            return Ok(1);
        }
        Err(e) => return Err(e),
    };
    if native.json {
        return match wrap {
            Some(target) => print_json(&json!({"source": "immunefi", "kind": kind.as_str(), "target": target, "data": result.data})),
            None => print_json(&result.data),
        };
    }
    note_source(&result);
    print!("{}", render::detail(kind, &result.data, &chosen, Style::for_stdout(native.no_color)));
    Ok(0)
}

fn cache(action: &str, native: &Native) -> Result<i32> {
    let client = make_client(native);
    let cache = &client.cache;
    if action == "clear" {
        cache.clear()?;
        println!("Cleared {}", cache.root.display());
        return Ok(0);
    }
    let entries = cache.entries();
    if native.json {
        let list: Vec<Value> = entries
            .iter()
            .map(|(k, e)| json!({"key": k, "source": e.source, "fetched_at": e.fetched_at, "fresh": cache.is_fresh(e)}))
            .collect();
        return print_json(&json!({"dir": cache.root, "ttl_hours": cache.ttl_seconds / 3600.0, "entries": list}));
    }
    println!("Cache dir: {}", cache.root.display());
    println!("TTL:       {}h (minimum {}h)", cache.ttl_seconds / 3600.0, MIN_TTL_HOURS);
    if entries.is_empty() {
        println!("No cached entries.");
        return Ok(0);
    }
    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|(k, e)| {
            vec![k.clone(), duration(e.age_seconds()), if cache.is_fresh(e) { "fresh" } else { "expired" }.into(), e.source.clone()]
        })
        .collect();
    println!();
    println!("{}", table(&["KEY", "AGE", "STATE", "SOURCE"], &rows, &Style::for_stdout(native.no_color), 3, &[]));
    Ok(0)
}

fn kyc(value: &Option<String>) -> Option<bool> {
    value.as_deref().map(|v| v == "yes")
}

fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Programs { filters, min_bounty, project_type, include_paused, sort, native } => {
            let f = NativeFilter {
                search: filters.search,
                ecosystem: filters.ecosystem,
                language: filters.language,
                program_type: filters.program_type,
                project_type,
                kyc: kyc(&filters.kyc),
                min_bounty,
                include_paused,
                status: None,
                sort,
                reverse: filters.reverse,
                limit: filters.limit,
            };
            list(Kind::Program, &f, &native)
        }
        Command::Competitions { filters, status, sort, native } => {
            let f = NativeFilter {
                search: filters.search,
                ecosystem: filters.ecosystem,
                language: filters.language,
                program_type: filters.program_type,
                kyc: kyc(&filters.kyc),
                status,
                sort,
                reverse: filters.reverse,
                limit: filters.limit,
                ..Default::default()
            };
            list(Kind::Competition, &f, &native)
        }
        Command::Program(args) => detail(Kind::Program, &args),
        Command::Competition(args) => detail(Kind::Competition, &args),
        Command::Contract { mut action } => {
            let common = action.common().clone().validated(&["IMMUNEFI_CACHE_TTL_HOURS"])?;
            *action.common_mut() = common;
            let dir = action.common().cache_dir.clone().unwrap_or_else(|| provider_cache("immunefi"));
            let source = crate::sources::PlatformScope::new("immunefi", action.common().clone(), dir);
            contract_cli::run(&action, &contract_cli::Ctx { evm_cache: crate::paths::evm_cache(), scope: Some(&source) })
        }
        Command::Cache { action, native } => cache(&action, &native),
    }
}

/// Entry point for `bb immunefi ARGS` (`argv` excludes the program name).
pub fn main(argv: Vec<OsString>) -> i32 {
    let cli = match Cli::try_parse_from(std::iter::once(OsString::from("bb immunefi")).chain(argv)) {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return e.exit_code();
        }
    };
    match run(cli) {
        Ok(code) => code,
        Err(Error::Payload(e)) => {
            eprintln!("error: could not read immunefi.com page data ({e}); the site layout may have changed");
            1
        }
        Err(e @ Error::Value(_)) => {
            eprintln!("error: {e}");
            2
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_sort_and_limit() {
        let items = vec![
            json!({"slug": "a", "project": "Alpha", "maxBounty": 10, "kyc": false, "tags": {"language": ["Solidity"]}}),
            json!({"slug": "b", "project": "Beta", "maxBounty": 50, "kyc": true, "tags": {"language": ["Rust"]}}),
            json!({"slug": "c", "project": "Gamma", "maxBounty": 30, "isPaused": true}),
        ];
        let base = NativeFilter { sort: "bounty".into(), ..Default::default() };
        let slugs = |v: Vec<Value>| v.iter().map(|x| s(x, "slug").to_string()).collect::<Vec<_>>();
        assert_eq!(slugs(filter_items(&items, Kind::Program, &base)), ["b", "a"]);
        let paused = NativeFilter { include_paused: true, reverse: true, ..base.clone() };
        assert_eq!(slugs(filter_items(&items, Kind::Program, &paused)), ["a", "c", "b"]);
        let lang = NativeFilter { language: Some("sol".into()), ..base.clone() };
        assert_eq!(slugs(filter_items(&items, Kind::Program, &lang)), ["a"]);
        let kyc = NativeFilter { kyc: Some(true), limit: Some(1), ..base.clone() };
        assert_eq!(slugs(filter_items(&items, Kind::Program, &kyc)), ["b"]);
        let by_name = NativeFilter { sort: "name".into(), include_paused: true, ..base };
        assert_eq!(slugs(filter_items(&items, Kind::Program, &by_name)), ["a", "b", "c"]);
        assert!(super::super::client::PUBLIC_API_URL.ends_with("bounties.json"));
    }
}
