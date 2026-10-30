"""Sherlock bug bounties and audit contests, from the public API behind audits.sherlock.xyz."""
from __future__ import annotations

import re
from urllib.parse import urlparse

from bb_cli.evm.client import NotFoundError
from ..common import JsonApi, apply_filters, iso, lookup, started
from .detail import render  # noqa: F401  (part of the provider interface)

NAME = 'Sherlock'
API = 'https://mainnet-contest.sherlock.xyz'
SITE = 'https://audits.sherlock.xyz'
PATHS = {'program': 'bug_bounties', 'competition': 'contests'}
PAGES = {'program': 'bug-bounties', 'competition': 'contests'}
CONTEST_STATUS = {'CREATED': 'upcoming', 'RUNNING': 'live', 'JUDGING': 'evaluating', 'ESCALATING': 'evaluating',
                  'SHERLOCK_JUDGING': 'evaluating', 'FINISHED': 'finished'}


def _records(kind: str, args, cache_dir) -> list[dict]:
    api = JsonApi(API, cache_dir, args)
    items, page = [], 1
    while True:
        data = api.get(f'{PATHS[kind]}-{page}', '/' + PATHS[kind], {'page': page, 'per_page': 500})
        items += data.get('items') or []
        if not data.get('has_next'):
            return items
        page += 1


def status(kind: str, record: dict) -> str:
    if kind == 'program':
        return 'live' if record.get('is_ready', True) else 'upcoming'
    value = CONTEST_STATUS.get(record.get('status'), (record.get('status') or '').lower())
    return 'upcoming' if value == 'live' and not started(iso(record.get('starts_at'))) else value


def url(kind: str, uid) -> str:
    return f'{SITE}/{PAGES[kind]}/{uid}'


def normalize(kind: str, record: dict) -> dict:
    uid = str(record.get('id'))
    if kind == 'program':
        reward, currency, start, end = record.get('payout'), record.get('display_currency'), record.get('live_since'), None
    else:
        reward, currency, start, end = record.get('rewards'), record.get('token'), record.get('starts_at'), record.get('ends_at')
    return {'source': 'sherlock', 'id': uid, 'target': 'sherlock:' + uid, 'kind': kind, 'name': record.get('title'),
            'url': url(kind, uid), 'status': status(kind, record), 'reward': float(reward or 0), 'currency': currency or 'USDC',
            'kyc': record.get('requires_kyc'), 'start': iso(start), 'end': iso(end), 'updated': iso(record.get('last_updated')),
            'raw': record}


def tags(row: dict) -> dict:
    raw = row['raw']
    return {'project': [c.get('name') for c in raw.get('categories') or []], 'language': raw.get('languages') or [],
            'type': [raw['type_label']] if raw.get('type_label') else []}


def listing(kind: str, args, cache_dir) -> list[dict]:
    return apply_filters([normalize(kind, r) for r in _records(kind, args, cache_dir)], args, kind, tags, NAME)


def candidates(kind: str, args, cache_dir) -> list[tuple[str, str, set]]:
    return [(str(r['id']), r.get('title') or str(r['id']), {str(r['id']), r.get('title') or ''}) for r in _records(kind, args, cache_dir)]


def identify(kind: str, target: str, args, cache_dir) -> str:
    if '://' in target:
        match = re.fullmatch(r'/(bug-bounties|contests)/(\d+)/?', urlparse(target).path)
        if not match:
            raise ValueError(f'Expected a {NAME} bug bounty or contest URL')
        found = 'program' if match[1] == 'bug-bounties' else 'competition'
        if found != kind:
            raise LookupError(f'{target} is a {found}: bb {found} sherlock:{match[2]}')
        return match[2]
    return lookup('sherlock', NAME, kind, target, lambda k: candidates(k, args, cache_dir))


def detail(kind: str, target: str, args, cache_dir) -> dict:
    uid = identify(kind, target, args, cache_dir)
    try:
        record = JsonApi(API, cache_dir, args).get(f'{kind}-{uid}', f'/{PATHS[kind]}/{uid}')
    except NotFoundError:
        raise ValueError(f'No {NAME} {kind} with id {uid}') from None
    return dict(record, kind=kind, url=url(kind, uid))


def scope_assets(record: dict) -> list[dict]:
    """Deployed contracts in scope; each names its network, so no explorer link is needed."""
    return [{'url': '', 'type': 'smart_contract', 'address': s['address'], 'chain': (s.get('network') or '').lower(),
             'description': s.get('display_name') or ''}
            for s in record.get('covered_scope') or [] if s.get('address') and s.get('selected', True)]
