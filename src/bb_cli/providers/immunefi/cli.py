"""Command-line entry point: ``immunefi <command> ...``."""

from __future__ import annotations

import argparse
import difflib
import json
import sys
from pathlib import Path
from typing import Any

from bb_cli import __version__
from bb_cli.evm import contract_cli
from . import render
from bb_cli.cache import MIN_TTL_HOURS, Cache, default_cache_dir, resolve_ttl_hours
from bb_cli.providers.immunefi.client import Client, FetchError, Kind, NotFoundError, Result, parse_target
from bb_cli.rsc import PayloadError

PROGRAM_SORTS = {
    "bounty": (lambda it: it.get("maxBounty") or 0, True),
    "vault": (lambda it: it.get("vaultBalance") or 0, True),
    "launched": (lambda it: it.get("launchDate") or "", True),
    "updated": (lambda it: it.get("updatedDate") or "", True),
    "name": (lambda it: (it.get("project") or "").lower(), False),
}
COMPETITION_SORTS = {
    "start": PROGRAM_SORTS["launched"],
    "end": (lambda it: it.get("endDate") or "", True),
    "pool": (lambda it: it.get("rewardsPool") or it.get("maxBounty") or 0, True),
    "name": PROGRAM_SORTS["name"],
}
PROGRAM_SECTIONS = ["overview", "information", "scope", "resources"]
COMPETITION_SECTIONS = PROGRAM_SECTIONS + ["leaderboard"]


def _common_options() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(add_help=False)
    g = p.add_argument_group("cache and output")
    g.add_argument("--json", action="store_true", help="print raw JSON instead of formatted text")
    g.add_argument("--refresh", action="store_true", help="ignore the cache and re-fetch from immunefi.com")
    g.add_argument("--offline", action="store_true", help="only use cached data, never hit the network")
    g.add_argument(
        "--ttl",
        type=float,
        metavar="HOURS",
        help=f"cache lifetime in hours (default 12, env IMMUNEFI_CACHE_TTL_HOURS, minimum {MIN_TTL_HOURS:g})",
    )
    g.add_argument("--cache-dir", type=Path, help="cache directory (default ~/.cache/immunefi-cli, env IMMUNEFI_CACHE_DIR)")
    g.add_argument("--no-color", action="store_true", help="disable ANSI colors (also honours NO_COLOR)")
    return p


def build_parser() -> argparse.ArgumentParser:
    common = _common_options()
    parser = argparse.ArgumentParser(
        prog="immunefi",
        description="Browse Immunefi bug-bounty programs and audit competitions. "
        f"Data is cached locally for at least {MIN_TTL_HOURS:g} hours.",
    )
    parser.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    sub = parser.add_subparsers(dest="command", required=True, metavar="<command>")

    def add_filters(p: argparse.ArgumentParser) -> None:
        f = p.add_argument_group("filters")
        f.add_argument("-s", "--search", help="substring match on slug or project name")
        f.add_argument("-e", "--ecosystem", help="ecosystem tag, e.g. ETH, Solana, Base")
        f.add_argument("-l", "--language", help="language tag, e.g. Solidity, Rust, Move")
        f.add_argument("-t", "--type", dest="program_type", help="program type, e.g. 'Smart Contract', 'Blockchain/DLT', 'Websites'")
        f.add_argument("--kyc", choices=["yes", "no"], help="only programs that do / do not require KYC")
        f.add_argument("-n", "--limit", type=int, help="show at most N rows")
        f.add_argument("-r", "--reverse", action="store_true", help="reverse the sort order")

    p = sub.add_parser("programs", aliases=["bounties", "ls"], parents=[common], help="list bug-bounty programs")
    add_filters(p)
    p.add_argument("--min-bounty", type=float, metavar="USD", help="minimum max-bounty in USD")
    p.add_argument("--project-type", help="project type tag, e.g. Defi, Infrastructure")
    p.add_argument("--include-paused", action="store_true", help="also list paused programs")
    p.add_argument("--sort", choices=PROGRAM_SORTS, default="bounty", help="sort key (default: bounty)")
    p.set_defaults(handler=cmd_list, kind="program")

    p = sub.add_parser("competitions", aliases=["comps"], parents=[common], help="list audit competitions")
    add_filters(p)
    p.add_argument(
        "--status",
        choices=["upcoming", "live", "evaluating", "finished", "active"],
        help="filter by status ('active' = upcoming or live)",
    )
    p.add_argument("--sort", choices=COMPETITION_SORTS, default="start", help="sort key (default: start)")
    p.set_defaults(handler=cmd_list, kind="competition")

    for name, aliases, kind, sections in (
        ("program", ["bounty"], "program", PROGRAM_SECTIONS),
        ("competition", ["comp"], "competition", COMPETITION_SECTIONS),
    ):
        p = sub.add_parser(
            name,
            aliases=aliases,
            parents=[common],
            help=f"show details of {'a bug-bounty program' if kind == 'program' else 'an audit competition'}",
        )
        p.add_argument("target", help="slug (e.g. ssvnetwork) or an immunefi.com URL")
        p.add_argument(
            "sections",
            nargs="*",

            metavar="section",
            help=f"one or more of: {', '.join(sections)}, all (default: overview)",
        )
        p.set_defaults(handler=cmd_detail, kind=kind, valid_sections=sections)

    from bb_cli.adapters import make_evm_client
    contract_cli.add_commands(sub, common, lambda args: make_evm_client(args,'immunefi'))

    p = sub.add_parser("cache", parents=[common], help="show or clear the local cache")
    p.add_argument("action", nargs="?", choices=["info", "clear"], default="info")
    p.set_defaults(handler=cmd_cache)
    return parser


def _make_client(args: argparse.Namespace) -> Client:
    from bb_cli.paths import provider_cache
    cache = Cache(args.cache_dir or provider_cache('immunefi'), resolve_ttl_hours(args.ttl))
    return Client(cache, refresh=args.refresh, offline=args.offline)


def _note_source(result: Result, args: argparse.Namespace) -> None:
    if result.degraded:
        print(
            "(public API fallback: codebase/documentation links, vault and some rule fields are unavailable)",
            file=sys.stderr,
        )
    if args.json:
        return
    if result.from_cache:
        state = "stale cache" if result.stale else "cache"
        age = render.duration(result.entry.age_seconds)
        print(f"({state}, fetched {age} ago; --refresh to update)", file=sys.stderr)


def _tag_match(values: Any, needle: str) -> bool:
    needle = needle.lower()
    return any(needle in str(v).lower() for v in values or [])


def _filter(items: list[dict], args: argparse.Namespace) -> list[dict]:
    out = []
    for it in items:
        tags = it.get("tags") or {}
        if args.search:
            hay = f"{it.get('slug', '')} {it.get('project', '')}".lower()
            if args.search.lower() not in hay:
                continue
        if args.ecosystem and not _tag_match(tags.get("ecosystem"), args.ecosystem):
            continue
        if args.language and not _tag_match(tags.get("language"), args.language):
            continue
        if args.program_type and not _tag_match(tags.get("programType"), args.program_type):
            continue
        if args.kyc and bool(it.get("kyc")) != (args.kyc == "yes"):
            continue
        if args.kind == "program":
            if args.min_bounty and (it.get("maxBounty") or 0) < args.min_bounty:
                continue
            if args.project_type and not _tag_match(tags.get("projectType"), args.project_type):
                continue
            if not args.include_paused and render.program_status(it) == "PAUSED":
                continue
        elif args.status:
            status = render.competition_status(it)
            wanted = {"UPCOMING", "LIVE"} if args.status == "active" else {args.status.upper()}
            if status not in wanted:
                continue
        out.append(it)
    key, descending = (PROGRAM_SORTS if args.kind == "program" else COMPETITION_SORTS)[args.sort]
    out.sort(key=key, reverse=descending != args.reverse)
    return out[: args.limit] if args.limit else out


def cmd_list(args: argparse.Namespace, style: render.Style) -> int:
    result = _make_client(args).listing(args.kind)
    items = _filter(result.data, args)
    if args.json:
        json.dump(items, sys.stdout, indent=2, ensure_ascii=False)
        print()
        return 0
    _note_source(result, args)
    if not items:
        print(f"No {args.kind}s match the given filters.")
        return 0
    if args.kind == "program":
        print(render.program_table(items, style))
    else:
        print(render.competition_table(items, style))
    noun = "program" if args.kind == "program" else "competition"
    print(style.dim(f"\n{len(items)} of {len(result.data)} {noun}s"))
    return 0


def _suggest(client: Client, kind: Kind, slug: str) -> str:
    """Point at the right command or close slugs when a detail page is not found."""
    other: Kind = "competition" if kind == "program" else "program"
    hints = []
    for k in (other, kind):
        try:
            items = client.listing(k).data
        except (FetchError, PayloadError):
            continue
        slugs = [it.get("slug", "") for it in items]
        if k == other and slug in slugs:
            return f"'{slug}' is a{'n audit' if k == 'competition' else ' bug-bounty'} {k}: try `immunefi {k} {slug}`"
        hints += difflib.get_close_matches(slug, slugs, n=3, cutoff=0.6)
        hints += [s for s, it in zip(slugs, items) if slug.lower() in (it.get("project") or "").lower()]
    hints = list(dict.fromkeys(hints))[:5]
    return f"did you mean: {', '.join(hints)}" if hints else f"run `immunefi {kind}s --search {slug}` to find it"


def cmd_detail(args: argparse.Namespace, style: render.Style) -> int:
    url_kind, slug = parse_target(args.target)
    kind: Kind = url_kind or args.kind
    valid = COMPETITION_SECTIONS if kind == "competition" else PROGRAM_SECTIONS
    sections = args.sections or ["overview"]
    if any(s not in valid + ["all"] for s in sections):
        raise ValueError("unknown detail section")
    sections = valid if "all" in sections else [s for s in valid if s in sections]
    client = _make_client(args)
    try:
        result = client.detail(kind, slug)
    except NotFoundError:
        print(f"error: no {kind} named '{slug}' on immunefi.com", file=sys.stderr)
        print(_suggest(client, kind, slug), file=sys.stderr)
        return 1
    if args.json:
        json.dump(result.data, sys.stdout, indent=2, ensure_ascii=False)
        print()
        return 0
    _note_source(result, args)
    sys.stdout.write(render.detail(kind, result.data, sections, style))
    return 0


def cmd_cache(args: argparse.Namespace, style: render.Style) -> int:
    client = _make_client(args)
    cache = client.cache
    if args.action == "clear":
        cache.clear()
        print(f"Cleared {cache.root}")
        return 0
    entries = cache.entries()
    if args.json:
        json.dump(
            {
                "dir": str(cache.root),
                "ttl_hours": cache.ttl_seconds / 3600,
                "entries": [
                    {"key": k, "source": e.source, "fetched_at": e.fetched_at, "fresh": cache.is_fresh(e)}
                    for k, e in entries
                ],
            },
            sys.stdout,
            indent=2,
        )
        print()
        return 0
    print(f"Cache dir: {cache.root}")
    print(f"TTL:       {cache.ttl_seconds / 3600:g}h (minimum {MIN_TTL_HOURS:g}h)")
    if not entries:
        print("No cached entries.")
        return 0
    rows = [
        [k, render.duration(e.age_seconds), "fresh" if cache.is_fresh(e) else "expired", e.source]
        for k, e in entries
    ]
    print()
    print(render.table(["KEY", "AGE", "STATE", "SOURCE"], rows, style, flex=3))
    return 0


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    style = render.Style(render.use_color(args.no_color))
    try:
        return args.handler(args, style)
    except PayloadError as exc:
        print(f"error: could not read immunefi.com page data ({exc}); the site layout may have changed", file=sys.stderr)
        return 1
    except FetchError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    except BrokenPipeError:
        sys.stderr.close()
        return 0
    except KeyboardInterrupt:
        return 130
