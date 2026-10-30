---
name: bug-bounty-cli
description: Use the `bb` CLI to find bug bounty programs and audit competitions across platforms, read their rules, rewards and scope, and inspect in-scope EVM contracts (verified source, ABI, proxies, view calls, storage). Use when the user asks which bounties or contests are live, what a program pays or covers, or wants to pull and examine a program's in-scope contracts.
---

# bug-bounty-cli (`bb`)

`bb` lists bug bounty programs and audit competitions from several platforms in one sorted
view, shows their details, and inspects their deployed contracts with read-only RPC calls.
Nothing is ever signed or broadcast.

## Setup

Check `bb --version`. If it is missing, ask the user before installing it. The installer
downloads a prebuilt binary into `~/.local/bin`; no runtime or toolchain is needed:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/victorl2/bug-bounty-cli/releases/latest/download/bug-bounty-cli-installer.sh | sh
```

On Windows: `powershell -ExecutionPolicy Bypass -c "irm https://github.com/victorl2/bug-bounty-cli/releases/latest/download/bug-bounty-cli-installer.ps1 | iex"`.
Inside a clone of the repository, `cargo run --release -- ARGS` runs it from source.

`bb sources` lists the supported platforms. Help text never names a platform, so check there
rather than assuming one.

## Rules for agents

- Pass `--json` whenever you parse output. Text tables are cut to 25 rows by default; JSON
  returns every match unless you pass `-n`.
- Notes and warnings go to stderr, so stdout stays clean JSON.
- Copy `target` values (for example `sherlock:56` or `cantina:f9df94db`) from a listing into
  later commands. A target can also be a name or the opportunity's URL. Unique id prefixes work.
- Responses are cached for 12 hours. Use `--refresh` only when the user needs current data, and
  `--offline` to work from the cache alone.
- Never print, log or commit credentials. That covers `ETHERSCAN_API_KEY`, QuickNode endpoint
  URLs (`BB_QUICKNODE_URL`), RPC URLs that embed tokens, and browser cookies. Don't echo
  `--rpc <url>` values back to the user.
- `--firefox-profile` reads a signed-in browser session. Use it only when the user asks for
  authenticated or participant-only data.

## Discover

```sh
bb programs --json -n 20                          # live programs, highest reward first
bb programs -l solidity -e ethereum --min-bounty 100000 --json
bb programs -s uniswap --json                     # search name or id
bb competitions --status active --json            # live + upcoming contests
bb competitions --sort end --json
bb --source sherlock competitions --json          # one platform only (also: bb sherlock competitions)
bb sites --json                                   # each platform's own listing pages
```

Filters:

| Flag | Values |
| --- | --- |
| `-s` | text |
| `-e` | ecosystem |
| `-l` | language |
| `-t` | asset type |
| `--project-type` | project tag |
| `--kyc` | `yes` or `no` |
| `--min-bounty` | top reward |
| `--status` | `live`, `upcoming`, `evaluating`, `finished`, `active`, `ended` or `all` |
| `--include-paused` | also list paused programs |

Sort keys for `--sort`: `bounty`, `name`, `start`, `end`, `updated`, `vault`. `-r` reverses the order.

A tag filter is skipped on a platform that has no tags of that kind. A note on stderr says so and
lists the platforms that were left out.

Each listing row has these fields:
`source, id, target, kind, name, url, status, reward, currency, kyc, start, end, updated, raw`.
`raw` holds the platform's original record.

## Read one opportunity

```sh
bb program sherlock:56 --json                   # {source, kind, target, data}
bb program immunefi:ssvnetwork scope            # sections: overview, information, scope,
bb competition <target> all                     #   resources, leaderboard, all
```

If you request a program as a competition, or the other way round, the error message gives
the right command to run.

## Inspect in-scope contracts

```sh
bb contract scope sherlock:56 --json --limit 5        # resolve in-scope addresses on explorers
bb contract scope <target> --competition --download ./src
bb contract info <address|explorer-url> --chain base --json
bb contract abi <address> --chain arbitrum --json
bb contract source <explorer-url> --impl -o ./impl    # proxy -> implementation source
bb contract read <address> --chain eth --json         # every argument-less view
bb contract call <address> 'balanceOf(address)(uint256)' 0xHolder --chain eth
bb contract storage <address> implementation --chain eth   # EIP-1967 slots, or a slot number
bb contract chains                                    # supported chains and providers
```

- `--chain` takes a chain id or a name. With an explorer URL as the target, the chain comes from
  the URL; otherwise the default is Ethereum.
- `--rpc auto` is the default. It tries QuickNode through the `qn` CLI, then the chain's public
  RPC, then the explorer's RPC, then Etherscan. You can also pass `quicknode`, `public`,
  `explorer`, `etherscan` or a URL.
- `--block N` pins live reads to a past block. Source and ABI data still come from the explorer's
  current metadata.
- `--no-live` skips checking deployed code.
- `scope --json` returns three keys:
  - `contracts`: each has `chainId`, `address`, `name`, `verified`, `implementation` and the
    explorer `url`.
  - `unsupported`: assets that could not be resolved.
  - `nonEvm`: a count of assets on non-EVM chains.

## Typical workflow

1. List candidates: `bb programs -l solidity --min-bounty 50000 --json`.
2. Pick a `target`, then read its rules and scope with `bb program <target> information scope`.
3. Resolve the contracts and download their sources:
   `bb contract scope <target> --download ./src --json`.
4. Check live state with `contract read`, `contract call` and `contract storage`.
5. Report what is in scope and out of scope as the program states it. Quote the program's own
   rules rather than inferring eligibility.

## Troubleshooting

- `error: ... matches several ids`: add more characters to the id prefix.
- Stale-cache warning: the platform could not be reached, so cached data was shown. Retry with
  `--refresh` later.
- A missing Etherscan key or QuickNode node only limits which sources and RPCs are used.
  `--rpc public` works without either.
- `bb cache info --json` shows what is cached. `bb cache clear [--source NAME]` resets it.
