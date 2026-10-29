import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from immunefi_cli import abi, client, explorers, onchain, rpc  # noqa: E402
from immunefi_cli.cache import Cache  # noqa: E402
from immunefi_cli.keccak import keccak256  # noqa: E402

# Reference values produced with Foundry's `cast keccak` / `cast calldata`.
TUPLE_ARRAY_CALLDATA = (
    "73942f08"
    "0000000000000000000000000000000000000000000000000000000000000040"
    "00000000000000000000000000000000000000000000000000000000000000e0"
    "0000000000000000000000000000000000000000000000000000000000000002"
    "00000000000000000000000000000000219ab540356cbb839cbe05303d7705fa"
    "0000000000000000000000000000000000000000000000000000000000000007"
    "0000000000000000000000000000000000000000000000000000000000000001"
    "0000000000000000000000000000000000000000000000000000000000000008"
    "0000000000000000000000000000000000000000000000000000000000000040"
    "0000000000000000000000000000000000000000000000000000000000000080"
    "0000000000000000000000000000000000000000000000000000000000000001"
    "6100000000000000000000000000000000000000000000000000000000000000"
    "0000000000000000000000000000000000000000000000000000000000000002"
    "6263000000000000000000000000000000000000000000000000000000000000"
)
NEG_INT_CALLDATA = (
    "f6cf013f"
    "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffb"
    "0000000000000000000000000000000000000000000000000000000000000060"
    "0000000000000000000000000000000000000000000000000000000000000001"
    "0000000000000000000000000000000000000000000000000000000000000004"
    "deadbeef00000000000000000000000000000000000000000000000000000000"
)


class KeccakTests(unittest.TestCase):
    def test_vectors(self):
        self.assertEqual(keccak256(b"").hex(), "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470")
        self.assertEqual(keccak256(b"transfer(address,uint256)")[:4].hex(), "a9059cbb")
        # 274 bytes: spans two 136-byte blocks
        self.assertEqual(keccak256(b"ab" * 137).hex(), "f5ae4363565d60c5fd94d59e528d9ab1e8f209dbf9c8c70aa0ffafa5e9f40e76")


class AbiTests(unittest.TestCase):
    def encode(self, sig, *args):
        fn = abi.parse_signature(sig)
        return fn.encode_call([abi.parse_value(t, a) for t, a in zip(fn.inputs, args)]).hex()

    def test_encoding_matches_cast(self):
        self.assertEqual(
            self.encode(
                "f((address,uint256)[],string[2])",
                "[(0x00000000219ab540356cBB839Cbe05303d7705Fa,7),(0x0000000000000000000000000000000000000001,8)]",
                '["a","bc"]',
            ),
            TUPLE_ARRAY_CALLDATA,
        )
        self.assertEqual(self.encode("f(int8,bytes,bool)", "-5", "0xdeadbeef", "true"), NEG_INT_CALLDATA)

    def test_decode_round_trip(self):
        fn = abi.parse_signature("f((address,uint256)[],string[2])")
        values = abi.decode(fn.inputs, bytes.fromhex(TUPLE_ARRAY_CALLDATA[8:]))
        self.assertEqual(values[0][0], ["0x00000000219ab540356cBB839Cbe05303d7705Fa", 7])
        self.assertEqual(values[1], ["a", "bc"])
        self.assertEqual(fn.encode_call(values).hex(), TUPLE_ARRAY_CALLDATA)
        neg = abi.parse_signature("f(int8,bytes,bool)")
        self.assertEqual(abi.decode(neg.inputs, bytes.fromhex(NEG_INT_CALLDATA[8:])), [-5, b"\xde\xad\xbe\xef", True])

    def test_decode_rejects_truncated_data(self):
        with self.assertRaises(abi.AbiError):
            abi.decode([abi.parse_type("uint256[]")], (2**200).to_bytes(32, "big"))

    def test_signatures_and_json_abi(self):
        fn = abi.parse_signature("balanceOf(address)(uint256)")
        self.assertEqual(fn.selector.hex(), "70a08231")
        self.assertEqual(fn.outputs, [abi.Basic("uint", 256)])
        self.assertIsNone(abi.parse_signature("balanceOf(address)").outputs)
        [getter] = abi.functions_from_abi([{
            "type": "function", "name": "getReserves", "stateMutability": "view", "inputs": [],
            "outputs": [{"type": "tuple[]", "name": "r", "components": [
                {"type": "uint112", "name": "a"}, {"type": "address", "name": "b"},
            ]}],
        }])
        self.assertEqual(getter.outputs[0].canonical(), "(uint112,address)[]")
        self.assertTrue(getter.is_read)

    def test_cli_values(self):
        self.assertEqual(abi.parse_int("1.5ether"), 1_500_000_000_000_000_000)
        self.assertEqual(abi.parse_int("0x10"), 16)
        self.assertEqual(abi.parse_int("1e6"), 1_000_000)
        with self.assertRaises(abi.AbiError):
            abi.parse_int("1.5")
        with self.assertRaises(abi.AbiError):
            abi.parse_value(abi.Basic("address"), "0x123")
        with self.assertRaises(abi.AbiError):
            abi.encode([abi.Basic("uint", 8)], [256])

    def test_checksum_and_json(self):
        self.assertEqual(
            abi.checksum_address("0xdd9bc35ae942ef0cfa76930954a156b3ff30a4e1"),
            "0xDD9BC35aE942eF0cFa76930954a156B3fF30a4E1",
        )
        self.assertEqual(abi.to_json(abi.Basic("uint", 256), 2**60), str(2**60))
        self.assertEqual(abi.to_json(abi.Basic("uint", 256), 5), 5)


ETH = explorers.Chain(1, "Ethereum Mainnet", "https://etherscan.io/", etherscan=True, etherscan_live=True)
ADDR = "0x" + "11" * 20


def contract(raw: dict) -> explorers.Contract:
    return explorers.Contract(ETH, ADDR, raw)


class SourceTests(unittest.TestCase):
    def test_standard_json_input(self):
        doc = {"language": "Solidity", "sources": {"src/A.sol": {"content": "a"}, "../../etc/x.sol": {"content": "x"}},
               "settings": {"optimizer": {"enabled": True}}}
        c = contract({"SourceCode": "{" + json.dumps(doc) + "}", "ABI": "[]", "ContractName": "A"})
        self.assertEqual(c.source_files(), {"src/A.sol": "a", "etc/x.sol": "x"})
        self.assertEqual(c.compiler_settings(), {"optimizer": {"enabled": True}})

    def test_bare_sources_map_and_single_file(self):
        c = contract({"SourceCode": json.dumps({"/abs/B.sol": {"content": "b"}}), "ABI": "[]"})
        self.assertEqual(c.source_files(), {"abs/B.sol": "b"})
        c = contract({"SourceCode": "contract C {}", "ABI": "[]", "ContractName": "C", "CompilerVersion": "v0.8"})
        self.assertEqual(c.source_files(), {"C.sol": "contract C {}"})
        vy = contract({"SourceCode": "# vyper", "ABI": "[]", "ContractName": "V", "CompilerVersion": "vyper:0.3.10"})
        self.assertEqual(list(vy.source_files()), ["V.vy"])

    def test_unverified(self):
        c = contract({"SourceCode": "", "ABI": "Contract source code not verified", "Proxy": "0"})
        self.assertFalse(c.verified)
        self.assertEqual(c.abi, [])
        self.assertIsNone(c.implementation)

    def test_blockscout_dialect_is_normalized(self):
        raw = explorers.normalize({
            "ContractName": "src/Gov.sol:Gov", "SourceCode": "contract Main {}", "FileName": "src/Main.sol",
            "AdditionalSources": [{"Filename": "src/Lib.sol", "SourceCode": "library L {}"}],
            "IsProxy": "true", "ImplementationAddress": "0x" + "33" * 20,
            "OptimizationUsed": "true", "OptimizationRuns": 200, "CompilerSettings": {"evmVersion": "paris"},
            "ABI": "[]",
        })
        c = contract(raw)
        self.assertEqual(c.name, "Gov")
        self.assertEqual(c.source_files(), {"src/Main.sol": "contract Main {}", "src/Lib.sol": "library L {}"})
        self.assertEqual(c.implementation, abi.checksum_address("0x" + "33" * 20))
        self.assertEqual((raw["OptimizationUsed"], raw["Runs"]), ("1", "200"))
        self.assertEqual(c.compiler_settings(), {"evmVersion": "paris"})

    def test_sourcify_document_maps_to_etherscan_shape(self):
        raw = explorers.from_sourcify({
            "match": "exact_match",
            "abi": [{"type": "function", "name": "f", "inputs": [], "outputs": [], "stateMutability": "view"}],
            "compilation": {"name": "Token", "compilerVersion": "0.8.20+commit.a1b79de6"},
            "stdJsonInput": {"language": "Solidity", "sources": {"T.sol": {"content": "t"}},
                             "settings": {"optimizer": {"enabled": True, "runs": 200}, "evmVersion": "paris"}},
            "proxyResolution": {"isProxy": True, "implementations": [{"address": "0x" + "22" * 20}]},
        })
        c = contract(raw)
        self.assertTrue(c.verified)
        self.assertEqual((c.name, c.source_files(), raw["Runs"]), ("Token", {"T.sol": "t"}, "200"))
        self.assertEqual(c.implementation, abi.checksum_address("0x" + "22" * 20))
        self.assertEqual([f.name for f in c.functions], ["f"])


CHAINLIST = [
    {"chainname": "Ethereum Mainnet", "chainid": "1", "blockexplorer": "https://etherscan.io/", "comment": "Free-tier available"},
    {"chainname": "Arbitrum One Mainnet", "chainid": "42161", "blockexplorer": "https://arbiscan.io/", "comment": "Free-tier available"},
    {"chainname": "Avalanche C-Chain", "chainid": "43114", "blockexplorer": "https://snowscan.xyz/", "comment": "Paid Plan required"},
    {"chainname": "Sepolia Testnet", "chainid": "11155111", "blockexplorer": "https://sepolia.etherscan.io/", "comment": "Free-tier available"},
]


class RegistryTests(unittest.TestCase):
    def setUp(self):
        self.reg = explorers.Registry(CHAINLIST)

    def test_explorer_links_resolve_to_chains(self):
        cases = {
            "https://snowtrace.io/address/0xabc": 43114,  # Routescan, merged into Etherscan's Avalanche
            "https://rootstock.blockscout.com/address/0xabc": 30,
            "https://blockscout.com/xdai/mainnet/address/0xabc": 100,
            "https://www.oklink.com/x-layer/address/0xabc": 196,
            "https://www.oklink.com/xlayer/address/0xabc/contract": 196,
            "https://www.oklink.com/fr/fantom/address/0xabc/contract": 250,  # locale prefix
            "https://explorer.zksync.io/address/0xabc#contract": 324,
            "arbiscan.io/address/0xabc": 42161,
        }
        for url, chain_id in cases.items():
            self.assertEqual(self.reg.for_url(url).id, chain_id, url)
        self.assertIsNone(self.reg.for_url("https://unknown.example/address/0xabc"))

    def test_providers_per_chain(self):
        avax = self.reg.get(43114)
        self.assertTrue(avax.etherscan and not avax.etherscan_live)
        self.assertEqual(avax.source_providers, ["etherscan", "routescan", "sourcify"])
        rsk = self.reg.get(30)
        self.assertEqual(rsk.explorer_rpcs, ("https://rootstock.blockscout.com/api/eth-rpc",))
        self.assertEqual(rsk.public_rpcs, ("https://public-node.rsk.co",))
        self.assertEqual(self.reg.get(1284).source_providers, ["sourcify"])
        self.assertEqual(self.reg.get(424242).name, "chain 424242")  # any id works (Sourcify, nodes)

    def test_name_resolution(self):
        self.assertEqual(self.reg.resolve("arbitrum").id, 42161)
        self.assertEqual(self.reg.resolve("rootstock").id, 30)
        self.assertEqual(self.reg.resolve(None).id, 1)
        with self.assertRaises(ValueError):
            self.reg.resolve("nonexistent")


class OnChainTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.client = client.Client(Cache(Path(self.tmp.name), 10))
        self.oc = onchain.OnChain(self.client, etherscan_key="SECRETKEY", rpc="auto")
        self.oc._quicknode = None  # no QuickNode in tests unless set explicitly
        for target in ("ETHERSCAN_INTERVAL_SECONDS", "OPEN_API_INTERVAL_SECONDS"):
            p = mock.patch.object(explorers, target, 0)
            p.start()
            self.addCleanup(p.stop)
        sleeper = mock.patch.object(explorers.time, "sleep")
        sleeper.start()
        self.addCleanup(sleeper.stop)

    def tearDown(self):
        self.tmp.cleanup()

    def serve(self, routes):
        """``routes``: list of (url substring, response or exception), consumed in order per match."""
        routes = list(routes)
        calls = []

        def fake(url, headers=None, data=None):
            if url == explorers.ETHERSCAN_CHAINLIST:
                return json.dumps({"result": CHAINLIST})
            calls.append(url)
            for i, (needle, response) in enumerate(routes):
                if needle in url:
                    routes.pop(i)
                    if isinstance(response, Exception):
                        raise response
                    return json.dumps(response)
            raise AssertionError(f"unexpected request {url}")

        patches = [mock.patch.object(m, "http_request", side_effect=fake) for m in (explorers, rpc, onchain)]
        return patches, calls

    def run_with(self, routes, fn):
        patches, calls = self.serve(routes)
        for p in patches:
            p.start()
        try:
            return fn(), calls
        finally:
            for p in patches:
                p.stop()

    def test_falls_back_to_sourcify_and_caches(self):
        unverified = {"status": "1", "result": [{"SourceCode": "", "ABI": "Contract source code not verified"}]}
        sourcify_doc = {"compilation": {"name": "Vault"}, "abi": [], "stdJsonInput": {"sources": {"V.sol": {"content": "v"}}}}
        chain = explorers.Registry(CHAINLIST).get(1)
        (c, calls) = self.run_with(
            [("api.etherscan.io", unverified), ("sourcify.dev", sourcify_doc)],
            lambda: (self.oc.contract(chain, ADDR), self.oc.contract(chain, ADDR))[1],
        )
        self.assertEqual((c.name, c.provider, c.verified), ("Vault", "sourcify", True))
        self.assertEqual(len(calls), 2)  # second lookup came from the cache

    def test_etherscan_failure_is_not_cached_as_unverified(self):
        chain = explorers.Registry(CHAINLIST).get(1)
        with self.assertRaises(explorers.ChainError):
            self.run_with([("api.etherscan.io", client.FetchError("down ?apikey=SECRETKEY"))],
                          lambda: self.oc.contract(chain, ADDR))
        self.assertIsNone(self.client.cache.read(f"contracts/1/{ADDR.lower()}"))

    def test_api_key_never_leaks_into_errors(self):
        chain = explorers.Registry(CHAINLIST).get(1)
        try:
            self.run_with([("api.etherscan.io", client.FetchError("could not reach ?apikey=SECRETKEY"))],
                          lambda: self.oc.balance(chain, ADDR))
        except explorers.ChainError as exc:
            self.assertNotIn("SECRETKEY", str(exc))
        else:
            self.fail("expected ChainError")

    def test_rate_limit_is_retried(self):
        chain = explorers.Registry(CHAINLIST).get(1)
        (bal, calls) = self.run_with([
            ("api.etherscan.io", {"status": "0", "message": "NOTOK", "result": "Max calls per sec rate limit reached (3/sec)"}),
            ("api.etherscan.io", {"status": "1", "message": "OK", "result": "42"}),
        ], lambda: self.oc.balance(chain, ADDR))
        self.assertEqual((bal, len(calls)), (42, 2))

    def test_historical_reads_need_a_real_node(self):
        chain = explorers.Registry(CHAINLIST).get(30)  # Rootstock: explorer RPC only
        with self.assertRaisesRegex(explorers.ChainError, "--block needs a real node"):
            self.run_with([], lambda: self.oc.call(chain, ADDR, b"", block=1000))
        node = rpc.JsonRpc("https://node.example/secret-token", "my node")
        self.oc.rpc_mode = "https://node.example/secret-token"
        word = "0x" + (5).to_bytes(32, "big").hex()
        (ret, calls) = self.run_with([("node.example", {"jsonrpc": "2.0", "id": 1, "result": word})],
                                     lambda: self.oc.call(chain, ADDR, b"", block=1000))
        self.assertEqual(int.from_bytes(ret, "big"), 5)
        self.assertEqual(node.label, "my node")

    def test_quicknode_is_preferred_and_urls_built_per_network(self):
        qn = rpc.QuickNode(self.client, "https://brave-cat.hype-mainnet.quiknode.pro/" + "ab" * 20 + "/evm",
                           {"ethereum-mainnet": "https://tooling-access-x.quiknode.pro/",
                            "avalanche-mainnet": "https://tooling-access-x.avalanche-mainnet.quiknode.pro/ext/bc/C/rpc/"})
        self.assertEqual(qn.url_for_network("avalanche-mainnet"),
                         "https://brave-cat.avalanche-mainnet.quiknode.pro/" + "ab" * 20 + "/ext/bc/C/rpc/")
        with mock.patch.object(rpc.QuickNode, "chain_ids", return_value={1: "ethereum-mainnet"}):
            self.oc._quicknode = qn
            node = self.oc.node(explorers.Registry(CHAINLIST).get(1), block=123)
        self.assertEqual(node.label, "QuickNode ethereum-mainnet")
        self.assertNotIn("ab" * 20, node.label)

    def test_paid_chain_message_and_revert_reason(self):
        reason = "08c379a0" + abi.encode([abi.Basic("string")], ["nope"]).hex()
        reg = explorers.Registry(CHAINLIST)
        with self.assertRaisesRegex(explorers.ChainError, "paid Etherscan plan"):
            self.run_with([("api.etherscan.io", {"status": "0", "message": "NOTOK", "result": "Free API access is not supported for this chain."})],
                          lambda: self.oc.call(reg.get(43114), ADDR, b""))
        with self.assertRaisesRegex(rpc.CallReverted, "nope"):
            self.run_with([("api.etherscan.io", {"jsonrpc": "2.0", "id": 1, "error": {"code": 3, "message": "execution reverted", "data": "0x" + reason}})],
                          lambda: self.oc.call(reg.get(1), ADDR, b""))

    def test_offline_refuses_live_reads(self):
        oc = onchain.OnChain(client.Client(Cache(Path(self.tmp.name), 10), offline=True), etherscan_key="k")
        with self.assertRaises(explorers.ChainError):
            oc.balance(ETH, ADDR)

    def test_batch_code_sizes_and_multicall_fallback(self):
        node = "https://node.example/rpc"
        self.oc.rpc_mode = node
        word = "0x" + (7).to_bytes(32, "big").hex()
        batch = [{"jsonrpc": "2.0", "id": 0, "result": "0x6001"}, {"jsonrpc": "2.0", "id": 1, "result": "0x"}]
        (sizes, _) = self.run_with([(node, batch)], lambda: self.oc.code_sizes(ETH, [ADDR, "0x" + "22" * 20]))
        self.assertEqual(list(sizes.values()), [2, 0])
        (out, calls) = self.run_with([
            (node, {"jsonrpc": "2.0", "id": 1, "result": "0x"}),  # no Multicall3 on this chain
            (node, {"jsonrpc": "2.0", "id": 1, "result": word}),
            (node, {"jsonrpc": "2.0", "id": 1, "error": {"code": 3, "message": "execution reverted", "data": "0x"}}),
        ], lambda: self.oc.multicall(ETH, [(ADDR, b"\x01"), (ADDR, b"\x02")]))
        self.assertEqual(out[0], (True, (7).to_bytes(32, "big")))
        self.assertFalse(out[1][0])
        self.assertEqual(len(calls), 3)

    def test_rate_limited_explorer_is_not_cached_as_unverified(self):
        chain = explorers.Registry(CHAINLIST).get(30)  # Rootstock: Blockscout, then Sourcify
        throttled = [("rootstock.blockscout.com/api", client.FetchError("HTTP 429", 429))] * explorers.COMPAT_RATE_LIMIT_RETRIES
        not_found = ("sourcify.dev", client.NotFoundError("404", 404))
        with self.assertRaises(explorers.RateLimited):
            self.run_with(throttled + [not_found], lambda: self.oc.contract(chain, ADDR))
        self.assertIsNone(self.client.cache.read(f"contracts/30/{ADDR.lower()}"))
        other = "0x" + "33" * 20  # the throttled API is skipped for the rest of the run
        with self.assertRaises(explorers.RateLimited):
            (_, calls) = self.run_with([not_found], lambda: self.oc.contract(chain, other))
        self.oc._apis.clear()
        busy = {"message": "Too many requests. Increase limits now", "result": None, "status": "0"}
        verified = {"status": "1", "result": [{"SourceCode": "x", "ABI": "[]", "ContractName": "MoC"}]}
        (c, calls) = self.run_with([("rootstock.blockscout.com/api", busy), ("rootstock.blockscout.com/api", verified)],
                                   lambda: self.oc.contract(chain, ADDR))
        self.assertEqual((c.name, len(calls)), ("MoC", 2))

    def test_sourcify_unsupported_chain_is_unverified(self):
        chain = explorers.Registry(CHAINLIST).get(128)  # HECO: Sourcify only, and it rejects the chain
        (c, _) = self.run_with([("sourcify.dev", client.FetchError("HTTP 400", 400))], lambda: self.oc.contract(chain, ADDR))
        self.assertFalse(c.verified)

    def test_public_rpc_before_explorer_rpc(self):
        chain = explorers.Registry(CHAINLIST).get(30)
        self.assertEqual(self.oc.node(chain).label, "public-node.rsk.co RPC")
        self.oc.rpc_mode = "explorer"
        self.assertEqual(self.oc.node(chain).label, "rootstock.blockscout.com RPC")

    def test_oversized_batches_are_split(self):
        node = "https://node.example/rpc"
        self.oc.rpc_mode = node
        addresses = ["0x" + f"{i:040x}" for i in range(1, 8)]
        reply = lambda n: [{"jsonrpc": "2.0", "id": i, "result": "0x60"} for i in range(n)]
        (sizes, calls) = self.run_with(
            [(node, client.FetchError("HTTP 413", 413)), (node, reply(5)), (node, reply(2))],
            lambda: self.oc.code_sizes(ETH, addresses),
        )
        self.assertEqual((list(sizes.values()), len(calls)), ([1] * 7, 3))

    def test_networks_file_parser(self):
        path = Path(self.tmp.name) / "networks.toml"
        path.write_text('[entry]\nendpoint_id = "1"\n\n[entry.networks]\npolygon = "https://t.matic.quiknode.pro/"\nhype-mainnet = "https://t.hype-mainnet.quiknode.pro/evm"\n')
        self.assertEqual(rpc._read_networks_file(path), {
            "polygon": "https://t.matic.quiknode.pro/", "hype-mainnet": "https://t.hype-mainnet.quiknode.pro/evm",
        })


if __name__ == "__main__":
    unittest.main()
