//! Cantina's own command line: `bb cantina <command> ...`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::LazyLock;

use clap::{Args, Parser, Subcommand};
use regex::Regex;
use serde_json::{Value, json};

use super::client::{Client, firefox_cookie, page_queries};
use super::detail::detail as render_detail;
use super::discovery::{self, Options, assets};
use crate::cache::{env_ttl_hours, json_files, now, read_entry};
use crate::error::{Error, Result};
use crate::evm::contract_cli::{self, ContractCmd};
use crate::jv::{self, s};
use crate::opts::{Kind, print_json};
use crate::render::{Style, money, table};
use crate::util::close_matches;

pub const SECTIONS: &[&str] = &["overview", "information", "scope", "resources", "leaderboard", "all"];

#[derive(Parser, Debug)]
#[command(name = "bb cantina", version, about = "Read-only Cantina opportunities, rewards, rules, scope and resources")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Debug, Clone, Default)]
pub struct Flags {
    /// machine-readable output
    #[arg(long)]
    pub json: bool,
    /// plain output without colours
    #[arg(long)]
    pub no_color: bool,
    /// ignore the cache and fetch again
    #[arg(long)]
    pub refresh: bool,
    /// use cached data only
    #[arg(long)]
    pub offline: bool,
    /// cache lifetime in hours (default 12, env CANTINA_CACHE_TTL_HOURS)
    #[arg(long, value_name = "HOURS")]
    pub ttl: Option<f64>,
    /// cache directory
    #[arg(long)]
    pub cache_dir: Option<PathBuf>,
    /// Read only Cantina cookies from this Firefox profile; kept in memory
    #[arg(long)]
    pub firefox_profile: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct ListArgs {
    /// Filter lifecycle status (default: current bounties; all competitions)
    #[arg(long, value_parser = ["live", "ended", "upcoming", "evaluating", "finished", "active", "all"])]
    pub status: Option<String>,
    #[arg(short, long)]
    pub ecosystem: Option<String>,
    #[arg(short, long)]
    pub language: Option<String>,
    #[arg(short = 't', long = "type")]
    pub program_type: Option<String>,
    #[arg(long)]
    pub project_type: Option<String>,
    #[arg(long, value_parser = ["yes", "no"])]
    pub kyc: Option<String>,
    #[arg(short, long)]
    pub reverse: bool,
    /// Include paused records if Cantina publishes them
    #[arg(long)]
    pub include_paused: bool,
    #[arg(short, long, default_value = "")]
    pub search: String,
    #[arg(long, default_value_t = 0.0)]
    pub min_bounty: f64,
    #[arg(long, value_parser = ["reward", "bounty", "pool", "vault", "name", "updated", "launched", "start", "end"])]
    pub sort: Option<String>,
    #[arg(short = 'n', long)]
    pub limit: Option<i64>,
    #[command(flatten)]
    pub flags: Flags,
}

#[derive(Args, Debug)]
struct DetailArgs {
    /// Read participant repository details instead of public metadata
    #[arg(long)]
    private: bool,
    target: String,
    /// overview, information, scope, resources, all
    #[arg(value_name = "SECTION")]
    sections: Vec<String>,
    #[command(flatten)]
    flags: Flags,
}

#[derive(Subcommand, Debug)]
enum Command {
    Opportunities(ListArgs),
    #[command(visible_aliases = ["bounties", "ls"])]
    Programs(ListArgs),
    #[command(visible_aliases = ["comps"])]
    Competitions(ListArgs),
    #[command(visible_aliases = ["bounty"])]
    Program(DetailArgs),
    #[command(visible_aliases = ["comp"])]
    Competition(DetailArgs),
    /// inspect and read smart contracts (any EVM chain)
    #[command(visible_aliases = ["c"])]
    Contract {
        #[command(subcommand)]
        action: ContractCmd,
    },
    Auth(Flags),
    Leaderboard(Flags),
    /// Decode query data from saved Cantina HTML
    ImportPage {
        file: PathBuf,
        #[command(flatten)]
        flags: Flags,
    },
    Cache {
        #[arg(default_value = "info", value_parser = ["info", "clear"])]
        action: String,
        #[command(flatten)]
        flags: Flags,
    },
}

static UUID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$").unwrap());
static URL_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/(bounties|competitions)/[0-9a-fA-F-]+/?$").unwrap());

/// An opportunity UUID, from the UUID itself or a cantina.xyz bounty/competition URL.
pub fn identifier(value: &str) -> Result<String> {
    let mut value = value.to_string();
    if value.contains("://") {
        let parsed = url::Url::parse(&value).ok();
        let ok = parsed.as_ref().is_some_and(|u| u.host_str() == Some("cantina.xyz") && URL_PATH.is_match(u.path()));
        if !ok {
            return Err(Error::value("Expected a Cantina bounty or competition URL"));
        }
        value = parsed.unwrap().path().trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
    }
    if !UUID.is_match(&value) {
        return Err(Error::value("Use an opportunity UUID or Cantina URL from the listing"));
    }
    Ok(value)
}

/// A UUID, URL, name or company handle → the opportunity's UUID.
pub fn resolve_target(client: &Client, value: &str, kind: Option<Kind>) -> Result<String> {
    match identifier(value) {
        Ok(id) => return Ok(id),
        Err(e) if value.contains("://") => return Err(e),
        Err(_) => {}
    }
    let rows = client.catalog()?;
    let wanted = value.to_lowercase();
    let matches: Vec<&Value> = rows
        .iter()
        .filter(|r| kind.is_none_or(|k| s(r, "kind").contains("contest") == (k == Kind::Competition)))
        .filter(|r| [s(r, "name").to_lowercase(), s(jv::at(r, "company"), "handle").to_lowercase()].contains(&wanted))
        .collect();
    if matches.len() == 1 {
        return Ok(s(matches[0], "id").to_string());
    }
    if !matches.is_empty() {
        let list: Vec<String> = matches.iter().take(5).map(|r| format!("{} {}", s(r, "name"), s(r, "id"))).collect();
        return Err(Error::value(format!("Ambiguous name; use a UUID: {}", list.join(", "))));
    }
    let hints = close_matches(value, rows.iter().map(|r| s(r, "name")).filter(|n| !n.is_empty()), 3, 0.5);
    let tail =
        if hints.is_empty() { "; use programs/competitions --search".to_string() } else { format!("; did you mean {}?", hints.join(", ")) };
    Err(Error::value(format!("No opportunity named {value}{tail}")))
}

/// The part of a record one section shows, for JSON output.
pub fn section(record: &Value, name: &str) -> Value {
    let pick = |keys: &[&str]| {
        Value::Object(
            record
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(k, _)| keys.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        )
    };
    match name {
        "all" => record.clone(),
        "leaderboard" => {
            record.get("leaderboard").cloned().unwrap_or_else(|| json!({"available": false, "reason": "Leaderboard not requested"}))
        }
        "overview" => Value::Object(
            record
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(k, _)| !matches!(k.as_str(), "assetGroups" | "instructions" | "leaderboard"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        "information" => pick(&[
            "id",
            "name",
            "instructions",
            "kycRequired",
            "submissionFee",
            "allowedSeverities",
            "timeframe",
            "status",
            "totalRewardPot",
            "currencyCode",
        ]),
        "scope" => {
            json!({"id": jv::at(record, "id"), "assetGroups": record.get("assetGroups").cloned().unwrap_or(json!([])), "assets": assets(record),
                          "instructions": jv::at(record, "instructions"), "gitRepoUrl": jv::at(record, "gitRepoUrl"), "commitHash": jv::at(record, "commitHash")})
        }
        _ => {
            let links = crate::util::dedupe(
                Regex::new(r"https?://[^\s<>\)]+").unwrap().find_iter(s(record, "instructions")).map(|m| m.as_str().to_string()),
            );
            let resources: Vec<Value> = assets(record).into_iter().filter(|a| !jv::t(a, "outOfScope")).collect();
            json!({"id": jv::at(record, "id"), "company": jv::at(record, "company"), "gitRepoUrl": jv::at(record, "gitRepoUrl"),
                   "commitHash": jv::at(record, "commitHash"), "readmePath": jv::at(record, "readmePath"), "instructionLinks": links,
                   "resources": resources})
        }
    }
}

fn ttl(flags: &Flags) -> Result<f64> {
    let ttl = flags.ttl.unwrap_or_else(|| env_ttl_hours(&["CANTINA_CACHE_TTL_HOURS"]));
    if !ttl.is_finite() || ttl < 0.0 {
        return Err(Error::value("TTL must be nonnegative"));
    }
    Ok(ttl)
}

pub fn make_client(flags: &Flags) -> Result<Client> {
    if flags.refresh && flags.offline {
        return Err(Error::value("--refresh and --offline cannot be combined"));
    }
    let profile = flags.firefox_profile.clone().or_else(|| std::env::var("CANTINA_FIREFOX_PROFILE").ok().filter(|p| !p.is_empty()));
    let cookie = match &profile {
        Some(p) => {
            let cookie = firefox_cookie(p)?;
            if cookie.is_empty() {
                return Err(Error::value("No unexpired Cantina cookies in this Firefox profile"));
            }
            Some(cookie)
        }
        None => None,
    };
    Ok(Client::new(flags.cache_dir.clone(), flags.refresh, flags.offline, ttl(flags)?, cookie))
}

/// Opportunities of `kinds` in the API's lifecycle groups that `status` needs.
pub fn fetch_listing(client: &Client, kinds: Option<&[&str]>, status: Option<&str>, competitions: bool) -> Result<Vec<Value>> {
    let groups: &[&str] = match status {
        Some("all" | "evaluating" | "finished") => &["live", "ended"],
        None if competitions => &["live", "ended"],
        Some("ended") => &["ended"],
        _ => &["live"],
    };
    let mut records: Vec<Value> = Vec::new();
    for group in groups {
        for r in client.opportunities(kinds, group)? {
            match records.iter_mut().find(|x| s(x, "id") == s(&r, "id")) {
                Some(existing) => *existing = r,
                None => records.push(r),
            }
        }
    }
    Ok(records)
}

/// Fill catalog metadata (tags, asset groups) into opportunity rows for tag filters.
pub fn enrich(client: &Client, records: Vec<Value>) -> Result<Vec<Value>> {
    let catalog = client.catalog()?;
    Ok(records
        .into_iter()
        .map(|r| match catalog.iter().find(|c| s(c, "id") == s(&r, "id")) {
            Some(extra) => jv::merge(extra, &r),
            None => r,
        })
        .collect())
}

fn list(cmd: &str, args: &ListArgs, client: &Client) -> Result<(Value, usize)> {
    let kinds: Option<&[&str]> = match cmd {
        "programs" => Some(&["public_bounty", "private_bounty"]),
        "competitions" => Some(&["public_contest", "private_contest"]),
        _ => None,
    };
    let mut result = fetch_listing(client, kinds, args.status.as_deref(), cmd == "competitions")?;
    if args.language.is_some() || args.ecosystem.is_some() || args.program_type.is_some() || args.project_type.is_some() {
        result = enrich(client, result)?;
        client.warn("Tag filters use explicit metadata and references; records with no matching evidence are excluded");
    }
    let sort = args.sort.clone().unwrap_or_else(|| if cmd == "competitions" { "start" } else { "bounty" }.into());
    if sort == "updated" && !result.iter().any(|r| jv::t(r, "updatedAt")) {
        client.warn("Cantina provides no update timestamps; updated sorting uses creation time");
    }
    if args.include_paused {
        client.warn("Cantina has no published paused-state equivalent; only published records can be returned");
    }
    let total = result.len();
    let options = Options {
        status: args.status.clone(),
        search: args.search.clone(),
        min_bounty: args.min_bounty,
        kyc: args.kyc.as_deref().map(|k| k == "yes"),
        language: args.language.clone(),
        ecosystem: args.ecosystem.clone(),
        program_type: args.program_type.clone(),
        project_type: args.project_type.clone(),
        sort,
        reverse: args.reverse,
        limit: args.limit.map(|n| n as usize),
    };
    Ok((Value::Array(discovery::apply(result, &options)?), total))
}

fn print_listing(items: &[Value], total: usize, style: &Style) {
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|r| {
            let reward = format!("{} {}", money(jv::f(r, "totalRewardPot").or(Some(0.0)), false), s(r, "currencyCode"));
            let kyc = match jv::tri(r, "kycRequired") {
                None => "?",
                Some(true) => "yes",
                Some(false) => "no",
            };
            let fee = if jv::t(r, "submissionFee") { jv::text(jv::at(r, "submissionFee")) } else { "-".into() };
            vec![jv::opt(r, "name").unwrap_or("(restricted)").into(), s(r, "id").into(), discovery::status(r), reward, kyc.into(), fee]
        })
        .collect();
    println!("{}", table(&["PROJECT", "ID", "STATUS", "REWARD", "KYC", "FEE"], &rows, style, 0, &[]));
    println!("\n{} of {total} opportunities", items.len());
}

fn cache_report(action: &str, client: &Client, flags: &Flags) -> Result<Value> {
    let files = json_files(&client.root);
    if action == "clear" {
        for f in &files {
            std::fs::remove_file(f)?;
        }
        return Ok(json!({"removed": files.len()}));
    }
    let entries: Vec<Value> = files
        .iter()
        .filter_map(|f| {
            let e = read_entry(f)?;
            let key = f.strip_prefix(&client.root).ok()?.to_string_lossy().to_string();
            Some(json!({"key": key, "source": e.source, "fetched_at": e.fetched_at, "fresh": now() - e.fetched_at < client.ttl_seconds}))
        })
        .collect();
    Ok(json!({"dir": client.root, "ttl_hours": ttl(flags)?, "entries": entries}))
}

fn run(cli: Cli) -> Result<i32> {
    let (flags, result, style_rows) = match cli.command {
        Command::Contract { mut action } => {
            let common = action.common().clone().validated(&["CANTINA_CACHE_TTL_HOURS"])?;
            *action.common_mut() = common;
            let dir = action.common().cache_dir.clone().unwrap_or_else(|| crate::paths::provider_cache("cantina"));
            let source = crate::sources::PlatformScope::new("cantina", action.common().clone(), dir);
            return contract_cli::run(&action, &contract_cli::Ctx { evm_cache: crate::paths::evm_cache(), scope: Some(&source) });
        }
        Command::Opportunities(args) => listing("opportunities", args)?,
        Command::Programs(args) => listing("programs", args)?,
        Command::Competitions(args) => listing("competitions", args)?,
        Command::Program(args) => return detail(Kind::Program, args),
        Command::Competition(args) => return detail(Kind::Competition, args),
        Command::Auth(flags) => {
            let client = make_client(&flags)?;
            if client.cookie.is_empty() {
                return Err(Error::value("Set CANTINA_COOKIE or use --firefox-profile to check authentication"));
            }
            let me = client.get("/api/v0/auth/me", &[], false, false)?;
            if !jv::truthy(&me) {
                return Err(Error::value("Cantina session is not authenticated"));
            }
            emit_warnings(&client);
            (flags, json!({"authenticated": true}), None)
        }
        Command::Leaderboard(flags) => {
            let client = make_client(&flags)?;
            let data = client.api("/api/v0/leaderboard")?;
            emit_warnings(&client);
            (flags, data, None)
        }
        Command::ImportPage { file, flags } => {
            let data = page_queries(&std::fs::read_to_string(&file)?)?;
            (flags, data, None)
        }
        Command::Cache { action, flags } => {
            let client = make_client(&flags)?;
            let data = cache_report(&action, &client, &flags)?;
            (flags, data, None)
        }
    };
    if flags.json {
        return print_json(&result);
    }
    match style_rows {
        Some(total) => print_listing(result.as_array().map(Vec::as_slice).unwrap_or(&[]), total, &Style::for_stdout(flags.no_color)),
        None => {
            let mut out = String::new();
            super::plain::render(&result, 0, &mut out);
            print!("{out}");
        }
    }
    Ok(0)
}

fn emit_warnings(client: &Client) {
    for w in client.take_warnings() {
        eprintln!("Warning: {w}");
    }
}

fn listing(cmd: &str, args: ListArgs) -> Result<(Flags, Value, Option<usize>)> {
    if !args.min_bounty.is_finite() || args.min_bounty < 0.0 {
        return Err(Error::value("Minimum bounty must be finite and nonnegative"));
    }
    if args.limit.is_some_and(|n| n < 1) {
        return Err(Error::value("Limit must be positive"));
    }
    let client = make_client(&args.flags)?;
    let (result, total) = list(cmd, &args, &client)?;
    emit_warnings(&client);
    let rows = result.as_array().is_some_and(|r| !r.is_empty());
    Ok((args.flags, result, if rows { Some(total) } else { None }))
}

fn detail(kind: Kind, args: DetailArgs) -> Result<i32> {
    show_detail(kind, &args.target, &args.sections, args.private, &args.flags, None)
}

/// A detail page. With `wrap` (the unified target), JSON is wrapped with its provenance.
pub fn show_detail(kind: Kind, target: &str, sections: &[String], private: bool, flags: &Flags, wrap: Option<&str>) -> Result<i32> {
    if sections.iter().any(|x| !SECTIONS.contains(&x.as_str())) {
        return Err(Error::value(format!("Unknown section; choose {}", SECTIONS.join(", "))));
    }
    let client = make_client(flags)?;
    let uid = resolve_target(&client, target, Some(kind))?;
    let mut record = client.detail(&uid, private)?;
    if !record.is_object() {
        return Err(Error::value("Unexpected repository response schema"));
    }
    let names: Vec<String> = if sections.is_empty() { vec!["overview".into()] } else { sections.to_vec() };
    let is_comp = s(&record, "kind").contains("contest") || kind == Kind::Competition;
    if is_comp && names.iter().any(|n| n == "leaderboard" || n == "all") {
        record["leaderboard"] = client.leaderboard(&uid)?;
    }
    emit_warnings(&client);
    if flags.json {
        let result = if names.len() == 1 {
            section(&record, &names[0])
        } else {
            Value::Object(names.iter().map(|n| (n.clone(), section(&record, n))).collect())
        };
        return match wrap {
            Some(target) => print_json(&json!({"source": "cantina", "kind": kind.as_str(), "target": target, "data": result})),
            None => print_json(&result),
        };
    }
    print!("{}", render_detail(&record, &names, Style::for_stdout(flags.no_color)));
    Ok(0)
}

/// Entry point for `bb cantina ARGS` (`argv` excludes the program name).
pub fn main(argv: Vec<OsString>) -> i32 {
    let cli = match Cli::try_parse_from(std::iter::once(OsString::from("bb cantina")).chain(argv)) {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return e.exit_code();
        }
    };
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cantina: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers() {
        let id = "f9df94db-c7b1-434b-bb06-d1360abdd1be";
        assert_eq!(identifier(id).unwrap(), id);
        assert_eq!(identifier(&format!("https://cantina.xyz/bounties/{id}")).unwrap(), id);
        assert!(identifier("https://evil.example/bounties/x").is_err());
        assert!(identifier("Uniswap").is_err());
    }

    #[test]
    fn json_sections() {
        let record = json!({"id": "1", "name": "N", "instructions": "see https://a.io/x", "assetGroups": [], "kycRequired": true});
        assert!(section(&record, "overview").get("instructions").is_none());
        assert_eq!(section(&record, "information")["kycRequired"], true);
        assert_eq!(section(&record, "resources")["instructionLinks"], json!(["https://a.io/x"]));
        assert_eq!(section(&record, "leaderboard")["available"], false);
    }
}
