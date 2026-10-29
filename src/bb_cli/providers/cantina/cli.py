import argparse
import json
from pathlib import Path
import re
import sys
import sqlite3
import math
from urllib.parse import urlparse
from bb_cli import __version__
from .client import Client, CantinaError, firefox_cookie, page_queries

SECTIONS = ['overview','information','scope','resources','leaderboard','all']

def assets(record):
    result = []
    def group(g, excluded=False):
        excluded = excluded or bool(g.get('outOfScope'))
        if g.get('hidden'): return
        for a in g.get('assets') or []:
            if not a.get('hidden'): result.append(dict(a, outOfScope=excluded, group=g.get('name')))
        for sub in g.get('subGroups') or []: group(sub, excluded)
    for g in record.get('assetGroups') or []: group(g)
    return result

def section(record, name):
    if name == 'all': return record
    if name == 'leaderboard': return record.get('leaderboard', {'available':False,'reason':'Leaderboard not requested'})
    if name == 'overview': return {k:v for k,v in record.items() if k not in ('assetGroups','instructions','leaderboard')}
    if name == 'information': return {k:v for k,v in record.items() if k in ('id','name','instructions','kycRequired','submissionFee','allowedSeverities','timeframe','status','totalRewardPot','currencyCode')}
    if name == 'scope': return {'id':record.get('id'),'assetGroups':record.get('assetGroups',[]),'assets':assets(record),'instructions':record.get('instructions'),'gitRepoUrl':record.get('gitRepoUrl'),'commitHash':record.get('commitHash')}
    links=list(dict.fromkeys(re.findall(r'https?://[^\s<>\)]+',record.get('instructions') or '')))
    return {'id':record.get('id'),'company':record.get('company'),'gitRepoUrl':record.get('gitRepoUrl'),'commitHash':record.get('commitHash'),'readmePath':record.get('readmePath'),'instructionLinks':links,'resources':[a for a in assets(record) if not a['outOfScope']]}


def resolve_target(client, value, kind=None):
    try: return identifier(value)
    except CantinaError:
        if '://' in value: raise
    rows=client.catalog()
    wanted=value.casefold()
    matches=[r for r in rows if (not kind or ('contest' in r.get('kind',''))==(kind=='competition')) and wanted in [(r.get('name') or '').casefold(), ((r.get('company') or {}).get('handle') or '').casefold()]]
    if len(matches)==1: return matches[0]['id']
    if matches: raise CantinaError('Ambiguous name; use a UUID: '+', '.join(str(r.get('name'))+' '+r['id'] for r in matches[:5]))
    import difflib
    hints=difflib.get_close_matches(value,[r['name'] for r in rows if r.get('name')],n=3,cutoff=0.5)
    raise CantinaError('No opportunity named '+value+('; did you mean '+', '.join(hints)+'?' if hints else '; use programs/competitions --search'))


def identifier(value):
    if '://' in value:
        u = urlparse(value)
        if u.hostname != 'cantina.xyz' or not re.fullmatch(r'/(bounties|competitions)/[0-9a-fA-F-]+/?',u.path): raise CantinaError('Expected a Cantina bounty or competition URL')
        value = u.path.rstrip('/').split('/')[-1]
    if not re.fullmatch(r'[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}',value): raise CantinaError('Use an opportunity UUID or Cantina URL from the listing')
    return value

def flags(p):
    p.add_argument('--json',action='store_true')
    p.add_argument('--no-color',action='store_true')
    p.add_argument('--refresh',action='store_true')
    p.add_argument('--offline',action='store_true')
    p.add_argument('--ttl',type=float,default=float(__import__('os').getenv('CANTINA_CACHE_TTL_HOURS','12')),metavar='HOURS')
    p.add_argument('--cache-dir')
    p.add_argument('--firefox-profile',default=__import__('os').getenv('CANTINA_FIREFOX_PROFILE'),help='Read only Cantina cookies from this Firefox profile; kept in memory')

def parser():
    p=argparse.ArgumentParser(description='Read-only Cantina opportunities, rewards, rules, scope and resources')
    p.add_argument('--version',action='version',version=__version__)
    sub=p.add_subparsers(dest='command',required=True)
    for cmd, aliases in [('opportunities',[]),('programs',['bounties','ls']),('competitions',['comps'])]:
        q=sub.add_parser(cmd,aliases=aliases); flags(q)
        q.add_argument('--status',default=None,choices=['live','ended','upcoming','evaluating','finished','active','all'],help='Filter lifecycle status (default: current bounties; all competitions)')
        q.add_argument('-e','--ecosystem');q.add_argument('-l','--language')
        q.add_argument('-t','--type',dest='program_type');q.add_argument('--project-type')
        q.add_argument('--kyc',choices=['yes','no']);q.add_argument('-r','--reverse',action='store_true')
        q.add_argument('--include-paused',action='store_true',help='Include paused records if Cantina publishes them')
        q.add_argument('-s','--search',default='');q.add_argument('--min-bounty',type=float,default=0)
        q.add_argument('--sort',choices=['reward','bounty','pool','vault','name','updated','launched','start','end'],default='start' if cmd=='competitions' else 'bounty')
        q.add_argument('-n','--limit',type=int)
    for cmd,aliases in [('program',['bounty']),('competition',['comp'])]:
        q=sub.add_parser(cmd,aliases=aliases);flags(q)
        q.add_argument('--private',action='store_true',help='Read participant repository details instead of public metadata');q.add_argument('target');q.add_argument('sections',nargs='*',metavar='SECTION',help='overview, information, scope, resources, all')
    from bb_cli.evm import contract_cli
    from .contracts import make_client
    common=argparse.ArgumentParser(add_help=False);flags(common)
    contract_cli.add_commands(sub,common,make_client)
    q=sub.add_parser('auth');flags(q)
    q=sub.add_parser('leaderboard');flags(q)
    q=sub.add_parser('import-page',help='Decode query data from saved Cantina HTML');flags(q);q.add_argument('file')
    q=sub.add_parser('cache');q.add_argument('action',choices=['info','clear'],default='info',nargs='?');flags(q)
    return p

def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    args=parser().parse_args(argv)
    try:
        if not math.isfinite(args.ttl) or args.ttl<0: raise CantinaError('TTL must be nonnegative')
        if args.refresh and args.offline: raise CantinaError('--refresh and --offline cannot be combined')
        if hasattr(args,'min_bounty') and (not math.isfinite(args.min_bounty) or args.min_bounty<0): raise CantinaError('Minimum bounty must be finite and nonnegative')
        if getattr(args,'limit',None) is not None and args.limit<1: raise CantinaError('Limit must be positive')
        if args.command in ('contract','c'):
            from bb_cli.evm import render as evm_render
            from bb_cli.evm.client import FetchError
            try: return args.handler(args,evm_render.Style(evm_render.use_color(args.no_color)))
            except FetchError as exc: raise CantinaError(str(exc)) from None
        cookie=firefox_cookie(args.firefox_profile) if args.firefox_profile else None
        if args.firefox_profile and not cookie: raise CantinaError('No unexpired Cantina cookies in this Firefox profile')
        c=Client(args.cache_dir,args.refresh,args.offline,args.ttl,cookie)
        cmd=args.command
        if cmd == 'cache':
            files=sorted(c.root.rglob('*.json'))
            if args.action == 'clear':
                for f in files: f.unlink()
                result={'removed':len(files)}
            else:
                import time
                entries=[]
                for f in files:
                    try:
                        entry=json.loads(f.read_text())
                        stamp=entry['fetched_at']
                        entries.append({'key':str(f.relative_to(c.root)),'source':entry.get('url',entry.get('source')),'fetched_at':stamp,'fresh':time.time()-stamp<c.ttl})
                    except (ValueError,KeyError,OSError,TypeError): pass
                result={'dir':str(c.root),'ttl_hours':args.ttl,'entries':entries}
        elif cmd == 'import-page': result=page_queries(Path(args.file).read_text())
        elif cmd == 'auth':
            if not c.cookie: raise CantinaError('Set CANTINA_COOKIE or use --firefox-profile to check authentication')
            me=c.get('/api/v0/auth/me',cache=False)
            if not me: raise CantinaError('Cantina session is not authenticated')
            result={'authenticated':True}
        elif cmd == 'leaderboard': result=c.get('/api/v0/leaderboard')
        elif cmd in ('program','bounty','competition','comp'):
            if any(s not in SECTIONS for s in args.sections): raise CantinaError('Unknown section; choose '+', '.join(SECTIONS))
            target=resolve_target(c,args.target,'competition' if cmd in ('competition','comp') else 'program')
            result=c.detail(target,args.private)
            if not isinstance(result,dict): raise CantinaError('Unexpected repository response schema')
            names=args.sections or ['overview']
            is_comp='contest' in result.get('kind','') or cmd in ('competition','comp')
            if is_comp and any(s in names for s in ('leaderboard','all')):
                result=dict(result,leaderboard=c.leaderboard(target))
            result=section(result,names[0]) if len(names)==1 else {s:section(result,s) for s in names}

        else:
            kind=['public_bounty','private_bounty'] if cmd in ('programs','bounties','ls') else ['public_contest','private_contest'] if cmd in ('competitions','comps') else None
            wanted=args.status
            groups=['live','ended'] if wanted in ('all','evaluating','finished') or (wanted is None and cmd in ('competitions','comps')) else ['ended'] if wanted=='ended' else ['live']
            result=[]
            for group in groups: result.extend(c.opportunities(kind,group))
            result=list({r['id']:r for r in result}.values())
            if args.language or args.ecosystem or args.program_type or args.project_type:
                catalog={r['id']:r for r in c.catalog()}
                result=[{**catalog.get(r['id'],{}),**r} for r in result]
                c.warnings.append('Tag filters use explicit metadata and references; records with no matching evidence are excluded')
            from .discovery import apply
            if args.sort=='updated' and not any(r.get('updatedAt') for r in result):
                c.warnings.append('Cantina provides no update timestamps; updated sorting uses creation time')
            if args.include_paused:
                c.warnings.append('Cantina has no published paused-state equivalent; only published records can be returned')
            total_count=len(result)
            result=apply(result,args)

        for warning in c.warnings: print('Warning: '+warning,file=sys.stderr)
        if args.json: print(json.dumps(result,indent=2,ensure_ascii=False))
        elif isinstance(result,list) and result and all(isinstance(r,dict) and 'name' in r and 'id'in r for r in result):
            from bb_cli.evm import render as tables
            from .discovery import status
            style=tables.Style(tables.use_color(args.no_color))
            rows=[]
            for r in result:
                reward=tables.money(float(r.get('totalRewardPot') or 0))+' '+(r.get('currencyCode') or '')
                rows.append([r.get('name') or '(restricted)',r['id'],status(r),reward,'?' if r.get('kycRequired') is None else 'yes' if r['kycRequired'] else 'no',r.get('submissionFee') or '-'])
            print(tables.table(['PROJECT','ID','STATUS','REWARD','KYC','FEE'],rows,style,flex=0))
            print(f"\n{len(result)} of {locals().get('total_count',len(result))} opportunities")

        else:
            from .render import render
            render(result)
        return 0
    except (CantinaError,OSError,ValueError,sqlite3.Error) as exc:
        print(f'cantina: {exc}',file=sys.stderr);return 1
    except BrokenPipeError: return 0
    except KeyboardInterrupt: return 130
