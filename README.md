# immunefi-cli

Browse [Immunefi](https://immunefi.com) bug-bounty programs and audit competitions from the
terminal. No runtime dependencies (Python 3.10+ standard library only).

## Install

```sh
uv tool install git+https://github.com/victorl2/immunefi-cli   # or: pipx install git+https://...
immunefi --help
```

From a local clone: `uv tool install --editable .` (or `pipx install -e .`).

Without installing: `uv run immunefi ...` from this directory, or `PYTHONPATH=src python3 -m immunefi_cli ...`.

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
python3 -m unittest discover -s tests
```
