"""``bb contract ...``: inspect and read smart contracts across EVM chains."""

from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime, timezone
from decimal import Decimal
from pathlib import Path
from typing import Any, Callable

from bb_cli.evm import abi, render
from bb_cli.evm.keccak import keccak256
from bb_cli.evm.client import Client, FetchError, Kind
from bb_cli.evm.explorers import NON_EVM_HOSTS, Chain, ChainError, Contract, first_address, host_of
from bb_cli.evm.onchain import RPC_MODES, SLOTS, OnChain


ClientFactory = Callable[[argparse.Namespace], Client]


def add_commands(sub: Any, common: argparse.ArgumentParser, make_client: ClientFactory) -> None:
    chain_opts = argparse.ArgumentParser(add_help=False)
    g = chain_opts.add_argument_group("chain")
    g.add_argument("--chain", help="chain id or name (eth, arbitrum, polygon, base, ...); default: from the explorer URL, else Ethereum")
    g.add_argument("--etherscan-key", metavar="KEY", help="Etherscan API key (default: $ETHERSCAN_API_KEY)")
    g.add_argument(
        "--rpc",
        metavar="URL|MODE",
        default="auto",
        help="node for live reads: a JSON-RPC URL, or auto (QuickNode via the `qn` CLI, then the chain's "
        "public RPC, its explorer's RPC, then Etherscan), quicknode, public, explorer, etherscan (default: auto)",
    )
    g.add_argument("--block", help="block number for live reads (default: latest)")
    parents = [common, chain_opts]

    contract = sub.add_parser(
        "contract",
        aliases=["c"],
        help="inspect and read smart contracts (any EVM chain)",
        description="Verified source comes from Etherscan, the chain's own explorer or Sourcify and is "
        "cached alongside Cantina metadata; balances, calls and storage come from a node (QuickNode, "
        "--rpc, explorer or Etherscan) and are never cached.",
    )
    verbs = contract.add_subparsers(dest="verb", required=True, metavar="<action>")

    p = verbs.add_parser("info", parents=parents, help="contract name, verification, proxy, creator, balance, functions")
    p.add_argument("target", help="address or explorer URL (e.g. https://etherscan.io/address/0x...)")
    p.set_defaults(handler=cmd_info)

    p = verbs.add_parser("abi", parents=parents, help="functions with selectors and events (--json for the raw ABI)")
    p.add_argument("target")
    p.set_defaults(handler=cmd_abi)

    p = verbs.add_parser("source", parents=parents, help="download verified source files")
    p.add_argument("target")
    p.add_argument("-o", "--out", type=Path, help="output directory (default ./<Name>-<address>); '-' prints to stdout")
    p.add_argument("--impl", action="store_true", help="for a proxy, download the implementation instead")
    p.set_defaults(handler=cmd_source)

    p = verbs.add_parser("read", parents=parents, help="call every argument-less view function (one batched request)")
    p.add_argument("target")
    p.set_defaults(handler=cmd_read)

    p = verbs.add_parser(
        "call",
        parents=parents,
        help="call a function: name, signature, 'sig(in)(out)' or raw 0x calldata",
        description="Simulated with eth_call: nothing is signed or sent.",
    )
    p.add_argument("target")
    p.add_argument("function", help="balanceOf | 'balanceOf(address)' | 'balanceOf(address)(uint256)' | 0x70a08231...")
    p.add_argument("args", nargs="*", help="arguments; arrays/tuples as [a,b] / (a,b); integers accept 1e18 or 1.5ether")
    p.set_defaults(handler=cmd_call)

    p = verbs.add_parser("storage", parents=parents, help="read raw storage slots")
    p.add_argument("target")
    p.add_argument("slot", help="slot number/hex, or implementation | admin | beacon (EIP-1967)")
    p.add_argument("-n", "--count", type=int, default=1, help="read N consecutive slots")
    p.set_defaults(handler=cmd_storage)

    p = verbs.add_parser("scope", parents=parents, help="resolve a program's in-scope contracts on their explorers")
    p.add_argument("program", help="program or competition target / provider URL")
    p.add_argument("--competition", action="store_true", help="the slug is an audit competition")
    p.add_argument("--download", type=Path, metavar="DIR", help="also download verified sources (implementations too) into DIR")
    p.add_argument("--limit", type=int, help="only the first N contracts")
    p.add_argument("--no-live", action="store_true", help="skip the deployed-code check against a node")
    p.set_defaults(handler=cmd_scope)

    p = verbs.add_parser("chains", parents=parents, help="list known chains, their source providers and nodes")
    p.set_defaults(handler=cmd_chains)

    contract.set_defaults(make_client=make_client)
    for parser in verbs.choices.values():
        parser.set_defaults(make_client=make_client)


# --- helpers --------------------------------------------------------------------------------------


def _onchain(args: argparse.Namespace) -> OnChain:
    return OnChain(args.make_client(args), getattr(args, "etherscan_key", None), getattr(args, "rpc", "auto"))


def _target(args: argparse.Namespace) -> tuple[OnChain, Chain, str]:
    es = _onchain(args)
    chain, address = es.parse_target(args.target, args.chain)
    return es, chain, address


def _dump(data: Any) -> int:
    json.dump(data, sys.stdout, indent=2, ensure_ascii=False)
    print()
    return 0


def _short(address: str | None) -> str:
    return f"{address[:6]}…{address[-4:]}" if address else "-"


def native(wei: int) -> str:
    return f"{Decimal(wei) / Decimal(10**18):,.6f}".rstrip("0").rstrip(".")


def _progress(text: str) -> None:
    if sys.stderr.isatty():
        print(f"\r\033[K{text}", end="", file=sys.stderr, flush=True)


def _progress_done() -> None:
    if sys.stderr.isatty():
        print("\r\033[K", end="", file=sys.stderr, flush=True)


def _compiler(c: Contract) -> str:
    raw = c.raw
    parts = [raw.get("CompilerVersion") or ""]
    if raw.get("OptimizationUsed") == "1":
        parts.append(f"optimizer {raw['Runs']} runs" if raw.get("Runs") else "optimizer")
    if raw.get("EVMVersion") and raw["EVMVersion"].lower() != "default":
        parts.append(f"evm {raw['EVMVersion']}")
    return ", ".join(p for p in parts if p)


def _write_sources(contract: Contract, out: Path) -> list[Path]:
    files = contract.source_files()
    written = []
    for rel, content in files.items():
        path = out / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        written.append(path)
    meta = {
        "address": contract.address,
        "chainId": contract.chain.id,
        "chain": contract.chain.name,
        "explorer": contract.chain.address_url(contract.address),
        "contractName": contract.name,
        "compilerVersion": contract.raw.get("CompilerVersion"),
        "optimizationUsed": contract.raw.get("OptimizationUsed") == "1",
        "runs": contract.raw.get("Runs"),
        "evmVersion": contract.raw.get("EVMVersion"),
        "license": contract.raw.get("LicenseType"),
        "provider": contract.provider,
        "constructorArguments": contract.raw.get("ConstructorArguments"),
        "proxy": contract.raw.get("Proxy") == "1",
        "implementation": contract.implementation,
        "settings": contract.compiler_settings(),
        "abi": contract.abi,
    }
    out.mkdir(parents=True, exist_ok=True)
    (out / "contract.json").write_text(json.dumps(meta, indent=2), encoding="utf-8")
    return written


def _function_lines(fns: list[abi.Function], style: render.Style) -> list[str]:
    return [f"  {style.dim('0x' + f.selector.hex())}  {f.describe()}" for f in sorted(fns, key=lambda f: f.name)]


# --- commands -------------------------------------------------------------------------------------


def cmd_chains(args: argparse.Namespace, style: render.Style) -> int:
    es = _onchain(args)
    chains = dict(es.registry.by_id)
    qn_chains: dict[int, str] = {}
    if args.rpc in ("auto", "quicknode") and not args.offline and es.quicknode:
        qn_chains = es.quicknode.chain_ids()
        for cid, key in qn_chains.items():
            chains.setdefault(cid, Chain(cid, key))
    records = []
    for chain in sorted(chains.values(), key=lambda c: c.id):
        if chain.id in qn_chains:
            live = f"quicknode ({qn_chains[chain.id]})"
        elif args.rpc not in RPC_MODES:
            live = "--rpc"
        elif chain.public_rpcs and args.rpc in ("auto", "public"):
            live = "public RPC (latest only)"
        elif chain.explorer_rpcs:
            live = "explorer (latest only)"
        elif chain.etherscan:
            live = "etherscan (latest only)" if chain.etherscan_live else "etherscan paid plan"
        else:
            live = "-"
        records.append({"id": chain.id, "name": chain.name, "explorer": chain.explorer,
                        "source": chain.source_providers, "live": live})
    if args.json:
        return _dump(records)
    rows = [[str(r["id"]), r["name"], ", ".join(r["source"]), r["live"]] for r in records]
    print(render.table(["ID", "CHAIN", "VERIFIED SOURCE", "LIVE READS"], rows, style, flex=2))
    print(style.dim("\nHistorical reads (--block) need QuickNode or --rpc; public and explorer RPCs only serve the latest state."))
    return 0

def cmd_info(args: argparse.Namespace, style: render.Style) -> int:
    es, chain, address = _target(args)
    # Live state is optional: on paid-only chains the free key still serves source and ABI.
    code_size = balance = None
    slot_impl = live_error = None
    try:
        code_size = es.code_size(chain, address, args.block)
        balance = es.balance(chain, address, args.block)
        if code_size:
            slot_impl = es.live_implementation(chain, address, args.block)
    except ChainError as exc:
        live_error = str(exc)
    if code_size == 0:
        if args.json:
            return _dump({"address": address, "chainId": chain.id, "contract": False, "balanceWei": str(balance)})
        print(f"{address} on {chain.name} has no code (EOA or not deployed); balance {native(balance or 0)}")
        return 0
    contract = es.contract(chain, address)
    try:
        creation = es.creation(chain, address)
    except FetchError:  # paid-only chain, or not cached in --offline mode
        creation = None
    fns, impl = es.functions_for(contract)
    reads = [f for f in fns if f.is_read]
    writes = [f for f in fns if not f.is_read]

    if args.json:
        return _dump({
            "address": address,
            "chainId": chain.id,
            "chain": chain.name,
            "explorer": chain.address_url(address),
            "name": contract.name,
            "verified": contract.verified,
            "compiler": _compiler(contract),
            "license": contract.raw.get("LicenseType"),
            "proxy": contract.raw.get("Proxy") == "1" or bool(slot_impl),
            "implementation": contract.implementation or slot_impl,
            "implementationName": impl.name if impl else None,
            "eip1967Implementation": slot_impl,
            "balanceWei": None if balance is None else str(balance),
            "codeSize": code_size,
            "liveError": live_error,
            "creation": creation,
            "readFunctions": [f.describe() for f in reads],
            "writeFunctions": [f.describe() for f in writes],
        })

    doc = render.Doc(style)
    title = (impl.name if impl else contract.name) or "Unverified contract"
    doc.raw(f"{style.bold(title)}  {style.link(chain.address_url(address))}")
    implementation = contract.implementation or slot_impl
    proxy_line = ""
    if implementation:
        target = es.contract(chain, implementation)
        proxy_line = f"yes ({contract.name or 'proxy'}) → {implementation}"
        proxy_line += f" ({target.name}{'' if target.verified else ', unverified'})"
        if slot_impl and contract.implementation and slot_impl != contract.implementation:
            proxy_line += f"; EIP-1967 slot now points to {slot_impl}"
    created = ""
    if creation:
        ts = creation.get("timestamp")
        when = datetime.fromtimestamp(int(ts), timezone.utc).strftime("%Y-%m-%d ") if str(ts or "").isdigit() else ""
        where = ", ".join(x for x in (
            f"block {creation['blockNumber']}" if creation.get("blockNumber") else "",
            f"tx {_short(creation.get('txHash'))}" if creation.get("txHash") else "",
        ) if x)
        created = f"{when}by {creation.get('contractCreator') or '?'}" + (f" ({where})" if where else "")
    doc.fields([
        ("Chain", f"{chain.name} ({chain.id})"),
        ("Verified", "yes" if contract.verified else "no"),
        ("Compiler", _compiler(contract)),
        ("License", contract.raw.get("LicenseType") or ""),
        ("Proxy", proxy_line or "no"),
        ("Balance", f"{native(balance)} (native token)" if balance is not None else ""),
        ("Code size", f"{code_size:,} bytes" if code_size else ""),
        ("Live data", style.dim(live_error) if live_error else ""),
        ("Created", created),
    ])
    if fns:
        doc.sub(f"Read functions ({len(reads)})")
        doc.raw("\n".join(_function_lines(reads, style)) or "  -")
        doc.sub(f"Write functions ({len(writes)})")
        doc.raw("\n".join(_function_lines(writes, style)) or "  -")
    hint = f"bb contract read {address} --chain {chain.id}"
    doc.raw("\n" + style.dim(f"Next: `{hint}` to dump state, `bb contract source {address} --chain {chain.id}{' --impl' if implementation else ''}` for code"))
    sys.stdout.write(doc.render())
    return 0


def cmd_abi(args: argparse.Namespace, style: render.Style) -> int:
    es, chain, address = _target(args)
    contract = es.contract(chain, address)
    fns, impl = es.functions_for(contract)
    source = impl or contract
    if not fns:
        raise ChainError(f"{address} is not verified on {chain.name}; no ABI available")
    if args.json:
        seen = {f.signature for f in source.functions}
        extra = [i for i in contract.abi if i.get("type") == "function"
                 and abi.functions_from_abi([i])[0].signature not in seen] if impl else []
        return _dump(source.abi + extra)
    if impl:
        print(style.dim(f"(proxy {address}: showing implementation {impl.address} {impl.name})"))
    print(style.bold(f"Read ({sum(f.is_read for f in fns)})"))
    print("\n".join(_function_lines([f for f in fns if f.is_read], style)))
    print(style.bold(f"\nWrite ({sum(not f.is_read for f in fns)})"))
    print("\n".join(_function_lines([f for f in fns if not f.is_read], style)))
    events = [i for i in source.abi if i.get("type") == "event"]
    if events:
        print(style.bold(f"\nEvents ({len(events)})"))
        for e in sorted(events, key=lambda e: e.get("name", "")):
            types = [abi.from_json(p) for p in e.get("inputs") or []]
            sig = f"{e.get('name')}({','.join(t.canonical() for t in types)})"
            print(f"  {style.dim('0x' + keccak256(sig.encode()).hex())}  {sig}")
    return 0


def cmd_source(args: argparse.Namespace, style: render.Style) -> int:
    es, chain, address = _target(args)
    contract = es.contract(chain, address)
    if args.impl:
        impl_address = contract.implementation or es.live_implementation(chain, address, args.block)
        if not impl_address:
            raise ChainError(f"{address} is not a proxy (no implementation found)")
        contract = es.contract(chain, impl_address)
    if not contract.verified:
        raise ChainError(f"{contract.address} is not verified on {chain.name}")
    if str(args.out) == "-":
        for path, content in contract.source_files().items():
            print(f"// ===== {path} =====\n{content}\n")
        return 0
    out = args.out or Path(f"{contract.name or 'Contract'}-{contract.address}")
    written = _write_sources(contract, out)
    if args.json:
        return _dump({"dir": str(out), "files": [str(p) for p in written]})
    print(f"Wrote {len(written)} files for {contract.name} ({contract.address}) to {out}/ (+ contract.json, from {contract.provider})")
    if not args.impl and contract.implementation:
        print(style.dim(f"This is a proxy for {contract.implementation}; add --impl to download the implementation."))
    return 0


def cmd_read(args: argparse.Namespace, style: render.Style) -> int:
    es, chain, address = _target(args)
    contract = es.contract(chain, address)
    fns, impl = es.functions_for(contract)
    getters = sorted((f for f in fns if f.is_read and not f.inputs), key=lambda f: f.name)
    if not getters:
        raise ChainError(f"no argument-less view functions found for {address} (is it verified?)")
    results = es.multicall(chain, [(address, f.encode_call([])) for f in getters], args.block)
    values: dict[str, Any] = {}
    lines = []
    width = max(len(f.name) for f in getters)
    for f, (ok, data) in zip(getters, results):
        if not ok:
            reason = data.decode("utf-8", "replace") if data and not data.startswith(b"\x08") else "reverted"
            values[f.name] = {"error": reason}
            lines.append(f"  {f.name.ljust(width)}  {style.dim(reason)}")
            continue
        try:
            decoded = abi.decode(f.outputs or [], data)
        except abi.AbiError:
            values[f.name] = "0x" + data.hex()
            lines.append(f"  {f.name.ljust(width)}  0x{data.hex()}")
            continue
        outs = f.outputs or []
        if len(outs) == 1:
            values[f.name] = abi.to_json(outs[0], decoded[0])
            text = abi.format_value(outs[0], decoded[0])
        else:
            values[f.name] = [abi.to_json(t, v) for t, v in zip(outs, decoded)]
            text = "(" + ", ".join(abi.format_value(t, v) for t, v in zip(outs, decoded)) + ")"
        lines.append(f"  {style.bold(f.name.ljust(width))}  {text}")
    if args.json:
        return _dump(values)
    label = f"{impl.name} via proxy" if impl else contract.name
    print(f"{style.bold(label)} {address} on {chain.name} @ {args.block or 'latest'}")
    print("\n".join(lines))
    return 0


def _resolve_function(fns: list[abi.Function], spec: str, nargs: int) -> abi.Function:
    if "(" in spec:
        parsed = abi.parse_signature(spec)
        known = next((f for f in fns if f.signature == parsed.signature), None)
        if known and parsed.outputs is None:
            return known
        return parsed
    matches = [f for f in fns if f.name == spec]
    if not matches:
        raise ValueError(f"no function named {spec!r} in the ABI; pass a full signature like '{spec}(uint256)(uint256)'")
    same_arity = [f for f in matches if len(f.inputs) == nargs]
    if len(same_arity) == 1:
        return same_arity[0]
    options = ", ".join(f.signature for f in (same_arity or matches))
    raise ValueError(f"{spec!r} is ambiguous or takes a different number of arguments: {options}")


def _print_raw(ret: bytes, args: argparse.Namespace) -> int:
    if args.json:
        return _dump({"result": "0x" + ret.hex()})
    print("0x" + ret.hex())
    return 0


def cmd_call(args: argparse.Namespace, style: render.Style) -> int:
    es, chain, address = _target(args)
    if args.function.startswith("0x"):
        try:
            data = bytes.fromhex(args.function[2:])
        except ValueError:
            raise ValueError(f"invalid calldata {args.function!r}") from None
        return _print_raw(es.call(chain, address, data, args.block), args)
    contract = es.contract(chain, address)
    fns, _ = es.functions_for(contract)
    fn = _resolve_function(fns, args.function, len(args.args))
    if len(args.args) != len(fn.inputs):
        raise ValueError(f"{fn.signature} takes {len(fn.inputs)} arguments, got {len(args.args)}")
    values = [abi.parse_value(t, a) for t, a in zip(fn.inputs, args.args)]
    if fn.mutability not in ("view", "pure", ""):
        print(style.dim(f"({fn.signature} is {fn.mutability}; simulated with eth_call, nothing is sent)"), file=sys.stderr)
    ret = es.call(chain, address, fn.encode_call(values), args.block)
    if fn.outputs is None:
        return _print_raw(ret, args)
    decoded = abi.decode(fn.outputs, ret)
    names = fn.output_names or [""] * len(fn.outputs)
    if args.json:
        if len(fn.outputs) == 1:
            return _dump(abi.to_json(fn.outputs[0], decoded[0]))
        return _dump([abi.to_json(t, v) for t, v in zip(fn.outputs, decoded)])
    for t, v, n in zip(fn.outputs, decoded, names):
        label = f"{style.dim(n or t.canonical())}  " if len(fn.outputs) > 1 else ""
        print(f"{label}{abi.format_value(t, v)}")
    return 0


def cmd_storage(args: argparse.Namespace, style: render.Style) -> int:
    es, chain, address = _target(args)
    start = SLOTS.get(args.slot.lower()) if args.slot.lower() in SLOTS else abi.parse_int(args.slot)
    rows = []
    for slot in range(start, start + max(args.count, 1)):
        word = es.storage(chain, address, slot, args.block)
        value = int.from_bytes(word, "big")
        rows.append((slot, word, value))
    if args.json:
        return _dump([{"slot": hex(s), "value": "0x" + w.hex()} for s, w, _ in rows])
    for slot, word, value in rows:
        hints = []
        if value and value < 1 << 160 and value >= 1 << 128:
            hints.append(f"address {abi.checksum_address(f'{value:040x}')}")
        elif value and value < 1 << 128:
            hints.append(f"uint {value}")
        print(f"{style.dim(hex(slot))}  0x{word.hex()}  {style.dim(' '.join(hints))}".rstrip())
    return 0


def cmd_scope(args: argparse.Namespace, style: render.Style) -> int:
    es = _onchain(args)
    slug = args.program
    kind: Kind = "competition" if args.competition else "program"
    bounty = es.client.detail(kind, slug).data

    targets: list[tuple[dict, str, Chain]] = []
    unsupported: list[tuple[dict, str]] = []
    non_evm = 0
    for asset in bounty.get("assets") or []:
        url = asset.get("url") or ""
        if asset.get("type") != "smart_contract" or not (address := first_address(url)):
            continue
        if host_of(url) in NON_EVM_HOSTS:
            non_evm += 1
        elif chain := es.chain_for_url(url):
            targets.append((asset, address, chain))
        else:
            unsupported.append((asset, address))
    if args.limit:
        targets = targets[: args.limit]
    if not targets and not unsupported:
        if args.json:
            return _dump({"program": bounty.get("slug"), "contracts": [], "unsupported": [], "nonEvm": non_evm})
        print(f"{bounty.get('project')} lists no EVM contract addresses on block explorers.")
        return 0

    records: list[dict[str, Any]] = []
    for i, (asset, address, chain) in enumerate(targets, 1):
        _progress(f"[{i}/{len(targets)}] {chain.name} {address}")
        record: dict[str, Any] = {
            "chainId": chain.id, "chain": chain.name, "address": address,
            "asset": asset.get("description") or "", "url": asset.get("url"),
        }
        try:
            contract = es.contract(chain, address)
            record.update(
                name=contract.name, verified=contract.verified, source=contract.provider,
                implementation=contract.implementation,
            )
            impl = es.contract(chain, contract.implementation) if contract.implementation else None
            record["implementationName"] = impl.name if impl and impl.verified else None
            if args.download:
                base = args.download / str(chain.id)
                if contract.verified:
                    _write_sources(contract, base / f"{contract.name}-{address}")
                if impl and impl.verified:
                    _write_sources(impl, base / f"{impl.name}-{impl.address}")
        except (ChainError, FetchError) as exc:
            record["error"] = str(exc)
        records.append(record)
    _progress_done()

    if not args.no_live:
        by_chain: dict[int, list[dict]] = {}
        for r in records:
            by_chain.setdefault(r["chainId"], []).append(r)
        for chain_id, group in by_chain.items():
            _progress(f"checking deployed code on {group[0]['chain']}")
            try:
                sizes = es.code_sizes(es.resolve_chain(chain_id), [r["address"] for r in group], args.block)
            except (ChainError, FetchError) as exc:
                for r in group:
                    r["codeError"] = str(exc)
                continue
            for r in group:
                size = sizes.get(r["address"])
                if isinstance(size, int):
                    r["codeSize"] = size
                else:
                    r["codeError"] = str(size)
        _progress_done()

    if args.json:
        return _dump({
            "program": bounty.get("slug"),
            "contracts": records,
            "unsupported": [{"address": a, "url": x.get("url")} for x, a in unsupported],
            "nonEvm": non_evm,
        })
    rows = []
    for r in records:
        if "error" in r:
            status = style.dim(r["error"][:60])
        elif not r.get("verified"):
            status = "unverified"
        elif r.get("implementation"):
            status = f"proxy → {r.get('implementationName') or 'unverified'} {_short(r['implementation'])}"
        else:
            status = "verified"
        code = "-" if "codeSize" not in r else ("yes" if r["codeSize"] else style.bold("NO CODE"))
        rows.append([r["chain"], r["address"], r.get("name") or "-", status, r.get("source") or "-", code, r["asset"]])
    print(f"{style.bold(bounty.get('project') or slug)}: {len(records)} EVM contracts")
    if rows:
        print(render.table(["CHAIN", "ADDRESS", "CONTRACT", "STATUS", "SOURCE", "CODE", "ASSET"], rows, style, flex=6))
    notes = []
    if unsupported:
        hosts = sorted({host_of(a.get("url") or "") for a, _ in unsupported})
        notes.append(f"{len(unsupported)} on unknown explorers: {', '.join(hosts)}")
    if non_evm:
        notes.append(f"{non_evm} non-EVM assets skipped")
    if any("codeError" in r for r in records):
        notes.append("CODE '-' = no node for that chain (see `bb contract chains`)")
    if notes:
        print(style.dim("\n" + "\n".join(notes)))
    if args.download:
        print(f"\nSources written under {args.download}/<chain-id>/<Name>-<address>/")
    return 0
