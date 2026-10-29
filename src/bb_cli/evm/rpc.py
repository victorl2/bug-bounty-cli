"""Live chain reads over JSON-RPC: your node, your QuickNode endpoint, or an explorer's proxy.

Only real nodes (``--rpc URL`` or QuickNode) are trusted for historical reads. Etherscan's proxy
answers a past block with the latest state, and explorer RPCs are inconsistent about it, so both
are limited to ``latest``.

QuickNode is discovered from the ``qn`` CLI: the account's multichain endpoint is read with
``qn endpoint list`` on each run (its token is kept in memory only) and the per-network hosts come
from ``~/.config/qn/networks.toml``. Which chain id each network serves is probed once and cached.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

from bb_cli.evm import abi
from bb_cli.evm.client import Client, FetchError, http_request
from bb_cli.evm.explorers import Chain, ChainError, EtherscanApi, RateLimited

QN_NETWORKS_FILE = Path("~/.config/qn/networks.toml").expanduser()
QN_PROBE_WORKERS = 8
SMALL_BATCH = 5  # Blockscout's limit; larger batches get HTTP 413
POST_ATTEMPTS = 4
_TOKEN_RE = re.compile(r"^[0-9a-f]{20,}$")


class CallReverted(ChainError):
    pass


class BatchTooLarge(ChainError):
    pass


def block_tag(block: int | str | None) -> str:
    if block is None or block == "latest":
        return "latest"
    if isinstance(block, str):
        block = abi.parse_int(block)
    return hex(block)


def revert_reason(data: bytes) -> str:
    if data[:4] == bytes.fromhex("08c379a0"):
        try:
            return abi.decode([abi.Basic("string")], data[4:])[0]
        except abi.AbiError:
            pass
    if data[:4] == bytes.fromhex("4e487b71") and len(data) >= 36:
        return f"panic 0x{int.from_bytes(data[4:36], 'big'):02x}"
    return "0x" + data.hex() if data else "no reason"


def _raise_rpc_error(method: str, err: dict, label: str) -> None:
    raw = err.get("data") or ""
    message = str(err.get("message", ""))
    if "revert" in message.lower():
        reason = revert_reason(bytes.fromhex(raw.removeprefix("0x"))) if isinstance(raw, str) else str(raw)
        raise CallReverted(f"execution reverted: {reason}")
    raise ChainError(f"{label}: {method} failed: {message[:200]}")


class JsonRpc:
    """A JSON-RPC endpoint. ``label`` is what errors show; the URL may embed a secret."""

    def __init__(self, url: str, label: str, archive: bool = True):
        self.url = url
        self.label = label
        self.archive = archive

    def _post(self, payload: Any) -> Any:
        """POST with retries for throttling (429) and dropped connections; other HTTP errors are final."""
        body = json.dumps(payload).encode()
        for attempt in range(POST_ATTEMPTS):
            try:
                return json.loads(http_request(self.url, {"Content-Type": "application/json"}, body))
            except FetchError as exc:
                if exc.status == 413:
                    raise BatchTooLarge(f"{self.label} rejected the batch size") from None
                retry = exc.status == 429 or (exc.status is None and attempt == 0)
                if not retry or attempt + 1 == POST_ATTEMPTS:
                    error = RateLimited if exc.status == 429 else ChainError
                    raise error(f"{self.label} did not answer" + (f" (HTTP {exc.status})" if exc.status else "")) from None
            except json.JSONDecodeError:
                raise ChainError(f"{self.label} returned a non-JSON response") from None
            time.sleep(1.0 + 2 * attempt)
        raise AssertionError("unreachable")

    def request(self, method: str, params: list[Any]) -> Any:
        data = self._post({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
        if not isinstance(data, dict):
            raise ChainError(f"{self.label}: unexpected response to {method}")
        if err := data.get("error"):
            _raise_rpc_error(method, err if isinstance(err, dict) else {"message": str(err)}, self.label)
        return data.get("result")

    def batch(self, calls: list[tuple[str, list[Any]]]) -> list[Any]:
        """One HTTP request for many calls; failed entries come back as ChainError instances."""
        if not calls:
            return []
        try:
            data = self._post([{"jsonrpc": "2.0", "id": i, "method": m, "params": p} for i, (m, p) in enumerate(calls)])
        except BatchTooLarge:
            if len(calls) == 1:
                raise
            size = SMALL_BATCH if len(calls) > SMALL_BATCH else 1
            return [r for i in range(0, len(calls), size) for r in self.batch(calls[i : i + size])]
        if not isinstance(data, list):
            return [self.request(m, p) for m, p in calls]
        by_id = {d.get("id"): d for d in data if isinstance(d, dict)}
        out: list[Any] = []
        for i, (method, _) in enumerate(calls):
            item = by_id.get(i, {})
            if err := item.get("error"):
                try:
                    _raise_rpc_error(method, err, self.label)
                except ChainError as exc:
                    out.append(exc)
            else:
                out.append(item.get("result"))
        return out


class EtherscanProxy:
    """Etherscan's ``module=proxy`` (plus ``account`` for balances); latest state only."""

    archive = False

    def __init__(self, api: EtherscanApi, chain: Chain):
        self.api = api
        self.chain = chain
        self.label = f"Etherscan ({chain.name})"

    def request(self, method: str, params: list[Any]) -> Any:
        if method == "eth_getBalance":
            return hex(int(self.api.result(self.chain, {
                "module": "account", "action": "balance", "address": params[0], "tag": "latest",
            })))
        if method == "eth_call":
            query = {"to": params[0]["to"], "data": params[0]["data"], "tag": params[1]}
        elif method == "eth_getStorageAt":
            query = {"address": params[0], "position": params[1], "tag": params[2]}
        else:
            query = {"address": params[0], "tag": params[1]}
        data = self.api.request(self.chain, {"module": "proxy", "action": method, **query})
        if data.get("status") == "0":
            raise ChainError(f"Etherscan: {data.get('result')}")
        if err := data.get("error"):
            _raise_rpc_error(method, err, self.label)
        return data.get("result")

    def batch(self, calls: list[tuple[str, list[Any]]]) -> list[Any]:
        out: list[Any] = []
        for method, params in calls:
            try:
                out.append(self.request(method, params))
            except ChainError as exc:
                out.append(exc)
        return out


# --- QuickNode ------------------------------------------------------------------------------------


def _read_networks_file(path: Path) -> dict[str, str]:
    """``[entry.networks]`` key = "url" pairs; a tiny subset of TOML (no tomllib on 3.10)."""
    networks, in_section = {}, False
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line.startswith("["):
            in_section = line == "[entry.networks]"
        elif in_section and (m := re.match(r'^"?([A-Za-z0-9_.-]+)"?\s*=\s*"([^"]+)"', line)):
            networks[m.group(1)] = m.group(2)
    return networks


def _multichain_endpoint() -> str | None:
    """HTTP URL of the account's multichain endpoint: $CANTINA_QUICKNODE_URL or `qn endpoint list`."""
    if env := (os.environ.get("BB_QUICKNODE_URL") or os.environ.get("IMMUNEFI_QUICKNODE_URL") or os.environ.get("CANTINA_QUICKNODE_URL")):
        return env
    if not shutil.which("qn"):
        return None
    try:
        proc = subprocess.run(["qn", "endpoint", "list", "-o", "json"], capture_output=True, text=True, timeout=30)
        data = json.loads(proc.stdout)
    except (OSError, subprocess.SubprocessError, json.JSONDecodeError):
        return None
    items = data if isinstance(data, list) else data.get("data") or data.get("endpoints") or []
    for ep in items:
        if ep.get("is_multichain") and ep.get("status", "active") == "active" and ep.get("http_url"):
            return ep["http_url"]
    return None


class QuickNode:
    """Per-chain JSON-RPC URLs built from one multichain endpoint."""

    def __init__(self, client: Client, endpoint_url: str, networks: dict[str, str]):
        u = urlparse(endpoint_url)
        self.subdomain = u.netloc.split(".")[0]
        tokens = [p for p in u.path.split("/") if _TOKEN_RE.match(p)]
        self.token = tokens[0] if tokens else ""
        self.client = client
        self.networks = networks

    @classmethod
    def discover(cls, client: Client) -> "QuickNode | None":
        if not QN_NETWORKS_FILE.is_file():
            return None
        endpoint = _multichain_endpoint()
        if not endpoint:
            return None
        node = cls(client, endpoint, _read_networks_file(QN_NETWORKS_FILE))
        return node if node.token and node.networks else None

    def url_for_network(self, key: str) -> str:
        tooling = urlparse(self.networks[key])
        host = f"{self.subdomain}.{tooling.netloc.split('.', 1)[1]}"
        return f"https://{host}/{self.token}{tooling.path}"

    def chain_ids(self) -> dict[int, str]:
        """{chain id: network key} for every EVM network; probed once per cache TTL."""

        def probe(key: str) -> tuple[str, int | None]:
            try:
                result = JsonRpc(self.url_for_network(key), f"QuickNode {key}").request("eth_chainId", [])
            except ChainError:
                return key, None
            ok = isinstance(result, str) and result.startswith("0x")
            return key, int(result, 16) if ok else None

        def fetch(_: str) -> dict[str, int]:
            with ThreadPoolExecutor(QN_PROBE_WORKERS) as pool:
                return {k: cid for k, cid in pool.map(probe, sorted(self.networks)) if cid is not None}

        # The cache stores network names and chain ids only - never the endpoint URL or token.
        mapping = self.client.cached("quicknode/chains", "quicknode:networks", fetch).data
        return {cid: key for key, cid in mapping.items()}

    def for_chain(self, chain_id: int) -> JsonRpc | None:
        key = self.chain_ids().get(chain_id)
        return JsonRpc(self.url_for_network(key), f"QuickNode {key}") if key else None

