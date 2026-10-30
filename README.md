# bug-bounty-cli (`bb`)

Discover and compare bug bounties and audit competitions across **Immunefi, Cantina, Sherlock and HackenProof**, retrieve
rules and scope, and inspect in-scope EVM contracts. A single self-contained binary for macOS, Linux
and Windows: nothing else to install, no runtime to set up.

## Install

macOS and Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/victorl2/bug-bounty-cli/releases/latest/download/bug-bounty-cli-installer.sh | sh
```

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/victorl2/bug-bounty-cli/releases/latest/download/bug-bounty-cli-installer.ps1 | iex"
```

The installer downloads the prebuilt binary for your platform from the
[latest release](https://github.com/victorl2/bug-bounty-cli/releases/latest), places it in
`~/.local/bin` (or `$XDG_BIN_HOME`) and adds that directory to your `PATH`. Archives for every
platform, with checksums, are attached to each release if you prefer to install by hand. With a
Rust toolchain you can also build from source: `cargo install --git https://github.com/victorl2/bug-bounty-cli`.

```sh
bb --help
bb sources
bb sites                                     # links to every platform's own listing pages
```

Everything runs through the single `bb` command; each platform's own commands are available as
`bb immunefi ...` and `bb cantina ...`.

## AI agent skill

[skills/bug-bounty-cli/SKILL.md](skills/bug-bounty-cli/SKILL.md) teaches AI coding agents how to use
`bb`: JSON-first commands, targets, filters, contract inspection and credential handling. Agents
working inside this repository pick it up through `.claude/skills/`. To use it everywhere, copy it
into your agent's skills directory:

```sh
cp -r skills/bug-bounty-cli ~/.claude/skills/
```

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
`storage` accepts `implementation`, `admin` and `beacon` for the EIP-1967 slots; when the first two
are empty it reads the older ZeppelinOS slots that proxies such as USDC use.
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
bb immunefi program ssvnetwork scope
bb cantina competition 'Revert v4lend Competition' all
```

Native commands preserve their provider's raw output and flags. Sherlock and HackenProof have no
separate native interface: `bb sherlock COMMAND` is the shared command limited to that platform. Commands only one platform offers
(`bb sources` lists them; today `auth`, `leaderboard`, `import-page` and `opportunities`) are routed
to that platform and rejected for an explicit `--source` that lacks them. They stay out of the
top-level help, which never names a platform. Firefox authentication remains scoped to Cantina; its cookies are never
sent to Immunefi, explorers or RPCs. Authenticated responses are never cached. Use `--private`
with a Cantina detail request only when that account already has participant access.

## Caching

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

Native provider commands still honor their legacy provider cache environment variables
(`IMMUNEFI_CACHE_DIR`, `CANTINA_CACHE_DIR`). Native `--cache-dir` selects that provider's response
directory directly, unlike unified `bb --cache-dir`, which selects the root. Native contract
commands share the default EVM cache; use unified `bb contract` for a custom EVM cache root. Caches
written by the earlier Python releases use the same layout and are reused as they are.

## Development

```sh
cargo build --release        # target/release/bb
cargo test                   # unit tests plus end-to-end runs of the binary, all offline
cargo clippy --all-targets
```

Architecture (`src/`):

- `cli.rs`, `sources.rs`: routing, platform registry, normalization and global ordering.
- `providers/immunefi/`, `providers/cantina/`: APIs, page decoding, native filters, presentation
  and each platform's own command line.
- `providers/sherlock/`, `providers/hackenproof/`: public JSON APIs and detail pages, served through
  the shared helpers in `providers/common.rs` (cached GETs, filters, name lookup). A new platform of
  this kind is one such module plus one `Source` entry in `sources.rs`.
- `evm/`: explorers, nodes, ABI encoding (via alloy) and the `bb contract` commands.
- `cache.rs`, `http.rs`, `rsc.rs`, `render.rs`, `paths.rs`: storage, HTTP, Next.js payload decoding,
  terminal tables and the cache layout.

Provider references are kept in [docs/immunefi.md](docs/immunefi.md) and
[docs/cantina.md](docs/cantina.md).

### Releases

Releases are built by [cargo-dist](https://github.com/axodotdev/cargo-dist). Bump `version` in
`Cargo.toml`, commit, and push a matching tag:

```sh
git tag v0.5.0 && git push origin v0.5.0
```

The release workflow then builds binaries for macOS (Apple Silicon and Intel), Linux (x86_64, also as a
statically linked musl build, and ARM64) and Windows, and publishes them on a GitHub release together with the
shell and PowerShell installers and checksums.

MIT. See LICENSE.
