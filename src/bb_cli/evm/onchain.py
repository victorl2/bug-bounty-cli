"""Chooses the right explorer and node per chain, and caches what is safe to cache.

Verified source, ABI and creation info are cached like opportunity data (10h+). Balances, calls
and storage are live state: never cached, refused with ``--offline``.
"""

from __future__ import annotations

import json
from typing import Any

from bb_cli.evm import abi
from bb_cli.evm.client import Client, FetchError, http_request
from bb_cli.evm.explorers import (
    ETHERSCAN_CHAINLIST,
    Chain,
    ChainError,
    CompatApi,
    Contract,
    EtherscanApi,
    RateLimited,
    Registry,
    Sourcify,
    first_address,
    host_of,
    normalize,
)
from bb_cli.evm.rpc import CallReverted, EtherscanProxy, JsonRpc, QuickNode, block_tag

MULTICALL3 = "0xcA11bde05977b3631167028862bE2a173976CA11"
MULTICALL_BATCH = 40
CODE_BATCH = 50
SLOTS = {
    "implementation": 0x360894A13BA1A3210667C828492DB98DCA3E2076CC3735A920A3CA505D382BBC,
    "admin": 0xB53127684A568B3173AE13B9F8A6016E243E63B6E8EE1178D6A717850B5D6103,
    "beacon": 0xA3F0AD74E5423AEBFD80D3EF4346578335A9A72AEAEE59FF6CB3582B35133D50,
}
RPC_MODES = ("auto", "quicknode", "public", "explorer", "etherscan")


class OnChain:
    """Contract metadata from explorers, live state from nodes.

    ``rpc`` is a node URL, or one of: ``auto`` (QuickNode when the ``qn`` CLI is set up, then the
    chain's public RPC, its explorer's RPC, then Etherscan), ``quicknode``, ``public``,
    ``explorer`` or ``etherscan``.
    """

    def __init__(self, client: Client, etherscan_key: str | None = None, rpc: str | None = "auto"):
        self.client = client
        self.etherscan = EtherscanApi(etherscan_key)
        self.sourcify = Sourcify()
        self.rpc_mode = rpc or "auto"
        self._registry: Registry | None = None
        self._quicknode: QuickNode | None | bool = False  # False = not looked up yet
        self._apis: dict[str, CompatApi] = {}

    # --- chains ---------------------------------------------------------------------------------

    @property
    def registry(self) -> Registry:
        if self._registry is None:
            def fetch(url: str) -> list[dict]:
                data = json.loads(http_request(url, {"Accept": "application/json"}))
                return data.get("result", data)

            rows = self.client.cached("etherscan/chainlist", ETHERSCAN_CHAINLIST, fetch).data
            self._registry = Registry(rows)
        return self._registry

    def resolve_chain(self, value: str | int | None) -> Chain:
        return self.registry.resolve(value)

    def chain_for_url(self, url: str) -> Chain | None:
        return self.registry.for_url(url)

    def parse_target(self, text: str, chain: str | None = None) -> tuple[Chain, str]:
        """Accept an address or an explorer URL (which also selects the chain)."""
        address = first_address(text)
        if not address:
            raise ValueError(f"no contract address in {text!r}")
        if chain is None and "/" in text:
            found = self.chain_for_url(text)
            if found is None:
                raise ValueError(f"{host_of(text if '://' in text else 'https://' + text)} is not a known explorer; pass --chain")
            return found, address
        return self.resolve_chain(chain), address

    # --- verified source (cached) ---------------------------------------------------------------

    def _api(self, base: str) -> CompatApi:
        return self._apis.setdefault(base, CompatApi(base))

    def _lookup_source(self, chain: Chain, address: str) -> dict:
        """First verified hit across Etherscan, the explorer's own API, then Sourcify.

        A failure of Etherscan or Sourcify raises instead of returning "unverified", so a
        transient error is never cached as a 10-hour false negative. The per-explorer APIs are
        best effort: several are permanently down, and Sourcify backs them up. One that is only
        rate limiting us raises too (after Sourcify had its chance), for the same reason.
        """
        unverified: dict | None = None
        throttled: RateLimited | None = None
        if chain.etherscan and self.etherscan.api_key:
            rows = self.etherscan.result(chain, {"module": "contract", "action": "getsourcecode", "address": address})
            raw = {**normalize(rows[0] if rows else {}), "_provider": "etherscan"}
            if Contract(chain, address, raw).verified:
                return raw
            unverified = raw
        for base in chain.apis:
            api = self._api(base)
            try:
                rows = api.result({"module": "contract", "action": "getsourcecode", "address": address})
            except RateLimited as exc:
                throttled = exc
                continue
            except ChainError:
                continue
            raw = {**normalize(rows[0] if isinstance(rows, list) and rows else {}), "_provider": api.label}
            if Contract(chain, address, raw).verified:
                return raw
            unverified = unverified or raw
        if raw := self.sourcify.source(chain, address):
            return {**raw, "_provider": "sourcify"}
        if throttled:
            raise throttled
        return unverified or {"_provider": "none", "ABI": "Contract source code not verified"}

    def contract(self, chain: Chain, address: str) -> Contract:
        source = f"{chain.address_url(address)}#code"
        entry = self.client.cached(
            f"contracts/{chain.id}/{address.lower()}", source, lambda _: self._lookup_source(chain, address)
        )
        return Contract(chain, address, entry.data)

    def creation(self, chain: Chain, address: str) -> dict | None:
        def fetch(_: str) -> dict:
            params = {"module": "contract", "action": "getcontractcreation", "contractaddresses": address}
            attempts = []
            if chain.etherscan and self.etherscan.api_key:
                attempts.append(lambda: self.etherscan.result(chain, params))
            attempts += [lambda base=base: self._api(base).result(params) for base in chain.apis]
            for attempt in attempts:
                try:
                    rows = attempt()
                except ChainError:
                    continue
                if rows:
                    row = rows[0]
                    return {k: row.get(k) for k in ("contractCreator", "txHash", "blockNumber", "timestamp", "contractFactory")}
            return self.contract(chain, address).raw.get("_creation") or {}

        source = f"{chain.address_url(address)}#creation"
        return self.client.cached(f"contracts/{chain.id}/{address.lower()}.creation", source, fetch).data or None

    # --- nodes ----------------------------------------------------------------------------------

    @property
    def quicknode(self) -> QuickNode | None:
        if self._quicknode is False:
            self._quicknode = QuickNode.discover(self.client)
        return self._quicknode  # type: ignore[return-value]

    def node(self, chain: Chain, block: int | str | None = None) -> Any:
        """The JSON-RPC provider for live reads on ``chain``."""
        if self.client.offline:
            raise ChainError("live chain reads are not available with --offline")
        mode = self.rpc_mode
        if mode not in RPC_MODES:
            return JsonRpc(mode, "--rpc endpoint")
        candidates: list[Any] = []
        if mode in ("auto", "quicknode") and self.quicknode:
            if qn := self.quicknode.for_chain(chain.id):
                candidates.append(qn)
        if mode == "quicknode" and not candidates:
            raise ChainError(f"QuickNode has no network for {chain.name} ({chain.id})" if self.quicknode
                             else "QuickNode not found: install and log in to the `qn` CLI, or set BB_QUICKNODE_URL")
        if mode in ("auto", "public"):
            candidates += [JsonRpc(url, f"{host_of(url)} RPC", archive=False) for url in chain.public_rpcs]
        if mode in ("auto", "explorer"):
            candidates += [JsonRpc(url, f"{host_of(url)} RPC", archive=False) for url in chain.explorer_rpcs]
        if mode in ("auto", "etherscan") and chain.etherscan and self.etherscan.api_key:
            candidates.append(EtherscanProxy(self.etherscan, chain))
        if not candidates:
            raise ChainError(f"no node for {chain.name} ({chain.id}); pass --rpc <url>")
        if block_tag(block) != "latest":
            archive = [c for c in candidates if c.archive]
            if not archive:
                raise ChainError(
                    f"--block needs a real node for {chain.name}: QuickNode or --rpc <archive URL> "
                    f"({candidates[0].label} only serves the latest state)"
                )
            return archive[0]
        return candidates[0]

    def _rpc(self, chain: Chain, method: str, params: list[Any], block: int | str | None) -> Any:
        return self.node(chain, block).request(method, params)

    def balance(self, chain: Chain, address: str, block: int | str | None = None) -> int:
        return int(self._rpc(chain, "eth_getBalance", [address, block_tag(block)], block), 16)

    def code_size(self, chain: Chain, address: str, block: int | str | None = None) -> int:
        code = self._rpc(chain, "eth_getCode", [address, block_tag(block)], block) or "0x"
        return (len(code) - 2) // 2

    def code_sizes(self, chain: Chain, addresses: list[str], block: int | str | None = None) -> dict[str, int | ChainError]:
        """Deployed code size for many addresses, batched into few requests."""
        node = self.node(chain, block)
        out: dict[str, int | ChainError] = {}
        for i in range(0, len(addresses), CODE_BATCH):
            chunk = addresses[i : i + CODE_BATCH]
            for address, code in zip(chunk, node.batch([("eth_getCode", [a, block_tag(block)]) for a in chunk])):
                out[address] = code if isinstance(code, ChainError) else (len(code or "0x") - 2) // 2
        return out

    def storage(self, chain: Chain, address: str, slot: int, block: int | str | None = None) -> bytes:
        raw = self._rpc(chain, "eth_getStorageAt", [address, hex(slot), block_tag(block)], block) or "0x"
        return bytes.fromhex(raw.removeprefix("0x").rjust(64, "0"))

    def call(self, chain: Chain, to: str, data: bytes, block: int | str | None = None) -> bytes:
        raw = self._rpc(chain, "eth_call", [{"to": to, "data": "0x" + data.hex()}, block_tag(block)], block) or "0x"
        return bytes.fromhex(raw.removeprefix("0x"))

    def live_implementation(self, chain: Chain, address: str, block: int | str | None = None) -> str | None:
        """Implementation address from the EIP-1967 slot, if the contract uses it."""
        word = int.from_bytes(self.storage(chain, address, SLOTS["implementation"], block), "big")
        return abi.checksum_address(f"{word:040x}") if word else None

    def multicall(
        self, chain: Chain, calls: list[tuple[str, bytes]], block: int | str | None = None
    ) -> list[tuple[bool, bytes]]:
        """Run many read calls through Multicall3 (one request per batch); falls back to one
        request per call when Multicall3 is not deployed on the chain."""
        fn = abi.parse_signature("aggregate3((address,bool,bytes)[])((bool,bytes)[])")
        out: list[tuple[bool, bytes]] = []
        for i in range(0, len(calls), MULTICALL_BATCH):
            batch = calls[i : i + MULTICALL_BATCH]
            try:
                ret = self.call(chain, MULTICALL3, fn.encode_call([[(t, True, d) for t, d in batch]]), block)
                out += [(ok, data) for ok, data in abi.decode(fn.outputs, ret)[0]]  # type: ignore[arg-type]
            except (CallReverted, abi.AbiError):
                for target, data in batch:
                    try:
                        out.append((True, self.call(chain, target, data, block)))
                    except CallReverted as exc:
                        out.append((False, str(exc).encode()))
        return out

    def functions_for(self, contract: Contract) -> tuple[list[abi.Function], Contract | None]:
        """ABI functions to use when calling ``contract``; for proxies, the implementation's."""
        impl_address = contract.implementation
        if impl_address is None and not contract.functions:
            try:
                impl_address = self.live_implementation(contract.chain, contract.address)
            except FetchError:
                impl_address = None
        if impl_address:
            impl = self.contract(contract.chain, impl_address)
            if impl.verified:
                seen = {f.signature for f in impl.functions}
                return impl.functions + [f for f in contract.functions if f.signature not in seen], impl
        return contract.functions, None
