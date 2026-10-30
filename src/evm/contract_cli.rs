//! `bb contract ...`: inspect and read smart contracts across EVM chains.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use alloy_primitives::{U256, keccak256};
use chrono::{TimeZone, Utc};
use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};

use super::abi::{self, Function};
use super::client::Client;
use super::explorers::{Chain, Contract, NON_EVM_HOSTS, first_address, host_of};
use super::onchain::{OnChain, RPC_MODES, slot};
use crate::cache::Cache;
use crate::error::{Error, Result};
use crate::jv;
use crate::opts::{Common, Kind, print_json};
use crate::render::{Doc, Style, table, thousands};

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "chain")]
pub struct ChainOpts {
    /// chain id or name (eth, arbitrum, polygon, base, ...); default: from the explorer URL, else Ethereum
    #[arg(long)]
    pub chain: Option<String>,
    /// Etherscan API key (default: $ETHERSCAN_API_KEY)
    #[arg(long, value_name = "KEY")]
    pub etherscan_key: Option<String>,
    /// node for live reads: a JSON-RPC URL, or auto (QuickNode via the `qn` CLI, then the chain's public RPC, its explorer's RPC, then Etherscan), quicknode, public, explorer, etherscan (default: auto)
    #[arg(long, value_name = "URL|MODE", default_value = "auto", hide_default_value = true)]
    pub rpc: String,
    /// block number for live reads (default: latest)
    #[arg(long)]
    pub block: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct TargetArgs {
    /// address or explorer URL (e.g. https://etherscan.io/address/0x...)
    pub target: String,
    #[command(flatten)]
    pub common: Common,
    #[command(flatten)]
    pub chain: ChainOpts,
}

#[derive(Args, Debug, Clone)]
pub struct SourceArgs {
    /// address or explorer URL (e.g. https://etherscan.io/address/0x...)
    pub target: String,
    /// output directory (default ./<Name>-<address>); '-' prints to stdout
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// for a proxy, download the implementation instead
    #[arg(long = "impl")]
    pub implementation: bool,
    #[command(flatten)]
    pub common: Common,
    #[command(flatten)]
    pub chain: ChainOpts,
}

#[derive(Args, Debug, Clone)]
pub struct CallArgs {
    /// address or explorer URL (e.g. https://etherscan.io/address/0x...)
    pub target: String,
    /// balanceOf | 'balanceOf(address)' | 'balanceOf(address)(uint256)' | 0x70a08231...
    pub function: String,
    /// arguments; arrays/tuples as [a,b] / (a,b); integers accept 1e18 or 1.5ether
    #[arg(allow_negative_numbers = true)]
    pub args: Vec<String>,
    #[command(flatten)]
    pub common: Common,
    #[command(flatten)]
    pub chain: ChainOpts,
}

#[derive(Args, Debug, Clone)]
pub struct StorageArgs {
    /// address or explorer URL (e.g. https://etherscan.io/address/0x...)
    pub target: String,
    /// slot number/hex, or implementation | admin | beacon (EIP-1967)
    pub slot: String,
    /// read N consecutive slots
    #[arg(short = 'n', long, default_value_t = 1)]
    pub count: i64,
    #[command(flatten)]
    pub common: Common,
    #[command(flatten)]
    pub chain: ChainOpts,
}

#[derive(Args, Debug, Clone)]
pub struct ScopeArgs {
    /// program or competition: name, source:id or opportunity URL
    pub program: String,
    /// the target is an audit competition
    #[arg(long)]
    pub competition: bool,
    /// also download verified sources (implementations too) into DIR
    #[arg(long, value_name = "DIR")]
    pub download: Option<PathBuf>,
    /// only the first N contracts
    #[arg(long)]
    pub limit: Option<usize>,
    /// skip the deployed-code check against a node
    #[arg(long)]
    pub no_live: bool,
    #[command(flatten)]
    pub common: Common,
    #[command(flatten)]
    pub chain: ChainOpts,
}

#[derive(Args, Debug, Clone)]
pub struct ChainsArgs {
    #[command(flatten)]
    pub common: Common,
    #[command(flatten)]
    pub chain: ChainOpts,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ContractCmd {
    /// contract name, verification, proxy, creator, balance, functions
    Info(TargetArgs),
    /// functions with selectors and events (--json for the raw ABI)
    Abi(TargetArgs),
    /// download verified source files
    Source(SourceArgs),
    /// call every argument-less view function (one batched request)
    Read(TargetArgs),
    /// call a function: name, signature, 'sig(in)(out)' or raw 0x calldata
    #[command(long_about = "Simulated with eth_call: nothing is signed or sent.")]
    Call(CallArgs),
    /// read raw storage slots
    Storage(StorageArgs),
    /// resolve a program's in-scope contracts on their explorers
    Scope(ScopeArgs),
    /// list known chains, their source providers and nodes
    Chains(ChainsArgs),
}

impl ContractCmd {
    fn parts(&self) -> (&Common, &ChainOpts) {
        match self {
            ContractCmd::Info(a) | ContractCmd::Abi(a) | ContractCmd::Read(a) => (&a.common, &a.chain),
            ContractCmd::Source(a) => (&a.common, &a.chain),
            ContractCmd::Call(a) => (&a.common, &a.chain),
            ContractCmd::Storage(a) => (&a.common, &a.chain),
            ContractCmd::Scope(a) => (&a.common, &a.chain),
            ContractCmd::Chains(a) => (&a.common, &a.chain),
        }
    }

    pub fn common(&self) -> &Common {
        self.parts().0
    }

    pub fn common_mut(&mut self) -> &mut Common {
        match self {
            ContractCmd::Info(a) | ContractCmd::Abi(a) | ContractCmd::Read(a) => &mut a.common,
            ContractCmd::Source(a) => &mut a.common,
            ContractCmd::Call(a) => &mut a.common,
            ContractCmd::Storage(a) => &mut a.common,
            ContractCmd::Scope(a) => &mut a.common,
            ContractCmd::Chains(a) => &mut a.common,
        }
    }

    /// For `scope`: the program target and whether --competition was given.
    pub fn scope_target_mut(&mut self) -> Option<(&mut String, Kind)> {
        match self {
            ContractCmd::Scope(a) => {
                let kind = if a.competition { Kind::Competition } else { Kind::Program };
                Some((&mut a.program, kind))
            }
            _ => None,
        }
    }
}

/// A program's assets, as a platform lists them.
pub struct ScopeRecord {
    pub slug: Value,
    pub project: Option<String>,
    /// Objects with "url", "type" ("smart_contract"), "description", and optionally "address" plus
    /// "chain" (a network name such as "ethereum", resolved like `--chain`).
    pub assets: Vec<Value>,
}

/// Where `scope` gets a program's assets from.
pub trait ScopeSource {
    fn scope(&self, kind: Kind, target: &str) -> Result<ScopeRecord>;
}

pub struct Ctx<'a> {
    pub evm_cache: PathBuf,
    pub scope: Option<&'a dyn ScopeSource>,
}

/// Runs the command; returns the process exit code.
pub fn run(cmd: &ContractCmd, ctx: &Ctx) -> Result<i32> {
    let (common, chain) = cmd.parts();
    let client = Client::new(Cache::new(&ctx.evm_cache, common.ttl_hours()), common.refresh, common.offline);
    let es = OnChain::new(client, chain.etherscan_key.clone(), Some(&chain.rpc));
    let style = common.style();
    match cmd {
        ContractCmd::Info(a) => cmd_info(&es, a, &style),
        ContractCmd::Abi(a) => cmd_abi(&es, a, &style),
        ContractCmd::Source(a) => cmd_source(&es, a, &style),
        ContractCmd::Read(a) => cmd_read(&es, a, &style),
        ContractCmd::Call(a) => cmd_call(&es, a, &style),
        ContractCmd::Storage(a) => cmd_storage(&es, a, &style),
        ContractCmd::Scope(a) => cmd_scope(&es, a, ctx.scope, &style),
        ContractCmd::Chains(a) => cmd_chains(&es, a, &style),
    }
}

// --- helpers ------------------------------------------------------------------------------------

/// Write to stdout; a closed pipe (`| head`) is not an error.
fn out(text: impl AsRef<str>) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_ref().as_bytes()).and_then(|_| stdout.write_all(b"\n"));
}

fn target(es: &OnChain, target: &str, chain: &ChainOpts) -> Result<(Chain, String)> {
    es.parse_target(target, chain.chain.as_deref())
}

fn short(address: Option<&str>) -> String {
    match address {
        Some(a) if a.len() > 10 => format!("{}…{}", &a[..6], &a[a.len() - 4..]),
        Some(a) => a.to_string(),
        None => "-".into(),
    }
}

/// Wei as the native token: `1,234.5` (6 decimals at most, trailing zeros dropped).
pub fn native(wei: U256) -> String {
    let unit = U256::from(10u64).pow(U256::from(12));
    let (mut scaled, rem) = wei.div_rem(unit);
    let half = unit / U256::from(2);
    if rem > half || (rem == half && scaled.bit(0)) {
        scaled += U256::from(1);
    }
    let million = U256::from(1_000_000u64);
    let (whole, frac) = scaled.div_rem(million);
    let whole = whole.to_string();
    let mut grouped = String::new();
    for (i, ch) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let text = format!("{grouped}.{:06}", frac.to::<u64>());
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn progress(text: &str) {
    if std::io::stderr().is_terminal() {
        eprint!("\r\x1b[K{text}");
    }
}

fn progress_done() {
    if std::io::stderr().is_terminal() {
        eprint!("\r\x1b[K");
    }
}

fn compiler(c: &Contract) -> String {
    let raw = &c.raw;
    let mut parts = vec![jv::s(raw, "CompilerVersion").to_string()];
    if jv::s(raw, "OptimizationUsed") == "1" {
        parts.push(match jv::opt(raw, "Runs") {
            Some(runs) => format!("optimizer {runs} runs"),
            None => "optimizer".into(),
        });
    }
    if let Some(evm) = jv::opt(raw, "EVMVersion").filter(|v| v.to_lowercase() != "default") {
        parts.push(format!("evm {evm}"));
    }
    parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(", ")
}

fn write_sources(contract: &Contract, dir: &Path) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for (rel, content) in contract.source_files() {
        let path = dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
        written.push(path);
    }
    let raw = &contract.raw;
    let meta = json!({
        "address": contract.address,
        "chainId": contract.chain.id,
        "chain": contract.chain.name,
        "explorer": contract.chain.address_url(&contract.address),
        "contractName": contract.name(),
        "compilerVersion": jv::at(raw, "CompilerVersion"),
        "optimizationUsed": jv::s(raw, "OptimizationUsed") == "1",
        "runs": jv::at(raw, "Runs"),
        "evmVersion": jv::at(raw, "EVMVersion"),
        "license": jv::at(raw, "LicenseType"),
        "provider": contract.provider(),
        "constructorArguments": jv::at(raw, "ConstructorArguments"),
        "proxy": jv::s(raw, "Proxy") == "1",
        "implementation": contract.implementation(),
        "settings": contract.compiler_settings(),
        "abi": contract.abi(),
    });
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("contract.json"), serde_json::to_string_pretty(&meta)?)?;
    Ok(written)
}

fn function_lines(fns: &[&Function], style: &Style) -> Vec<String> {
    let mut sorted: Vec<&&Function> = fns.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    sorted.into_iter().map(|f| format!("  {}  {}", style.dim(&format!("0x{}", hex::encode(f.selector()))), f.describe())).collect()
}

fn hex0x(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// Errors that mean a provider or node could not answer (Python's `ChainError`).
fn is_chain_error(e: &Error) -> bool {
    matches!(e, Error::Chain(_) | Error::RateLimited(_) | Error::Reverted(_))
}

// --- commands -----------------------------------------------------------------------------------

fn cmd_chains(es: &OnChain, a: &ChainsArgs, style: &Style) -> Result<i32> {
    let rpc = a.chain.rpc.as_str();
    let mut chains: Vec<Chain> = es.registry()?.chains.clone();
    let mut qn_chains = std::collections::HashMap::new();
    if matches!(rpc, "auto" | "quicknode") && !a.common.offline {
        if let Some(qn) = es.quicknode() {
            qn_chains = qn.chain_ids(&es.client)?;
            for (id, key) in &qn_chains {
                if !chains.iter().any(|c| c.id == *id) {
                    chains.push(Chain::bare(*id, key.clone()));
                }
            }
        }
    }
    chains.sort_by_key(|c| c.id);
    let records: Vec<Value> = chains
        .iter()
        .map(|chain| {
            let live = if let Some(key) = qn_chains.get(&chain.id) {
                format!("quicknode ({key})")
            } else if !RPC_MODES.contains(&rpc) {
                "--rpc".into()
            } else if !chain.public_rpcs.is_empty() && matches!(rpc, "auto" | "public") {
                "public RPC (latest only)".into()
            } else if !chain.explorer_rpcs.is_empty() {
                "explorer (latest only)".into()
            } else if chain.etherscan {
                if chain.etherscan_live { "etherscan (latest only)" } else { "etherscan paid plan" }.into()
            } else {
                "-".into()
            };
            json!({"id": chain.id, "name": chain.name, "explorer": chain.explorer, "source": chain.source_providers(), "live": live})
        })
        .collect();
    if a.common.json {
        return print_json(&records);
    }
    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|r| {
            let sources: Vec<&str> = r["source"].as_array().map(|s| s.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            vec![r["id"].to_string(), jv::s(r, "name").into(), sources.join(", "), jv::s(r, "live").into()]
        })
        .collect();
    out(table(&["ID", "CHAIN", "VERIFIED SOURCE", "LIVE READS"], &rows, style, 2, &[]));
    out(style.dim("\nHistorical reads (--block) need QuickNode or --rpc; public and explorer RPCs only serve the latest state."));
    Ok(0)
}

fn cmd_info(es: &OnChain, a: &TargetArgs, style: &Style) -> Result<i32> {
    let (chain, address) = target(es, &a.target, &a.chain)?;
    let block = a.chain.block.as_deref();
    // Live state is optional: on paid-only chains the free key still serves source and ABI.
    let (mut code_size, mut balance, mut slot_impl, mut live_error) = (None, None, None, None);
    let live = (|| -> Result<()> {
        code_size = Some(es.code_size(&chain, &address, block)?);
        balance = Some(es.balance(&chain, &address, block)?);
        if code_size.unwrap_or(0) > 0 {
            slot_impl = es.live_implementation(&chain, &address, block)?;
        }
        Ok(())
    })();
    match live {
        Err(e) if is_chain_error(&e) => live_error = Some(e.to_string()),
        Err(e) => return Err(e),
        Ok(()) => {}
    }
    if code_size == Some(0) {
        if a.common.json {
            let wei = balance.map(|b| b.to_string()).unwrap_or_else(|| "None".into());
            return print_json(&json!({"address": address, "chainId": chain.id, "contract": false, "balanceWei": wei}));
        }
        out(format!("{address} on {} has no code (EOA or not deployed); balance {}", chain.name, native(balance.unwrap_or_default())));
        return Ok(0);
    }
    let contract = es.contract(&chain, &address)?;
    let creation = match es.creation(&chain, &address) {
        Ok(c) => c,
        Err(e) if e.is_chain_failure() => None, // paid-only chain, or not cached in --offline mode
        Err(e) => return Err(e),
    };
    let (fns, implementation) = es.functions_for(&contract)?;
    let reads: Vec<&Function> = fns.iter().filter(|f| f.is_read()).collect();
    let writes: Vec<&Function> = fns.iter().filter(|f| !f.is_read()).collect();

    if a.common.json {
        return print_json(&json!({
            "address": address,
            "chainId": chain.id,
            "chain": chain.name,
            "explorer": chain.address_url(&address),
            "name": contract.name(),
            "verified": contract.verified(),
            "compiler": compiler(&contract),
            "license": jv::at(&contract.raw, "LicenseType"),
            "proxy": jv::s(&contract.raw, "Proxy") == "1" || slot_impl.is_some(),
            "implementation": contract.implementation().or_else(|| slot_impl.clone()),
            "implementationName": implementation.as_ref().map(|c| c.name()),
            "eip1967Implementation": slot_impl,
            "balanceWei": balance.map(|b| b.to_string()),
            "codeSize": code_size,
            "liveError": live_error,
            "creation": creation,
            "readFunctions": reads.iter().map(|f| f.describe()).collect::<Vec<_>>(),
            "writeFunctions": writes.iter().map(|f| f.describe()).collect::<Vec<_>>(),
        }));
    }

    let mut doc = Doc::new(*style);
    let title = implementation.as_ref().map(|c| c.name()).unwrap_or(contract.name());
    let title = if title.is_empty() { "Unverified contract" } else { title };
    doc.raw(format!("{}  {}", style.bold(title), style.link(&chain.address_url(&address))));
    let impl_address = contract.implementation().or_else(|| slot_impl.clone());
    let mut proxy_line = String::new();
    if let Some(implementation) = &impl_address {
        let target = es.contract(&chain, implementation)?;
        let own = if contract.name().is_empty() { "proxy" } else { contract.name() };
        proxy_line = format!("yes ({own}) → {implementation}");
        proxy_line += &format!(" ({}{})", target.name(), if target.verified() { "" } else { ", unverified" });
        if let (Some(slot_address), Some(listed)) = (&slot_impl, contract.implementation()) {
            if *slot_address != listed {
                proxy_line += &format!("; EIP-1967 slot now points to {slot_address}");
            }
        }
    }
    let mut created = String::new();
    if let Some(c) = &creation {
        let ts = jv::text(jv::at(c, "timestamp"));
        let when = if !ts.is_empty() && ts.chars().all(|ch| ch.is_ascii_digit()) {
            ts.parse::<i64>()
                .ok()
                .and_then(|t| Utc.timestamp_opt(t, 0).single())
                .map(|d| d.format("%Y-%m-%d ").to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let mut where_parts = Vec::new();
        if jv::t(c, "blockNumber") {
            where_parts.push(format!("block {}", jv::text(jv::at(c, "blockNumber"))));
        }
        if jv::t(c, "txHash") {
            where_parts.push(format!("tx {}", short(jv::opt(c, "txHash"))));
        }
        let creator = jv::opt(c, "contractCreator").unwrap_or("?");
        created = format!("{when}by {creator}");
        if !where_parts.is_empty() {
            created += &format!(" ({})", where_parts.join(", "));
        }
    }
    let license = jv::s(&contract.raw, "LicenseType").to_string();
    doc.fields(&[
        ("Chain", format!("{} ({})", chain.name, chain.id)),
        ("Verified", if contract.verified() { "yes" } else { "no" }.to_string()),
        ("Compiler", compiler(&contract)),
        ("License", license),
        ("Proxy", if proxy_line.is_empty() { "no".into() } else { proxy_line }),
        ("Balance", balance.map(|b| format!("{} (native token)", native(b))).unwrap_or_default()),
        ("Code size", code_size.filter(|s| *s > 0).map(|s| format!("{} bytes", thousands(s as f64))).unwrap_or_default()),
        ("Live data", live_error.as_deref().map(|e| style.dim(e)).unwrap_or_default()),
        ("Created", created),
    ]);
    if !fns.is_empty() {
        doc.sub(&format!("Read functions ({})", reads.len()));
        let lines = function_lines(&reads, style);
        doc.raw(if lines.is_empty() { "  -".to_string() } else { lines.join("\n") });
        doc.sub(&format!("Write functions ({})", writes.len()));
        let lines = function_lines(&writes, style);
        doc.raw(if lines.is_empty() { "  -".to_string() } else { lines.join("\n") });
    }
    let hint = format!("bb contract read {address} --chain {}", chain.id);
    let source_hint = format!("bb contract source {address} --chain {}{}", chain.id, if impl_address.is_some() { " --impl" } else { "" });
    doc.raw(format!("\n{}", style.dim(&format!("Next: `{hint}` to dump state, `{source_hint}` for code"))));
    let text = doc.render();
    out(text.strip_suffix('\n').unwrap_or(&text));
    Ok(0)
}

fn cmd_abi(es: &OnChain, a: &TargetArgs, style: &Style) -> Result<i32> {
    let (chain, address) = target(es, &a.target, &a.chain)?;
    let contract = es.contract(&chain, &address)?;
    let (fns, implementation) = es.functions_for(&contract)?;
    let source = implementation.as_ref().unwrap_or(&contract);
    if fns.is_empty() {
        return Err(Error::chain(format!("{address} is not verified on {}; no ABI available", chain.name)));
    }
    if a.common.json {
        let mut items = source.abi();
        if implementation.is_some() {
            let seen: Vec<String> = source.functions()?.iter().map(Function::signature).collect();
            for item in contract.abi() {
                if jv::s(&item, "type") != "function" {
                    continue;
                }
                let parsed = abi::functions_from_abi(std::slice::from_ref(&item))?;
                if parsed.first().is_some_and(|f| !seen.contains(&f.signature())) {
                    items.push(item);
                }
            }
        }
        return print_json(&items);
    }
    if let Some(i) = &implementation {
        out(style.dim(&format!("(proxy {address}: showing implementation {} {})", i.address, i.name())));
    }
    let reads: Vec<&Function> = fns.iter().filter(|f| f.is_read()).collect();
    let writes: Vec<&Function> = fns.iter().filter(|f| !f.is_read()).collect();
    out(style.bold(&format!("Read ({})", reads.len())));
    out(function_lines(&reads, style).join("\n"));
    out(style.bold(&format!("\nWrite ({})", writes.len())));
    out(function_lines(&writes, style).join("\n"));
    let mut events: Vec<Value> = source.abi().into_iter().filter(|i| jv::s(i, "type") == "event").collect();
    if !events.is_empty() {
        out(style.bold(&format!("\nEvents ({})", events.len())));
        events.sort_by(|x, y| jv::s(x, "name").cmp(jv::s(y, "name")));
        for e in &events {
            let types = jv::arr(e, "inputs").iter().map(abi::from_json).collect::<Result<Vec<_>>>()?;
            let sig = format!("{}({})", jv::text(jv::at(e, "name")), types.iter().map(|t| t.canonical()).collect::<Vec<_>>().join(","));
            out(format!("  {}  {sig}", style.dim(&hex0x(keccak256(sig.as_bytes()).as_slice()))));
        }
    }
    Ok(0)
}

fn cmd_source(es: &OnChain, a: &SourceArgs, style: &Style) -> Result<i32> {
    let (chain, address) = target(es, &a.target, &a.chain)?;
    let mut contract = es.contract(&chain, &address)?;
    if a.implementation {
        let impl_address = match contract.implementation() {
            Some(i) => Some(i),
            None => es.live_implementation(&chain, &address, a.chain.block.as_deref())?,
        };
        let Some(impl_address) = impl_address else {
            return Err(Error::chain(format!("{address} is not a proxy (no implementation found)")));
        };
        contract = es.contract(&chain, &impl_address)?;
    }
    if !contract.verified() {
        return Err(Error::chain(format!("{} is not verified on {}", contract.address, chain.name)));
    }
    if a.out.as_deref() == Some(Path::new("-")) {
        for (path, content) in contract.source_files() {
            out(format!("// ===== {path} =====\n{content}\n"));
        }
        return Ok(0);
    }
    let name = if contract.name().is_empty() { "Contract" } else { contract.name() };
    let dir = a.out.clone().unwrap_or_else(|| PathBuf::from(format!("{name}-{}", contract.address)));
    let written = write_sources(&contract, &dir)?;
    if a.common.json {
        let files: Vec<String> = written.iter().map(|p| p.display().to_string()).collect();
        return print_json(&json!({"dir": dir.display().to_string(), "files": files}));
    }
    out(format!(
        "Wrote {} files for {} ({}) to {}/ (+ contract.json, from {})",
        written.len(),
        contract.name(),
        contract.address,
        dir.display(),
        contract.provider()
    ));
    if !a.implementation {
        if let Some(i) = contract.implementation() {
            out(style.dim(&format!("This is a proxy for {i}; add --impl to download the implementation.")));
        }
    }
    Ok(0)
}

fn cmd_read(es: &OnChain, a: &TargetArgs, style: &Style) -> Result<i32> {
    let (chain, address) = target(es, &a.target, &a.chain)?;
    let block = a.chain.block.as_deref();
    let contract = es.contract(&chain, &address)?;
    let (fns, implementation) = es.functions_for(&contract)?;
    let mut getters: Vec<&Function> = fns.iter().filter(|f| f.is_read() && f.inputs.is_empty()).collect();
    getters.sort_by(|x, y| x.name.cmp(&y.name));
    if getters.is_empty() {
        return Err(Error::chain(format!("no argument-less view functions found for {address} (is it verified?)")));
    }
    let calls: Vec<(String, Vec<u8>)> = getters.iter().map(|f| (address.clone(), f.encode_call(&[]))).collect();
    let results = es.multicall(&chain, &calls, block)?;
    let mut values = Map::new();
    let mut lines = Vec::new();
    let width = getters.iter().map(|f| f.name.chars().count()).max().unwrap_or(0);
    for (f, (ok, data)) in getters.iter().zip(results) {
        let name = format!("{:<width$}", f.name);
        if !ok {
            let reason = if !data.is_empty() && data[0] != 0x08 { String::from_utf8_lossy(&data).into_owned() } else { "reverted".into() };
            lines.push(format!("  {name}  {}", style.dim(&reason)));
            values.insert(f.name.clone(), json!({"error": reason}));
            continue;
        }
        let outs = f.outputs.clone().unwrap_or_default();
        let Ok(decoded) = abi::decode(&outs, &data) else {
            values.insert(f.name.clone(), json!(hex0x(&data)));
            lines.push(format!("  {name}  {}", hex0x(&data)));
            continue;
        };
        let text = if outs.len() == 1 {
            values.insert(f.name.clone(), abi::to_json(&outs[0], &decoded[0]));
            abi::format_value(&outs[0], &decoded[0])
        } else {
            values.insert(f.name.clone(), Value::Array(outs.iter().zip(&decoded).map(|(t, v)| abi::to_json(t, v)).collect()));
            format!("({})", outs.iter().zip(&decoded).map(|(t, v)| abi::format_value(t, v)).collect::<Vec<_>>().join(", "))
        };
        lines.push(format!("  {}  {text}", style.bold(&name)));
    }
    if a.common.json {
        return print_json(&Value::Object(values));
    }
    let label = match &implementation {
        Some(i) => format!("{} via proxy", i.name()),
        None => contract.name().to_string(),
    };
    out(format!("{} {address} on {} @ {}", style.bold(&label), chain.name, block.unwrap_or("latest")));
    out(lines.join("\n"));
    Ok(0)
}

fn resolve_function(fns: &[Function], spec: &str, nargs: usize) -> Result<Function> {
    if spec.contains('(') {
        let parsed = abi::parse_signature(spec)?;
        if parsed.outputs.is_none() {
            if let Some(known) = fns.iter().find(|f| f.signature() == parsed.signature()) {
                return Ok(known.clone());
            }
        }
        return Ok(parsed);
    }
    let matches: Vec<&Function> = fns.iter().filter(|f| f.name == spec).collect();
    if matches.is_empty() {
        return Err(Error::value(format!(
            "no function named {} in the ABI; pass a full signature like '{spec}(uint256)(uint256)'",
            abi::py_repr(spec)
        )));
    }
    let same_arity: Vec<&Function> = matches.iter().copied().filter(|f| f.inputs.len() == nargs).collect();
    if same_arity.len() == 1 {
        return Ok(same_arity[0].clone());
    }
    let options = if same_arity.is_empty() { &matches } else { &same_arity };
    Err(Error::value(format!(
        "{} is ambiguous or takes a different number of arguments: {}",
        abi::py_repr(spec),
        options.iter().map(|f| f.signature()).collect::<Vec<_>>().join(", ")
    )))
}

fn print_raw(ret: &[u8], json: bool) -> Result<i32> {
    if json {
        return print_json(&json!({"result": hex0x(ret)}));
    }
    out(hex0x(ret));
    Ok(0)
}

fn cmd_call(es: &OnChain, a: &CallArgs, style: &Style) -> Result<i32> {
    let (chain, address) = target(es, &a.target, &a.chain)?;
    let block = a.chain.block.as_deref();
    if let Some(raw) = a.function.strip_prefix("0x") {
        let data = hex::decode(raw).map_err(|_| Error::value(format!("invalid calldata {}", abi::py_repr(&a.function))))?;
        return print_raw(&es.call(&chain, &address, &data, block)?, a.common.json);
    }
    let contract = es.contract(&chain, &address)?;
    let (fns, _) = es.functions_for(&contract)?;
    let f = resolve_function(&fns, &a.function, a.args.len())?;
    if a.args.len() != f.inputs.len() {
        return Err(Error::value(format!("{} takes {} arguments, got {}", f.signature(), f.inputs.len(), a.args.len())));
    }
    let values = f.inputs.iter().zip(&a.args).map(|(t, v)| abi::parse_value(t, v)).collect::<Result<Vec<_>>>()?;
    if !matches!(f.mutability.as_str(), "view" | "pure" | "") {
        eprintln!("{}", style.dim(&format!("({} is {}; simulated with eth_call, nothing is sent)", f.signature(), f.mutability)));
    }
    let ret = es.call(&chain, &address, &f.encode_call(&values), block)?;
    let Some(outputs) = &f.outputs else { return print_raw(&ret, a.common.json) };
    let decoded = abi::decode(outputs, &ret)?;
    if a.common.json {
        if outputs.len() == 1 {
            return print_json(&abi::to_json(&outputs[0], &decoded[0]));
        }
        return print_json(&Value::Array(outputs.iter().zip(&decoded).map(|(t, v)| abi::to_json(t, v)).collect()));
    }
    for (i, (t, v)) in outputs.iter().zip(&decoded).enumerate() {
        let label = if outputs.len() > 1 {
            let name = f.output_names.get(i).filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| t.canonical());
            format!("{}  ", style.dim(&name))
        } else {
            String::new()
        };
        out(format!("{label}{}", abi::format_value(t, v)));
    }
    Ok(0)
}

fn cmd_storage(es: &OnChain, a: &StorageArgs, style: &Style) -> Result<i32> {
    let (chain, address) = target(es, &a.target, &a.chain)?;
    let start = match slot(&a.slot.to_lowercase()) {
        Some(s) => s,
        None => abi::parse_int(&a.slot)?.unsigned()?,
    };
    let mut rows = Vec::new();
    for i in 0..a.count.max(1) as u64 {
        let index = start.wrapping_add(U256::from(i));
        rows.push((index, es.storage(&chain, &address, index, a.chain.block.as_deref())?));
    }
    if a.common.json {
        let items: Vec<Value> = rows.iter().map(|(s, w)| json!({"slot": format!("{s:#x}"), "value": hex0x(w)})).collect();
        return print_json(&items);
    }
    for (index, word) in rows {
        let value = U256::from_be_bytes(word);
        let mut hints = Vec::new();
        if !value.is_zero() && value < (U256::from(1u8) << 160usize) && value >= (U256::from(1u8) << 128usize) {
            hints.push(format!("address {}", abi::word_address(&word)));
        } else if !value.is_zero() && value < (U256::from(1u8) << 128usize) {
            hints.push(format!("uint {value}"));
        }
        let line = format!("{}  {}  {}", style.dim(&format!("{index:#x}")), hex0x(&word), style.dim(&hints.join(" ")));
        out(line.trim_end());
    }
    Ok(0)
}

fn cmd_scope(es: &OnChain, a: &ScopeArgs, source: Option<&dyn ScopeSource>, style: &Style) -> Result<i32> {
    let source = source.ok_or_else(|| Error::value("Scope lookups need a platform target"))?;
    let kind = if a.competition { Kind::Competition } else { Kind::Program };
    let bounty = source.scope(kind, &a.program)?;
    let block = a.chain.block.as_deref();

    let mut targets: Vec<(Value, String, Chain)> = Vec::new();
    let mut unsupported: Vec<(Value, String)> = Vec::new();
    let mut non_evm = 0usize;
    for asset in &bounty.assets {
        let url = jv::s(asset, "url").to_string();
        let text = jv::opt(asset, "address").unwrap_or(&url);
        let address = match first_address(text) {
            Some(a) if jv::s(asset, "type") == "smart_contract" => a,
            _ => continue,
        };
        if let Some(network) = jv::opt(asset, "chain") {
            // the platform names the network instead of linking an explorer
            match es.resolve_chain(Some(network)) {
                Ok(chain) => {
                    let mut asset = asset.clone();
                    if url.is_empty() {
                        let link = if chain.explorer.is_empty() {
                            String::new()
                        } else {
                            format!("{}/address/{address}", chain.explorer.trim_end_matches('/'))
                        };
                        asset["url"] = json!(link);
                    }
                    targets.push((asset, address, chain));
                }
                Err(e @ (Error::Value(_) | Error::Chain(_) | Error::RateLimited(_))) => {
                    let _ = e;
                    unsupported.push((asset.clone(), address));
                }
                Err(e) => return Err(e),
            }
        } else if NON_EVM_HOSTS.contains(&host_of(&url).as_str()) {
            non_evm += 1;
        } else if let Some(chain) = es.chain_for_url(&url)? {
            targets.push((asset.clone(), address, chain));
        } else {
            unsupported.push((asset.clone(), address));
        }
    }
    if let Some(limit) = a.limit.filter(|l| *l > 0) {
        targets.truncate(limit);
    }
    let title = bounty.project.clone().unwrap_or_else(|| jv::text(&bounty.slug));
    if targets.is_empty() && unsupported.is_empty() {
        if a.common.json {
            return print_json(&json!({"program": bounty.slug, "contracts": [], "unsupported": [], "nonEvm": non_evm}));
        }
        out(format!("{title} lists no EVM contract addresses on block explorers."));
        return Ok(0);
    }

    let mut records: Vec<Map<String, Value>> = Vec::new();
    let total = targets.len();
    for (i, (asset, address, chain)) in targets.iter().enumerate() {
        progress(&format!("[{}/{total}] {} {address}", i + 1, chain.name));
        let mut record = Map::new();
        record.insert("chainId".into(), json!(chain.id));
        record.insert("chain".into(), json!(chain.name));
        record.insert("address".into(), json!(address));
        record.insert("asset".into(), json!(jv::s(asset, "description")));
        record.insert("url".into(), jv::at(asset, "url").clone());
        let step = (|| -> Result<()> {
            let contract = es.contract(chain, address)?;
            record.insert("name".into(), json!(contract.name()));
            record.insert("verified".into(), json!(contract.verified()));
            record.insert("source".into(), json!(contract.provider()));
            record.insert("implementation".into(), json!(contract.implementation()));
            let implementation = match contract.implementation() {
                Some(i) => Some(es.contract(chain, &i)?),
                None => None,
            };
            let impl_name = implementation.as_ref().filter(|c| c.verified()).map(|c| c.name().to_string());
            record.insert("implementationName".into(), json!(impl_name));
            if let Some(dir) = &a.download {
                let base = dir.join(chain.id.to_string());
                if contract.verified() {
                    write_sources(&contract, &base.join(format!("{}-{address}", contract.name())))?;
                }
                if let Some(i) = implementation.filter(|c| c.verified()) {
                    write_sources(&i, &base.join(format!("{}-{}", i.name(), i.address)))?;
                }
            }
            Ok(())
        })();
        match step {
            Ok(()) => {}
            Err(e) if e.is_chain_failure() => {
                record.insert("error".into(), json!(e.to_string()));
            }
            Err(e) => {
                progress_done();
                return Err(e);
            }
        }
        records.push(record);
    }
    progress_done();

    if !a.no_live {
        let mut order: Vec<u64> = Vec::new();
        for r in &records {
            let id = r["chainId"].as_u64().unwrap_or(0);
            if !order.contains(&id) {
                order.push(id);
            }
        }
        for chain_id in order {
            let group: Vec<usize> = (0..records.len()).filter(|&i| records[i]["chainId"].as_u64() == Some(chain_id)).collect();
            progress(&format!("checking deployed code on {}", jv::s(&Value::Object(records[group[0]].clone()), "chain")));
            let addresses: Vec<String> = group.iter().map(|&i| jv::text(&records[i]["address"])).collect();
            let sizes = es.resolve_chain(Some(&chain_id.to_string())).and_then(|c| es.code_sizes(&c, &addresses, block));
            match sizes {
                Err(e) if e.is_chain_failure() => {
                    for &i in &group {
                        records[i].insert("codeError".into(), json!(e.to_string()));
                    }
                }
                Err(e) => {
                    progress_done();
                    return Err(e);
                }
                Ok(sizes) => {
                    for &i in &group {
                        let address = jv::text(&records[i]["address"]);
                        match sizes.iter().find(|(a, _)| *a == address).map(|(_, s)| s) {
                            Some(Ok(size)) => {
                                records[i].insert("codeSize".into(), json!(size));
                            }
                            Some(Err(e)) => {
                                records[i].insert("codeError".into(), json!(e.to_string()));
                            }
                            None => {
                                records[i].insert("codeError".into(), json!("None"));
                            }
                        }
                    }
                }
            }
        }
        progress_done();
    }

    if a.common.json {
        let unsupported: Vec<Value> = unsupported.iter().map(|(x, address)| json!({"address": address, "url": jv::at(x, "url")})).collect();
        return print_json(&json!({
            "program": bounty.slug,
            "contracts": records.into_iter().map(Value::Object).collect::<Vec<_>>(),
            "unsupported": unsupported,
            "nonEvm": non_evm,
        }));
    }
    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|r| {
            let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let status = if let Some(err) = r.get("error").and_then(Value::as_str) {
                err.chars().take(60).collect()
            } else if r.get("verified") != Some(&json!(true)) {
                "unverified".into()
            } else if let Some(i) = r.get("implementation").and_then(Value::as_str) {
                let name = r.get("implementationName").and_then(Value::as_str).unwrap_or("unverified");
                format!("proxy → {name} {}", short(Some(i)))
            } else {
                "verified".into()
            };
            let code = match r.get("codeSize").and_then(Value::as_u64) {
                None => "-".into(),
                Some(0) => "NO CODE".into(),
                Some(_) => "yes".into(),
            };
            let name = if s("name").is_empty() { "-".into() } else { s("name") };
            let src = if s("source").is_empty() { "-".into() } else { s("source") };
            vec![s("chain"), s("address"), name, status, src, code, s("asset")]
        })
        .collect();
    out(format!("{}: {} EVM contracts", style.bold(&title), records.len()));
    if !rows.is_empty() {
        out(table(&["CHAIN", "ADDRESS", "CONTRACT", "STATUS", "SOURCE", "CODE", "ASSET"], &rows, style, 6, &[]));
    }
    let mut notes = Vec::new();
    if !unsupported.is_empty() {
        let mut hosts: Vec<String> = unsupported.iter().map(|(x, _)| host_of(jv::s(x, "url"))).collect();
        hosts.sort();
        hosts.dedup();
        notes.push(format!("{} on unknown explorers: {}", unsupported.len(), hosts.join(", ")));
    }
    if non_evm > 0 {
        notes.push(format!("{non_evm} non-EVM assets skipped"));
    }
    if records.iter().any(|r| r.contains_key("codeError")) {
        notes.push("CODE '-' = no node for that chain (see `bb contract chains`)".into());
    }
    if !notes.is_empty() {
        out(style.dim(&format!("\n{}", notes.join("\n"))));
    }
    if let Some(dir) = &a.download {
        out(format!("\nSources written under {}/<chain-id>/<Name>-<address>/", dir.display()));
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: ContractCmd,
    }

    #[test]
    fn native_amounts() {
        let ether = U256::from(10u64).pow(U256::from(18));
        assert_eq!(native(ether * U256::from(1234) + ether / U256::from(2)), "1,234.5");
        assert_eq!(native(U256::ZERO), "0");
        assert_eq!(native(U256::from(1u64)), "0");
        assert_eq!(native(U256::from(1_500_000_000_000u64)), "0.000002");
    }

    #[test]
    fn parses_commands_and_negative_arguments() {
        let cli = Cli::try_parse_from(["c", "call", "0xabc", "f(int8)", "-5", "--chain", "base", "--json"]).unwrap();
        let ContractCmd::Call(a) = &cli.cmd else { panic!() };
        assert_eq!(a.args, ["-5"]);
        assert_eq!(a.chain.chain.as_deref(), Some("base"));
        assert!(cli.cmd.common().json);
        assert_eq!(a.chain.rpc, "auto");
        let mut cli = Cli::try_parse_from(["c", "scope", "sherlock:56", "--competition", "--no-live"]).unwrap();
        let (target, kind) = cli.cmd.scope_target_mut().unwrap();
        assert_eq!((target.as_str(), kind), ("sherlock:56", Kind::Competition));
        *target = "56".into();
        let ContractCmd::Scope(a) = &cli.cmd else { panic!() };
        assert_eq!(a.program, "56");
        assert!(Cli::try_parse_from(["c", "info", "0xabc"]).unwrap().cmd.scope_target_mut().is_none());
    }

    #[test]
    fn function_resolution() {
        let abi = json!([
            {"type": "function", "name": "f", "inputs": [{"type": "uint256"}], "outputs": [], "stateMutability": "view"},
            {"type": "function", "name": "f", "inputs": [], "outputs": [{"type": "bool"}], "stateMutability": "view"},
        ]);
        let fns = abi::functions_from_abi(abi.as_array().unwrap()).unwrap();
        assert_eq!(resolve_function(&fns, "f", 0).unwrap().signature(), "f()");
        assert_eq!(resolve_function(&fns, "f", 1).unwrap().signature(), "f(uint256)");
        assert!(resolve_function(&fns, "f", 2).unwrap_err().to_string().contains("ambiguous"));
        assert!(resolve_function(&fns, "g", 0).unwrap_err().to_string().contains("no function named 'g'"));
        let known = resolve_function(&fns, "f()", 0).unwrap();
        assert_eq!(known.outputs.unwrap().len(), 1);
        assert_eq!(resolve_function(&fns, "f()(uint8)", 0).unwrap().outputs.unwrap()[0].canonical(), "uint8");
    }

    #[test]
    fn writes_sources_and_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let chain = Chain { explorer: "https://etherscan.io/".into(), ..Chain::bare(1, "Ethereum Mainnet") };
        let doc = json!({"sources": {"src/A.sol": {"content": "a"}}});
        let c = Contract::new(
            chain,
            "0x1111111111111111111111111111111111111111",
            json!({
                "SourceCode": format!("{{{doc}}}"), "ABI": "[]", "ContractName": "A", "_provider": "etherscan",
            }),
        );
        let written = write_sources(&c, dir.path()).unwrap();
        assert_eq!(written, [dir.path().join("src/A.sol")]);
        let meta: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("contract.json")).unwrap()).unwrap();
        assert_eq!(meta["contractName"], "A");
        assert_eq!(meta["explorer"], "https://etherscan.io/address/0x1111111111111111111111111111111111111111");
    }
}

/// Live smoke test against real explorers and public nodes: `cargo test --lib live_ -- --ignored --nocapture`.
#[cfg(test)]
mod live {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: ContractCmd,
    }

    struct Fixed;

    impl ScopeSource for Fixed {
        fn scope(&self, _: Kind, _: &str) -> Result<ScopeRecord> {
            Ok(ScopeRecord {
                slug: json!(56),
                project: Some("Usual".into()),
                assets: vec![json!({"url": "", "type": "smart_contract", "description": "USD0",
                                    "address": "0x73A15FeD60Bf67631dC6cd7Bc5B6e8da8190aCF5", "chain": "ethereum"})],
            })
        }
    }

    fn go(args: &[&str]) -> i32 {
        let dir = std::env::temp_dir().join("bb-evm-live");
        let mut argv = vec!["c"];
        argv.extend_from_slice(args);
        let cli = Cli::try_parse_from(argv).unwrap();
        let mut cmd = cli.cmd;
        *cmd.common_mut() = cmd.common().clone().validated(&[]).unwrap();
        run(&cmd, &Ctx { evm_cache: dir, scope: Some(&Fixed) }).unwrap()
    }

    #[test]
    #[ignore]
    fn live_commands() {
        let usdc = "https://etherscan.io/address/0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
        assert_eq!(go(&["info", usdc, "--rpc", "public", "--no-color"]), 0);
        assert_eq!(go(&["read", usdc, "--no-color"]), 0);
        assert_eq!(go(&["call", usdc, "decimals()(uint8)"]), 0);
        assert_eq!(go(&["storage", usdc, "implementation"]), 0);
        assert_eq!(go(&["scope", "x", "--json"]), 0);
        assert_eq!(go(&["abi", usdc]), 0);
    }
}
