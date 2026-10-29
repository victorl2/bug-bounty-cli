"""Filters use explicit metadata and file/explorer references, never guessed chain names."""
import re
from datetime import datetime, timezone
from .client import CantinaError

EVALUATING={'judging','escalations','escalations_ended'}
FINISHED={'complete','published','migrated'}

def status(record):
    raw=record.get('status') or ''
    if raw in EVALUATING: return 'evaluating'
    if raw in FINISHED: return 'finished'
    if raw=='live':
        try:
            start=datetime.fromisoformat(record['timeframe']['start'].replace('Z','+00:00'))
            end=record.get('timeframe',{}).get('end')
            now=datetime.now(timezone.utc)
            if start>now: return 'upcoming'
            if end and datetime.fromisoformat(end.replace('Z','+00:00'))<=now: return 'evaluating'
        except (ValueError,KeyError,TypeError): pass
    return raw


def evidence(record):
    from .cli import assets
    from bb_cli.evm.explorers import host_of
    refs=[a.get('reference') or '' for a in assets(record) if not a['outOfScope']]
    text='\n'.join(refs)+'\n'+(record.get('instructions') or '')
    languages=[]
    for ext,name in [('sol','Solidity'),('rs','Rust'),('move','Move'),('vy','Vyper'),('cairo','Cairo'),('go','Go')]:
        if re.search(r'\.'+ext+r'\b',text): languages.append(name)
    ecosystems=[]
    # The bundled explorer registry is authoritative for URLs; avoid RPC/network calls.
    # Registry chain mapping is performed separately by chain_for_url when available;
    # discovery uses common, unambiguous explorer hosts only.
    hosts={'etherscan.io':'Ethereum','arbiscan.io':'Arbitrum','basescan.org':'Base','optimistic.etherscan.io':'Optimism','bscscan.com':'BSC','polygonscan.com':'Polygon','snowtrace.io':'Avalanche','solscan.io':'Solana','suivision.xyz':'Sui','starkscan.co':'Starknet'}
    for ref in refs:
        host=host_of(ref)
        if host in hosts: ecosystems.append(hosts[host])
    types=[g.get('name') or '' for g in record.get('assetGroups') or [] if not g.get('outOfScope')]
    tags=record.get('tags') or {}
    return {'language':sorted(set(languages+list(tags.get('language') or []))),
            'ecosystem':sorted(set(ecosystems+list(tags.get('ecosystem') or []))),
            'programType':types+list(tags.get('programType') or []),
            'projectType':list(tags.get('projectType') or []),
            'source':'explicit tags, scope group names, explorer URLs and file extensions in instructions/references'}


def apply(records,args):
    out=[]
    for r in records:
        wanted=args.status
        actual=status(r)
        if wanted not in ('all','ended',None):
            allowed={'upcoming','live'} if wanted=='active' else {wanted}
            if actual not in allowed: continue
        if wanted=='ended' and actual not in EVALUATING|FINISHED|{'evaluating','finished'}: continue
        if args.search.lower() not in (' '.join(str(v or '') for v in [r.get('id'),r.get('name'),(r.get('company') or {}).get('name'),(r.get('company') or {}).get('handle')])).lower(): continue
        if float(r.get('totalRewardPot') or 0)<args.min_bounty: continue
        if args.kyc and (r.get('kycRequired') is None or bool(r.get('kycRequired')) != (args.kyc=='yes')): continue
        if any([args.language,args.ecosystem,args.program_type,args.project_type]):
            ev=evidence(r)
            pairs=[(args.language,'language'),(args.ecosystem,'ecosystem'),(args.program_type,'programType'),(args.project_type,'projectType')]
            if any(needle and not any(needle.casefold() in str(tag).casefold() for tag in ev[key]) for needle,key in pairs): continue
            r=dict(r,filterEvidence=ev)
        out.append(r)
    sort=args.sort
    if sort=='vault': raise CantinaError('Cantina does not publish an Immunefi-style vault balance; sort by bounty/reward instead')
    if args.project_type and not any((r.get('tags') or {}).get('projectType') for r in records):
        raise CantinaError('Cantina does not expose project-type tags; this filter cannot be applied reliably')
    def key(r):
        if sort in ('reward','bounty','pool'): return float(r.get('totalRewardPot') or 0)
        if sort=='name': return (r.get('name') or '').casefold()
        if sort in ('start','launched'): return (r.get('timeframe') or {}).get('start') or ''
        if sort=='end': return (r.get('timeframe') or {}).get('end') or ''
        return r.get('updatedAt') or r.get('createdAt') or ''
    out.sort(key=key,reverse=(sort!='name') != args.reverse)
    return out[:args.limit] if args.limit else out
