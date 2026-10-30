# Immunefi commands

Browse [Immunefi](https://immunefi.com) bug-bounty programs and audit competitions from the
terminal, and read their in-scope smart contracts on any EVM chain. These are Immunefi's own
commands: run them as `bb immunefi ...` or with the `immunefi` command the installer adds next to
`bb` (see the [README](../README.md#install)).

## Usage

```sh
# Lists
immunefi programs                                  # all active bug bounties, by max bounty
immunefi programs -e ETH -l Solidity --min-bounty 100000 -n 20
immunefi programs --search lido --sort updated
immunefi competitions                              # all audit competitions, newest first
immunefi competitions --status active              # upcoming + live

# Program details: overview (default), information, scope, resources, all
immunefi program ssvnetwork
immunefi program ssvnetwork scope resources
immunefi program https://immunefi.com/bug-bounty/ssvnetwork/information/#top all

# Competition details: same sections plus leaderboard
immunefi competition audit-comp-quantus
immunefi competition audit-comp-quantus information leaderboard

# Machine-readable output
immunefi programs --json | jq '.[].slug'
immunefi program ssvnetwork --json | jq '.assets'
```

| Section       | Contents                                                                                     |
|---------------|----------------------------------------------------------------------------------------------|
| `overview`    | Status, max bounty / reward pool, dates, vault, KYC, PoC, tags, features, description        |
| `information` | Vault, rewards by threat level, program overview, audits, known issues, KYC, rules           |
| `scope`       | Assets in scope, impacts in scope by severity, out-of-scope rules                            |
| `resources`   | Codebases, documentation, website/GitHub links, audits                                       |
| `leaderboard` | Competitions only: whitehat ranking, earnings and findings by severity                       |

Aliases: `bounties`/`ls` = `programs`, `comps` = `competitions`, `bounty` = `program`, `comp` = `competition`.

## Smart contracts

`immunefi contract` reads in-scope contracts on any EVM chain. It combines two kinds of source:

| Need | Sources, in order | Keys |
|---|---|---|
| Verified source, ABI, proxy info | [Etherscan API v2](https://docs.etherscan.io/) → the chain's own explorer API (Blockscout instances, Routescan, zkSync) → [Sourcify](https://sourcify.dev) | `ETHERSCAN_API_KEY` (free); the rest need none |
| Live state (balances, calls, storage) | your [QuickNode](https://www.quicknode.com) multichain endpoint → the chain's public RPC → the explorer's RPC → Etherscan's proxy | QuickNode via the `qn` CLI |
| Historical state (`--block`) | QuickNode, or `--rpc <archive URL>` | |

`immunefi contract chains` shows which source and node each of the ~130 known chains uses.

```sh
# A program's in-scope contracts: verified source, proxy -> implementation, deployed code
immunefi contract scope ssvnetwork
immunefi contract scope ssvnetwork --download ./ssv-src     # verified sources, implementations too

# Any contract: an address (Ethereum by default) or an explorer URL, which selects the chain
immunefi contract info https://etherscan.io/address/0xDD9BC35aE942eF0cFa76930954a156B3fF30a4E1
immunefi contract info https://rootstock.blockscout.com/address/0xf773B590aF754D597770937Fa8ea7AbDf2668370
immunefi contract info 0x... --chain arbitrum
immunefi contract abi 0x...                                 # functions with selectors, events with topics
immunefi contract source 0x... --impl -o ./impl             # proxy -> implementation source

# Live and historical state
immunefi contract read 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48     # every argument-less view function
immunefi contract call 0xA0b8... balanceOf 0x28C6c06298d514Db089934071355E5743bf21d60
immunefi contract call 0x4200000000000000000000000000000000000006 totalSupply --chain base --block 20000000
immunefi contract call 0xA0b8... 0x313ce567                  # raw calldata, raw result
immunefi contract storage 0x... implementation               # EIP-1967 slots, or any slot number
```

### Nodes and QuickNode

`--rpc` picks the node for live reads: a JSON-RPC URL, or one of `auto` (default), `quicknode`,
`public`, `explorer` or `etherscan`.

In `auto` mode the CLI uses your QuickNode account when the [`qn` CLI](https://www.quicknode.com/docs)
is installed and logged in:
- It reads your multichain endpoint with `qn endpoint list` and the per-network hosts from
  `~/.config/qn/networks.toml`. Alternatively, set `IMMUNEFI_QUICKNODE_URL` to the endpoint's
  HTTP URL.
- It probes once which chain ID each network serves, and caches only network names and chain IDs.
- The endpoint token stays in memory and is never written to disk or printed.

Behaviour verified against the live services:
- QuickNode serves real historical state on 75 chains. It is pruned on Monad, MegaETH and Sei
  testnet.
- Etherscan's proxy silently answers a past block with the latest state. Public and explorer RPCs
  are inconsistent about it. All three are therefore limited to `latest`, and `--block` without a real node
  is refused instead of returning wrong data.
- With a free Etherscan key, live reads on Base, BNB Chain, OP, Avalanche and Gnosis need a paid
  plan. QuickNode covers them.
- Keyless Blockscout APIs allow about 10 calls per hour. When one is exhausted the CLI says
  "rate limited" for the affected contracts instead of caching them as unverified, so a later
  run fills them in.

### Details

- **Proxies** are detected from explorer proxy flags and the EIP-1967 implementation slot.
  `read`, `call` and `abi` use the implementation's ABI against the proxy address.
- **`read`** batches every getter into one call through
  [Multicall3](https://www.multicall3.com/), falling back to one call per getter where Multicall3
  isn't deployed.
- **`call`** accepts a function name (resolved from the ABI), a signature, a cast-style
  `sig(inputs)(outputs)` for unverified contracts, or raw `0x` calldata. Arguments: arrays as
  `[a,b]`, tuples as `(a,b)`, and integers as `1e18` or `1.5ether`. Calls are simulated with
  `eth_call`, so nothing is signed or sent.
- **`scope`** checks with one batched request per chain that every address actually has code
  there. "NO CODE" usually means a wrong chain or a wrong address in the program listing.
- **Caching:** verified source, ABI and creation info are cached like the Immunefi data (10h+).
  A provider failure is never cached as "unverified". Balances, calls and storage are live and
  never cached, and `--offline` refuses them.
- **Rate limits:** Etherscan's free tier allows 3 requests per second. Requests are spaced out
  and retried when rate-limited, so a first `scope` of a 300-contract program takes a few minutes.
- **Not covered:** non-EVM assets (Starknet, Sui, Aptos, Stacks, ...) are skipped. A few explorers
  are offline or block API access; Sourcify still covers many of their contracts.

The ABI codec and Keccak-256 are implemented in pure Python (no dependencies) and are tested
against Foundry's `cast` output.

## Caching

Every list and detail page is cached as parsed JSON under `~/.cache/immunefi-cli`
(`$XDG_CACHE_HOME` and `IMMUNEFI_CACHE_DIR` are honoured). Entries are reused for **12 hours** by
default; the TTL can be raised with `--ttl HOURS` or `IMMUNEFI_CACHE_TTL_HOURS`, but never goes
below **10 hours**.

- `--refresh` forces a re-fetch of what the command needs.
- `--offline` only reads the cache.
- If a fetch fails and an expired entry exists, the stale data is shown with a warning.
- `immunefi cache` shows entries and their age; `immunefi cache clear` deletes them.

Competition status is advanced by the competition's dates when shown from cache, so a cached
`UPCOMING` competition reads as `LIVE` once its start date has passed.

## How it works

Immunefi's site is a Next.js App Router app. Its listing and program pages make no JSON API
calls of their own: the data is rendered on the server and streamed to the browser as a React
Server Components (RSC) payload. The CLI requests that stream directly (`RSC: 1` header, the same
request the site's client-side navigation makes), so it gets the data without the HTML around it:

- `/bug-bounty/` and `/audit-competition/` carry a `bounties` array (the lists).
- `/<kind>/<slug>/information/` carries the full `bounty` object, which backs all of the page's
  tabs (information, scope, resources, leaderboard), plus the Immunefi `vault`.

So each detail view costs one request (~20 KB compressed), however many sections you print.
For a missing page the RSC stream answers 200 with a `NEXT_HTTP_ERROR_FALLBACK;404` marker, which
the CLI reports as not found.

**Fallback.** Immunefi also publishes `/public-api/bounties.json`: every program and competition
in one JSON document (~1.3 MB compressed, ~6 MB decoded). It lacks the Resources tab (codebase and
documentation links), vault data and a few rule fields, so it is not the primary source. When a
page cannot be fetched or decoded (for example after a site redesign) and nothing is cached, the
CLI falls back to it automatically. It caches the document for the same TTL and prints a notice
saying which fields are missing.

| Source                     | Requests                | Has resources and vault | Format        |
|----------------------------|-------------------------|-------------------------|---------------|
| Page RSC stream (primary)  | 1 per list or detail    | yes                     | RSC flight    |
| `public-api/bounties.json` | 1 for everything        | no                      | stable JSON   |

Other endpoints the site calls (`/api/audit-competition/count`, `/public-api/bounties/assets/dice`)
return a live/upcoming competition count and a random asset; neither is useful here.

## Development

```sh
cargo test
```

## License

MIT. See [LICENSE](../LICENSE).
