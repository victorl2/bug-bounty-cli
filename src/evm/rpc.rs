//! Live chain reads over JSON-RPC: your node, your QuickNode endpoint, or an explorer's proxy.
//!
//! Only real nodes (`--rpc URL` or QuickNode) are trusted for historical reads. Etherscan's proxy
//! answers a past block with the latest state, and explorer RPCs are inconsistent about it, so both
//! are limited to `latest`.
//!
//! QuickNode is discovered from the `qn` CLI: the account's multichain endpoint is read with
//! `qn endpoint list` on each run (its token is kept in memory only) and the per-network hosts come
//! from `~/.config/qn/networks.toml`. Which chain id each network serves is probed once and cached.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use alloy_primitives::U256;
use regex::Regex;
use serde_json::{Value, json};

use super::abi::{self, AbiType};
use super::client::{Client, Net};
use super::explorers::{Chain, EtherscanApi, split_url};
use crate::error::{Error, Result};
use crate::jv;

pub const QN_PROBE_WORKERS: usize = 8;
/// Blockscout's limit; larger batches get HTTP 413.
pub const SMALL_BATCH: usize = 5;
pub const POST_ATTEMPTS: usize = 4;
static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-f]{20,}$").unwrap());

pub fn qn_networks_file() -> PathBuf {
    crate::paths::home().join(".config/qn/networks.toml")
}

/// `latest`, or a block number as a hex quantity.
pub fn block_tag(block: Option<&str>) -> Result<String> {
    match block {
        None | Some("latest") => Ok("latest".into()),
        Some(text) => Ok(format!("{:#x}", abi::parse_int(text)?.unsigned()?)),
    }
}

pub fn revert_reason(data: &[u8]) -> String {
    if data.len() >= 4 && data[..4] == [0x08, 0xc3, 0x79, 0xa0] {
        if let Ok(values) = abi::decode(&[AbiType::string()], &data[4..]) {
            if let Some(s) = values.first().and_then(|v| v.as_str()) {
                return s.to_string();
            }
        }
    }
    if data.len() >= 36 && data[..4] == [0x4e, 0x48, 0x7b, 0x71] {
        return format!("panic {:#04x}", U256::from_be_slice(&data[4..36]));
    }
    if data.is_empty() { "no reason".into() } else { format!("0x{}", hex::encode(data)) }
}

fn rpc_error(method: &str, err: &Value, label: &str) -> Error {
    let message = jv::text(jv::at(err, "message"));
    if message.to_lowercase().contains("revert") {
        let raw = jv::at(err, "data");
        let reason = match raw {
            Value::String(s) => match hex::decode(s.strip_prefix("0x").unwrap_or(s)) {
                Ok(bytes) => revert_reason(&bytes),
                Err(_) => s.clone(),
            },
            Value::Null => revert_reason(&[]),
            other => jv::text(other),
        };
        return Error::Reverted(format!("execution reverted: {reason}"));
    }
    Error::chain(format!("{label}: {method} failed: {}", message.chars().take(200).collect::<String>()))
}

/// A JSON-RPC endpoint. `label` is what errors show; the URL may embed a secret.
#[derive(Clone)]
pub struct JsonRpc {
    url: String,
    pub label: String,
    pub archive: bool,
    net: Net,
}

const TOO_LARGE: u16 = 413;

impl JsonRpc {
    pub fn new(url: &str, label: &str, archive: bool, net: Net) -> Self {
        JsonRpc { url: url.to_string(), label: label.to_string(), archive, net }
    }

    /// POST with retries for throttling (429) and dropped connections; other HTTP errors are final.
    /// An oversized batch comes back as a fetch error with status 413.
    fn post(&self, payload: &Value) -> Result<Value> {
        let body = payload.to_string();
        for attempt in 0..POST_ATTEMPTS {
            match self.net.request(&self.url, &[("Content-Type", "application/json")], Some(body.as_bytes())) {
                Ok(text) => {
                    return serde_json::from_str(&text).map_err(|_| Error::chain(format!("{} returned a non-JSON response", self.label)));
                }
                Err(e) => {
                    let status = e.status();
                    if status == Some(TOO_LARGE) {
                        return Err(Error::fetch(format!("{} rejected the batch size", self.label), Some(TOO_LARGE)));
                    }
                    let retry = status == Some(429) || (status.is_none() && attempt == 0);
                    if !retry || attempt + 1 == POST_ATTEMPTS {
                        let message =
                            format!("{} did not answer{}", self.label, status.map(|s| format!(" (HTTP {s})")).unwrap_or_default());
                        return Err(if status == Some(429) { Error::RateLimited(message) } else { Error::chain(message) });
                    }
                }
            }
            self.net.sleep(1.0 + 2.0 * attempt as f64);
        }
        unreachable!("the last attempt always returns")
    }

    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        let data = self
            .post(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .map_err(|e| if e.status() == Some(TOO_LARGE) { Error::chain(e.message()) } else { e })?;
        if !data.is_object() {
            return Err(Error::chain(format!("{}: unexpected response to {method}", self.label)));
        }
        let err = jv::at(&data, "error");
        if jv::truthy(err) {
            let err = if err.is_object() { err.clone() } else { json!({"message": jv::text(err)}) };
            return Err(rpc_error(method, &err, &self.label));
        }
        Ok(jv::at(&data, "result").clone())
    }

    /// One HTTP request for many calls; failed entries come back as errors in place.
    pub fn batch(&self, calls: &[(&str, Value)]) -> Result<Vec<Result<Value>>> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }
        let payload: Vec<Value> =
            calls.iter().enumerate().map(|(i, (m, p))| json!({"jsonrpc": "2.0", "id": i, "method": m, "params": p})).collect();
        let data = match self.post(&Value::Array(payload)) {
            Ok(data) => data,
            Err(e) if e.status() == Some(TOO_LARGE) => {
                if calls.len() == 1 {
                    return Err(Error::chain(e.message()));
                }
                let size = if calls.len() > SMALL_BATCH { SMALL_BATCH } else { 1 };
                let mut out = Vec::new();
                for chunk in calls.chunks(size) {
                    out.extend(self.batch(chunk)?);
                }
                return Ok(out);
            }
            Err(e) => return Err(e),
        };
        let Some(items) = data.as_array() else {
            return calls.iter().map(|(m, p)| self.request(m, p.clone()).map(Ok)).collect();
        };
        let by_id: HashMap<u64, &Value> = items.iter().filter_map(|d| Some((jv::at(d, "id").as_u64()?, d))).collect();
        Ok(calls
            .iter()
            .enumerate()
            .map(|(i, (method, _))| {
                let item = by_id.get(&(i as u64)).copied().unwrap_or(&Value::Null);
                let err = jv::at(item, "error");
                if jv::truthy(err) { Err(rpc_error(method, err, &self.label)) } else { Ok(jv::at(item, "result").clone()) }
            })
            .collect())
    }
}

/// Etherscan's `module=proxy` (plus `account` for balances); latest state only.
pub struct EtherscanProxy {
    api: Rc<EtherscanApi>,
    chain: Chain,
    pub label: String,
}

impl EtherscanProxy {
    pub fn new(api: Rc<EtherscanApi>, chain: Chain) -> Self {
        let label = format!("Etherscan ({})", chain.name);
        EtherscanProxy { api, chain, label }
    }

    pub fn request(&self, method: &str, params: &Value) -> Result<Value> {
        let p = |i: usize| jv::text(&params[i]);
        if method == "eth_getBalance" {
            let result = self.api.result(
                &self.chain,
                &[("module", "account".into()), ("action", "balance".into()), ("address", p(0)), ("tag", "latest".into())],
            )?;
            let wei = abi::parse_int(&jv::text(&result))?.magnitude;
            return Ok(json!(format!("{wei:#x}")));
        }
        let mut query: Vec<(&str, String)> = vec![("module", "proxy".into()), ("action", method.into())];
        match method {
            "eth_call" => query.extend([("to", jv::text(&params[0]["to"])), ("data", jv::text(&params[0]["data"])), ("tag", p(1))]),
            "eth_getStorageAt" => query.extend([("address", p(0)), ("position", p(1)), ("tag", p(2))]),
            _ => query.extend([("address", p(0)), ("tag", p(1))]),
        }
        let data = self.api.request(&self.chain, &query)?;
        if jv::s(&data, "status") == "0" {
            return Err(Error::chain(format!("Etherscan: {}", super::explorers::py_str(jv::at(&data, "result")))));
        }
        let err = jv::at(&data, "error");
        if jv::truthy(err) {
            return Err(rpc_error(method, err, &self.label));
        }
        Ok(jv::at(&data, "result").clone())
    }
}

/// Where live reads go for one chain.
pub enum Node {
    Rpc(JsonRpc),
    Etherscan(EtherscanProxy),
}

impl Node {
    pub fn label(&self) -> &str {
        match self {
            Node::Rpc(n) => &n.label,
            Node::Etherscan(n) => &n.label,
        }
    }

    pub fn archive(&self) -> bool {
        match self {
            Node::Rpc(n) => n.archive,
            Node::Etherscan(_) => false,
        }
    }

    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        match self {
            Node::Rpc(n) => n.request(method, params),
            Node::Etherscan(n) => n.request(method, &params),
        }
    }

    pub fn batch(&self, calls: &[(&str, Value)]) -> Result<Vec<Result<Value>>> {
        match self {
            Node::Rpc(n) => n.batch(calls),
            Node::Etherscan(n) => Ok(calls
                .iter()
                .map(|(m, p)| match n.request(m, p) {
                    Err(e @ (Error::Chain(_) | Error::RateLimited(_) | Error::Reverted(_))) => Err(e),
                    Err(e) => Err(Error::chain(e.message())),
                    ok => ok,
                })
                .collect()),
        }
    }
}

// --- QuickNode ----------------------------------------------------------------------------------

/// `[entry.networks]` key = "url" pairs.
pub fn read_networks_file(path: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else { return BTreeMap::new() };
    if let Ok(doc) = text.parse::<toml::Table>() {
        if let Some(networks) = doc.get("entry").and_then(|e| e.get("networks")).and_then(|n| n.as_table()) {
            return networks.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect();
        }
    }
    // Not strict TOML: fall back to the line format the qn CLI writes.
    static LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^"?([A-Za-z0-9_.-]+)"?\s*=\s*"([^"]+)""#).unwrap());
    let mut networks = BTreeMap::new();
    let mut in_section = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_section = line == "[entry.networks]";
        } else if in_section {
            if let Some(c) = LINE.captures(line) {
                networks.insert(c[1].to_string(), c[2].to_string());
            }
        }
    }
    networks
}

fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(program)).find(|p| p.is_file())
}

/// HTTP URL of the account's multichain endpoint: `$BB_QUICKNODE_URL` or `qn endpoint list`.
fn multichain_endpoint() -> Option<String> {
    for key in ["BB_QUICKNODE_URL", "IMMUNEFI_QUICKNODE_URL", "CANTINA_QUICKNODE_URL"] {
        if let Some(v) = std::env::var(key).ok().filter(|v| !v.is_empty()) {
            return Some(v);
        }
    }
    let qn = which("qn")?;
    let mut child = Command::new(qn)
        .args(["endpoint", "list", "-o", "json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_secs(30) => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    let data: Value = serde_json::from_slice(&output.stdout).ok()?;
    let items = match &data {
        Value::Array(a) => a.clone(),
        other => jv::first(other, &["data", "endpoints"]).as_array().cloned().unwrap_or_default(),
    };
    items.iter().find_map(|ep| {
        let status = ep.get("status").and_then(Value::as_str).unwrap_or("active");
        (jv::t(ep, "is_multichain") && status == "active").then(|| jv::opt(ep, "http_url").map(str::to_string)).flatten()
    })
}

/// Per-chain JSON-RPC URLs built from one multichain endpoint.
pub struct QuickNode {
    subdomain: String,
    token: String,
    networks: BTreeMap<String, String>,
    net: Net,
}

impl QuickNode {
    pub fn new(endpoint_url: &str, networks: BTreeMap<String, String>, net: Net) -> Self {
        let (netloc, path) = split_url(endpoint_url);
        let subdomain = netloc.split('.').next().unwrap_or("").to_string();
        let token = path.split('/').find(|p| TOKEN.is_match(p)).unwrap_or("").to_string();
        QuickNode { subdomain, token, networks, net }
    }

    pub fn discover(net: Net) -> Option<QuickNode> {
        let file = qn_networks_file();
        if !file.is_file() {
            return None;
        }
        let endpoint = multichain_endpoint()?;
        let node = QuickNode::new(&endpoint, read_networks_file(&file), net);
        (!node.token.is_empty() && !node.networks.is_empty()).then_some(node)
    }

    pub fn url_for_network(&self, key: &str) -> String {
        let tooling = self.networks.get(key).map(String::as_str).unwrap_or("");
        let (netloc, path) = split_url(tooling);
        let domain = netloc.split_once('.').map(|(_, d)| d).unwrap_or(netloc);
        format!("https://{}.{domain}/{}{path}", self.subdomain, self.token)
    }

    /// {chain id: network key} for every EVM network; probed once per cache TTL.
    pub fn chain_ids(&self, client: &Client) -> Result<HashMap<u64, String>> {
        let probe = |key: &String| -> Option<u64> {
            let node = JsonRpc::new(&self.url_for_network(key), &format!("QuickNode {key}"), true, self.net.clone());
            let result = node.request("eth_chainId", json!([])).ok()?;
            let hex_id = result.as_str().filter(|s| s.starts_with("0x"))?;
            u64::from_str_radix(&hex_id[2..], 16).ok()
        };
        // The cache stores network names and chain ids only - never the endpoint URL or token.
        let mapping = client.cached("quicknode/chains", "quicknode:networks", || {
            let keys: Vec<&String> = self.networks.keys().collect();
            let mut found: Vec<Option<u64>> = vec![None; keys.len()];
            std::thread::scope(|s| {
                for (chunk_keys, chunk_out) in keys
                    .chunks(keys.len().div_ceil(QN_PROBE_WORKERS).max(1))
                    .zip(found.chunks_mut(keys.len().div_ceil(QN_PROBE_WORKERS).max(1)))
                {
                    s.spawn(|| {
                        for (k, slot) in chunk_keys.iter().zip(chunk_out.iter_mut()) {
                            *slot = probe(k);
                        }
                    });
                }
            });
            let map: serde_json::Map<String, Value> =
                keys.iter().zip(found).filter_map(|(k, id)| Some(((*k).clone(), json!(id?)))).collect();
            Ok(Value::Object(map))
        })?;
        let mut out = HashMap::new();
        if let Some(map) = mapping.data.as_object() {
            for (key, id) in map {
                if let Some(id) = id.as_u64() {
                    out.insert(id, key.clone());
                }
            }
        }
        Ok(out)
    }

    pub fn for_chain(&self, client: &Client, chain_id: u64) -> Result<Option<JsonRpc>> {
        Ok(self
            .chain_ids(client)?
            .get(&chain_id)
            .map(|key| JsonRpc::new(&self.url_for_network(key), &format!("QuickNode {key}"), true, self.net.clone())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_and_reasons() {
        assert_eq!(block_tag(None).unwrap(), "latest");
        assert_eq!(block_tag(Some("1000")).unwrap(), "0x3e8");
        assert_eq!(block_tag(Some("0x10")).unwrap(), "0x10");
        let encoded = abi::encode(&[alloy_dyn_abi::DynSolValue::String("nope".into())]);
        let mut data = vec![0x08, 0xc3, 0x79, 0xa0];
        data.extend(encoded);
        assert_eq!(revert_reason(&data), "nope");
        let mut panic = vec![0x4e, 0x48, 0x7b, 0x71];
        panic.extend(U256::from(0x11).to_be_bytes::<32>());
        assert_eq!(revert_reason(&panic), "panic 0x11");
        assert_eq!(revert_reason(&[]), "no reason");
        assert_eq!(revert_reason(&[0xde, 0xad]), "0xdead");
    }

    #[test]
    fn networks_file_parser() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("networks.toml");
        std::fs::write(
            &path,
            "[entry]\nendpoint_id = \"1\"\n\n[entry.networks]\npolygon = \"https://t.matic.quiknode.pro/\"\nhype-mainnet = \"https://t.hype-mainnet.quiknode.pro/evm\"\n",
        )
        .unwrap();
        let networks = read_networks_file(&path);
        assert_eq!(networks.len(), 2);
        assert_eq!(networks["polygon"], "https://t.matic.quiknode.pro/");
        assert_eq!(networks["hype-mainnet"], "https://t.hype-mainnet.quiknode.pro/evm");
    }

    #[test]
    fn quicknode_urls_per_network() {
        let token = "ab".repeat(20);
        let networks = BTreeMap::from([
            ("ethereum-mainnet".to_string(), "https://tooling-access-x.quiknode.pro/".to_string()),
            ("avalanche-mainnet".to_string(), "https://tooling-access-x.avalanche-mainnet.quiknode.pro/ext/bc/C/rpc/".to_string()),
        ]);
        let qn = QuickNode::new(&format!("https://brave-cat.hype-mainnet.quiknode.pro/{token}/evm"), networks, Net::default());
        assert_eq!(
            qn.url_for_network("avalanche-mainnet"),
            format!("https://brave-cat.avalanche-mainnet.quiknode.pro/{token}/ext/bc/C/rpc/")
        );
        assert_eq!(qn.url_for_network("ethereum-mainnet"), format!("https://brave-cat.quiknode.pro/{token}/"));
    }
}
