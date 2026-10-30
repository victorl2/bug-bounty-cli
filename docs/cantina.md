# Cantina commands

Browse Cantina bounties and competitions, retrieve their rules, scope, resources and leaderboards,
and inspect in-scope EVM contracts. These are Cantina's own commands: run them as `bb cantina ...`
or with the `cantina` command the installer adds next to `bb` (see the [README](../README.md#install)).

## Discovery

```sh
cantina programs
cantina programs --search uniswap --min-bounty 1000000
cantina programs -l Solidity -e ETH -t 'Smart Contract' --kyc yes -n 20 --json
cantina programs --sort launched --reverse
cantina competitions --status active --sort end
cantina competitions --status finished --sort pool -n 10
cantina opportunities --status ended
```

Aliases match Immunefi: `bounties`/`ls`, `comps`, `bounty`, `comp`, `c`.
Bounties default to current records; competitions default to current and ended records.
`--status` accepts `active`, `upcoming`, `live`, `evaluating`, `finished`, `ended`, `all`.
Active means upcoming or live. Dates advance cached lifecycle status. Listings follow every
`nextValue` cursor before filtering/sorting/limiting; a bounded result is not presented as all scope.

Sorts: `bounty`/`reward`/`pool`, `name`, `start`/`launched`, `end`, `updated`.
Cantina does not provide update timestamps, so `updated` warns when falling back to creation time.
Language filters use file extensions in explicit references/instructions. Ecosystem filters use
recognized explorer URLs or explicit tags. Type filters use scope group names or explicit tags.
Matching JSON records include `filterEvidence`; missing evidence is excluded, never guessed.

Cantina publishes no Immunefi-style vault balances or project-type taxonomy. `--sort vault` and
`--project-type` explain that limitation. `--include-paused` warns that Cantina does not expose
an equivalent paused state; it cannot retrieve unpublished programs.

## Details

```sh
cantina program Uniswap
cantina program Uniswap information scope resources --json
cantina competition 'Revert v4lend Competition' all --json
cantina competition e7af4986-183d-4764-8bd2-1d6b47f87d99 leaderboard --json
```

Targets may be a UUID, Cantina URL, exact opportunity name, or company handle. Ambiguous names
produce UUID suggestions. Names can be reused across competitions; use the UUID to disambiguate.
Sections: `overview`, `information`, `scope`, `resources`, `leaderboard`, `all`.
`--json` preserves source fields for the requested section(s); `all --json` includes the full
record and, for competitions, leaderboard availability and published rankings.

Details combine current opportunity metadata with the public repository catalog: pinned source
URL and commit, README path, asset groups and rewards, exclusions, KYC, fees, rules and instructions.
Competition scopes often live in Markdown instructions rather than structured asset groups;
`scope` retains those instructions and pinned repository metadata. Resources include links from
instructions. Published leaderboards contain team ranks and earnings/findings as supplied by
Cantina. An unpublished leaderboard explicitly reports `available: false`; denials and provider
errors are not mislabeled as unpublished results.

## Contract inspection

```sh
cantina contract chains
cantina contract scope Polymarket --limit 1 --rpc quicknode --json
cantina contract scope Polymarket --no-live --download ./sources
cantina contract info https://etherscan.io/address/0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48
cantina contract abi 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48 --chain eth --json
cantina contract source <explorer-url> --impl -o ./implementation
cantina contract read <explorer-url> --rpc quicknode --block 21000000
cantina contract call <address> 'balanceOf(address)(uint256)' <holder> --chain eth --rpc quicknode --block 21000000
cantina contract storage <address> implementation --chain eth --rpc quicknode --block 21000000
```

The bundled MIT-licensed engine is adapted from the sibling `immunefi-cli`. It supports explorer
and Sourcify source/ABI lookup, proxy implementation resolution, verified source downloads,
multicall getters, ABI encoding/decoding, raw calldata, storage slots, QuickNode discovery, and
historical block selection. Historical flags pin RPC state; verified source and ABI metadata remain the explorer’s current metadata. `call` simulates with `eth_call`; nothing is signed or broadcast.

`scope` resolves unique visible in-scope explorer references, reports unknown explorers/non-EVM
assets, checks deployed code in batches, and downloads contract and implementation sources.
`--limit`, `--no-live`, `--download`, `--competition`, `--chain`, `--rpc`, `--block`, and cache/output
flags are supported. `--no-live` skips RPC code checks but can still retrieve verified source.
Source-only scopes do not imply deployed addresses: GitHub commits are never treated as addresses.
A bare address with no explorer context is reported unresolved rather than assigned to Ethereum.

For standalone addresses, specify `--chain`. `--rpc` accepts `auto`, `quicknode`, `public`,
`explorer`, `etherscan`, or a node URL. QuickNode is discovered from the logged-in `qn` CLI, or
`CANTINA_QUICKNODE_URL`. Explorer keys use `ETHERSCAN_API_KEY`. Credentials remain private;
use environment variables rather than shell literals. Historical reads require a real archive
node; explorer/public latest-only fallbacks cannot silently answer historical queries.

## Authentication

```sh
cantina auth --firefox-profile auto --json
cantina program Uniswap all --firefox-profile auto --json
```

`auto` selects the default profile from Firefox's `profiles.ini`; alternatively supply the path
shown in `about:profiles`. Set `CANTINA_FIREFOX_PROFILE=auto` to reuse it. Unexpired Cantina cookies
are selected from a mode-0700 temporary snapshot of Firefox's database and WAL, deleted after
reading. Cookie headers remain in memory and never appear in output or cache. Container/partitioned
cookies are excluded to prevent mixing identities. Private browsing sessions are not in this DB.
An existing Cookie header may instead be supplied with the `CANTINA_COOKIE` environment variable.

Public details always use the public route, including when authenticated. `--private` requests
participant-only repository details and requires that account's existing access; it does not join
competitions, accept agreements, refresh credentials, submit findings or pay deposits. Authenticated
responses are not persisted, so authenticated offline reads are unavailable by design.

## Cache and resilience

Public responses are cached for 12 hours under `~/.cache/cantina-cli`, with EVM metadata under
`evm/`. Override with `CANTINA_CACHE_DIR`, `XDG_CACHE_HOME`, `--cache-dir`, `--ttl HOURS` or
`CANTINA_CACHE_TTL_HOURS`. `cache info --json` includes timestamps and freshness for API and EVM
metadata. `cache clear` clears both. Live state is never cached; offline mode refuses live reads.

`--refresh` bypasses cached data; `--offline` uses only cached responses. Transient API errors are
retried with bounded backoff. Network failures can use stale entries with a warning. If the
opportunity API fails, the public repository catalog can supply a degraded listing; details can
fall back to that catalog and then the page's RSC payload. These paths are labeled, since catalog
entries can differ from the curated opportunity list. Authorization failures never use fallback
success. `cantina import-page saved.html --json` also decodes saved page query data; a listing page
contains only its initial page and is not a complete catalog.

## Validation and parity

Version 0.2.0 covers all eight contract actions (`chains`, `info`, `abi`, `source`, `read`, `call`,
`storage`, `scope`), including scope source downloads and deployed-code checks, with no external CLI
dependency. Discovery, detail sections, competition/global leaderboards, filters, sorting, names,
JSON, cache controls, fallback handling and Firefox authentication are implemented. The platform
metadata differences above remain explicit rather than represented by invented Immunefi fields.

Live checks on 2026-10-29 covered paginated discovery (52 bounties, one current competition, 144
ended opportunities), public/catalog-enriched details, pinned competition source metadata, global
and published/unpublished competition leaderboards, and the authenticated Firefox session.
Polymarket scope resolved a verified Polygon contract, confirmed 15,007 bytes of runtime code and
downloaded its source. Historical USDC proxy inspection, getters, storage and calls succeeded at
Ethereum block 21,000,000 through QuickNode (`decimals` returned 6). Provider and chain availability
can vary; per-contract errors and unsupported chains remain visible in results.

```sh
cargo test
```

The deterministic tests cover discovery/fallbacks, lifecycle status, names, filters, exclusions, auth isolation,
cache semantics, ABI/Keccak vectors, proxies, provider retries, credential redaction, multicall,
and historical/offline behavior. Live checks are documented separately from deterministic tests.

MIT. See [LICENSE](../LICENSE).
