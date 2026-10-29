"""Platform-neutral command router; each platform's native interface remains available."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import contextlib
import difflib
import io
import json
import math
import os
from pathlib import Path
import sys
import time
from urllib.parse import urlparse
from . import __version__
from .paths import cache_root
from .sources import SOURCES, candidates, listing, source_for_host, supporting
from .evm import contract_cli, render
from .evm.client import FetchError

DETAIL_COMMANDS = ('program', 'bounty', 'competition', 'comp')
SOURCE_HELP = 'platform to use: ' + ', '.join(SOURCES) + ' or all (see `bb sources`)'


def extract_source(argv):
    """Accept --source before or after the command without inspecting flag values."""
    rest=[];selected=None;i=0
    while i<len(argv):
        item=argv[i]
        if item=='--':rest.extend(argv[i:]);break
        if item=='--source' or item.startswith('--source='):
            if item=='--source':
                if i+1>=len(argv):raise ValueError('--source requires one of: '+', '.join([*SOURCES,'all']))
                i+=1;value=argv[i]
            else:value=item.split('=',1)[1]
            if selected is not None and selected!=value:raise ValueError('Conflicting --source values')
            if value not in (*SOURCES,'all'):raise ValueError(f'Unknown source {value!r}; choose one of: '+', '.join([*SOURCES,'all']))
            selected=value
        else:rest.append(item)
        i+=1
    return selected,rest


def target_source(target,selected=None):
    """The platform a target names explicitly (``source:id`` or a platform URL), else the selected one."""
    source=None;value=target
    if ':' in target and '://' not in target:
        prefix,rest=target.split(':',1)
        if prefix in SOURCES:source,value=prefix,rest
    if '://' in target:
        source=source_for_host(urlparse(target).hostname)
        if source is None:raise ValueError('Not an opportunity URL from a supported platform (see `bb sources`)')
    if source and selected not in (None,'all',source):raise ValueError('Target source conflicts with --source')
    return source or (selected if selected!='all' else None),value


def resolve_target(value,kind,args,root):
    """Find a bare name, handle or id on every platform; it must match exactly one opportunity."""
    wanted=value.casefold();matches=[];labels={};failures=[]
    for sid in SOURCES:
        try:rows=candidates(sid,kind,args,root/sid)
        except (FetchError,ValueError,OSError) as exc:failures.append(f'{SOURCES[sid].name}: {exc}');continue
        for target,name,keys in rows:
            labels.setdefault(name.casefold(),[]).append(f'{name} ({sid}:{target})')
            if wanted in {k.casefold() for k in keys if k}:matches.append((sid,target,name))
    if len(matches)==1:return matches[0][:2]
    if matches:raise ValueError(f'{value!r} matches several {kind}s; use one of: '+', '.join(f'{s}:{t} ({n})' for s,t,n in matches))
    other='program' if kind=='competition' else 'competition'
    if not failures:
        with contextlib.suppress(FetchError,ValueError,OSError):
            elsewhere=[f'{sid}:{t}' for sid in SOURCES for t,_,keys in candidates(sid,other,args,root/sid) if wanted in {k.casefold() for k in keys if k}]
            if elsewhere:raise LookupError(f'{value!r} is a {other}: bb {other} {elsewhere[0]}')
    hints=[label for key in difflib.get_close_matches(wanted,list(labels),n=3,cutoff=0.6) for label in labels[key]]
    message=f'No {kind} named {value!r}'+('; did you mean: '+', '.join(hints[:3]) if hints else f'; search with bb {kind}s -s TEXT')
    if failures:message+=' (not searched: '+'; '.join(failures)+')'
    raise ValueError(message)


def common(p):
    p.add_argument('--json',action='store_true',help='machine-readable output')
    p.add_argument('--refresh',action='store_true',help='ignore the cache and fetch again')
    p.add_argument('--offline',action='store_true',help='use cached data only')
    p.add_argument('--ttl',type=float,default=float(os.getenv('BB_CACHE_TTL_HOURS','12')),metavar='HOURS',help='cache lifetime (default: 12)')
    p.add_argument('--cache-dir',type=Path,help='cache root (one subdirectory per platform, plus evm/)')
    p.add_argument('--no-color',action='store_true')
    p.add_argument('--firefox-profile',default=os.getenv('BB_FIREFOX_PROFILE') or os.getenv('CANTINA_FIREFOX_PROFILE'),metavar='PATH|auto',help='Firefox profile with a signed-in session, for platforms that need one')


def build_parser():
    p=argparse.ArgumentParser(prog='bb',description='Discover bug bounties and audit competitions across platforms, read their rules and scope, and inspect in-scope EVM contracts.',
                              epilog='Use --source NAME (before or after the command) to restrict to one platform; `bb sources` lists them. Targets accept a name, a source:id from a listing, or the opportunity URL.')
    p.add_argument('--version',action='version',version='bb '+__version__)
    sub=p.add_subparsers(dest='command',required=True,metavar='<command>')
    for cmd,aliases,what in [('programs',['bounties','ls'],'list bug bounty programs'),('competitions',['comps'],'list audit competitions')]:
        q=sub.add_parser(cmd,aliases=aliases,help=what,description=what.capitalize()+' from every platform, sorted together.');common(q)
        q.add_argument('-s','--search',default='',help='text in the name, id or company');q.add_argument('-e','--ecosystem');q.add_argument('-l','--language')
        q.add_argument('-t','--type',dest='program_type');q.add_argument('--project-type');q.add_argument('--kyc',choices=['yes','no'])
        q.add_argument('--min-bounty',type=float,default=0,metavar='AMOUNT');q.add_argument('--include-paused',action='store_true')
        q.add_argument('--status',choices=['active','upcoming','live','evaluating','finished','ended','all'])
        q.add_argument('--sort',choices=['bounty','reward','pool','name','launched','start','end','updated','vault'],default='bounty' if cmd=='programs' else 'start')
        q.add_argument('-r','--reverse',action='store_true');q.add_argument('-n','--limit',type=int)
    for cmd,aliases,what in [('program',['bounty'],'show a bug bounty program'),('competition',['comp'],'show an audit competition')]:
        q=sub.add_parser(cmd,aliases=aliases,help=what);common(q)
        q.add_argument('target',help='name, source:id or opportunity URL')
        q.add_argument('sections',nargs='*',metavar='SECTION',help='overview (default), information, scope, resources, leaderboard, all')
        q.add_argument('--private',action='store_true',help='participant-only details, on platforms that offer them')
    opts=argparse.ArgumentParser(add_help=False);common(opts)
    contract_cli.add_commands(sub,opts,lambda args:None)
    q=sub.add_parser('cache',help='show or clear cached responses');common(q);q.add_argument('action',choices=['info','clear'],default='info',nargs='?')
    q=sub.add_parser('sources',help='list supported platforms and their extra commands');q.add_argument('--json',action='store_true')
    return p


def _native(source,argv):
    return SOURCES[source].native.main(argv)


def _detail(source,args,root):
    cmd='competition' if args.command in ('competition','comp') else 'program'
    platform=SOURCES[source]
    argv=[cmd,args.target,*args.sections,'--cache-dir',str(root/source),'--ttl',str(args.ttl)]
    for flag in ['json','refresh','offline','no_color']:
        if getattr(args,flag):argv.append('--'+flag.replace('_','-'))
    if args.firefox_profile and 'session' in platform.features:argv+=['--firefox-profile',args.firefox_profile]
    if args.private:
        if 'private' not in platform.features:raise ValueError(f'--private is not available on {platform.name}')
        argv.append('--private')
    if not args.json:return _native(source,argv)
    buffer=io.StringIO()
    with contextlib.redirect_stdout(buffer):code=_native(source,argv)
    if code:return code
    data=json.loads(buffer.getvalue())
    print(json.dumps({'source':source,'kind':cmd,'target':source+':'+args.target,'data':data},indent=2,ensure_ascii=False))
    return 0


def _cache(args,root,selected):
    roots=[root/selected] if selected in SOURCES else [root]
    files=sorted({p for folder in roots for p in folder.rglob('*.json')})
    if args.action=='clear':
        for p in files:p.unlink()
        result={'removed':len(files),'dir':str(root)}
    else:
        entries=[]
        for p in files:
            try:
                e=json.loads(p.read_text());stamp=float(e['fetched_at'])
                entries.append({'key':str(p.relative_to(root)),'source':e.get('url',e.get('source')),'fetched_at':stamp,'fresh':time.time()-stamp<args.ttl*3600})
            except (OSError,ValueError,KeyError,TypeError):continue
        result={'dir':str(root),'ttl_hours':args.ttl,'entries':entries}
    if args.json:print(json.dumps(result,indent=2));return 0
    if args.action=='clear':print(f"Removed {result['removed']} cached responses from {root}");return 0
    groups={}
    for e in result['entries']:
        g=groups.setdefault(e['key'].split('/',1)[0],[0,0]);g[0]+=1;g[1]+=e['fresh']
    style=render.Style(render.use_color(args.no_color))
    print(render.table(['AREA','ENTRIES','FRESH'],[[k,str(n),str(f)] for k,(n,f) in sorted(groups.items())],style,flex=0) if groups else 'Cache is empty')
    print(f"\n{root}  (TTL {args.ttl:g}h)")
    return 0


def _sources(as_json):
    result=[{'id':s.id,'name':s.name,'url':s.site,'commands':list(s.commands),'features':list(s.features)} for s in SOURCES.values()]
    if as_json:print(json.dumps(result,indent=2));return 0
    rows=[[s['id'],s['name'],s['url'],', '.join(s['commands']) or '-'] for s in result]
    print(render.table(['ID','NAME','SITE','EXTRA COMMANDS'],rows,render.Style(render.use_color(False)),flex=3))
    print('\nEvery platform also has its native interface: bb <id> --help')
    return 0


def _platform_command(command,selected,argv):
    owners=[s.id for s in supporting(command=command)]
    if selected in SOURCES:
        if selected not in owners:raise ValueError(f'{command} is not available on {SOURCES[selected].name}')
        return _native(selected,argv)
    if len(owners)>1:raise ValueError(f'{command} exists on several platforms; pass --source')
    return _native(owners[0],argv)


def _listing(args,selected,root):
    kind='competition' if args.command in ('competitions','comps') else 'program'
    sources=list(SOURCES) if selected in (None,'all') else [selected]
    results=[]
    with ThreadPoolExecutor(max_workers=len(sources)) as pool:
        futures={source:pool.submit(listing,source,kind,args,root/source) for source in sources}
        for source,future in futures.items():
            try:results.extend(future.result())
            except (FetchError,ValueError) as exc:raise ValueError(f'{SOURCES[source].name}: {exc}; no partial combined listing was returned') from None
    total=len(results)
    def key(r):
        if args.sort in ('bounty','reward','pool'):return float(r['reward'] or 0)
        if args.sort=='name':return (r['name'] or '').casefold()
        if args.sort=='vault':return float(r['raw'].get('vaultBalance') or 0)
        return r.get({'launched':'start'}.get(args.sort,args.sort)) or ''
    results.sort(key=key,reverse=(args.sort!='name')!=args.reverse)
    if args.limit:results=results[:args.limit]
    if args.json:print(json.dumps(results,indent=2,ensure_ascii=False));return 0
    if not results:print(f'No matching {kind}s.');return 0
    style=render.Style(render.use_color(args.no_color))
    rows=[[r['name'] or '(restricted)',r['target'],r['status'].upper(),render.money(float(r['reward']))+' '+str(r['currency'] or ''),'?' if r['kyc'] is None else 'yes' if r['kyc'] else 'no'] for r in results]
    print(render.table(['PROJECT','TARGET','STATUS','REWARD','KYC'],rows,style,flex=0,styler={2:style.status}))
    print(f'\n{len(results)} of {total} matching {kind}s')
    return 0


def main(argv=None):
    try:
        selected,argv=extract_source(list(sys.argv[1:] if argv is None else argv))
        if argv and argv[0] in SOURCES:
            source=argv.pop(0)
            if selected and selected!=source:raise ValueError('Native command conflicts with --source')
            return _native(source,argv)
        if argv and supporting(command=argv[0]):return _platform_command(argv[0],selected,argv)
        args=build_parser().parse_args(argv)
        if args.command=='sources':return _sources(args.json)
        if args.refresh and args.offline:raise ValueError('--refresh and --offline cannot be combined')
        if not math.isfinite(args.ttl) or args.ttl<0:raise ValueError('TTL must be finite and nonnegative')
        if getattr(args,'limit',None) is not None and args.limit<=0:raise ValueError('Limit must be positive')
        if hasattr(args,'min_bounty') and (not math.isfinite(args.min_bounty) or args.min_bounty<0):raise ValueError('Minimum bounty must be finite and nonnegative')
        root=(args.cache_dir or cache_root()).expanduser()
        if args.command=='cache':return _cache(args,root,selected)
        if args.command in DETAIL_COMMANDS:
            source,args.target=target_source(args.target,selected)
            if source is None:source,args.target=resolve_target(args.target,'competition' if args.command in ('competition','comp') else 'program',args,root)
            return _detail(source,args,root)
        if args.command in ('contract','c'):
            from .adapters import make_evm_client
            source=None  # standalone EVM reads need no platform
            if args.verb=='scope':
                source,args.program=target_source(args.program,selected)
                if source is None:source,args.program=resolve_target(args.program,'competition' if args.competition else 'program',args,root)
            args.cache_dir=root/(source or 'evm');args.evm_cache_dir=root/'evm'
            args.make_client=lambda a:make_evm_client(a,source)
            return args.handler(args,render.Style(render.use_color(args.no_color)))
        return _listing(args,selected,root)
    except BrokenPipeError:return 0
    except KeyboardInterrupt:return 130
    except (ValueError,LookupError,FetchError,OSError) as exc:
        print('bb: '+str(exc),file=sys.stderr);return 1
