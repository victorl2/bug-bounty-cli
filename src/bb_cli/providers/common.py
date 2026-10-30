"""Shared plumbing for platforms served entirely by the unified CLI (no native interface).

Such a platform module provides ``listing``, ``candidates``, ``detail``, ``render`` and
``scope_assets``; everything here is platform-neutral.
"""
from __future__ import annotations

import difflib
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlencode

from bb_cli.cache import Cache
from bb_cli.evm.client import FetchError, NotFoundError, http_request

SECTIONS = ('overview', 'information', 'scope', 'resources', 'leaderboard', 'all')


class JsonApi:
    """Cached GETs against one JSON API; a stale copy is used when the API is unreachable."""

    def __init__(self, base: str, cache_dir: Path, args):
        self.base = base.rstrip('/')
        self.cache = Cache(Path(cache_dir), args.ttl)
        self.refresh, self.offline = args.refresh, args.offline

    def get(self, key: str, path: str, params: dict | None = None):
        url = self.base + path + ('?' + urlencode(params) if params else '')
        entry = self.cache.read(key)
        if entry and (self.offline or (not self.refresh and self.cache.is_fresh(entry))):
            return entry.data
        if self.offline:
            raise FetchError(f'No cached data for {key} (offline mode)')
        try:
            data = json.loads(http_request(url, headers={'Accept': 'application/json'}))
        except NotFoundError:
            raise
        except (FetchError, ValueError) as exc:
            if entry:
                print(f'Warning: {url} unavailable ({exc}); using a stale cached copy', file=sys.stderr)
                return entry.data
            raise FetchError(str(exc)) from None
        self.cache.write(key, data, url)
        return data


def iso(value) -> str | None:
    """Epoch seconds or a '25 Oct 2026' style date as an ISO 8601 UTC string, so sources sort together."""
    if value in (None, ''):
        return None
    try:
        if isinstance(value, (int, float)):
            dt = datetime.fromtimestamp(value, timezone.utc)
        else:
            dt = datetime.strptime(value, '%d %b %Y').replace(tzinfo=timezone.utc)
    except (ValueError, OverflowError, OSError):
        return None
    return dt.strftime('%Y-%m-%dT%H:%M:%SZ')


def started(start: str | None) -> bool:
    return not start or start <= datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')


TAG_FILTERS = (('ecosystem', 'ecosystem'), ('language', 'language'), ('program_type', 'type'), ('project_type', 'project'))


def apply_filters(rows: list[dict], args, kind: str, tags, name: str) -> list[dict]:
    """The generic listing filters, over normalized rows; ``tags(row)`` gives its tag lists by category."""
    wanted = args.status
    out = []
    for row in rows:
        status = row['status']
        if wanted is None:
            if kind == 'program' and status not in ('live', 'upcoming') and not (args.include_paused and status == 'paused'):
                continue
        elif wanted == 'active':
            if status not in ('live', 'upcoming'):
                continue
        elif wanted == 'ended':
            if status not in ('evaluating', 'finished'):
                continue
        elif wanted != 'all' and status != wanted:
            continue
        hay = ' '.join(str(v or '') for v in (row['id'], row['name'], row['raw'].get('company_name')))
        if args.search and args.search.casefold() not in hay.casefold():
            continue
        if float(row['reward'] or 0) < args.min_bounty:
            continue
        if args.kyc and (row['kyc'] is None or row['kyc'] != (args.kyc == 'yes')):
            continue
        out.append(row)
    for option, category in TAG_FILTERS:
        needle = getattr(args, option, None)
        if not needle:
            continue
        if not any(tags(r).get(category) for r in out):
            print(f'Warning: {name} publishes no {option.replace("_", "-")} tags for {kind}s; they are excluded', file=sys.stderr)
        out = [r for r in out if any(needle.casefold() in str(t).casefold() for t in tags(r).get(category) or [])]
    return out


def pick(value: str, rows: list[tuple[str, str, set]], kind: str, name: str) -> str:
    """The id of the one row whose id, name or handle equals ``value`` (case-insensitively)."""
    wanted = value.casefold()
    matches = list(dict.fromkeys(t for t, _, keys in rows if wanted in {str(k).casefold() for k in keys if k}))
    if len(matches) == 1:
        return matches[0]
    labels = {n.casefold(): f'{n} ({t})' for t, n, _ in rows}
    if matches:
        raise ValueError(f'{value!r} matches several {name} {kind}s: ' + ', '.join(labels.get(m, m) for m in matches[:5]))
    hints = [labels[k] for k in difflib.get_close_matches(wanted, list(labels), n=3, cutoff=0.6)]
    raise ValueError(f'No {name} {kind} named {value!r}' + ('; did you mean: ' + ', '.join(hints) if hints else ''))


_LINK_RE = re.compile(r'https?://[^\s<>\)\]"\'`*]+')


def links(*texts) -> list[str]:
    return list(dict.fromkeys(l.rstrip('.,;:') for t in texts if isinstance(t, str) for l in _LINK_RE.findall(t)))


def rows_table(doc, headers: list[str], rows: list[list[str]], flex: int) -> None:
    from bb_cli.evm.render import table
    if rows:
        doc.raw('\n'.join('  ' + line for line in table(headers, rows, doc.style, flex=flex).splitlines()))
