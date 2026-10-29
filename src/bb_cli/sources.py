"""Provider registry and the normalized discovery record used across sources."""
from argparse import Namespace
from .cache import Cache
from .providers.immunefi import cli as immunefi_cli, render as immunefi_render
from .providers.immunefi.client import Client as ImmunefiClient
from .providers.cantina import cli as cantina_cli, discovery
from .providers.cantina.client import Client as CantinaClient, firefox_cookie

PROVIDERS={'immunefi':immunefi_cli,'cantina':cantina_cli}


def normalize(source, kind, record):
    if source=='immunefi':
        status=(immunefi_render.program_status(record) if kind=='program' else immunefi_render.competition_status(record)).lower()
        if kind=='program' and status=='active': status='live'
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
    native=Namespace(**vars(args));native.limit=None;native.reverse=False
    if source=='immunefi':
        client=ImmunefiClient(Cache(cache_dir,args.ttl),refresh=args.refresh,offline=args.offline)
        native.kind=kind
        native.sort={'reward':'bounty' if kind=='program' else 'pool','pool':'bounty' if kind=='program' else 'pool','bounty':'bounty' if kind=='program' else 'pool','start':'launched' if kind=='program' else 'start','launched':'launched' if kind=='program' else 'start'}.get(args.sort,args.sort)
        sorts=immunefi_cli.PROGRAM_SORTS if kind=='program' else immunefi_cli.COMPETITION_SORTS
        if native.sort not in sorts:
            raise ValueError(f'{source} does not support {args.sort!r} sorting for {kind}s')
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
        client.warnings.append('Cantina tag filters use explicit tags and reference evidence, not a complete provider taxonomy')
    if args.sort=='updated' and not any(r.get('updatedAt') for r in records):
        client.warnings.append('Cantina publishes no update timestamps; normalized updated values remain unknown')
    if args.include_paused:
        client.warnings.append('Cantina publishes no equivalent paused state')
    records=discovery.apply(records,native)
    import sys
    for warning in client.warnings: print('Warning: '+warning,file=sys.stderr)
    return [normalize(source,kind,r) for r in records]
