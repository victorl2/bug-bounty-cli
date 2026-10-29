"""Block explorers: which chain an explorer link belongs to, and where verified source lives.

Verified source/ABI is looked up per chain in this order, stopping at the first verified hit:

1. Etherscan API v2 (one key, ~60 chains)
2. the chain's own Etherscan-compatible explorer API (Blockscout instances, Routescan, zkSync)
3. Sourcify (open verification repository, any EVM chain, no key)

Every provider returns Etherscan's ``getsourcecode`` shape, so the rest of the code only deals
with one format.
"""

from __future__ import annotations

import json
import os
import re
import time
from dataclasses import dataclass, field, replace
from pathlib import PurePosixPath
from typing import Any
from urllib.parse import urlencode, urlparse

from bb_cli.evm import abi
from bb_cli.evm.client import FetchError, NotFoundError, http_request

ETHERSCAN_API = "https://api.etherscan.io/v2/api"
ETHERSCAN_CHAINLIST = "https://api.etherscan.io/v2/chainlist"
SOURCIFY_API = "https://sourcify.dev/server/v2/contract"
ETHERSCAN_INTERVAL_SECONDS = 0.35  # free tier allows 3 calls/sec
OPEN_API_INTERVAL_SECONDS = 0.2
RATE_LIMIT_RETRIES = 5
COMPAT_RATE_LIMIT_RETRIES = 2  # keyless Blockscout allows ~10 calls per hour: waiting rarely helps
UNVERIFIED = "Contract source code not verified"

# Explorers Immunefi links to that Etherscan v2 does not list under that host. The flavour says
# how to reach verified source: "blockscout" (Etherscan-compatible /api plus /api/eth-rpc),
# "compat" (Etherscan-compatible /api only), "routescan"/"routescan-testnet", "zksync", or None
# when only Sourcify has it. Keys may carry a path prefix for explorers hosting several chains.
EXPLORERS: list[tuple[str, int, str, str | None]] = [
    ("snowtrace.io", 43114, "Avalanche C-Chain", "routescan"),
    ("testnet.snowtrace.io", 43113, "Avalanche Fuji Testnet", "routescan-testnet"),
    ("subnets.avax.network", 43114, "Avalanche C-Chain", None),
    ("explorer.optimism.io", 10, "OP Mainnet", "blockscout"),
    ("unichain.blockscout.com", 130, "Unichain", "blockscout"),
    ("explorer.mantle.xyz", 5000, "Mantle", "blockscout"),
    ("seitrace.com", 1329, "Sei", None),
    ("robinhoodchain.blockscout.com", 4663, "Robinhood Chain", "blockscout"),
    ("beratrail.io", 80094, "Berachain", None),
    ("purrsec.com", 999, "HyperEVM", None),
    ("rootstock.blockscout.com", 30, "Rootstock", "blockscout"),
    ("scrollscan.com", 534352, "Scroll", "blockscout"),
    ("pacific-explorer.manta.network", 169, "Manta Pacific", "blockscout"),
    ("blockscout.com/xdai/mainnet", 100, "Gnosis", "blockscout"),
    ("explorer.zksync.io", 324, "zkSync Era", "zksync"),
    ("explorer.lyra.finance", 957, "Derive", "blockscout"),
    ("oklink.com/x-layer", 196, "X Layer", None),
    ("oklink.com/xlayer", 196, "X Layer", None),
    ("oklink.com/fantom", 250, "Fantom", None),
    ("explorer.inkonchain.com", 57073, "Ink", "blockscout"),
    ("moonscan.io", 1284, "Moonbeam", None),
    ("moonbeam.moonscan.io", 1284, "Moonbeam", None),
    ("ftmscan.com", 250, "Fantom", None),
    ("explore.tempo.xyz", 4217, "Tempo", None),
    ("explore.mainnet.tempo.xyz", 4217, "Tempo", None),
    ("hecoinfo.com", 128, "HECO", None),
    ("explorer.intuition.systems", 1155, "Intuition", "blockscout"),
    ("explorer.orderly.network", 291, "Orderly", "blockscout"),
    ("explorer.gobob.xyz", 60808, "BOB", "blockscout"),
    ("explorer.plume.org", 98866, "Plume", "blockscout"),
    ("explorer.tac.build", 239, "TAC", "blockscout"),
    ("explorer.hemi.xyz", 43111, "Hemi", "blockscout"),
    ("explorer.immutable.com", 13371, "Immutable zkEVM", "blockscout"),
    ("explorer.morphl2.io", 2818, "Morph", None),
    ("explorer.morph.network", 2818, "Morph", None),
    ("zkevm.polygonscan.com", 1101, "Polygon zkEVM", None),
    ("megaeth.blockscout.com", 4326, "MegaETH", "blockscout"),
    ("flarescan.com", 14, "Flare", None),
    ("cornscan.io", 21000000, "Corn", None),
    ("evm.confluxscan.net", 1030, "Conflux eSpace", None),
    ("bobascan.com", 288, "Boba", None),
    ("blockexplorer.boba.network", 288, "Boba", None),
    ("explorer.zora.energy", 7777777, "Zora", "blockscout"),
    ("explorer.zero.network", 543210, "ZERO", "blockscout"),
    ("explorer.mainnet.aurora.dev", 1313161554, "Aurora", "blockscout"),
    ("aurorascan.dev", 1313161554, "Aurora", "compat"),
]
# Keyless public JSON-RPC endpoints for chains QuickNode does not serve, each checked with
# eth_getCode. Used for latest-state reads only.
PUBLIC_RPCS: dict[int, tuple[str, ...]] = {
    30: ("https://public-node.rsk.co",),
    169: ("https://pacific-rpc.manta.network/http",),
    239: ("https://rpc.tac.build",),
    250: ("https://rpcapi.fantom.network",),
    288: ("https://mainnet.boba.network",),
    291: ("https://rpc.orderly.network",),
    957: ("https://rpc.derive.xyz",),
    1030: ("https://evm.confluxrpc.com",),
    1101: ("https://zkevm-rpc.com",),
    1155: ("https://rpc.intuition.systems",),
    1284: ("https://moonbeam.drpc.org",),
    13371: ("https://rpc.immutable.com",),
    60808: ("https://rpc.gobob.xyz",),
    98866: ("https://rpc.plume.org",),
    7777777: ("https://rpc.zora.energy",),
    1313161554: ("https://mainnet.aurora.dev",),
}
NON_EVM_HOSTS = {
    "explorer.hiro.so", "starkscan.co", "voyager.online", "suiscan.xyz", "explorer.iota.org",
    "aptoscan.com", "explorer.aptoslabs.com", "hashscan.io",
}
CHAIN_ALIASES = {
    "eth": 1, "ethereum": 1, "mainnet": 1, "arb": 42161, "arbitrum": 42161, "base": 8453,
    "op": 10, "optimism": 10, "polygon": 137, "matic": 137, "bsc": 56, "bnb": 56,
    "gnosis": 100, "xdai": 100, "avax": 43114, "avalanche": 43114, "sonic": 146, "linea": 59144,
    "celo": 42220, "scroll": 534352, "zksync": 324, "mantle": 5000, "blast": 81457,
    "hyperevm": 999, "berachain": 80094, "sei": 1329, "sepolia": 11155111, "hoodi": 560048,
}
_ADDRESS_IN_TEXT_RE = re.compile(r"0x[0-9a-fA-F]{40}")
_LOCALE_RE = re.compile(r"^[a-z]{2}(-[a-z]{2,4})?/")
_TESTNET_RE = re.compile(r"testnet|sepolia|hoodi|fuji|amoy|bokuto|devnet")


class ChainError(FetchError):
    """A chain, explorer or node could not answer."""


class RateLimited(ChainError):
    """Still throttled after retrying: transient, so never cached as a result."""


def _is_rate_limited(exc: FetchError | None = None, message: str = "") -> bool:
    return (exc is not None and exc.status == 429) or "too many requests" in message.lower()


def host_of(url: str) -> str:
    return (urlparse(url).netloc or url).lower().removeprefix("www.")


def first_address(text: str) -> str | None:
    m = _ADDRESS_IN_TEXT_RE.search(text)
    return abi.checksum_address(m.group(0)) if m else None


@dataclass(frozen=True)
class Chain:
    id: int
    name: str
    explorer: str = ""  # base URL used for links
    etherscan: bool = False  # Etherscan v2 serves verified source for it
    etherscan_live: bool = False  # ...and live reads on a free key
    apis: tuple[str, ...] = ()  # Etherscan-compatible explorer APIs (no key)
    explorer_rpcs: tuple[str, ...] = ()  # explorer JSON-RPC endpoints (latest state only)
    public_rpcs: tuple[str, ...] = ()  # the chain's public JSON-RPC (latest state only)

    def address_url(self, address: str) -> str:
        if not self.explorer:  # chain known only by id: Etherscan's cross-chain search
            return f"https://blockscan.com/address/{address}"
        return f"{self.explorer.rstrip('/')}/address/{address}"

    @property
    def source_providers(self) -> list[str]:
        out = ["etherscan"] if self.etherscan else []
        out += [_api_label(a) for a in self.apis]
        return out + ["sourcify"]


def _api_label(url: str) -> str:
    host = host_of(url)
    if "routescan" in host:
        return "routescan"
    if "zksync" in host:
        return "zksync"
    return host


def _flavour_endpoints(key: str, chain_id: int, flavour: str | None) -> tuple[tuple[str, ...], tuple[str, ...]]:
    base = f"https://{key}"
    if flavour == "blockscout":
        return (f"{base}/api",), (f"{base}/api/eth-rpc",)
    if flavour == "compat":
        return (f"{base}/api",), ()
    if flavour in ("routescan", "routescan-testnet"):
        net = "testnet" if flavour.endswith("testnet") else "mainnet"
        return (f"https://api.routescan.io/v2/network/{net}/evm/{chain_id}/etherscan/api",), ()
    if flavour == "zksync":
        return ("https://block-explorer-api.mainnet.zksync.io/api",), ()
    return (), ()


class Registry:
    """Chains by id, and explorer links (host or host/path prefix) to chains."""

    def __init__(self, etherscan_rows: list[dict]):
        self.by_id: dict[int, Chain] = {}
        self.links: dict[str, int] = {}
        for r in etherscan_rows:
            chain = Chain(
                int(r["chainid"]), r["chainname"], r["blockexplorer"], etherscan=True,
                etherscan_live="Free" in (r.get("comment") or ""),
            )
            self.by_id[chain.id] = chain
            self.links[host_of(chain.explorer)] = chain.id
        for key, chain_id, name, flavour in EXPLORERS:
            apis, rpcs = _flavour_endpoints(key, chain_id, flavour)
            existing = self.by_id.get(chain_id)
            if existing:
                self.by_id[chain_id] = replace(
                    existing, apis=existing.apis + apis, explorer_rpcs=existing.explorer_rpcs + rpcs
                )
            else:
                self.by_id[chain_id] = Chain(chain_id, name, f"https://{key}", apis=apis, explorer_rpcs=rpcs)
            self.links[key] = chain_id
        for chain_id, urls in PUBLIC_RPCS.items():
            if chain_id in self.by_id:
                self.by_id[chain_id] = replace(self.by_id[chain_id], public_rpcs=urls)

    def get(self, chain_id: int) -> Chain:
        return self.by_id.get(chain_id) or Chain(chain_id, f"chain {chain_id}")

    def for_url(self, url: str) -> Chain | None:
        parsed = urlparse(url if "://" in url else f"https://{url}")
        host = host_of(parsed.geturl())
        paths = [parsed.path.strip("/")]
        if _LOCALE_RE.match(paths[0]):  # oklink.com/fr/fantom/...
            paths.append(paths[0].split("/", 1)[1])
        keys = [f"{host}/{key}" for p in paths for key in ("/".join(p.split("/")[:2]), p.split("/")[0])]
        for key in keys + [host]:
            if key in self.links:
                return self.by_id[self.links[key]]
        return None

    def resolve(self, value: str | int | None) -> Chain:
        if value is None:
            return self.get(1)
        text = str(value).strip().lower()
        if text.isdigit():
            return self.get(int(text))
        if text in CHAIN_ALIASES:
            return self.get(CHAIN_ALIASES[text])
        matches = [c for c in self.by_id.values() if text in c.name.lower()]
        mainnets = [c for c in matches if not _TESTNET_RE.search(c.name.lower())]
        matches = mainnets or matches
        if len(matches) == 1:
            return matches[0]
        if matches:
            raise ValueError(f"chain {value!r} is ambiguous: " + ", ".join(f"{c.name} ({c.id})" for c in matches))
        raise ValueError(f"unknown chain {value!r}; use a chain id or see `bb contract chains`")


# --- verified source providers ----------------------------------------------------------------------


class _Throttle:
    def __init__(self, interval: float):
        self.interval = interval
        self.last = 0.0

    def wait(self) -> None:
        delay = self.last + self.interval - time.monotonic()
        if delay > 0:
            time.sleep(delay)
        self.last = time.monotonic()


class EtherscanApi:
    """Etherscan v2 transport: key handling, rate limiting and redaction."""

    def __init__(self, api_key: str | None = None):
        self.api_key = api_key if api_key is not None else os.environ.get("ETHERSCAN_API_KEY")
        self.throttle = _Throttle(ETHERSCAN_INTERVAL_SECONDS)

    def request(self, chain: Chain, params: dict[str, Any]) -> dict:
        if not self.api_key:
            raise ChainError(
                "ETHERSCAN_API_KEY is not set: create a free key at https://etherscan.io/myapikey "
                "and export it (or pass --etherscan-key)"
            )
        query = urlencode({"chainid": chain.id, **params, "apikey": self.api_key})
        for attempt in range(RATE_LIMIT_RETRIES):
            self.throttle.wait()
            try:
                data = json.loads(http_request(f"{ETHERSCAN_API}?{query}", {"Accept": "application/json"}))
            except FetchError as exc:
                raise ChainError(str(exc).replace(self.api_key, "***")) from None
            except json.JSONDecodeError:
                raise ChainError("Etherscan returned a non-JSON response") from None
            message = str(data.get("result") if data.get("status") == "0" else "")
            if "rate limit" in message.lower() and attempt + 1 < RATE_LIMIT_RETRIES:
                time.sleep(1.0 + attempt)
                continue
            if "Free API access is not supported" in message:
                raise ChainError(f"live reads on {chain.name} need a paid Etherscan plan or a node (--rpc)")
            return data
        raise ChainError("Etherscan rate limit reached; try again shortly")

    def result(self, chain: Chain, params: dict[str, Any]) -> Any:
        data = self.request(chain, params)
        if data.get("status") == "0":
            raise ChainError(f"Etherscan: {data.get('result') or data.get('message')}")
        return data.get("result")


class CompatApi:
    """An explorer's own keyless Etherscan-compatible API (Blockscout, Routescan, zkSync, ...)."""

    def __init__(self, base: str):
        self.base = base
        self.label = _api_label(base)
        self.throttle = _Throttle(OPEN_API_INTERVAL_SECONDS)
        self.exhausted = False  # rate limited after retrying: skip it for the rest of the run

    def result(self, params: dict[str, Any]) -> Any:
        if self.exhausted:
            raise RateLimited(f"{self.label}: rate limited; try again later")
        for attempt in range(COMPAT_RATE_LIMIT_RETRIES):
            self.throttle.wait()
            try:
                data = json.loads(http_request(f"{self.base}?{urlencode(params)}", {"Accept": "application/json"}))
            except FetchError as exc:
                if not _is_rate_limited(exc):
                    raise ChainError(f"{self.label}: {exc}") from None
                data = {"status": "0", "message": "Too many requests"}
            except json.JSONDecodeError:
                raise ChainError(f"{self.label}: non-JSON response") from None
            if not isinstance(data, dict):
                raise ChainError(f"{self.label}: {data}")
            message = str(data.get("result") or data.get("message") or "")
            if str(data.get("status")) != "0":
                return data.get("result")
            if not _is_rate_limited(message=message):
                raise ChainError(f"{self.label}: {message}")
            if attempt + 1 < COMPAT_RATE_LIMIT_RETRIES:
                time.sleep(1.0 + 2 * attempt)
        self.exhausted = True
        raise RateLimited(f"{self.label}: rate limited; try again later")


def _truthy(value: Any) -> bool:
    return str(value).lower() in ("1", "true")


def normalize(raw: dict) -> dict:
    """Bring Blockscout/zkSync dialects of ``getsourcecode`` onto Etherscan's field names."""
    out = dict(raw)
    if "IsProxy" in raw or "ImplementationAddress" in raw:
        out["Proxy"] = "1" if _truthy(raw.get("IsProxy")) else "0"
        out["Implementation"] = raw.get("ImplementationAddress") or ""
    if "OptimizationUsed" in raw:
        out["OptimizationUsed"] = "1" if _truthy(raw["OptimizationUsed"]) else "0"
    if not out.get("Runs") and raw.get("OptimizationRuns") is not None:
        out["Runs"] = str(raw["OptimizationRuns"])
    if ":" in (out.get("ContractName") or ""):  # zkSync: "src/Foo.sol:Foo"
        out["ContractName"] = out["ContractName"].rsplit(":", 1)[1]
    extra = raw.get("AdditionalSources") or []
    code = raw.get("SourceCode") or ""
    if extra and code and not code.startswith("{"):  # Blockscout: main file + additional files
        sources = {raw.get("FileName") or f"{out.get('ContractName') or 'Contract'}.sol": {"content": code}}
        sources.update({s.get("Filename"): {"content": s.get("SourceCode", "")} for s in extra if s.get("Filename")})
        std = {"language": "Solidity", "sources": sources, "settings": raw.get("CompilerSettings") or {}}
        out["SourceCode"] = "{" + json.dumps(std) + "}"
        out.pop("AdditionalSources", None)
    return out


def from_sourcify(doc: dict) -> dict:
    """Map a Sourcify v2 contract document onto Etherscan's getsourcecode shape."""
    comp = doc.get("compilation") or {}
    std = doc.get("stdJsonInput") or {}
    settings = std.get("settings") or comp.get("compilerSettings") or {}
    optimizer = settings.get("optimizer") or {}
    proxy = doc.get("proxyResolution") or {}
    impls = [i.get("address") for i in proxy.get("implementations") or [] if i.get("address")]
    return {
        "SourceCode": "{" + json.dumps(std) + "}" if std else "",
        "ABI": json.dumps(doc.get("abi") or []),
        "ContractName": comp.get("name") or "",
        "CompilerVersion": comp.get("compilerVersion") or "",
        "OptimizationUsed": "1" if optimizer.get("enabled") else "0",
        "Runs": str(optimizer.get("runs") or ""),
        "EVMVersion": settings.get("evmVersion") or "",
        "LicenseType": "",
        "Proxy": "1" if proxy.get("isProxy") and impls else "0",
        "Implementation": impls[0] if impls else "",
        "Match": doc.get("match") or "",
    }


class Sourcify:
    def __init__(self):
        self.throttle = _Throttle(OPEN_API_INTERVAL_SECONDS)

    def source(self, chain: Chain, address: str) -> dict | None:
        self.throttle.wait()
        fields = "stdJsonInput,abi,compilation,proxyResolution,deployment"
        try:
            body = http_request(f"{SOURCIFY_API}/{chain.id}/{address}?fields={fields}", {"Accept": "application/json"})
        except NotFoundError:
            return None
        except FetchError as exc:
            if exc.status == 400:  # chain not supported by Sourcify
                return None
            raise ChainError(f"sourcify: {exc}") from None
        doc = json.loads(body)
        raw = from_sourcify(doc)
        deployment = doc.get("deployment") or {}
        raw["_creation"] = {
            "contractCreator": deployment.get("deployer"),
            "txHash": deployment.get("transactionHash"),
            "blockNumber": deployment.get("blockNumber"),
            "timestamp": None,
        } if deployment.get("transactionHash") else None
        return raw


# --- contract metadata ------------------------------------------------------------------------------


@dataclass
class Contract:
    """Verified-source metadata in Etherscan's ``getsourcecode`` shape."""

    chain: Chain
    address: str
    raw: dict = field(default_factory=dict)

    @property
    def provider(self) -> str:
        return self.raw.get("_provider") or ""

    @property
    def verified(self) -> bool:
        return bool(self.raw.get("SourceCode")) and self.raw.get("ABI") != UNVERIFIED

    @property
    def name(self) -> str:
        return self.raw.get("ContractName") or ""

    @property
    def implementation(self) -> str | None:
        impl = self.raw.get("Implementation") or ""
        return abi.checksum_address(impl) if self.raw.get("Proxy") == "1" and abi.is_address(impl) else None

    @property
    def abi(self) -> list[dict]:
        try:
            return json.loads(self.raw.get("ABI") or "[]") if self.verified else []
        except json.JSONDecodeError:
            return []

    @property
    def functions(self) -> list[abi.Function]:
        return abi.functions_from_abi(self.abi)

    def source_files(self) -> dict[str, str]:
        """Verified source as {relative path: content}, whatever format the explorer stored."""
        code = self.raw.get("SourceCode") or ""
        if not code:
            return {}
        doc: Any = None
        if code.startswith("{{") and code.endswith("}}"):
            doc = json.loads(code[1:-1])  # standard-json input
        elif code.startswith("{"):
            try:
                doc = json.loads(code)  # bare {path: {content}} map
            except json.JSONDecodeError:
                doc = None
        if isinstance(doc, dict):
            sources = doc.get("sources", doc)
            return {
                _safe_path(path): body.get("content", "")
                for path, body in sources.items()
                if isinstance(body, dict)
            }
        ext = ".vy" if "vyper" in (self.raw.get("CompilerVersion") or "").lower() else ".sol"
        return {_safe_path(f"{self.name or 'Contract'}{ext}"): code}

    def compiler_settings(self) -> dict:
        code = self.raw.get("SourceCode") or ""
        if code.startswith("{{"):
            try:
                return json.loads(code[1:-1]).get("settings", {})
            except json.JSONDecodeError:
                return {}
        return {}


def _safe_path(path: str) -> str:
    """Keep a source path inside the output directory."""
    parts = [p for p in PurePosixPath(path.replace("\\", "/")).parts if p not in ("", "/", ".", "..")]
    return "/".join(parts) or "Contract.sol"
