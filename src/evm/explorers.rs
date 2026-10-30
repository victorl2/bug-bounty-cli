//! Block explorers: which chain an explorer link belongs to, and where verified source lives.
//!
//! Verified source/ABI is looked up per chain in this order, stopping at the first verified hit:
//!
//! 1. Etherscan API v2 (one key, ~60 chains)
//! 2. the chain's own Etherscan-compatible explorer API (Blockscout instances, Routescan, zkSync)
//! 3. Sourcify (open verification repository, any EVM chain, no key)
//!
//! Every provider returns Etherscan's `getsourcecode` shape, so the rest of the code only deals
//! with one format.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Instant;

use regex::Regex;
use serde_json::{Map, Value, json};

use super::abi::{self, Function, py_repr};
use super::client::Net;
use crate::error::{Error, Result};
use crate::jv;

pub const ETHERSCAN_API: &str = "https://api.etherscan.io/v2/api";
pub const ETHERSCAN_CHAINLIST: &str = "https://api.etherscan.io/v2/chainlist";
pub const SOURCIFY_API: &str = "https://sourcify.dev/server/v2/contract";
pub const RATE_LIMIT_RETRIES: usize = 5;
/// Keyless Blockscout allows ~10 calls per hour: waiting rarely helps.
pub const COMPAT_RATE_LIMIT_RETRIES: usize = 2;
pub const UNVERIFIED: &str = "Contract source code not verified";

/// Explorers platforms link to that Etherscan v2 does not list under that host. The flavour says
/// how to reach verified source: "blockscout" (Etherscan-compatible /api plus /api/eth-rpc),
/// "compat" (Etherscan-compatible /api only), "routescan"/"routescan-testnet", "zksync", or None
/// when only Sourcify has it. Keys may carry a path prefix for explorers hosting several chains.
pub const EXPLORERS: &[(&str, u64, &str, Option<&str>)] = &[
    ("snowtrace.io", 43114, "Avalanche C-Chain", Some("routescan")),
    ("testnet.snowtrace.io", 43113, "Avalanche Fuji Testnet", Some("routescan-testnet")),
    ("subnets.avax.network", 43114, "Avalanche C-Chain", None),
    ("explorer.optimism.io", 10, "OP Mainnet", Some("blockscout")),
    ("unichain.blockscout.com", 130, "Unichain", Some("blockscout")),
    ("explorer.mantle.xyz", 5000, "Mantle", Some("blockscout")),
    ("seitrace.com", 1329, "Sei", None),
    ("robinhoodchain.blockscout.com", 4663, "Robinhood Chain", Some("blockscout")),
    ("beratrail.io", 80094, "Berachain", None),
    ("purrsec.com", 999, "HyperEVM", None),
    ("rootstock.blockscout.com", 30, "Rootstock", Some("blockscout")),
    ("scrollscan.com", 534352, "Scroll", Some("blockscout")),
    ("pacific-explorer.manta.network", 169, "Manta Pacific", Some("blockscout")),
    ("blockscout.com/xdai/mainnet", 100, "Gnosis", Some("blockscout")),
    ("explorer.zksync.io", 324, "zkSync Era", Some("zksync")),
    ("explorer.lyra.finance", 957, "Derive", Some("blockscout")),
    ("oklink.com/x-layer", 196, "X Layer", None),
    ("oklink.com/xlayer", 196, "X Layer", None),
    ("oklink.com/fantom", 250, "Fantom", None),
    ("explorer.inkonchain.com", 57073, "Ink", Some("blockscout")),
    ("moonscan.io", 1284, "Moonbeam", None),
    ("moonbeam.moonscan.io", 1284, "Moonbeam", None),
    ("ftmscan.com", 250, "Fantom", None),
    ("explore.tempo.xyz", 4217, "Tempo", None),
    ("explore.mainnet.tempo.xyz", 4217, "Tempo", None),
    ("hecoinfo.com", 128, "HECO", None),
    ("explorer.intuition.systems", 1155, "Intuition", Some("blockscout")),
    ("explorer.orderly.network", 291, "Orderly", Some("blockscout")),
    ("explorer.gobob.xyz", 60808, "BOB", Some("blockscout")),
    ("explorer.plume.org", 98866, "Plume", Some("blockscout")),
    ("explorer.tac.build", 239, "TAC", Some("blockscout")),
    ("explorer.hemi.xyz", 43111, "Hemi", Some("blockscout")),
    ("explorer.immutable.com", 13371, "Immutable zkEVM", Some("blockscout")),
    ("explorer.morphl2.io", 2818, "Morph", None),
    ("explorer.morph.network", 2818, "Morph", None),
    ("zkevm.polygonscan.com", 1101, "Polygon zkEVM", None),
    ("megaeth.blockscout.com", 4326, "MegaETH", Some("blockscout")),
    ("flarescan.com", 14, "Flare", None),
    ("cornscan.io", 21000000, "Corn", None),
    ("evm.confluxscan.net", 1030, "Conflux eSpace", None),
    ("bobascan.com", 288, "Boba", None),
    ("blockexplorer.boba.network", 288, "Boba", None),
    ("explorer.zora.energy", 7777777, "Zora", Some("blockscout")),
    ("explorer.zero.network", 543210, "ZERO", Some("blockscout")),
    ("explorer.mainnet.aurora.dev", 1313161554, "Aurora", Some("blockscout")),
    ("aurorascan.dev", 1313161554, "Aurora", Some("compat")),
    ("explorer.cronos.com", 25, "Cronos", None),
    ("explorer.cronos.org", 25, "Cronos", None),
    ("cronos.org/explorer", 25, "Cronos", None),
    ("cronos.crypto.org/explorer", 25, "Cronos", None),
    ("explorer.zkevm.cronos.org", 388, "Cronos zkEVM", None),
    ("explorer.mainnet.citrea.xyz", 4114, "Citrea", Some("blockscout")),
];

/// Keyless public JSON-RPC endpoints for chains QuickNode does not serve, each checked with
/// eth_getCode. Used for latest-state reads only.
pub const PUBLIC_RPCS: &[(u64, &[&str])] = &[
    (25, &["https://evm.cronos.org"]),
    (30, &["https://public-node.rsk.co"]),
    (169, &["https://pacific-rpc.manta.network/http"]),
    (239, &["https://rpc.tac.build"]),
    (250, &["https://rpcapi.fantom.network"]),
    (288, &["https://mainnet.boba.network"]),
    (291, &["https://rpc.orderly.network"]),
    (388, &["https://mainnet.zkevm.cronos.org"]),
    (957, &["https://rpc.derive.xyz"]),
    (1030, &["https://evm.confluxrpc.com"]),
    (1101, &["https://zkevm-rpc.com"]),
    (1155, &["https://rpc.intuition.systems"]),
    (1284, &["https://moonbeam.drpc.org"]),
    (4114, &["https://rpc.mainnet.citrea.xyz"]),
    (13371, &["https://rpc.immutable.com"]),
    (60808, &["https://rpc.gobob.xyz"]),
    (98866, &["https://rpc.plume.org"]),
    (7777777, &["https://rpc.zora.energy"]),
    (1313161554, &["https://mainnet.aurora.dev"]),
];

pub const NON_EVM_HOSTS: &[&str] = &[
    "explorer.hiro.so",
    "starkscan.co",
    "voyager.online",
    "suiscan.xyz",
    "explorer.iota.org",
    "aptoscan.com",
    "explorer.aptoslabs.com",
    "hashscan.io",
];

pub const CHAIN_ALIASES: &[(&str, u64)] = &[
    ("eth", 1),
    ("ethereum", 1),
    ("mainnet", 1),
    ("arb", 42161),
    ("arbitrum", 42161),
    ("base", 8453),
    ("op", 10),
    ("optimism", 10),
    ("polygon", 137),
    ("matic", 137),
    ("bsc", 56),
    ("bnb", 56),
    ("gnosis", 100),
    ("xdai", 100),
    ("avax", 43114),
    ("avalanche", 43114),
    ("sonic", 146),
    ("linea", 59144),
    ("celo", 42220),
    ("scroll", 534352),
    ("zksync", 324),
    ("mantle", 5000),
    ("blast", 81457),
    ("hyperevm", 999),
    ("berachain", 80094),
    ("sei", 1329),
    ("sepolia", 11155111),
    ("hoodi", 560048),
];

static ADDRESS_IN_TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"0x[0-9a-fA-F]{40}").unwrap());
static LOCALE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z]{2}(-[a-z]{2,4})?/").unwrap());
static TESTNET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"testnet|sepolia|hoodi|fuji|amoy|bokuto|devnet").unwrap());

pub(crate) fn is_rate_limited(err: Option<&Error>, message: &str) -> bool {
    err.is_some_and(|e| e.status() == Some(429)) || message.to_lowercase().contains("too many requests")
}

/// `(netloc, path)` the way Python's `urlparse` splits them; netloc is empty without a scheme.
pub(crate) fn split_url(url: &str) -> (&str, &str) {
    let Some((_, rest)) = url.split_once("://") else { return ("", url) };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let netloc = &rest[..end];
    let tail = &rest[end..];
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    (netloc, &tail[..path_end])
}

pub fn host_of(url: &str) -> String {
    let (netloc, _) = split_url(url);
    let host = if netloc.is_empty() { url } else { netloc }.to_lowercase();
    host.strip_prefix("www.").map(str::to_string).unwrap_or(host)
}

pub fn first_address(text: &str) -> Option<String> {
    ADDRESS_IN_TEXT.find(text).map(|m| abi::checksum_address(m.as_str()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    pub id: u64,
    pub name: String,
    /// Base URL used for links.
    pub explorer: String,
    /// Etherscan v2 serves verified source for it...
    pub etherscan: bool,
    /// ...and live reads on a free key.
    pub etherscan_live: bool,
    /// Etherscan-compatible explorer APIs (no key).
    pub apis: Vec<String>,
    /// Explorer JSON-RPC endpoints (latest state only).
    pub explorer_rpcs: Vec<String>,
    /// The chain's public JSON-RPC (latest state only).
    pub public_rpcs: Vec<String>,
}

impl Chain {
    pub fn bare(id: u64, name: impl Into<String>) -> Self {
        Chain {
            id,
            name: name.into(),
            explorer: String::new(),
            etherscan: false,
            etherscan_live: false,
            apis: Vec::new(),
            explorer_rpcs: Vec::new(),
            public_rpcs: Vec::new(),
        }
    }

    pub fn address_url(&self, address: &str) -> String {
        if self.explorer.is_empty() {
            // chain known only by id: Etherscan's cross-chain search
            return format!("https://blockscan.com/address/{address}");
        }
        format!("{}/address/{address}", self.explorer.trim_end_matches('/'))
    }

    pub fn source_providers(&self) -> Vec<String> {
        let mut out: Vec<String> = if self.etherscan { vec!["etherscan".into()] } else { Vec::new() };
        out.extend(self.apis.iter().map(|a| api_label(a)));
        out.push("sourcify".into());
        out
    }
}

pub(crate) fn api_label(url: &str) -> String {
    let host = host_of(url);
    if host.contains("routescan") {
        "routescan".into()
    } else if host.contains("zksync") {
        "zksync".into()
    } else {
        host
    }
}

fn flavour_endpoints(key: &str, chain_id: u64, flavour: Option<&str>) -> (Vec<String>, Vec<String>) {
    let base = format!("https://{key}");
    match flavour {
        Some("blockscout") => (vec![format!("{base}/api")], vec![format!("{base}/api/eth-rpc")]),
        Some("compat") => (vec![format!("{base}/api")], vec![]),
        Some(f @ ("routescan" | "routescan-testnet")) => {
            let net = if f.ends_with("testnet") { "testnet" } else { "mainnet" };
            (vec![format!("https://api.routescan.io/v2/network/{net}/evm/{chain_id}/etherscan/api")], vec![])
        }
        Some("zksync") => (vec!["https://block-explorer-api.mainnet.zksync.io/api".into()], vec![]),
        _ => (vec![], vec![]),
    }
}

/// Chains by id, and explorer links (host or host/path prefix) to chains.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    /// In insertion order: Etherscan's list, then the extra explorers.
    pub chains: Vec<Chain>,
    index: HashMap<u64, usize>,
    links: HashMap<String, u64>,
}

impl Registry {
    pub fn new(etherscan_rows: &[Value]) -> Self {
        let mut reg = Registry::default();
        for r in etherscan_rows {
            let Some(id) = jv::num(jv::at(r, "chainid")).map(|n| n as u64) else { continue };
            let explorer = jv::s(r, "blockexplorer").to_string();
            let chain = Chain {
                explorer: explorer.clone(),
                etherscan: true,
                etherscan_live: jv::s(r, "comment").contains("Free"),
                ..Chain::bare(id, jv::s(r, "chainname"))
            };
            reg.insert(chain);
            reg.links.insert(host_of(&explorer), id);
        }
        for (key, chain_id, name, flavour) in EXPLORERS {
            let (apis, rpcs) = flavour_endpoints(key, *chain_id, *flavour);
            match reg.index.get(chain_id) {
                Some(&i) => {
                    reg.chains[i].apis.extend(apis);
                    reg.chains[i].explorer_rpcs.extend(rpcs);
                }
                None => {
                    reg.insert(Chain { explorer: format!("https://{key}"), apis, explorer_rpcs: rpcs, ..Chain::bare(*chain_id, *name) })
                }
            }
            reg.links.insert(key.to_string(), *chain_id);
        }
        for (chain_id, urls) in PUBLIC_RPCS {
            if let Some(&i) = reg.index.get(chain_id) {
                reg.chains[i].public_rpcs = urls.iter().map(|u| u.to_string()).collect();
            }
        }
        reg
    }

    fn insert(&mut self, chain: Chain) {
        match self.index.get(&chain.id) {
            Some(&i) => self.chains[i] = chain,
            None => {
                self.index.insert(chain.id, self.chains.len());
                self.chains.push(chain);
            }
        }
    }

    pub fn by_id(&self, chain_id: u64) -> Option<&Chain> {
        self.index.get(&chain_id).map(|&i| &self.chains[i])
    }

    pub fn get(&self, chain_id: u64) -> Chain {
        self.by_id(chain_id).cloned().unwrap_or_else(|| Chain::bare(chain_id, format!("chain {chain_id}")))
    }

    pub fn for_url(&self, url: &str) -> Option<Chain> {
        let full = if url.contains("://") { url.to_string() } else { format!("https://{url}") };
        let host = host_of(&full);
        let (_, path) = split_url(&full);
        let mut paths = vec![path.trim_matches('/').to_string()];
        if LOCALE.is_match(&paths[0]) {
            // oklink.com/fr/fantom/...
            let rest = paths[0].split_once('/').map(|(_, r)| r.to_string()).unwrap_or_default();
            paths.push(rest);
        }
        let mut keys = Vec::new();
        for p in &paths {
            let parts: Vec<&str> = p.split('/').collect();
            keys.push(format!("{host}/{}", parts[..parts.len().min(2)].join("/")));
            keys.push(format!("{host}/{}", parts[0]));
        }
        keys.push(host);
        keys.iter().find_map(|k| self.links.get(k)).map(|id| self.get(*id))
    }

    pub fn resolve(&self, value: Option<&str>) -> Result<Chain> {
        let Some(value) = value else { return Ok(self.get(1)) };
        let text = value.trim().to_lowercase();
        if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
            return text.parse().map(|id| self.get(id)).map_err(|_| Error::value(format!("unknown chain {}", py_repr(value))));
        }
        if let Some((_, id)) = CHAIN_ALIASES.iter().find(|(alias, _)| *alias == text) {
            return Ok(self.get(*id));
        }
        let matches: Vec<&Chain> = self.chains.iter().filter(|c| c.name.to_lowercase().contains(&text)).collect();
        let mainnets: Vec<&Chain> = matches.iter().copied().filter(|c| !TESTNET.is_match(&c.name.to_lowercase())).collect();
        let matches = if mainnets.is_empty() { matches } else { mainnets };
        match matches.len() {
            1 => Ok(matches[0].clone()),
            0 => Err(Error::value(format!("unknown chain {}; use a chain id or see `bb contract chains`", py_repr(value)))),
            _ => Err(Error::value(format!(
                "chain {} is ambiguous: {}",
                py_repr(value),
                matches.iter().map(|c| format!("{} ({})", c.name, c.id)).collect::<Vec<_>>().join(", ")
            ))),
        }
    }
}

// --- verified source providers ------------------------------------------------------------------

pub(crate) struct Throttle {
    interval: f64,
    last: Cell<Option<Instant>>,
}

impl Throttle {
    pub(crate) fn new(interval: f64) -> Self {
        Throttle { interval, last: Cell::new(None) }
    }

    pub(crate) fn wait(&self, net: &Net) {
        if let Some(last) = self.last.get() {
            let delay = self.interval - last.elapsed().as_secs_f64();
            net.sleep(delay);
        }
        self.last.set(Some(Instant::now()));
    }
}

fn query(params: &[(&str, String)]) -> String {
    url::form_urlencoded::Serializer::new(String::new()).extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str()))).finish()
}

/// Etherscan v2 transport: key handling, rate limiting and redaction.
pub struct EtherscanApi {
    pub api_key: Option<String>,
    net: Net,
    throttle: Throttle,
}

impl EtherscanApi {
    /// `api_key` wins; otherwise `$ETHERSCAN_API_KEY`.
    pub fn new(api_key: Option<String>, net: Net) -> Self {
        let api_key = api_key.or_else(|| std::env::var("ETHERSCAN_API_KEY").ok()).filter(|k| !k.is_empty());
        let throttle = Throttle::new(net.etherscan_interval);
        EtherscanApi { api_key, net, throttle }
    }

    fn redact(&self, text: &str) -> String {
        match &self.api_key {
            Some(key) => text.replace(key.as_str(), "***"),
            None => text.to_string(),
        }
    }

    pub fn request(&self, chain: &Chain, params: &[(&str, String)]) -> Result<Value> {
        let Some(key) = &self.api_key else {
            return Err(Error::chain(
                "ETHERSCAN_API_KEY is not set: create a free key at https://etherscan.io/myapikey and export it (or pass --etherscan-key)",
            ));
        };
        let mut all = vec![("chainid", chain.id.to_string())];
        all.extend(params.iter().cloned());
        all.push(("apikey", key.clone()));
        let url = format!("{ETHERSCAN_API}?{}", query(&all));
        for attempt in 0..RATE_LIMIT_RETRIES {
            self.throttle.wait(&self.net);
            let body =
                self.net.request(&url, &[("Accept", "application/json")], None).map_err(|e| Error::chain(self.redact(e.message())))?;
            let data: Value = serde_json::from_str(&body).map_err(|_| Error::chain("Etherscan returned a non-JSON response"))?;
            let message = if jv::s(&data, "status") == "0" { py_str(jv::at(&data, "result")) } else { String::new() };
            if message.to_lowercase().contains("rate limit") && attempt + 1 < RATE_LIMIT_RETRIES {
                self.net.sleep(1.0 + attempt as f64);
                continue;
            }
            if message.contains("Free API access is not supported") {
                return Err(Error::chain(format!("live reads on {} need a paid Etherscan plan or a node (--rpc)", chain.name)));
            }
            return Ok(data);
        }
        Err(Error::chain("Etherscan rate limit reached; try again shortly"))
    }

    pub fn result(&self, chain: &Chain, params: &[(&str, String)]) -> Result<Value> {
        let data = self.request(chain, params)?;
        if jv::s(&data, "status") == "0" {
            let detail = jv::first(&data, &["result", "message"]);
            return Err(Error::chain(format!("Etherscan: {}", py_str(detail))));
        }
        Ok(jv::at(&data, "result").clone())
    }
}

/// Python's `str()` of a JSON value, as error messages show it.
pub(crate) fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        other => jv::text(other),
    }
}

/// An explorer's own keyless Etherscan-compatible API (Blockscout, Routescan, zkSync, ...).
pub struct CompatApi {
    pub base: String,
    pub label: String,
    net: Net,
    throttle: Throttle,
    /// Rate limited after retrying: skip it for the rest of the run.
    exhausted: Cell<bool>,
}

impl CompatApi {
    pub fn new(base: &str, net: Net) -> Self {
        let throttle = Throttle::new(net.open_interval);
        CompatApi { base: base.to_string(), label: api_label(base), net, throttle, exhausted: Cell::new(false) }
    }

    pub fn result(&self, params: &[(&str, String)]) -> Result<Value> {
        let limited = || Error::RateLimited(format!("{}: rate limited; try again later", self.label));
        if self.exhausted.get() {
            return Err(limited());
        }
        let url = format!("{}?{}", self.base, query(params));
        for attempt in 0..COMPAT_RATE_LIMIT_RETRIES {
            self.throttle.wait(&self.net);
            let data = match self.net.request(&url, &[("Accept", "application/json")], None) {
                Ok(body) => serde_json::from_str::<Value>(&body).map_err(|_| Error::chain(format!("{}: non-JSON response", self.label)))?,
                Err(e) if is_rate_limited(Some(&e), "") => json!({"status": "0", "message": "Too many requests"}),
                Err(e) => return Err(Error::chain(format!("{}: {}", self.label, e))),
            };
            if !data.is_object() {
                return Err(Error::chain(format!("{}: {}", self.label, data)));
            }
            let message = py_str(jv::first(&data, &["result", "message"]));
            let message = if message == "None" { String::new() } else { message };
            if jv::text(jv::at(&data, "status")) != "0" {
                return Ok(jv::at(&data, "result").clone());
            }
            if !is_rate_limited(None, &message) {
                return Err(Error::chain(format!("{}: {message}", self.label)));
            }
            if attempt + 1 < COMPAT_RATE_LIMIT_RETRIES {
                self.net.sleep(1.0 + 2.0 * attempt as f64);
            }
        }
        self.exhausted.set(true);
        Err(limited())
    }
}

fn truthy_flag(value: &Value) -> bool {
    matches!(jv::text(value).to_lowercase().as_str(), "1" | "true")
}

/// Bring Blockscout/zkSync dialects of `getsourcecode` onto Etherscan's field names.
pub fn normalize(raw: &Value) -> Value {
    let mut out: Map<String, Value> = raw.as_object().cloned().unwrap_or_default();
    let has = |k: &str| raw.get(k).is_some();
    if has("IsProxy") || has("ImplementationAddress") {
        out.insert("Proxy".into(), json!(if truthy_flag(jv::at(raw, "IsProxy")) { "1" } else { "0" }));
        let implementation = if jv::t(raw, "ImplementationAddress") { jv::at(raw, "ImplementationAddress").clone() } else { json!("") };
        out.insert("Implementation".into(), implementation);
    }
    if has("OptimizationUsed") {
        out.insert("OptimizationUsed".into(), json!(if truthy_flag(jv::at(raw, "OptimizationUsed")) { "1" } else { "0" }));
    }
    let runs_missing = out.get("Runs").is_none_or(|r| !jv::truthy(r));
    if runs_missing && raw.get("OptimizationRuns").is_some_and(|r| !r.is_null()) {
        out.insert("Runs".into(), json!(jv::text(jv::at(raw, "OptimizationRuns"))));
    }
    let name = out.get("ContractName").and_then(Value::as_str).unwrap_or("").to_string();
    if let Some((_, short)) = name.rsplit_once(':') {
        // zkSync: "src/Foo.sol:Foo"
        out.insert("ContractName".into(), json!(short));
    }
    let extra = jv::arr(raw, "AdditionalSources");
    let code = jv::s(raw, "SourceCode");
    if !extra.is_empty() && !code.is_empty() && !code.starts_with('{') {
        // Blockscout: main file + additional files
        let contract = out.get("ContractName").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("Contract");
        let main = jv::opt(raw, "FileName").map(str::to_string).unwrap_or_else(|| format!("{contract}.sol"));
        let mut sources = Map::new();
        sources.insert(main, json!({"content": code}));
        for s in extra {
            if let Some(file) = jv::opt(s, "Filename") {
                sources.insert(file.to_string(), json!({"content": jv::s(s, "SourceCode")}));
            }
        }
        let settings = if jv::t(raw, "CompilerSettings") { jv::at(raw, "CompilerSettings").clone() } else { json!({}) };
        let std = json!({"language": "Solidity", "sources": sources, "settings": settings});
        out.insert("SourceCode".into(), json!(format!("{{{std}}}")));
        out.remove("AdditionalSources");
    }
    Value::Object(out)
}

/// Map a Sourcify v2 contract document onto Etherscan's getsourcecode shape.
pub fn from_sourcify(doc: &Value) -> Value {
    let comp = jv::at(doc, "compilation");
    let std = jv::at(doc, "stdJsonInput");
    let settings = if jv::t(std, "settings") { jv::at(std, "settings") } else { jv::at(comp, "compilerSettings") };
    let optimizer = jv::at(settings, "optimizer");
    let proxy = jv::at(doc, "proxyResolution");
    let impls: Vec<&str> = jv::arr(proxy, "implementations").iter().filter_map(|i| jv::opt(i, "address")).collect();
    let abi = if jv::t(doc, "abi") { jv::at(doc, "abi").clone() } else { json!([]) };
    json!({
        "SourceCode": if jv::truthy(std) { format!("{{{std}}}") } else { String::new() },
        "ABI": abi.to_string(),
        "ContractName": jv::s(comp, "name"),
        "CompilerVersion": jv::s(comp, "compilerVersion"),
        "OptimizationUsed": if jv::t(optimizer, "enabled") { "1" } else { "0" },
        "Runs": if jv::t(optimizer, "runs") { jv::text(jv::at(optimizer, "runs")) } else { String::new() },
        "EVMVersion": jv::s(settings, "evmVersion"),
        "LicenseType": "",
        "Proxy": if jv::t(proxy, "isProxy") && !impls.is_empty() { "1" } else { "0" },
        "Implementation": impls.first().copied().unwrap_or(""),
        "Match": jv::s(doc, "match"),
    })
}

pub struct Sourcify {
    net: Net,
    throttle: Throttle,
}

impl Sourcify {
    pub fn new(net: Net) -> Self {
        let throttle = Throttle::new(net.open_interval);
        Sourcify { net, throttle }
    }

    pub fn source(&self, chain: &Chain, address: &str) -> Result<Option<Value>> {
        self.throttle.wait(&self.net);
        let fields = "stdJsonInput,abi,compilation,proxyResolution,deployment";
        let url = format!("{SOURCIFY_API}/{}/{address}?fields={fields}", chain.id);
        let body = match self.net.request(&url, &[("Accept", "application/json")], None) {
            Ok(body) => body,
            Err(Error::NotFound(_)) => return Ok(None),
            Err(e) if e.status() == Some(400) => return Ok(None), // chain not supported by Sourcify
            Err(e) => return Err(Error::chain(format!("sourcify: {e}"))),
        };
        let doc: Value = serde_json::from_str(&body).map_err(|_| Error::chain("sourcify: non-JSON response"))?;
        let mut raw = from_sourcify(&doc);
        let deployment = jv::at(&doc, "deployment");
        let creation = if jv::t(deployment, "transactionHash") {
            json!({
                "contractCreator": jv::at(deployment, "deployer"),
                "txHash": jv::at(deployment, "transactionHash"),
                "blockNumber": jv::at(deployment, "blockNumber"),
                "timestamp": null,
            })
        } else {
            Value::Null
        };
        raw.as_object_mut().expect("object").insert("_creation".into(), creation);
        Ok(Some(raw))
    }
}

// --- contract metadata --------------------------------------------------------------------------

/// Verified-source metadata in Etherscan's `getsourcecode` shape.
#[derive(Debug, Clone)]
pub struct Contract {
    pub chain: Chain,
    pub address: String,
    pub raw: Value,
}

impl Contract {
    pub fn new(chain: Chain, address: &str, raw: Value) -> Self {
        Contract { chain, address: address.to_string(), raw }
    }

    pub fn provider(&self) -> &str {
        jv::s(&self.raw, "_provider")
    }

    pub fn verified(&self) -> bool {
        jv::t(&self.raw, "SourceCode") && jv::at(&self.raw, "ABI").as_str() != Some(UNVERIFIED)
    }

    pub fn name(&self) -> &str {
        jv::s(&self.raw, "ContractName")
    }

    pub fn implementation(&self) -> Option<String> {
        let implementation = jv::s(&self.raw, "Implementation");
        (jv::s(&self.raw, "Proxy") == "1" && abi::is_address(implementation)).then(|| abi::checksum_address(implementation))
    }

    pub fn abi(&self) -> Vec<Value> {
        if !self.verified() {
            return Vec::new();
        }
        serde_json::from_str::<Value>(jv::s(&self.raw, "ABI")).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
    }

    pub fn functions(&self) -> Result<Vec<Function>> {
        abi::functions_from_abi(&self.abi())
    }

    /// Verified source as (relative path, content), whatever format the explorer stored.
    pub fn source_files(&self) -> Vec<(String, String)> {
        let code = jv::s(&self.raw, "SourceCode");
        if code.is_empty() {
            return Vec::new();
        }
        let doc: Option<Value> = if code.starts_with("{{") && code.ends_with("}}") {
            serde_json::from_str(&code[1..code.len() - 1]).ok() // standard-json input
        } else if code.starts_with('{') {
            serde_json::from_str(code).ok() // bare {path: {content}} map
        } else {
            None
        };
        if let Some(Value::Object(map)) = &doc {
            let sources = map.get("sources").and_then(Value::as_object).unwrap_or(map);
            let mut out: Vec<(String, String)> = Vec::new();
            for (path, body) in sources {
                if body.is_object() {
                    let content = body.get("content").and_then(Value::as_str).unwrap_or("").to_string();
                    let path = safe_path(path);
                    match out.iter_mut().find(|(p, _)| *p == path) {
                        Some(existing) => existing.1 = content,
                        None => out.push((path, content)),
                    }
                }
            }
            return out;
        }
        let vyper = jv::s(&self.raw, "CompilerVersion").to_lowercase().contains("vyper");
        let name = if self.name().is_empty() { "Contract" } else { self.name() };
        vec![(safe_path(&format!("{name}{}", if vyper { ".vy" } else { ".sol" })), code.to_string())]
    }

    pub fn compiler_settings(&self) -> Value {
        let code = jv::s(&self.raw, "SourceCode");
        if code.starts_with("{{") && code.len() >= 2 {
            if let Ok(doc) = serde_json::from_str::<Value>(&code[1..code.len() - 1]) {
                return doc.get("settings").cloned().unwrap_or_else(|| json!({}));
            }
        }
        json!({})
    }
}

/// Keep a source path inside the output directory.
pub fn safe_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty() && *p != "." && *p != "..").collect();
    if parts.is_empty() { "Contract.sol".into() } else { parts.join("/") }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn chainlist() -> Vec<Value> {
        json!([
            {"chainname": "Ethereum Mainnet", "chainid": "1", "blockexplorer": "https://etherscan.io/", "comment": "Free-tier available"},
            {"chainname": "Arbitrum One Mainnet", "chainid": "42161", "blockexplorer": "https://arbiscan.io/", "comment": "Free-tier available"},
            {"chainname": "Avalanche C-Chain", "chainid": "43114", "blockexplorer": "https://snowscan.xyz/", "comment": "Paid Plan required"},
            {"chainname": "Sepolia Testnet", "chainid": "11155111", "blockexplorer": "https://sepolia.etherscan.io/", "comment": "Free-tier available"},
        ])
        .as_array()
        .unwrap()
        .clone()
    }

    pub(crate) const ADDR: &str = "0x1111111111111111111111111111111111111111";

    fn eth() -> Chain {
        Chain { explorer: "https://etherscan.io/".into(), etherscan: true, etherscan_live: true, ..Chain::bare(1, "Ethereum Mainnet") }
    }

    fn contract(raw: Value) -> Contract {
        Contract::new(eth(), ADDR, raw)
    }

    fn files(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn standard_json_input() {
        let doc = json!({"language": "Solidity", "sources": {"src/A.sol": {"content": "a"}, "../../etc/x.sol": {"content": "x"}},
                         "settings": {"optimizer": {"enabled": true}}});
        let c = contract(json!({"SourceCode": format!("{{{doc}}}"), "ABI": "[]", "ContractName": "A"}));
        assert_eq!(c.source_files(), files(&[("src/A.sol", "a"), ("etc/x.sol", "x")]));
        assert_eq!(c.compiler_settings(), json!({"optimizer": {"enabled": true}}));
    }

    #[test]
    fn bare_sources_map_and_single_file() {
        let c = contract(json!({"SourceCode": json!({"/abs/B.sol": {"content": "b"}}).to_string(), "ABI": "[]"}));
        assert_eq!(c.source_files(), files(&[("abs/B.sol", "b")]));
        let c = contract(json!({"SourceCode": "contract C {}", "ABI": "[]", "ContractName": "C", "CompilerVersion": "v0.8"}));
        assert_eq!(c.source_files(), files(&[("C.sol", "contract C {}")]));
        let vy = contract(json!({"SourceCode": "# vyper", "ABI": "[]", "ContractName": "V", "CompilerVersion": "vyper:0.3.10"}));
        assert_eq!(vy.source_files()[0].0, "V.vy");
    }

    #[test]
    fn unverified() {
        let c = contract(json!({"SourceCode": "", "ABI": UNVERIFIED, "Proxy": "0"}));
        assert!(!c.verified());
        assert!(c.abi().is_empty());
        assert_eq!(c.implementation(), None);
    }

    #[test]
    fn blockscout_dialect_is_normalized() {
        let raw = normalize(&json!({
            "ContractName": "src/Gov.sol:Gov", "SourceCode": "contract Main {}", "FileName": "src/Main.sol",
            "AdditionalSources": [{"Filename": "src/Lib.sol", "SourceCode": "library L {}"}],
            "IsProxy": "true", "ImplementationAddress": format!("0x{}", "33".repeat(20)),
            "OptimizationUsed": "true", "OptimizationRuns": 200, "CompilerSettings": {"evmVersion": "paris"},
            "ABI": "[]",
        }));
        let c = contract(raw.clone());
        assert_eq!(c.name(), "Gov");
        assert_eq!(c.source_files(), files(&[("src/Main.sol", "contract Main {}"), ("src/Lib.sol", "library L {}")]));
        assert_eq!(c.implementation(), Some(abi::checksum_address(&format!("0x{}", "33".repeat(20)))));
        assert_eq!((jv::s(&raw, "OptimizationUsed"), jv::s(&raw, "Runs")), ("1", "200"));
        assert_eq!(c.compiler_settings(), json!({"evmVersion": "paris"}));
    }

    #[test]
    fn sourcify_document_maps_to_etherscan_shape() {
        let raw = from_sourcify(&json!({
            "match": "exact_match",
            "abi": [{"type": "function", "name": "f", "inputs": [], "outputs": [], "stateMutability": "view"}],
            "compilation": {"name": "Token", "compilerVersion": "0.8.20+commit.a1b79de6"},
            "stdJsonInput": {"language": "Solidity", "sources": {"T.sol": {"content": "t"}},
                             "settings": {"optimizer": {"enabled": true, "runs": 200}, "evmVersion": "paris"}},
            "proxyResolution": {"isProxy": true, "implementations": [{"address": format!("0x{}", "22".repeat(20))}]},
        }));
        let c = contract(raw.clone());
        assert!(c.verified());
        assert_eq!((c.name(), c.source_files(), jv::s(&raw, "Runs")), ("Token", files(&[("T.sol", "t")]), "200"));
        assert_eq!(c.implementation(), Some(abi::checksum_address(&format!("0x{}", "22".repeat(20)))));
        assert_eq!(c.functions().unwrap()[0].name, "f");
    }

    #[test]
    fn explorer_links_resolve_to_chains() {
        let reg = Registry::new(&chainlist());
        let cases = [
            ("https://snowtrace.io/address/0xabc", 43114), // Routescan, merged into Etherscan's Avalanche
            ("https://rootstock.blockscout.com/address/0xabc", 30),
            ("https://blockscout.com/xdai/mainnet/address/0xabc", 100),
            ("https://www.oklink.com/x-layer/address/0xabc", 196),
            ("https://www.oklink.com/xlayer/address/0xabc/contract", 196),
            ("https://www.oklink.com/fr/fantom/address/0xabc/contract", 250), // locale prefix
            ("https://explorer.zksync.io/address/0xabc#contract", 324),
            ("arbiscan.io/address/0xabc", 42161),
            ("https://explorer.cronos.org/address/0xabc", 25),
            ("https://cronos.org/explorer/address/0xabc", 25),
            ("https://explorer.zkevm.cronos.org/address/0xabc", 388),
            ("https://explorer.mainnet.citrea.xyz/address/0xabc", 4114),
        ];
        for (url, id) in cases {
            assert_eq!(reg.for_url(url).map(|c| c.id), Some(id), "{url}");
        }
        assert!(reg.for_url("https://unknown.example/address/0xabc").is_none());
    }

    #[test]
    fn providers_per_chain() {
        let reg = Registry::new(&chainlist());
        let avax = reg.get(43114);
        assert!(avax.etherscan && !avax.etherscan_live);
        assert_eq!(avax.source_providers(), ["etherscan", "routescan", "sourcify"]);
        let rsk = reg.get(30);
        assert_eq!(rsk.explorer_rpcs, ["https://rootstock.blockscout.com/api/eth-rpc"]);
        assert_eq!(rsk.public_rpcs, ["https://public-node.rsk.co"]);
        assert_eq!(reg.get(1284).source_providers(), ["sourcify"]);
        assert_eq!(reg.get(424242).name, "chain 424242");
        assert_eq!(reg.get(4114).explorer_rpcs, ["https://explorer.mainnet.citrea.xyz/api/eth-rpc"]);
        assert_eq!(reg.get(25).public_rpcs, ["https://evm.cronos.org"]);
    }

    #[test]
    fn name_resolution() {
        let reg = Registry::new(&chainlist());
        assert_eq!(reg.resolve(Some("arbitrum")).unwrap().id, 42161);
        assert_eq!(reg.resolve(Some("rootstock")).unwrap().id, 30);
        assert_eq!(reg.resolve(Some("Ethereum")).unwrap().id, 1);
        assert_eq!(reg.resolve(None).unwrap().id, 1);
        assert!(reg.resolve(Some("nonexistent")).is_err());
    }

    #[test]
    fn hosts_and_addresses() {
        assert_eq!(host_of("https://WWW.Etherscan.io/address/0x1"), "etherscan.io");
        assert_eq!(host_of("arbiscan.io/address"), "arbiscan.io/address");
        assert_eq!(
            first_address("see https://etherscan.io/address/0xdd9bc35ae942ef0cfa76930954a156b3ff30a4e1#code").as_deref(),
            Some("0xDD9BC35aE942eF0cFa76930954a156B3fF30a4E1")
        );
        assert_eq!(first_address("nothing"), None);
        assert_eq!(safe_path("../"), "Contract.sol");
        assert_eq!(safe_path("a\\b/./c.sol"), "a/b/c.sol");
    }
}
