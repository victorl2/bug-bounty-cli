# bug-bounty-cli (`bb`)

Discover and compare bug bounties and audit competitions across **Immunefi, Cantina, Sherlock and HackenProof**, retrieve
rules and scope, and inspect in-scope EVM contracts. One package, a shared contract engine, no
runtime dependencies; Python 3.10+.

This project replaces the former `immunefi-cli` and incorporates `cantina-cli`. Existing Git history
is retained at [victorl2/bug-bounty-cli](https://github.com/victorl2/bug-bounty-cli).

## Install

```sh
uv tool install git+https://github.com/victorl2/bug-bounty-cli
# From a local clone: uv tool install --editable .
bb --help
bb sources
bb sites                                     # links to every platform's own listing pages
```

The package installs `bb`, `bug-bounty`, and compatibility commands `immunefi` and `cantina`.
When replacing separately installed old tools, uninstall their old distributions first:

```sh
uv tool uninstall immunefi-cli
uv tool uninstall cantina-cli
uv tool install --editable .
```

Without installing: `PYTHONPATH=src python3 -m bb_cli ...`.

## Compare sources

```sh
bb programs                                  # every platform, sorted together by maximum reward
bb programs --source immunefi --search lido
bb --source cantina programs --min-bounty 1000000
bb sherlock programs                         # same as --source sherlock
bb hackenproof competitions --status all     # HackenProof audit programs
bb programs -l Solidity -e ETH --kyc yes -n 20 --json
bb competitions --status active --sort end
bb competitions --source cantina --status finished --sort pool
```

`--source immunefi|cantina|sherlock|hackenproof|all` can appear before or after the command. Discovery defaults to all
sources. Results are filtered per source, then globally sorted and limited: the terminal shows 25
rows by default (`-n N` or `--all` for more), while `--json` returns every match. Programs default
to live ones by top reward; competitions show live and upcoming ones first, then the most recent.
Long UUID ids are shown as their first 8 characters (`cantina:f9df94db`), and any unique prefix is
accepted wherever a target is. The same project on
two platforms stays as two records, because rules and scope can differ. If either selected source
fails without usable fallback data, the command fails without printing a partial combined list.

Common filters: `--search`, `--ecosystem`, `--language`, `--type`, `--project-type`, `--kyc`,
`--min-bounty`, `--status`, `--include-paused`. Sorts: `bounty`/`reward`/`pool`, `name`,
`launched`/`start`, `end`, `updated`, `vault`; `--reverse` reverses order, `--limit` bounds output.
Provider-specific sort/filter availability remains enforced. `--sort vault` lists only platforms that publish vault balances (today Immunefi).
A tag filter a platform has no tags for excludes its records, with a warning on stderr. Cantina's language/ecosystem filters use explicit
reference evidence rather than a complete tag taxonomy. Unknown values stay unknown.

Reward sorting compares published numeric maximums, **without exchange-rate conversion**. The
currency remains visible; USD, USDC and token-denominated or mixed rewards are not interchangeable.
An absent Cantina update timestamp stays null in normalized output. No creation timestamp is
presented as an update timestamp.

## Source-qualified details

```sh
bb program immunefi:ssvnetwork all --json
bb program cantina:Uniswap scope resources --json
bb competition cantina:e7af4986-183d-4764-8bd2-1d6b47f87d99 leaderboard --json
bb program https://immunefi.com/bug-bounty/ssvnetwork/information/ all
bb program Uniswap scope                     # bare names are looked up on every platform
bb program "SSV Network"
bb program "Usual Labs" scope                # Sherlock bug bounty: contracts with their networks
bb competition https://audits.sherlock.xyz/contests/1209 all
bb program hackenproof:cetus-smart-contracts information leaderboard
```

A provider URL or `source:target` prefix chooses the provider. An explicit conflicting `--source`
is rejected. A bare name, handle or id is looked up on every platform (or only the `--source` one)
and must match exactly one opportunity: an ambiguous name lists the qualified targets to choose
from, a typo gets close-match suggestions from all platforms, and a competition requested as a
program (or the reverse) names the right command. `bb contract scope` resolves targets the same way.

The terminal views of all platforms share one layout: overview facts, rewards per severity and
asset group, scope tables, resources and links, and published leaderboards. `--json` output is
unchanged.

Sections: `overview`, `information`, `scope`, `resources`, `leaderboard`, and `all`. Leaderboards
come from Cantina competitions and HackenProof programs; Sherlock publishes none through its API.
Source-specific detail behavior is preserved. Normalized detail JSON wraps the provider response
as `{"source": "cantina", "kind": "program", "target": "cantina:Uniswap", "data": {...}}`.

Discovery JSON uses a common record:

```json
{
  "source": "cantina",
  "id": "f9df94db-c7b1-434b-bb06-d1360abdd1be",
  "target": "cantina:f9df94db-c7b1-434b-bb06-d1360abdd1be",
  "kind": "program",
  "name": "Uniswap",
  "url": "https://cantina.xyz/bounties/f9df94db-c7b1-434b-bb06-d1360abdd1be",
  "status": "live",
  "reward": 15500000,
  "currency": "USDC",
  "kyc": true,
  "raw": {}
}
```

`raw` contains the entire original provider record, not the empty placeholder in the example.
The record also includes nullable `start`, `end` and `updated` timestamps. Source-qualified targets
from a listing can be fed directly to detail and contract-scope commands.

## One EVM engine

```sh
bb contract chains
bb contract scope immunefi:ssvnetwork --rpc quicknode --limit 2 --json
bb contract scope cantina:Polymarket --rpc quicknode --download ./sources --limit 2
bb contract scope cantina:Polymarket --no-live --offline --json
bb contract scope sherlock:56 --limit 3       # contracts listed by network, no explorer link needed
bb contract scope hackenproof:layer3-smart-contracts
bb contract info https://etherscan.io/address/0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48
bb contract abi <address> --chain base --json
bb contract source <explorer-url> --impl -o ./implementation
bb contract read <explorer-url> --rpc quicknode --block 21000000
bb contract call <address> 'balanceOf(address)(uint256)' <holder> --chain eth --rpc quicknode --block 21000000
bb contract storage <address> implementation --chain eth --rpc quicknode --block 21000000
```

Standalone contract commands are source-independent. Scope metadata comes from the chosen
provider; verified source/ABI, proxy resolution, EIP-1967 reads, batched code checks, source downloads,
getter multicalls, ABI encoding and historical RPC reads use the same implementation and cache.
Contracts are never signed or broadcast. `--no-live` skips deployed-code checks; it can still fetch
verified source. Source-only scopes do not imply deployed addresses or chains.

Use `ETHERSCAN_API_KEY` for supported explorer APIs. QuickNode is discovered through the logged-in
`qn` CLI or `BB_QUICKNODE_URL`; legacy `IMMUNEFI_QUICKNODE_URL` and `CANTINA_QUICKNODE_URL` are also
accepted. Historical flags pin RPC state; source/ABI metadata still reflects current explorer
metadata. Credentials never belong in reports, committed files, or command output.

## Platform links

```sh
bb sites                                     # bug bounty and competition pages of every platform
bb sites sherlock hackenproof -k programs    # only some platforms, only bounty pages
bb links cantina --open                      # open them in the default browser
```

## Native commands and authentication

```sh
bb immunefi programs --sort vault --json
bb cantina programs --search uniswap --json
bb cantina auth --firefox-profile auto --json
bb program cantina:Uniswap all --firefox-profile auto --json
immunefi program ssvnetwork scope
cantina competition 'Revert v4lend Competition' all
```

Native commands preserve their provider's raw output and flags. Sherlock and HackenProof have no
separate native interface: `bb sherlock COMMAND` is the shared command limited to that platform. Commands only one platform offers
(`bb sources` lists them; today `auth`, `leaderboard`, `import-page` and `opportunities`) are routed
to that platform and rejected for an explicit `--source` that lacks them. They stay out of the
top-level help, which never names a platform. Firefox authentication remains scoped to Cantina; its cookies are never
sent to Immunefi, explorers or RPCs. Authenticated responses are never cached. Use `--private`
with a Cantina detail request only when that account already has participant access.

## Caching and migration

The default cache is `~/.cache/bug-bounty-cli`, configurable with `BB_CACHE_DIR`, `XDG_CACHE_HOME`,
or `--cache-dir`. Unified commands treat `--cache-dir` as the root:

```text
bug-bounty-cli/
  immunefi/   # provider responses
  cantina/    # provider responses
  sherlock/
  hackenproof/
  evm/        # shared source, ABI, explorer and chain metadata
```

```sh
bb programs --refresh --json
bb programs --offline --json
bb cache info --json
bb cache clear --source cantina   # only Cantina responses
bb cache clear                    # all providers and shared EVM metadata
```

TTL defaults to 12 hours (`BB_CACHE_TTL_HOURS`, `--ttl HOURS`). Stale fallback and degraded sources
produce warnings on stderr. Live state is not cached. A fresh combined result requires data from
every selected source; using `--source` lets you explicitly operate with only one provider.

Existing `~/.cache/immunefi-cli` and `~/.cache/cantina-cli` directories are retained, not deleted or
silently imported; the new package fetches into the unified cache. Native provider commands still
honor their legacy provider cache environment variables. Native `--cache-dir` selects that
provider's response directory directly, unlike unified `bb --cache-dir`, which selects the root.
Native contract commands share the default EVM cache; use unified `bb contract` for a custom EVM
cache root. The GitHub repository and local origin now use `victorl2/bug-bounty-cli`; old GitHub URLs redirect.

## Development

```sh
PYTHONPATH=src python3 -m unittest discover -s tests -v
uv build
```

Architecture:

- `bb_cli/cli.py`, `sources.py`: routing, provider registry, normalization and global ordering.
- `bb_cli/providers/immunefi/`, `providers/cantina/`: APIs, decoding, native filters and presentation.
- `bb_cli/providers/sherlock/`, `providers/hackenproof/`: public JSON APIs and detail pages, served
  in-process through the shared helpers in `providers/common.py` (cached GETs, filters, name lookup).
  A new platform of this kind is one such package plus one `Source` entry in `sources.py`.
- `bb_cli/adapters.py`: provider scopes adapted to one EVM interface.
- `bb_cli/evm/`: shared explorer/RPC/ABI/contract tools and terminal primitives.
- `bb_cli/cache.py`, `rsc.py`, `paths.py`: shared storage, decoding and cache layout.
- `immunefi_cli`, `cantina_cli`: compatibility import/entry-point shims, not duplicate engines.

The 114-test deterministic suite passes and covers all providers and shared EVM behavior, plus aggregation, global
limits, source collisions, provenance, routing conflicts, cache isolation and partial-failure handling.
Provider references are retained in [docs/immunefi.md](docs/immunefi.md) and
[docs/cantina.md](docs/cantina.md); their original standalone installation instructions are historical.

MIT. See LICENSE.

Live migration checks on 2026-10-29 passed for combined/source-specific discovery, details from
both providers, Cantina's published leaderboard and Firefox authentication, source export and
scope resolution from both providers. The shared engine confirmed Polymarket's Polygon deployment
(15,007 bytes) and a historical Ethereum call at block 21,000,000. The built wheel was also exercised
in isolation for combined cached discovery and both providers' cached contract scopes.
