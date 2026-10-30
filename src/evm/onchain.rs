//! Chooses the right explorer and node per chain, and caches what is safe to cache.
//!
//! Verified source, ABI and creation info are cached like opportunity data (10h+). Balances, calls
//! and storage are live state: never cached, refused with `--offline`.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use alloy_dyn_abi::DynSolValue;
use alloy_primitives::{Address, U256, keccak256};
use serde_json::{Map, Value, json};

use super::abi::{self, Function};
use super::client::Client;
use super::explorers::Chain;
use super::explorers::{
    COMPAT_RATE_LIMIT_RETRIES, CompatApi, Contract, ETHERSCAN_CHAINLIST, EtherscanApi, Registry, Sourcify, UNVERIFIED, first_address,
    host_of, normalize,
};
use super::rpc::{EtherscanProxy, JsonRpc, Node, QuickNode, block_tag};
use crate::error::{Error, Result};
use crate::jv;

pub const MULTICALL3: &str = "0xcA11bde05977b3631167028862bE2a173976CA11";
pub const MULTICALL_BATCH: usize = 40;
pub const CODE_BATCH: usize = 50;
pub const RPC_MODES: &[&str] = &["auto", "quicknode", "public", "explorer", "etherscan"];

/// EIP-1967 slots.
pub fn slot(name: &str) -> Option<U256> {
    let hex = match name {
        "implementation" => "360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc",
        "admin" => "b53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103",
        "beacon" => "a3f0ad74e5423aebfd80d3ef4346578335a9a72aeaee59ff6cb3582b35133d50",
        _ => return None,
    };
    U256::from_str_radix(hex, 16).ok()
}

/// The pre-EIP-1967 slot OpenZeppelin's ZeppelinOS proxies (USDC among them) use for `name`.
pub fn legacy_slot(name: &str) -> Option<U256> {
    let key = match name {
        "implementation" => "org.zeppelinos.proxy.implementation",
        "admin" => "org.zeppelinos.proxy.admin",
        _ => return None,
    };
    Some(U256::from_be_bytes(keccak256(key).0))
}

/// Contract metadata from explorers, live state from nodes.
///
/// `rpc` is a node URL, or one of: `auto` (QuickNode when the `qn` CLI is set up, then the
/// chain's public RPC, its explorer's RPC, then Etherscan), `quicknode`, `public`, `explorer`
/// or `etherscan`.
pub struct OnChain {
    pub client: Client,
    pub etherscan: Rc<EtherscanApi>,
    sourcify: Sourcify,
    pub rpc_mode: String,
    registry: OnceCell<Registry>,
    quicknode: OnceCell<Option<QuickNode>>,
    apis: RefCell<HashMap<String, Rc<CompatApi>>>,
}

impl OnChain {
    pub fn new(client: Client, etherscan_key: Option<String>, rpc: Option<&str>) -> Self {
        let net = client.net.clone();
        OnChain {
            etherscan: Rc::new(EtherscanApi::new(etherscan_key, net.clone())),
            sourcify: Sourcify::new(net),
            rpc_mode: rpc.filter(|r| !r.is_empty()).unwrap_or("auto").to_string(),
            client,
            registry: OnceCell::new(),
            quicknode: OnceCell::new(),
            apis: RefCell::new(HashMap::new()),
        }
    }

    /// Use this QuickNode (or none) instead of discovering one.
    pub fn set_quicknode(&mut self, node: Option<QuickNode>) {
        self.quicknode = OnceCell::from(node);
    }

    // --- chains ---------------------------------------------------------------------------------

    pub fn registry(&self) -> Result<&Registry> {
        if let Some(reg) = self.registry.get() {
            return Ok(reg);
        }
        let rows = self
            .client
            .cached("etherscan/chainlist", ETHERSCAN_CHAINLIST, || {
                let data = self.client.net.get_json(ETHERSCAN_CHAINLIST)?;
                Ok(data.get("result").cloned().unwrap_or(data))
            })?
            .data;
        let reg = Registry::new(rows.as_array().map(Vec::as_slice).unwrap_or(&[]));
        Ok(self.registry.get_or_init(|| reg))
    }

    pub fn resolve_chain(&self, value: Option<&str>) -> Result<Chain> {
        self.registry()?.resolve(value)
    }

    pub fn chain_for_url(&self, url: &str) -> Result<Option<Chain>> {
        Ok(self.registry()?.for_url(url))
    }

    /// Accept an address or an explorer URL (which also selects the chain).
    pub fn parse_target(&self, text: &str, chain: Option<&str>) -> Result<(Chain, String)> {
        let address = first_address(text).ok_or_else(|| Error::value(format!("no contract address in {}", abi::py_repr(text))))?;
        if chain.is_none() && text.contains('/') {
            return match self.chain_for_url(text)? {
                Some(found) => Ok((found, address)),
                None => {
                    let url = if text.contains("://") { text.to_string() } else { format!("https://{text}") };
                    Err(Error::value(format!("{} is not a known explorer; pass --chain", host_of(&url))))
                }
            };
        }
        Ok((self.resolve_chain(chain)?, address))
    }

    // --- verified source (cached) ---------------------------------------------------------------

    fn api(&self, base: &str) -> Rc<CompatApi> {
        self.apis.borrow_mut().entry(base.to_string()).or_insert_with(|| Rc::new(CompatApi::new(base, self.client.net.clone()))).clone()
    }

    /// Forget which explorer APIs were rate limited.
    pub fn reset_apis(&self) {
        self.apis.borrow_mut().clear();
    }

    fn with_provider(raw: Value, provider: &str) -> Value {
        let mut map = raw.as_object().cloned().unwrap_or_default();
        map.insert("_provider".into(), json!(provider));
        Value::Object(map)
    }

    /// First verified hit across Etherscan, the explorer's own API, then Sourcify.
    ///
    /// A failure of Etherscan or Sourcify raises instead of returning "unverified", so a
    /// transient error is never cached as a 10-hour false negative. The per-explorer APIs are
    /// best effort: several are permanently down, and Sourcify backs them up. One that is only
    /// rate limiting us raises too (after Sourcify had its chance), for the same reason.
    fn lookup_source(&self, chain: &Chain, address: &str) -> Result<Value> {
        let mut unverified: Option<Value> = None;
        let mut throttled: Option<Error> = None;
        let params =
            |action: &str| vec![("module", "contract".to_string()), ("action", action.to_string()), ("address", address.to_string())];
        if chain.etherscan && self.etherscan.api_key.is_some() {
            let rows = self.etherscan.result(chain, &params("getsourcecode"))?;
            let raw = Self::with_provider(normalize(rows.get(0).unwrap_or(&json!({}))), "etherscan");
            if Contract::new(chain.clone(), address, raw.clone()).verified() {
                return Ok(raw);
            }
            unverified = Some(raw);
        }
        for base in &chain.apis {
            let api = self.api(base);
            let rows = match api.result(&params("getsourcecode")) {
                Ok(rows) => rows,
                Err(e @ Error::RateLimited(_)) => {
                    throttled = Some(e);
                    continue;
                }
                Err(Error::Chain(_)) => continue,
                Err(e) => return Err(e),
            };
            let first = rows.as_array().and_then(|r| r.first()).cloned().unwrap_or_else(|| json!({}));
            let raw = Self::with_provider(normalize(&first), &api.label);
            if Contract::new(chain.clone(), address, raw.clone()).verified() {
                return Ok(raw);
            }
            unverified = unverified.or(Some(raw));
        }
        if let Some(raw) = self.sourcify.source(chain, address)? {
            return Ok(Self::with_provider(raw, "sourcify"));
        }
        if let Some(e) = throttled {
            return Err(e);
        }
        Ok(unverified.unwrap_or_else(|| json!({"_provider": "none", "ABI": UNVERIFIED})))
    }

    pub fn contract(&self, chain: &Chain, address: &str) -> Result<Contract> {
        let source = format!("{}#code", chain.address_url(address));
        let key = format!("contracts/{}/{}", chain.id, address.to_lowercase());
        let entry = self.client.cached(&key, &source, || self.lookup_source(chain, address))?;
        Ok(Contract::new(chain.clone(), address, entry.data))
    }

    pub fn creation(&self, chain: &Chain, address: &str) -> Result<Option<Value>> {
        let fetch = || -> Result<Value> {
            let params = vec![
                ("module", "contract".to_string()),
                ("action", "getcontractcreation".to_string()),
                ("contractaddresses", address.to_string()),
            ];
            let mut attempts: Vec<Box<dyn Fn() -> Result<Value> + '_>> = Vec::new();
            if chain.etherscan && self.etherscan.api_key.is_some() {
                attempts.push(Box::new(|| self.etherscan.result(chain, &params)));
            }
            for base in &chain.apis {
                let params = &params;
                attempts.push(Box::new(move || self.api(base).result(params)));
            }
            for attempt in attempts {
                let rows = match attempt() {
                    Ok(rows) => rows,
                    Err(Error::Chain(_) | Error::RateLimited(_) | Error::Reverted(_)) => continue,
                    Err(e) => return Err(e),
                };
                if let Some(row) = rows.as_array().and_then(|r| r.first()) {
                    let mut out = Map::new();
                    for k in ["contractCreator", "txHash", "blockNumber", "timestamp", "contractFactory"] {
                        out.insert(k.into(), jv::at(row, k).clone());
                    }
                    return Ok(Value::Object(out));
                }
            }
            let creation = jv::at(&self.contract(chain, address)?.raw, "_creation").clone();
            Ok(if jv::truthy(&creation) { creation } else { json!({}) })
        };
        let source = format!("{}#creation", chain.address_url(address));
        let key = format!("contracts/{}/{}.creation", chain.id, address.to_lowercase());
        let data = self.client.cached(&key, &source, fetch)?.data;
        Ok(jv::truthy(&data).then_some(data))
    }

    // --- nodes ----------------------------------------------------------------------------------

    pub fn quicknode(&self) -> Option<&QuickNode> {
        self.quicknode.get_or_init(|| QuickNode::discover(self.client.net.clone())).as_ref()
    }

    /// The JSON-RPC provider for live reads on `chain`.
    pub fn node(&self, chain: &Chain, block: Option<&str>) -> Result<Node> {
        if self.client.offline {
            return Err(Error::chain("live chain reads are not available with --offline"));
        }
        let net = self.client.net.clone();
        let mode = self.rpc_mode.as_str();
        if !RPC_MODES.contains(&mode) {
            return Ok(Node::Rpc(JsonRpc::new(mode, "--rpc endpoint", true, net)));
        }
        let mut candidates: Vec<Node> = Vec::new();
        if matches!(mode, "auto" | "quicknode") {
            if let Some(qn) = self.quicknode() {
                if let Some(node) = qn.for_chain(&self.client, chain.id)? {
                    candidates.push(Node::Rpc(node));
                }
            }
        }
        if mode == "quicknode" && candidates.is_empty() {
            return Err(Error::chain(if self.quicknode().is_some() {
                format!("QuickNode has no network for {} ({})", chain.name, chain.id)
            } else {
                "QuickNode not found: install and log in to the `qn` CLI, or set BB_QUICKNODE_URL".into()
            }));
        }
        let rpc = |url: &String| Node::Rpc(JsonRpc::new(url, &format!("{} RPC", host_of(url)), false, net.clone()));
        if matches!(mode, "auto" | "public") {
            candidates.extend(chain.public_rpcs.iter().map(rpc));
        }
        if matches!(mode, "auto" | "explorer") {
            candidates.extend(chain.explorer_rpcs.iter().map(rpc));
        }
        if matches!(mode, "auto" | "etherscan") && chain.etherscan && self.etherscan.api_key.is_some() {
            candidates.push(Node::Etherscan(EtherscanProxy::new(self.etherscan.clone(), chain.clone())));
        }
        if candidates.is_empty() {
            return Err(Error::chain(format!("no node for {} ({}); pass --rpc <url>", chain.name, chain.id)));
        }
        if block_tag(block)? != "latest" {
            let first = candidates[0].label().to_string();
            return candidates.into_iter().find(Node::archive).ok_or_else(|| {
                Error::chain(format!(
                    "--block needs a real node for {}: QuickNode or --rpc <archive URL> ({first} only serves the latest state)",
                    chain.name
                ))
            });
        }
        Ok(candidates.into_iter().next().expect("non-empty"))
    }

    fn rpc(&self, chain: &Chain, method: &str, params: Value, block: Option<&str>) -> Result<Value> {
        self.node(chain, block)?.request(method, params)
    }

    pub fn balance(&self, chain: &Chain, address: &str, block: Option<&str>) -> Result<U256> {
        let result = self.rpc(chain, "eth_getBalance", json!([address, block_tag(block)?]), block)?;
        parse_quantity(&result)
    }

    pub fn code_size(&self, chain: &Chain, address: &str, block: Option<&str>) -> Result<usize> {
        let code = self.rpc(chain, "eth_getCode", json!([address, block_tag(block)?]), block)?;
        Ok(hex_len(&code))
    }

    /// Deployed code size for many addresses, batched into few requests.
    pub fn code_sizes(&self, chain: &Chain, addresses: &[String], block: Option<&str>) -> Result<Vec<(String, Result<usize>)>> {
        let node = self.node(chain, block)?;
        let tag = block_tag(block)?;
        let mut out = Vec::new();
        for chunk in addresses.chunks(CODE_BATCH) {
            let calls: Vec<(&str, Value)> = chunk.iter().map(|a| ("eth_getCode", json!([a, tag]))).collect();
            for (address, code) in chunk.iter().zip(node.batch(&calls)?) {
                out.push((address.clone(), code.map(|c| hex_len(&c))));
            }
        }
        Ok(out)
    }

    pub fn storage(&self, chain: &Chain, address: &str, slot: U256, block: Option<&str>) -> Result<[u8; 32]> {
        let raw = self.rpc(chain, "eth_getStorageAt", json!([address, format!("{slot:#x}"), block_tag(block)?]), block)?;
        let text = raw.as_str().unwrap_or("0x");
        let digits = text.strip_prefix("0x").unwrap_or(text);
        let padded = format!("{digits:0>64}");
        let bytes = hex::decode(&padded[padded.len() - 64..]).map_err(|_| Error::chain("node returned invalid storage"))?;
        Ok(bytes.try_into().expect("32 bytes"))
    }

    pub fn call(&self, chain: &Chain, to: &str, data: &[u8], block: Option<&str>) -> Result<Vec<u8>> {
        let params = json!([{"to": to, "data": format!("0x{}", hex::encode(data))}, block_tag(block)?]);
        let raw = self.rpc(chain, "eth_call", params, block)?;
        let text = raw.as_str().unwrap_or("0x");
        hex::decode(text.strip_prefix("0x").unwrap_or(text)).map_err(|_| Error::chain("node returned invalid call data"))
    }

    /// Implementation address from the EIP-1967 slot, if the contract uses it.
    pub fn live_implementation(&self, chain: &Chain, address: &str, block: Option<&str>) -> Result<Option<String>> {
        let word = self.storage(chain, address, slot("implementation").expect("known slot"), block)?;
        Ok(word.iter().any(|b| *b != 0).then(|| abi::word_address(&word)))
    }

    /// Run many read calls through Multicall3 (one request per batch); falls back to one
    /// request per call when Multicall3 is not deployed on the chain.
    pub fn multicall(&self, chain: &Chain, calls: &[(String, Vec<u8>)], block: Option<&str>) -> Result<Vec<(bool, Vec<u8>)>> {
        let f = abi::parse_signature("aggregate3((address,bool,bytes)[])((bool,bytes)[])")?;
        let outputs = f.outputs.clone().unwrap_or_default();
        let mut out = Vec::new();
        for batch in calls.chunks(MULTICALL_BATCH) {
            let items: Vec<DynSolValue> = batch
                .iter()
                .map(|(target, data)| {
                    let address = target.parse::<Address>().unwrap_or_default();
                    DynSolValue::Tuple(vec![DynSolValue::Address(address), DynSolValue::Bool(true), DynSolValue::Bytes(data.clone())])
                })
                .collect();
            let attempt = self
                .call(chain, MULTICALL3, &f.encode_call(&[DynSolValue::Array(items)]), block)
                .and_then(|ret| abi::decode(&outputs, &ret));
            match attempt {
                Ok(values) => {
                    for result in values.first().and_then(|v| v.as_array()).unwrap_or(&[]) {
                        let fields = result.as_tuple().unwrap_or(&[]);
                        let ok = matches!(fields.first(), Some(DynSolValue::Bool(true)));
                        let data = fields.get(1).and_then(|d| d.as_bytes()).unwrap_or(&[]).to_vec();
                        out.push((ok, data));
                    }
                }
                Err(Error::Reverted(_) | Error::Value(_)) => {
                    for (target, data) in batch {
                        match self.call(chain, target, data, block) {
                            Ok(ret) => out.push((true, ret)),
                            Err(e @ Error::Reverted(_)) => out.push((false, e.to_string().into_bytes())),
                            Err(e) => return Err(e),
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// ABI functions to use when calling `contract`; for proxies, the implementation's.
    pub fn functions_for(&self, contract: &Contract) -> Result<(Vec<Function>, Option<Contract>)> {
        let own = contract.functions()?;
        let mut impl_address = contract.implementation();
        if impl_address.is_none() && own.is_empty() {
            impl_address = self.live_implementation(&contract.chain, &contract.address, None).unwrap_or(None);
        }
        if let Some(address) = impl_address {
            let implementation = self.contract(&contract.chain, &address)?;
            if implementation.verified() {
                let mut fns = implementation.functions()?;
                let seen: Vec<String> = fns.iter().map(Function::signature).collect();
                fns.extend(own.into_iter().filter(|f| !seen.contains(&f.signature())));
                return Ok((fns, Some(implementation)));
            }
        }
        Ok((own, None))
    }
}

fn hex_len(code: &Value) -> usize {
    let text = code.as_str().unwrap_or("0x");
    text.len().saturating_sub(2) / 2
}

fn parse_quantity(value: &Value) -> Result<U256> {
    let text = value.as_str().unwrap_or("0x0");
    U256::from_str_radix(text.strip_prefix("0x").unwrap_or(text), 16).map_err(|_| Error::chain(format!("invalid quantity {text}")))
}

/// How many times a throttled explorer is retried; re-exported for tests.
pub const EXPLORER_RETRIES: usize = COMPAT_RATE_LIMIT_RETRIES;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::cache::Cache;
    use crate::evm::client::Net;
    use crate::evm::explorers::tests::{ADDR, chainlist};

    type Route = (String, std::result::Result<Value, Error>);

    /// A fake network: `routes` are consumed in order per matching URL substring.
    struct Fake {
        routes: Arc<Mutex<Vec<Route>>>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        fn new() -> (Self, Net) {
            let routes: Arc<Mutex<Vec<Route>>> = Arc::default();
            let calls: Arc<Mutex<Vec<String>>> = Arc::default();
            let (r, c) = (routes.clone(), calls.clone());
            let net = Net::fake(move |url, _, _| {
                if url == ETHERSCAN_CHAINLIST {
                    return Ok(json!({"result": chainlist()}).to_string());
                }
                c.lock().unwrap().push(url.to_string());
                let mut routes = r.lock().unwrap();
                let i = routes.iter().position(|(needle, _)| url.contains(needle.as_str()));
                match i {
                    Some(i) => routes.remove(i).1.map(|v| v.to_string()),
                    None => panic!("unexpected request {url}"),
                }
            });
            (Fake { routes, calls }, net)
        }

        fn serve(&self, routes: Vec<(&str, std::result::Result<Value, Error>)>) {
            self.calls.lock().unwrap().clear();
            *self.routes.lock().unwrap() = routes.into_iter().map(|(n, r)| (n.to_string(), r)).collect();
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn setup(offline: bool) -> (tempfile::TempDir, OnChain, Fake) {
        let dir = tempfile::tempdir().unwrap();
        let (fake, net) = Fake::new();
        let mut client = Client::new(Cache::new(dir.path(), 10.0), false, offline);
        client.net = net;
        let mut oc = OnChain::new(client, Some("SECRETKEY".into()), Some("auto"));
        oc.set_quicknode(None);
        (dir, oc, fake)
    }

    fn reg() -> Registry {
        Registry::new(&chainlist())
    }

    fn word(n: u64) -> String {
        format!("0x{}", hex::encode(U256::from(n).to_be_bytes::<32>()))
    }

    #[test]
    fn zeppelinos_slots() {
        let hex = |name| legacy_slot(name).map(|s| format!("{s:#x}"));
        assert_eq!(hex("implementation").unwrap(), "0x7050c9e0f4ca769c69bd3a8ef740bc37934f8e2c036e5a723fd8ee048ed3f8c3");
        assert_eq!(hex("admin").unwrap(), "0x10d6a54a4754c8869d6886b5f5d7fbfa5b4522237ea5c60d11bc4e7a1ff9390b");
        assert_eq!(hex("beacon"), None);
    }

    #[test]
    fn falls_back_to_sourcify_and_caches() {
        let (_d, oc, fake) = setup(false);
        let unverified = json!({"status": "1", "result": [{"SourceCode": "", "ABI": UNVERIFIED}]});
        let doc = json!({"compilation": {"name": "Vault"}, "abi": [], "stdJsonInput": {"sources": {"V.sol": {"content": "v"}}}});
        fake.serve(vec![("api.etherscan.io", Ok(unverified)), ("sourcify.dev", Ok(doc))]);
        let chain = reg().get(1);
        oc.contract(&chain, ADDR).unwrap();
        let c = oc.contract(&chain, ADDR).unwrap();
        assert_eq!((c.name(), c.provider(), c.verified()), ("Vault", "sourcify", true));
        assert_eq!(fake.calls().len(), 2); // second lookup came from the cache
    }

    #[test]
    fn etherscan_failure_is_not_cached_and_key_never_leaks() {
        let (_d, oc, fake) = setup(false);
        let chain = reg().get(1);
        fake.serve(vec![("api.etherscan.io", Err(Error::fetch("down ?apikey=SECRETKEY", None)))]);
        let err = oc.contract(&chain, ADDR).unwrap_err();
        assert!(matches!(err, Error::Chain(_)));
        assert!(!err.to_string().contains("SECRETKEY"));
        assert!(oc.client.cache.read(&format!("contracts/1/{}", ADDR.to_lowercase())).is_none());
        fake.serve(vec![("api.etherscan.io", Err(Error::fetch("could not reach ?apikey=SECRETKEY", None)))]);
        let err = oc.balance(&chain, ADDR, None).unwrap_err();
        assert!(!err.to_string().contains("SECRETKEY"));
    }

    #[test]
    fn rate_limit_is_retried() {
        let (_d, oc, fake) = setup(false);
        fake.serve(vec![
            ("api.etherscan.io", Ok(json!({"status": "0", "message": "NOTOK", "result": "Max calls per sec rate limit reached (3/sec)"}))),
            ("api.etherscan.io", Ok(json!({"status": "1", "message": "OK", "result": "42"}))),
        ]);
        assert_eq!(oc.balance(&reg().get(1), ADDR, None).unwrap(), U256::from(42));
        assert_eq!(fake.calls().len(), 2);
    }

    #[test]
    fn historical_reads_need_a_real_node() {
        let (_d, mut oc, fake) = setup(false);
        let chain = reg().get(30); // Rootstock: public and explorer RPCs only
        fake.serve(vec![]);
        let err = oc.call(&chain, ADDR, b"", Some("1000")).unwrap_err();
        assert!(err.to_string().contains("--block needs a real node"), "{err}");
        oc.rpc_mode = "https://node.example/secret-token".into();
        fake.serve(vec![("node.example", Ok(json!({"jsonrpc": "2.0", "id": 1, "result": word(5)})))]);
        let ret = oc.call(&chain, ADDR, b"", Some("1000")).unwrap();
        assert_eq!(U256::from_be_slice(&ret), U256::from(5));
    }

    #[test]
    fn quicknode_is_preferred_and_labels_hide_the_token() {
        let (_d, mut oc, fake) = setup(false);
        let token = "ab".repeat(20);
        let networks = BTreeMap::from([("ethereum-mainnet".to_string(), "https://tooling-access-x.quiknode.pro/".to_string())]);
        let qn = QuickNode::new(&format!("https://brave-cat.hype-mainnet.quiknode.pro/{token}/evm"), networks, oc.client.net.clone());
        oc.client.cache.write("quicknode/chains", json!({"ethereum-mainnet": 1}), "quicknode:networks").unwrap();
        oc.set_quicknode(Some(qn));
        fake.serve(vec![]);
        let node = oc.node(&reg().get(1), Some("123")).unwrap();
        assert_eq!(node.label(), "QuickNode ethereum-mainnet");
        assert!(!node.label().contains(&token));
        let cached = std::fs::read_to_string(oc.client.cache.root.join("quicknode/chains.json")).unwrap();
        assert!(!cached.contains(&token) && !cached.contains("quiknode.pro"));
    }

    #[test]
    fn quicknode_chain_probe_is_cached_without_secrets() {
        let (_d, oc, fake) = setup(false);
        let token = "cd".repeat(20);
        let networks = BTreeMap::from([
            ("base-mainnet".to_string(), "https://t.base-mainnet.quiknode.pro/".to_string()),
            ("broken".to_string(), "https://t.broken.quiknode.pro/".to_string()),
        ]);
        let qn = QuickNode::new(&format!("https://cat.quiknode.pro/{token}/"), networks, oc.client.net.clone());
        fake.serve(vec![
            ("base-mainnet", Ok(json!({"jsonrpc": "2.0", "id": 1, "result": "0x2105"}))),
            ("broken", Ok(json!({"jsonrpc": "2.0", "id": 1, "error": {"message": "nope"}}))),
        ]);
        let ids = qn.chain_ids(&oc.client).unwrap();
        assert_eq!(ids.get(&8453).map(String::as_str), Some("base-mainnet"));
        assert_eq!(ids.len(), 1);
        let cached = std::fs::read_to_string(oc.client.cache.root.join("quicknode/chains.json")).unwrap();
        assert!(!cached.contains(&token));
    }

    #[test]
    fn paid_chain_message_and_revert_reason() {
        let (_d, oc, fake) = setup(false);
        let mut reason = vec![0x08, 0xc3, 0x79, 0xa0];
        reason.extend(abi::encode(&[DynSolValue::String("nope".into())]));
        fake.serve(vec![(
            "api.etherscan.io",
            Ok(json!({"status": "0", "message": "NOTOK", "result": "Free API access is not supported for this chain."})),
        )]);
        let err = oc.call(&reg().get(43114), ADDR, b"", None).unwrap_err();
        assert!(err.to_string().contains("paid Etherscan plan"), "{err}");
        fake.serve(vec![(
            "api.etherscan.io",
            Ok(json!({"jsonrpc": "2.0", "id": 1, "error": {"code": 3, "message": "execution reverted", "data": format!("0x{}", hex::encode(&reason))}})),
        )]);
        let err = oc.call(&reg().get(1), ADDR, b"", None).unwrap_err();
        assert!(matches!(&err, Error::Reverted(m) if m.contains("nope")), "{err}");
    }

    #[test]
    fn offline_refuses_live_reads() {
        let (_d, oc, _fake) = setup(true);
        assert!(matches!(oc.balance(&reg().get(1), ADDR, None), Err(Error::Chain(_))));
    }

    #[test]
    fn batch_code_sizes_and_multicall_fallback() {
        let (_d, mut oc, fake) = setup(false);
        let node = "https://node.example/rpc";
        oc.rpc_mode = node.into();
        let eth = reg().get(1);
        let other = format!("0x{}", "22".repeat(20));
        fake.serve(vec![(node, Ok(json!([{"jsonrpc": "2.0", "id": 0, "result": "0x6001"}, {"jsonrpc": "2.0", "id": 1, "result": "0x"}])))]);
        let sizes = oc.code_sizes(&eth, &[ADDR.to_string(), other], None).unwrap();
        let sizes: Vec<usize> = sizes.into_iter().map(|(_, s)| s.unwrap()).collect();
        assert_eq!(sizes, [2, 0]);
        fake.serve(vec![
            (node, Ok(json!({"jsonrpc": "2.0", "id": 1, "result": "0x"}))), // no Multicall3 on this chain
            (node, Ok(json!({"jsonrpc": "2.0", "id": 1, "result": word(7)}))),
            (node, Ok(json!({"jsonrpc": "2.0", "id": 1, "error": {"code": 3, "message": "execution reverted", "data": "0x"}}))),
        ]);
        let out = oc.multicall(&eth, &[(ADDR.to_string(), vec![1]), (ADDR.to_string(), vec![2])], None).unwrap();
        assert_eq!(out[0], (true, U256::from(7).to_be_bytes::<32>().to_vec()));
        assert!(!out[1].0);
        assert_eq!(fake.calls().len(), 3);
    }

    #[test]
    fn rate_limited_explorer_is_not_cached_as_unverified() {
        let (_d, oc, fake) = setup(false);
        let chain = reg().get(30); // Rootstock: Blockscout, then Sourcify
        let mut routes: Vec<(&str, std::result::Result<Value, Error>)> =
            (0..EXPLORER_RETRIES).map(|_| ("rootstock.blockscout.com/api", Err(Error::fetch("HTTP 429", Some(429))))).collect();
        routes.push(("sourcify.dev", Err(Error::NotFound("404".into()))));
        fake.serve(routes);
        assert!(matches!(oc.contract(&chain, ADDR), Err(Error::RateLimited(_))));
        assert!(oc.client.cache.read(&format!("contracts/30/{}", ADDR.to_lowercase())).is_none());
        let other = format!("0x{}", "33".repeat(20)); // the throttled API is skipped for the rest of the run
        fake.serve(vec![("sourcify.dev", Err(Error::NotFound("404".into())))]);
        assert!(matches!(oc.contract(&chain, &other), Err(Error::RateLimited(_))));
        assert_eq!(fake.calls().len(), 1);
        oc.reset_apis();
        let busy = json!({"message": "Too many requests. Increase limits now", "result": null, "status": "0"});
        let verified = json!({"status": "1", "result": [{"SourceCode": "x", "ABI": "[]", "ContractName": "MoC"}]});
        fake.serve(vec![("rootstock.blockscout.com/api", Ok(busy)), ("rootstock.blockscout.com/api", Ok(verified))]);
        let c = oc.contract(&chain, ADDR).unwrap();
        assert_eq!((c.name(), fake.calls().len()), ("MoC", 2));
    }

    #[test]
    fn sourcify_unsupported_chain_is_unverified() {
        let (_d, oc, fake) = setup(false);
        fake.serve(vec![("sourcify.dev", Err(Error::fetch("HTTP 400", Some(400))))]);
        let c = oc.contract(&reg().get(128), ADDR).unwrap(); // HECO: Sourcify only, and it rejects the chain
        assert!(!c.verified());
    }

    #[test]
    fn public_rpc_before_explorer_rpc() {
        let (_d, mut oc, _fake) = setup(false);
        let chain = reg().get(30);
        assert_eq!(oc.node(&chain, None).unwrap().label(), "public-node.rsk.co RPC");
        oc.rpc_mode = "explorer".into();
        assert_eq!(oc.node(&chain, None).unwrap().label(), "rootstock.blockscout.com RPC");
    }

    #[test]
    fn oversized_batches_are_split() {
        let (_d, mut oc, fake) = setup(false);
        let node = "https://node.example/rpc";
        oc.rpc_mode = node.into();
        let addresses: Vec<String> = (1..8).map(|i| format!("0x{i:040x}")).collect();
        let reply = |n: usize| Ok(Value::Array((0..n).map(|i| json!({"jsonrpc": "2.0", "id": i, "result": "0x60"})).collect()));
        fake.serve(vec![(node, Err(Error::fetch("HTTP 413", Some(413)))), (node, reply(5)), (node, reply(2))]);
        let sizes = oc.code_sizes(&reg().get(1), &addresses, None).unwrap();
        assert_eq!(sizes.into_iter().map(|(_, s)| s.unwrap()).collect::<Vec<_>>(), [1; 7]);
        assert_eq!(fake.calls().len(), 3);
    }

    #[test]
    fn cache_keys_match_the_python_layout() {
        let (_d, oc, fake) = setup(false);
        let chain = reg().get(1);
        fake.serve(vec![(
            "api.etherscan.io",
            Ok(json!({"status": "1", "result": [{"SourceCode": "x", "ABI": "[]", "ContractName": "A"}]})),
        )]);
        oc.contract(&chain, ADDR).unwrap();
        oc.registry().unwrap();
        let root = &oc.client.cache.root;
        assert!(root.join(format!("contracts/1/{}.json", ADDR.to_lowercase())).is_file());
        assert!(root.join("etherscan/chainlist.json").is_file());
    }
}
