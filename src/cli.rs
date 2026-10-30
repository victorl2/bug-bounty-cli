//! Platform-neutral command router; each platform's native interface remains available.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use regex::Regex;
use serde_json::{Value, json};

use crate::cache::{json_files, now, read_entry};
use crate::error::{Error, Result};
use crate::evm::contract_cli::{self, ContractCmd};
use crate::jv;
use crate::opts::{Common, Kind, print_json};
use crate::paths::cache_root;
use crate::providers::common::{Filters, Record, SECTIONS};
use crate::providers::{cantina, immunefi};
use crate::render::{Style, Styler, money, table};
use crate::sources::{self, SOURCES};
use crate::util::close_matches;

pub const DEFAULT_ROWS: usize = 25;

const ABOUT: &str =
    "Discover bug bounties and audit competitions across platforms, read their rules and scope, and inspect in-scope EVM contracts.";
/// Help for commands whose description is longer than their one-line summary.
const DESCRIBED: &str = "{before-help}{usage-heading} {usage}\n\n{all-args}{after-help}";
const EPILOG: &str = "Use --source NAME (before or after the command) to restrict to one platform; `bb sources` lists them. \
Targets accept a name, a source:id from a listing, or the opportunity URL.";

#[derive(Parser, Debug)]
#[command(name = "bb", version, about = ABOUT, after_help = EPILOG, disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// list bug bounty programs
    #[command(
        visible_aliases = ["bounties", "ls"],
        before_help = "List bug bounty programs from every platform, sorted together. Default: live programs, highest reward first.",
        help_template = DESCRIBED
    )]
    Programs(ListArgs),
    /// list audit competitions
    #[command(
        visible_aliases = ["comps"],
        before_help = "List audit competitions from every platform, sorted together. Default: live and upcoming first, then the most recent.",
        help_template = DESCRIBED
    )]
    Competitions(ListArgs),
    /// show a bug bounty program
    #[command(visible_aliases = ["bounty"])]
    Program(DetailArgs),
    /// show an audit competition
    #[command(visible_aliases = ["comp"])]
    Competition(DetailArgs),
    /// inspect and read smart contracts (any EVM chain)
    #[command(
        visible_aliases = ["c"],
        help_template = DESCRIBED,
        before_help = "Verified source comes from Etherscan, the chain's own explorer or Sourcify and is cached; balances, calls and \
storage come from a node (QuickNode, --rpc, explorer or Etherscan) and are never cached.",
        subcommand_value_name = "ACTION"
    )]
    Contract {
        #[command(subcommand)]
        action: ContractCmd,
    },
    /// show or clear cached responses
    Cache {
        #[arg(default_value = "info", value_parser = ["info", "clear"])]
        action: String,
        #[command(flatten)]
        common: Common,
    },
    /// list supported platforms and their extra commands
    Sources {
        /// machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// links to each platform's own bounty and competition pages
    #[command(
        visible_aliases = ["links"],
        before_help = "Print the pages where each platform lists its bug bounties and audit competitions.",
        help_template = DESCRIBED
    )]
    Sites(SitesArgs),
}

#[derive(Args, Debug, Clone)]
struct ListArgs {
    /// text in the name or id
    #[arg(short, long, value_name = "TEXT", help_heading = "Filters")]
    search: Option<String>,
    /// chain or ecosystem tag, e.g. ethereum, solana
    #[arg(short, long, value_name = "NAME", help_heading = "Filters")]
    ecosystem: Option<String>,
    /// language tag, e.g. solidity, rust
    #[arg(short, long, value_name = "NAME", help_heading = "Filters")]
    language: Option<String>,
    /// asset type tag, e.g. smart contract, websites
    #[arg(short = 't', long = "type", value_name = "NAME", help_heading = "Filters")]
    program_type: Option<String>,
    /// project tag, e.g. defi, bridge
    #[arg(long, value_name = "NAME", help_heading = "Filters")]
    project_type: Option<String>,
    /// only opportunities that require (yes) or do not require (no) KYC
    #[arg(long, value_parser = ["yes", "no"], help_heading = "Filters")]
    kyc: Option<String>,
    /// minimum top reward, in its own currency
    #[arg(long, default_value_t = 0.0, value_name = "AMOUNT", hide_default_value = true, help_heading = "Filters")]
    min_bounty: f64,
    /// live, upcoming, evaluating (judging) or finished; active = live + upcoming, ended = evaluating + finished, all
    #[arg(long, value_name = "STATUS", hide_possible_values = true, help_heading = "Filters",
          value_parser = ["active", "upcoming", "live", "evaluating", "finished", "ended", "all"])]
    status: Option<String>,
    /// also show paused programs
    #[arg(long, help_heading = "Filters")]
    include_paused: bool,
    /// bounty (= reward, pool), name, start (= launched), end, updated or vault
    #[arg(long, value_name = "KEY", hide_possible_values = true, help_heading = "Output",
          value_parser = ["bounty", "reward", "pool", "name", "launched", "start", "end", "updated", "vault"])]
    sort: Option<String>,
    /// reverse the order
    #[arg(short, long, help_heading = "Output")]
    reverse: bool,
    /// rows to show (default 25; JSON output is not limited)
    #[arg(short = 'n', long, value_name = "N", help_heading = "Output")]
    limit: Option<i64>,
    /// show every matching row
    #[arg(short, long, help_heading = "Output")]
    all: bool,
    #[command(flatten)]
    common: Common,
}

#[derive(Args, Debug, Clone)]
struct DetailArgs {
    /// name, source:id or opportunity URL
    target: String,
    /// overview (default), information, scope, resources, leaderboard, all
    #[arg(value_name = "SECTION")]
    sections: Vec<String>,
    /// participant-only details, on platforms that offer them
    #[arg(long)]
    private: bool,
    #[command(flatten)]
    common: Common,
}

#[derive(Args, Debug, Clone)]
struct SitesArgs {
    /// only these platforms (default: all)
    #[arg(value_name = "PLATFORM")]
    platforms: Vec<String>,
    /// only bounty or only competition pages
    #[arg(short, long, value_parser = ["programs", "competitions"])]
    kind: Option<String>,
    /// open the pages in the default browser
    #[arg(long)]
    open: bool,
    /// machine-readable output
    #[arg(long)]
    json: bool,
}

/// Accept `--source` before or after the command without inspecting flag values.
pub fn extract_source(argv: Vec<String>) -> Result<(Option<String>, Vec<String>)> {
    let choices = || [sources::ids(), vec!["all"]].concat().join(", ");
    let mut rest = Vec::new();
    let mut selected: Option<String> = None;
    let mut items = argv.into_iter();
    while let Some(item) = items.next() {
        if item == "--" {
            rest.push(item);
            rest.extend(items);
            break;
        }
        let value = if item == "--source" {
            items.next().ok_or_else(|| Error::value(format!("--source requires one of: {}", choices())))?
        } else if let Some(v) = item.strip_prefix("--source=") {
            v.to_string()
        } else {
            rest.push(item);
            continue;
        };
        if selected.as_ref().is_some_and(|s| *s != value) {
            return Err(Error::value("Conflicting --source values"));
        }
        if value != "all" && sources::get(&value).is_none() {
            return Err(Error::value(format!("Unknown source '{value}'; choose one of: {}", choices())));
        }
        selected = Some(value);
    }
    Ok((selected, rest))
}

/// The platform a target names explicitly (`source:id` or a platform URL), else the selected one.
pub fn target_source(target: &str, selected: Option<&str>) -> Result<(Option<String>, String)> {
    let mut source: Option<String> = None;
    let mut value = target.to_string();
    if target.contains(':') && !target.contains("://") {
        let (prefix, rest) = target.split_once(':').unwrap();
        if sources::get(prefix).is_some() {
            source = Some(prefix.to_string());
            value = rest.to_string();
        }
    }
    if target.contains("://") {
        let host = url::Url::parse(target).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        source = Some(
            sources::for_host(&host)
                .ok_or_else(|| Error::value("Not an opportunity URL from a supported platform (see `bb sources`)"))?
                .to_string(),
        );
    }
    if let Some(s) = &source {
        if !matches!(selected, None | Some("all")) && selected != Some(s.as_str()) {
            return Err(Error::value("Target source conflicts with --source"));
        }
    }
    let chosen = source.or_else(|| selected.filter(|s| *s != "all").map(str::to_string));
    Ok((chosen, value))
}

static UUID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$").unwrap());
static PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f]{6,}(?:-[0-9a-f-]*)?$").unwrap());

/// `source:id`, with a UUID cut to its first 8 characters (unique prefixes are accepted back).
pub fn short(source: &str, target: &str) -> String {
    if UUID.is_match(target) { format!("{source}:{}", &target[..8]) } else { format!("{source}:{target}") }
}

/// A unique id prefix (as listings print it) becomes the full id; anything else is returned unchanged.
fn expand_prefix(source: &str, value: &str, kind: Kind, opts: &Common, root: &Path) -> Result<String> {
    if !PREFIX.is_match(value) || UUID.is_match(value) {
        return Ok(value.to_string());
    }
    let mut ids: Vec<String> = Vec::new();
    for k in [kind, kind.other()] {
        ids.extend(sources::candidates(source, k, opts, &root.join(source))?.into_iter().map(|(t, _, _)| t));
    }
    if ids.iter().any(|i| i == value) {
        return Ok(value.to_string());
    }
    let found = crate::util::dedupe(ids.into_iter().filter(|t| t.to_lowercase().starts_with(&value.to_lowercase())));
    if found.len() > 1 {
        return Err(Error::value(format!("{source}:{value} matches several ids; add more characters")));
    }
    Ok(found.into_iter().next().unwrap_or_else(|| value.to_string()))
}

/// Find a bare name, handle or id on every platform; it must match exactly one opportunity.
fn resolve_target(value: &str, kind: Kind, opts: &Common, root: &Path) -> Result<(String, String)> {
    let wanted = value.to_lowercase();
    let mut matches: Vec<(&str, String, String)> = Vec::new();
    let mut labels: Vec<(String, String)> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for src in SOURCES {
        let rows = match sources::candidates(src.id, kind, opts, &root.join(src.id)) {
            Ok(rows) => rows,
            Err(e) => {
                failures.push(format!("{}: {e}", src.name));
                continue;
            }
        };
        for (target, name, keys) in rows {
            labels.push((name.to_lowercase(), format!("{name} ({})", short(src.id, &target))));
            if keys.iter().any(|k| !k.is_empty() && k.to_lowercase() == wanted) {
                matches.push((src.id, target, name));
            }
        }
    }
    if matches.len() == 1 {
        let (s, t, _) = matches.remove(0);
        return Ok((s.to_string(), t));
    }
    if !matches.is_empty() {
        let list: Vec<String> = matches.iter().map(|(s, t, n)| format!("{} ({n})", short(s, t))).collect();
        return Err(Error::value(format!("'{value}' matches several {kind}s; use one of: {}", list.join(", "))));
    }
    let other = kind.other();
    if failures.is_empty() {
        for src in SOURCES {
            let Ok(rows) = sources::candidates(src.id, other, opts, &root.join(src.id)) else { break };
            if let Some((t, _, _)) = rows.iter().find(|(_, _, keys)| keys.iter().any(|k| !k.is_empty() && k.to_lowercase() == wanted)) {
                return Err(Error::Lookup(format!("'{value}' is a {other}: bb {other} {}", short(src.id, t))));
            }
        }
    }
    let names = crate::util::dedupe(labels.iter().map(|(k, _)| k.as_str()));
    let hints: Vec<String> = close_matches(&wanted, names, 3, 0.6)
        .into_iter()
        .flat_map(|key| labels.iter().filter(move |(k, _)| k == key).map(|(_, l)| l.clone()))
        .take(3)
        .collect();
    let mut message = format!("No {kind} named '{value}'");
    if hints.is_empty() {
        message += &format!("; search with bb {kind}s -s TEXT");
    } else {
        message += &format!("; did you mean: {}", hints.join(", "));
    }
    if !failures.is_empty() {
        message += &format!(" (not searched: {})", failures.join("; "));
    }
    Err(Error::value(message))
}

fn detail(source: &str, kind: Kind, args: &DetailArgs, target: &str, root: &Path) -> Result<i32> {
    let platform = sources::source(source);
    let opts = &args.common;
    let wrap = format!("{source}:{target}");
    if args.private && !platform.has_feature("private") {
        return Err(Error::value(format!("--private is not available on {}", platform.name)));
    }
    match source {
        "immunefi" => {
            let native = immunefi::cli::Native {
                json: opts.json,
                refresh: opts.refresh,
                offline: opts.offline,
                ttl: opts.ttl,
                cache_dir: Some(root.join(source)),
                no_color: opts.no_color,
            };
            immunefi::cli::show_detail(kind, target, &args.sections, &native, Some(&wrap))
        }
        "cantina" => {
            let flags = cantina::cli::Flags {
                json: opts.json,
                no_color: opts.no_color,
                refresh: opts.refresh,
                offline: opts.offline,
                ttl: opts.ttl,
                cache_dir: Some(root.join(source)),
                firefox_profile: opts.firefox_profile.clone().filter(|_| platform.has_feature("session")),
            };
            cantina::cli::show_detail(kind, target, &args.sections, args.private, &flags, Some(&wrap))
        }
        _ => {
            let sections = if args.sections.is_empty() { vec!["overview".to_string()] } else { args.sections.clone() };
            if let Some(bad) = sections.iter().find(|s| !SECTIONS.contains(&s.as_str())) {
                return Err(Error::value(format!("Unknown section '{bad}'; choose from: {}", SECTIONS.join(", "))));
            }
            let record = sources::served_detail(source, kind, target, opts, &root.join(source))?;
            if opts.json {
                let uid = jv::text(jv::first(&record, &["slug", "id"]));
                return print_json(&json!({"source": source, "kind": kind.as_str(), "target": format!("{source}:{uid}"), "data": record}));
            }
            print!("{}", sources::served_render(source, &record, &sections, opts.style()));
            Ok(0)
        }
    }
}

fn sites(args: &SitesArgs) -> Result<i32> {
    if let Some(bad) = args.platforms.iter().find(|p| sources::get(p).is_none()) {
        return Err(Error::value(format!("Unknown platform '{bad}'; choose from: {}", sources::ids().join(", "))));
    }
    let kinds: Vec<Kind> = match args.kind.as_deref() {
        Some("programs") => vec![Kind::Program],
        Some(_) => vec![Kind::Competition],
        None => vec![Kind::Program, Kind::Competition],
    };
    let rows: Vec<Value> = SOURCES
        .iter()
        .filter(|s| args.platforms.is_empty() || args.platforms.iter().any(|p| p == s.id))
        .flat_map(|s| kinds.iter().map(move |k| json!({"source": s.id, "name": s.name, "kind": k.as_str(), "url": s.listing_url(*k)})))
        .collect();
    if args.open {
        for r in &rows {
            let _ = webbrowser::open(jv::s(r, "url"));
        }
    }
    if args.json {
        return print_json(&rows);
    }
    let table_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let label = if jv::s(r, "kind") == "program" { "bug bounties" } else { "competitions" };
            vec![jv::s(r, "name").into(), label.into(), jv::s(r, "url").into()]
        })
        .collect();
    println!("{}", table(&["PLATFORM", "LISTS", "URL"], &table_rows, &Style::new(false), 1, &[]));
    if args.open {
        println!("\nOpened {} page{}", rows.len(), if rows.len() == 1 { "" } else { "s" });
    }
    Ok(0)
}

fn cache(action: &str, opts: &Common, root: &Path, selected: Option<&str>) -> Result<i32> {
    let scope = match selected {
        Some(s) if sources::get(s).is_some() => root.join(s),
        _ => root.to_path_buf(),
    };
    let files = json_files(&scope);
    let ttl = opts.ttl_hours();
    if action == "clear" {
        for f in &files {
            std::fs::remove_file(f)?;
        }
        if opts.json {
            return print_json(&json!({"removed": files.len(), "dir": root}));
        }
        println!("Removed {} cached responses from {}", files.len(), root.display());
        return Ok(0);
    }
    let entries: Vec<Value> = files
        .iter()
        .filter_map(|p| {
            let e = read_entry(p)?;
            let key = p.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/");
            Some(json!({"key": key, "source": e.source, "fetched_at": e.fetched_at, "fresh": now() - e.fetched_at < ttl * 3600.0}))
        })
        .collect();
    if opts.json {
        return print_json(&json!({"dir": root, "ttl_hours": ttl, "entries": entries}));
    }
    let mut groups: Vec<(String, usize, usize)> = Vec::new();
    for e in &entries {
        let area = jv::s(e, "key").split('/').next().unwrap_or("").to_string();
        let fresh = usize::from(jv::t(e, "fresh"));
        match groups.iter_mut().find(|g| g.0 == area) {
            Some(g) => {
                g.1 += 1;
                g.2 += fresh;
            }
            None => groups.push((area, 1, fresh)),
        }
    }
    groups.sort();
    if groups.is_empty() {
        println!("Cache is empty");
    } else {
        let rows: Vec<Vec<String>> = groups.iter().map(|(a, n, f)| vec![a.clone(), n.to_string(), f.to_string()]).collect();
        println!("{}", table(&["AREA", "ENTRIES", "FRESH"], &rows, &opts.style(), 0, &[]));
    }
    println!("\n{}  (TTL {ttl}h)", root.display());
    Ok(0)
}

fn sources_table(json: bool) -> Result<i32> {
    let result: Vec<Value> = SOURCES
        .iter()
        .map(|s| {
            json!({"id": s.id, "name": s.name, "url": s.site,
                   "listings": {"program": s.program_listing, "competition": s.competition_listing},
                   "commands": s.commands, "features": s.features})
        })
        .collect();
    if json {
        return print_json(&result);
    }
    let rows: Vec<Vec<String>> = SOURCES
        .iter()
        .map(|s| vec![s.id.into(), s.name.into(), s.site.into(), if s.commands.is_empty() { "-".into() } else { s.commands.join(", ") }])
        .collect();
    println!("{}", table(&["ID", "NAME", "SITE", "EXTRA COMMANDS"], &rows, &Style::new(false), 3, &[]));
    println!("\nbb <id> COMMAND runs a command on one platform only; `bb sites` links to their own listings.");
    Ok(0)
}

fn platform_command(command: &str, selected: Option<&str>, argv: Vec<String>) -> Result<i32> {
    let owners = sources::with_command(command);
    let owner = match selected.and_then(sources::get) {
        Some(s) => {
            if !owners.iter().any(|o| o.id == s.id) {
                return Err(Error::value(format!("{command} is not available on {}", s.name)));
            }
            s
        }
        None if owners.len() > 1 => return Err(Error::value(format!("{command} exists on several platforms; pass --source"))),
        None => owners[0],
    };
    Ok((owner.native.expect("platform commands belong to native interfaces"))(argv.into_iter().map(OsString::from).collect()))
}

/// Order key for one row under `sort`.
fn sort_value(r: &Record, sort: &str) -> (f64, String) {
    match sort {
        "bounty" | "reward" | "pool" => (r.reward(), String::new()),
        "name" => (0.0, r.name.clone().unwrap_or_default().to_lowercase()),
        "vault" => (jv::f(&r.raw, "vaultBalance").unwrap_or(0.0), String::new()),
        "launched" | "start" => (0.0, r.start.clone().unwrap_or_default()),
        "end" => (0.0, r.end.clone().unwrap_or_default()),
        _ => (0.0, r.updated.clone().unwrap_or_default()),
    }
}

fn status_rank(status: &str) -> usize {
    ["live", "upcoming", "evaluating", "finished"].iter().position(|s| *s == status).unwrap_or(4)
}

fn listing(kind: Kind, args: &ListArgs, selected: Option<&str>, root: &Path) -> Result<i32> {
    let mut chosen: Vec<&str> = match selected {
        None | Some("all") => sources::ids(),
        Some(s) => vec![sources::source(s).id],
    };
    let default_order = args.sort.is_none();
    let sort = args.sort.clone().unwrap_or_else(|| if kind == Kind::Program { "bounty" } else { "start" }.into());
    if sort == "vault" {
        if kind == Kind::Competition {
            return Err(Error::value("Vault balances exist for programs only; sort competitions by pool instead"));
        }
        let vaults: Vec<&str> = sources::with_feature("vault").iter().map(|s| s.id).collect();
        if let Some(s) = selected.and_then(sources::get) {
            if !vaults.contains(&s.id) {
                return Err(Error::value(format!("{} publishes no vault balances; sort by bounty instead", s.name)));
            }
        }
        if chosen.len() > 1 {
            let names: Vec<&str> = vaults.iter().map(|v| sources::source(v).name).collect();
            eprintln!("Warning: only {} publishes vault balances; listing only those programs", names.join(", "));
        }
        chosen.retain(|s| vaults.contains(s));
    }
    let filters = Filters {
        search: args.search.clone().unwrap_or_default(),
        ecosystem: args.ecosystem.clone(),
        language: args.language.clone(),
        program_type: args.program_type.clone(),
        project_type: args.project_type.clone(),
        kyc: args.kyc.as_deref().map(|k| k == "yes"),
        min_bounty: args.min_bounty,
        status: args.status.clone(),
        include_paused: args.include_paused,
        sort: Some(sort.clone()),
        reverse: false,
    };
    let opts = &args.common;
    let results: Vec<(&str, Result<Vec<Record>>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = chosen
            .iter()
            .map(|id| {
                let filters = &filters;
                let dir = root.join(id);
                (*id, scope.spawn(move || sources::listing(id, kind, filters, opts, &dir)))
            })
            .collect();
        handles.into_iter().map(|(id, h)| (id, h.join().unwrap_or_else(|_| Err(Error::value("listing thread panicked"))))).collect()
    });
    let mut rows: Vec<Record> = Vec::new();
    for (id, result) in results {
        match result {
            Ok(found) => rows.extend(found),
            Err(e) => {
                return Err(Error::value(format!("{}: {e}; no partial combined listing was returned", sources::source(id).name)));
            }
        }
    }
    let total = rows.len();
    let descending = (sort != "name") != args.reverse;
    rows.sort_by(|a, b| {
        let (x, y) = (sort_value(a, &sort), sort_value(b, &sort));
        let ord = x.0.total_cmp(&y.0).then_with(|| x.1.cmp(&y.1));
        if descending { ord.reverse() } else { ord }
    });
    if default_order && kind == Kind::Competition {
        // Stable: newest first within live, upcoming, judging, finished.
        rows.sort_by_key(|r| status_rank(&r.status));
    }
    let limit = if args.all || (opts.json && args.limit.is_none()) { None } else { Some(args.limit.map_or(DEFAULT_ROWS, |n| n as usize)) };
    if let Some(n) = limit {
        rows.truncate(n);
    }
    if opts.json {
        return print_json(&rows);
    }
    if rows.is_empty() {
        println!("No matching {kind}s.");
        return Ok(0);
    }
    let style = opts.style();
    let table_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let kyc = match r.kyc {
                None => "?",
                Some(true) => "yes",
                Some(false) => "no",
            };
            vec![
                r.name.clone().unwrap_or_else(|| "(restricted)".into()),
                short(&r.source, &r.id_text()),
                r.status.to_uppercase(),
                format!("{} {}", money(Some(r.reward()), false), r.currency.clone().unwrap_or_default()),
                kyc.into(),
            ]
        })
        .collect();
    println!("{}", table(&["PROJECT", "TARGET", "STATUS", "REWARD", "KYC"], &table_rows, &style, 0, &[(2, Styler::Status)]));
    let more = if rows.len() < total { "  (-n N or --all for more)" } else { "" };
    println!("\n{} of {total} matching {kind}s{more}", rows.len());
    Ok(0)
}

fn contract(mut action: ContractCmd, selected: Option<&str>, root: &Path) -> Result<i32> {
    let mut source: Option<String> = None;
    let opts = action.common().clone();
    if let Some((program, kind)) = action.scope_target_mut() {
        let (found, value) = target_source(program, selected)?;
        let resolved = match found {
            Some(s) => {
                let value = expand_prefix(&s, &value, kind, &opts, root)?;
                (s, value)
            }
            None => resolve_target(&value, kind, &opts, root)?,
        };
        *program = resolved.1;
        source = Some(resolved.0);
    }
    let evm_cache = root.join("evm");
    let platform = source.as_deref().map(|s| sources::PlatformScope::new(s, opts.clone(), root.join(s)));
    contract_cli::run(&action, &contract_cli::Ctx { evm_cache, scope: platform.as_ref().map(|p| p as &dyn contract_cli::ScopeSource) })
}

fn validate(common: &mut Common) -> Result<PathBuf> {
    *common = std::mem::take(common).validated(&[])?;
    Ok(crate::paths::expand(&common.cache_dir.clone().unwrap_or_else(cache_root)))
}

fn run(argv: Vec<String>) -> Result<i32> {
    let (mut selected, mut argv) = extract_source(argv)?;
    let mut as_command = false;
    if let Some(first) = argv.first().cloned() {
        if let Some(platform) = sources::get(&first) {
            argv.remove(0);
            if selected.as_ref().is_some_and(|s| s != platform.id) {
                return Err(Error::value("Native command conflicts with --source"));
            }
            if let Some(native) = platform.native {
                return Ok(native(argv.into_iter().map(OsString::from).collect()));
            }
            selected = Some(platform.id.to_string()); // no native interface: the shared commands, limited to this platform
            as_command = true;
        }
    }
    if let Some(first) = argv.first().cloned() {
        if !sources::with_command(&first).is_empty() {
            return platform_command(&first, selected.as_deref(), argv);
        }
    }
    let mut command = Cli::command();
    if let Some(platform) = selected.as_deref().and_then(sources::get) {
        if as_command {
            command = command
                .bin_name(format!("bb {}", platform.id))
                .about(format!("The shared bb commands, limited to {}.", platform.name))
                .after_help("Targets accept a name, an id from a listing, or the opportunity URL.");
        }
        for sub in ["programs", "competitions"] {
            command = command.mut_subcommand(sub, |c| {
                let help = c
                    .get_before_help()
                    .map(|h| h.to_string().replace("from every platform, sorted together", &format!("on {}", platform.name)));
                match help {
                    Some(help) => c.before_help(help),
                    None => c,
                }
            });
        }
    }
    let parsed =
        command.try_get_matches_from(std::iter::once("bb".to_string()).chain(argv)).and_then(|mut m| Cli::from_arg_matches_mut(&mut m));
    let cli = match parsed {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return Ok(e.exit_code());
        }
    };
    let selected = selected.as_deref();
    match cli.command {
        Command::Sources { json } => sources_table(json),
        Command::Sites(args) => sites(&args),
        Command::Cache { action, mut common } => {
            let root = validate(&mut common)?;
            cache(&action, &common, &root, selected)
        }
        Command::Programs(mut args) | Command::Competitions(mut args) if args.limit.is_some_and(|n| n <= 0) => {
            let _ = validate(&mut args.common)?;
            Err(Error::value("Limit must be positive"))
        }
        Command::Programs(mut args) => {
            let root = validate_list(&mut args)?;
            listing(Kind::Program, &args, selected, &root)
        }
        Command::Competitions(mut args) => {
            let root = validate_list(&mut args)?;
            listing(Kind::Competition, &args, selected, &root)
        }
        Command::Program(args) => detail_command(Kind::Program, args, selected),
        Command::Competition(args) => detail_command(Kind::Competition, args, selected),
        Command::Contract { mut action } => {
            let root = validate(action.common_mut())?;
            contract(action, selected, &root)
        }
    }
}

fn validate_list(args: &mut ListArgs) -> Result<PathBuf> {
    let root = validate(&mut args.common)?;
    if !args.min_bounty.is_finite() || args.min_bounty < 0.0 {
        return Err(Error::value("Minimum bounty must be finite and nonnegative"));
    }
    Ok(root)
}

fn detail_command(kind: Kind, mut args: DetailArgs, selected: Option<&str>) -> Result<i32> {
    let root = validate(&mut args.common)?;
    let (source, value) = target_source(&args.target, selected)?;
    let (source, target) = match source {
        Some(s) => {
            let expanded = expand_prefix(&s, &value, kind, &args.common, &root)?;
            (s, expanded)
        }
        None => resolve_target(&value, kind, &args.common, &root)?,
    };
    detail(&source, kind, &args, &target, &root)
}

/// Entry point for `bb ARGS` (`argv` excludes the program name).
pub fn main(argv: Vec<OsString>) -> i32 {
    let argv: Vec<String> = argv.into_iter().map(|a| a.to_string_lossy().into_owned()).collect();
    match run(argv) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("bb: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn source_selection_anywhere() {
        for argv in [strings(&["--source", "cantina", "programs"]), strings(&["programs", "--source=cantina"])] {
            assert_eq!(extract_source(argv).unwrap(), (Some("cantina".into()), strings(&["programs"])));
        }
        assert!(extract_source(strings(&["--source", "nope"])).is_err());
        assert!(extract_source(strings(&["--source=cantina", "--source", "immunefi"])).is_err());
        assert_eq!(extract_source(strings(&["x", "--", "--source", "y"])).unwrap().1, strings(&["x", "--", "--source", "y"]));
    }

    #[test]
    fn target_prefix_and_url_inference() {
        assert_eq!(target_source("cantina:Uniswap", None).unwrap(), (Some("cantina".into()), "Uniswap".into()));
        assert_eq!(target_source("https://immunefi.com/bug-bounty/sky/information/", None).unwrap().0.as_deref(), Some("immunefi"));
        assert!(target_source("cantina:Uniswap", Some("immunefi")).is_err());
        assert!(target_source("https://example.com/x", None).is_err());
        assert_eq!(target_source("Uniswap", Some("sherlock")).unwrap(), (Some("sherlock".into()), "Uniswap".into()));
        assert_eq!(target_source("odd:name", None).unwrap(), (None, "odd:name".into()));
    }

    #[test]
    fn short_ids() {
        assert_eq!(short("cantina", "f9df94db-c7b1-434b-bb06-d1360abdd1be"), "cantina:f9df94db");
        assert_eq!(short("sherlock", "56"), "sherlock:56");
    }

    #[test]
    fn prefix_expansion_uses_cached_candidates() {
        let root = tempfile::tempdir().unwrap();
        let opts = Common { offline: true, ttl: Some(12.0), ..Default::default() };
        let dir = root.path().join("sherlock");
        let cache = crate::cache::Cache::new(&dir, 12.0);
        cache.write("bug_bounties-1", json!({"items": [{"id": "abcdef12"}, {"id": "abcdef34"}], "has_next": false}), "u").unwrap();
        cache.write("contests-1", json!({"items": [{"id": "fedcba98"}], "has_next": false}), "u").unwrap();
        let expand = |v: &str| expand_prefix("sherlock", v, Kind::Program, &opts, root.path());
        assert_eq!(expand("fedcba").unwrap(), "fedcba98");
        assert!(expand("abcdef").unwrap_err().to_string().contains("matches several ids"));
        assert_eq!(expand("Usual").unwrap(), "Usual");
    }
}
