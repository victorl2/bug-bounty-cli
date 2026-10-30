"""Platform registry and the normalized discovery record used across sources.

Everything platform-specific the unified CLI needs is declared here, so the router, help text and
error messages stay generic and name a platform only when the user selected or targeted one.
"""
from argparse import Namespace
from dataclasses import dataclass
from types import ModuleType
from .cache import Cache
from .providers.immunefi import cli as immunefi_cli, render as immunefi_render
from .providers.immunefi.client import Client as ImmunefiClient
from .providers.cantina import cli as cantina_cli, discovery
from .providers.cantina.client import Client as CantinaClient, firefox_cookie
from .providers.hackenproof import api as hackenproof_api
from .providers.sherlock import api as sherlock_api


@dataclass(frozen=True)
class Source:
    id: str
    name: str
    site: str
    hosts: tuple[str, ...]
    listings: dict  # kind -> the platform's own page listing them
    native: ModuleType | None = None  # the platform's own CLI (``bb <id> ...``), when it has one
    api: ModuleType | None = None  # otherwise served in-process: listing, candidates, detail, render, scope_assets
    commands: tuple[str, ...] = ()  # platform-only commands also reachable as ``bb <command>``
    features: tuple[str, ...] = ()  # optional capabilities: 'private', 'session', 'vault'


SOURCES = {s.id: s for s in (
    Source('immunefi', 'Immunefi', 'https://immunefi.com', ('immunefi.com', 'www.immunefi.com'),
           {'program': 'https://immunefi.com/bug-bounty/', 'competition': 'https://immunefi.com/audit-competition/'}, immunefi_cli,
           features=('vault',)),
    Source('cantina', 'Cantina', 'https://cantina.xyz', ('cantina.xyz', 'www.cantina.xyz'),
           {'program': 'https://cantina.xyz/bounties', 'competition': 'https://cantina.xyz/competitions'}, cantina_cli,
           commands=('auth', 'leaderboard', 'import-page', 'opportunities'), features=('private', 'session')),
    Source('sherlock', 'Sherlock', 'https://audits.sherlock.xyz', ('audits.sherlock.xyz', 'app.sherlock.xyz', 'sherlock.xyz', 'www.sherlock.xyz'),
           {'program': 'https://audits.sherlock.xyz/bug-bounties', 'competition': 'https://audits.sherlock.xyz/contests'}, api=sherlock_api),
    Source('hackenproof', 'HackenProof', 'https://hackenproof.com', ('hackenproof.com', 'www.hackenproof.com', 'dashboard.hackenproof.com'),
           {'program': 'https://hackenproof.com/programs', 'competition': 'https://hackenproof.com/audit-programs'}, api=hackenproof_api),
)}
PROVIDERS = {sid: s.native for sid, s in SOURCES.items() if s.native}


def source_for_host(host):
    return next((s.id for s in SOURCES.values() if host in s.hosts), None)


def supporting(feature=None, command=None):
    return [s for s in SOURCES.values() if (feature and feature in s.features) or (command and command in s.commands)]


def candidates(source, kind, args, cache_dir):
    """Every opportunity of ``kind`` on ``source`` as (target id, display name, match keys)."""
    if SOURCES[source].api:
        return SOURCES[source].api.candidates(kind, args, cache_dir)
    if source == 'immunefi':
        client = ImmunefiClient(Cache(cache_dir, args.ttl), refresh=args.refresh, offline=args.offline)
        return [(r['slug'], r.get('project') or r['slug'], {r['slug'], r.get('project') or ''})
                for r in client.listing(kind).data if r.get('slug')]
    cookie = firefox_cookie(args.firefox_profile) if getattr(args, 'firefox_profile', None) else None
    client = CantinaClient(cache_dir, args.refresh, args.offline, args.ttl, cookie)
    contest = kind == 'competition'
    return [(r['id'], r.get('name') or r['id'], {r['id'], r.get('name') or '', (r.get('company') or {}).get('handle') or ''})
            for r in client.catalog() if ('contest' in (r.get('kind') or '')) == contest]


def normalize(source, kind, record):
    if source=='immunefi':
        status=(immunefi_render.program_status(record) if kind=='program' else immunefi_render.competition_status(record)).lower()
        uid=record.get('slug')
        path='bug-bounty' if kind=='program' else 'audit-competition'
        return {'source':source,'id':uid,'target':source+':'+str(uid),'kind':kind,
                'name':record.get('project'),'url':record.get('url') or f'https://immunefi.com/{path}/{uid}/information/',
                'status':status,'reward':record.get('rewardsPool') or record.get('maxBounty') or 0,'currency':'USD',
                'kyc':record.get('kyc'),'start':record.get('launchDate'),'end':record.get('endDate'),
                'updated':record.get('updatedDate'),'raw':record}
    return {'source':source,'id':record.get('id'),'target':source+':'+str(record.get('id')),'kind':kind,
            'name':record.get('name'),'url':record.get('url') or 'https://cantina.xyz/'+('bounties/' if kind=='program' else 'competitions/')+record['id'],
            'status':discovery.status(record),'reward':float(record.get('totalRewardPot') or 0),'currency':record.get('currencyCode'),
            'kyc':record.get('kycRequired'),'start':(record.get('timeframe') or {}).get('start'),
            'end':(record.get('timeframe') or {}).get('end'),'updated':record.get('updatedAt'),'raw':record}


def listing(source,kind,args,cache_dir):
    """Apply native provider filters before the caller globally sorts and limits."""
    if SOURCES[source].api:
        return SOURCES[source].api.listing(kind,args,cache_dir)
    native=Namespace(**vars(args));native.limit=None;native.reverse=False
    native.sort=args.sort or ('bounty' if kind=='program' else 'start')
    if source=='immunefi':
        client=ImmunefiClient(Cache(cache_dir,args.ttl),refresh=args.refresh,offline=args.offline)
        native.kind=kind
        native.sort={'reward':'bounty' if kind=='program' else 'pool','pool':'bounty' if kind=='program' else 'pool','bounty':'bounty' if kind=='program' else 'pool','start':'launched' if kind=='program' else 'start','launched':'launched' if kind=='program' else 'start'}.get(native.sort,native.sort)
        sorts=immunefi_cli.PROGRAM_SORTS if kind=='program' else immunefi_cli.COMPETITION_SORTS
        if native.sort not in sorts:
            raise ValueError(f'{native.sort!r} sorting is not available for {kind}s')
        if args.status in ('all','ended'): native.status=None
        elif args.status=='evaluating':native.status='evaluating'
        native.search=args.search or None
        records=immunefi_cli._filter(client.listing(kind).data,native)
        rows=[normalize(source,kind,r) for r in records]
        rows=[r for r in rows if float(r['reward'] or 0)>=args.min_bounty]
        if args.status=='ended':rows=[r for r in rows if r['status'] in ('evaluating','finished')]
        if kind=='program' and args.status not in (None,'all','ended'):
            allowed={'live','upcoming'} if args.status=='active' else {args.status}
            rows=[r for r in rows if r['status'] in allowed]
        return rows
    cookie=firefox_cookie(args.firefox_profile) if args.firefox_profile else None
    client=CantinaClient(cache_dir,args.refresh,args.offline,args.ttl,cookie)
    kinds=['public_bounty','private_bounty'] if kind=='program' else ['public_contest','private_contest']
    groups=['live','ended'] if args.status in ('all','finished','evaluating') or (kind=='competition' and args.status is None) else ['ended'] if args.status=='ended' else ['live']
    records=[]
    for group in groups: records.extend(client.opportunities(kinds,group))
    records=list({r['id']:r for r in records}.values())
    if any([args.language,args.ecosystem,args.program_type,args.project_type]):
        extra={r['id']:r for r in client.catalog()}
        records=[{**extra.get(r['id'],{}),**r} for r in records]
    if native.sort=='updated' and not any(r.get('updatedAt') for r in records):
        client.warnings.append('Cantina has no update dates; its records sort last')
    records=discovery.apply(records,native)
    import sys
    for warning in client.warnings: print('Warning: '+warning,file=sys.stderr)
    return [normalize(source,kind,r) for r in records]
