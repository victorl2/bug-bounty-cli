"""HackenProof bug bounty programs and audit programs, from the public JSON API behind hackenproof.com."""
from __future__ import annotations

import re
from urllib.parse import urlparse

from bb_cli.evm.client import NotFoundError
from bb_cli.evm.explorers import first_address
from ..common import JsonApi, apply_filters, iso, pick, started
from .detail import render  # noqa: F401  (part of the provider interface)

NAME = 'HackenProof'
SITE = 'https://hackenproof.com'
PATHS = {'program': 'programs', 'competition': 'audits'}
PAGES = {'program': 'programs', 'competition': 'audit-programs'}
PER_PAGE = 100  # the API's maximum


def _records(kind: str, args, cache_dir) -> list[dict]:
    api = JsonApi(SITE + '/programs-api', cache_dir, args)
    params = {'not_audits': 'true'} if kind == 'program' else {}
    items, page = [], 1
    while True:
        data = api.get(f'{PATHS[kind]}-{page}', '/' + PATHS[kind], {**params, 'page': page, 'per_page': PER_PAGE})
        items += data.get(PATHS[kind]) or []
        if not data.get('next_page') or page >= 50:
            return items
        page += 1


def status(record: dict) -> str:
    if record.get('state') == 'paused':
        return 'paused'
    name = ((record.get('status') or {}).get('name') or '').lower()
    if name == 'completed':
        return 'finished'
    if name == 'active':
        return 'live' if started(iso(record.get('start_date'))) else 'upcoming'
    return name or record.get('state') or ''


def url(kind: str, slug: str) -> str:
    return f'{SITE}/{PAGES[kind]}/{slug}'


def normalize(kind: str, record: dict) -> dict:
    slug = record.get('slug')
    return {'source': 'hackenproof', 'id': slug, 'target': f'hackenproof:{slug}', 'kind': kind, 'name': record.get('title'),
            'url': url(kind, slug), 'status': status(record), 'reward': float(record.get('max_bounty') or 0), 'currency': 'USD',
            'kyc': record.get('kyc_required'), 'start': iso(record.get('start_date')),
            'end': None if record.get('unending_program') else iso(record.get('end_date')),
            'updated': iso(record.get('updated_at')), 'raw': record}


def tags(row: dict) -> dict:
    labels = row['raw'].get('labels') or {}
    return {'ecosystem': labels.get('platforms') or [], 'language': labels.get('languages') or [],
            'type': labels.get('types') or [], 'project': labels.get('project_types') or []}


def listing(kind: str, args, cache_dir) -> list[dict]:
    if args.sort == 'vault':
        raise ValueError(f'{NAME} publishes no vault balances; sort by bounty instead')
    return apply_filters([normalize(kind, r) for r in _records(kind, args, cache_dir)], args, kind, tags, NAME)


def candidates(kind: str, args, cache_dir) -> list[tuple[str, str, set]]:
    return [(r['slug'], r.get('title') or r['slug'], {r['slug'], r.get('title') or ''}) for r in _records(kind, args, cache_dir) if r.get('slug')]


def identify(kind: str, target: str, args, cache_dir) -> str:
    if '://' in target:
        match = re.fullmatch(r'/(programs|audit-programs)/([\w-]+)/?', urlparse(target).path)
        if not match:
            raise ValueError(f'Expected a {NAME} program or audit program URL')
        found = 'program' if match[1] == 'programs' else 'competition'
        if found != kind:
            raise LookupError(f'{target} is a {found}: bb {found} hackenproof:{match[2]}')
        return match[2]
    rows = candidates(kind, args, cache_dir)
    return target if target in {r[0] for r in rows} else pick(target, rows, kind, NAME)


def detail(kind: str, target: str, args, cache_dir) -> dict:
    slug = identify(kind, target, args, cache_dir)
    try:
        record = JsonApi(SITE + '/programs-api', cache_dir, args).get(f'{kind}-{slug}', f'/{PATHS[kind]}/{slug}')
    except NotFoundError:
        raise ValueError(f'No {NAME} {kind} named {slug!r}') from None
    return dict(record, kind=kind, url=url(kind, slug))


def scope_assets(record: dict) -> list[dict]:
    return [{'url': s.get('target') or '', 'type': 'smart_contract', 'description': s.get('title') or ''}
            for s in record.get('scopes') or [] if not s.get('out_of_scope') and first_address(s.get('target') or '')]
