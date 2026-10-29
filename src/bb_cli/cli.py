"""Multi-provider command router; native provider interfaces remain available."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import contextlib
import io
import json
import math
import os
from pathlib import Path
import sys
from urllib.parse import urlparse
from . import __version__
from .paths import cache_root
from .sources import PROVIDERS, listing
from .evm import contract_cli, render
from .evm.client import FetchError
from .providers.cantina.client import CantinaError


def extract_source(argv):
    """Accept --source before or after the command without inspecting flag values."""
    rest=[];selected=None;i=0
    while i<len(argv):
        item=argv[i]
        if item=='--':rest.extend(argv[i:]);break
        if item=='--source' or item.startswith('--source='):
            if item=='--source':
                if i+1>=len(argv):raise ValueError('--source requires immunefi, cantina or all')
                i+=1;value=argv[i]
            else:value=item.split('=',1)[1]
            if selected is not None and selected!=value:raise ValueError('Conflicting --source values')
            if value not in (*PROVIDERS,'all'):raise ValueError('Unknown source '+value)
            selected=value
        else:rest.append(item)
        i+=1
    return selected,rest


def target_source(target,selected=None):
    source=None;value=target
    if ':' in target and '://' not in target:
        prefix,rest=target.split(':',1)
        if prefix in PROVIDERS:source,value=prefix,rest
    if '://' in target:
        host=urlparse(target).hostname
        source={'immunefi.com':'immunefi','www.immunefi.com':'immunefi','cantina.xyz':'cantina'}.get(host)
        if source is None:raise ValueError('Use an Immunefi or Cantina opportunity URL')
    if selected=='all':raise ValueError('Select one source for a detail or scope request')
    if source and selected and source!=selected:raise ValueError('Target source conflicts with --source')
    return source or selected or 'immunefi',value


def common(p):
    p.add_argument('--json',action='store_true')
    p.add_argument('--refresh',action='store_true')
    p.add_argument('--offline',action='store_true')
    p.add_argument('--ttl',type=float,default=float(os.getenv('BB_CACHE_TTL_HOURS','12')))
    p.add_argument('--cache-dir',type=Path,help='Unified cache root (provider and EVM subdirectories)')
    p.add_argument('--no-color',action='store_true')
    p.add_argument('--firefox-profile',default=os.getenv('CANTINA_FIREFOX_PROFILE'),help='Cantina session: profile path or auto')


def build_parser():
    p=argparse.ArgumentParser(prog='bb',description='Bug bounty discovery and read-only EVM inspection across Immunefi and Cantina',epilog='Use --source immunefi|cantina|all anywhere. Lists default to all; bare detail targets default to Immunefi. Native interfaces: bb immunefi … / bb cantina …')
    p.add_argument('--version',action='version',version='bb '+__version__)
    sub=p.add_subparsers(dest='command',required=True)
    for cmd,aliases in [('programs',['bounties','ls']),('competitions',['comps'])]:
        q=sub.add_parser(cmd,aliases=aliases);common(q)
        q.add_argument('-s','--search',default='');q.add_argument('-e','--ecosystem');q.add_argument('-l','--language')
        q.add_argument('-t','--type',dest='program_type');q.add_argument('--project-type');q.add_argument('--kyc',choices=['yes','no'])
        q.add_argument('--min-bounty',type=float,default=0);q.add_argument('--include-paused',action='store_true')
        q.add_argument('--status',choices=['active','upcoming','live','evaluating','finished','ended','all'])
        q.add_argument('--sort',choices=['bounty','reward','pool','name','launched','start','end','updated','vault'],default='bounty' if cmd=='programs' else 'start')
        q.add_argument('-r','--reverse',action='store_true');q.add_argument('-n','--limit',type=int)
    for cmd,aliases in [('program',['bounty']),('competition',['comp'])]:
        q=sub.add_parser(cmd,aliases=aliases);common(q);q.add_argument('target');q.add_argument('sections',nargs='*');q.add_argument('--private',action='store_true')
    opts=argparse.ArgumentParser(add_help=False);common(opts)
    contract_cli.add_commands(sub,opts,lambda args:None)
    q=sub.add_parser('cache');common(q);q.add_argument('action',choices=['info','clear'],default='info',nargs='?')
    for name in ('auth','leaderboard','import-page','opportunities'):
        q=sub.add_parser(name,help='Cantina-specific command; use bb cantina '+name+' --help');q.add_argument('arguments',nargs=argparse.REMAINDER)
    q=sub.add_parser('sources',help='List installed source adapters');q.add_argument('--json',action='store_true')
    return p


def _native(source,argv):
    return PROVIDERS[source].main(argv)


def _detail(source,args,root):
    cmd='competition' if args.command in ('competition','comp') else 'program'
    argv=[cmd,args.target,*args.sections,'--cache-dir',str(root/source),'--ttl',str(args.ttl)]
    for flag in ['json','refresh','offline','no_color']:
        if getattr(args,flag):argv.append('--'+flag.replace('_','-'))
    if args.firefox_profile and source=='cantina':argv+=['--firefox-profile',args.firefox_profile]
    if args.private:
        if source!='cantina':raise ValueError('--private is only available for Cantina')
        argv.append('--private')
    if not args.json:return _native(source,argv)
    buffer=io.StringIO()
    with contextlib.redirect_stdout(buffer):code=_native(source,argv)
    if code:return code
    data=json.loads(buffer.getvalue())
    print(json.dumps({'source':source,'kind':cmd,'target':source+':'+args.target,'data':data},indent=2,ensure_ascii=False))
    return 0


def _cache(args,root,selected):
    import time
    roots=[root/selected] if selected in PROVIDERS else [root]
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
    if args.json:print(json.dumps(result,indent=2))
    else:
        from .providers.cantina.render import render as display
        display(result)
    return 0


def main(argv=None):
    try:
        selected,argv=extract_source(list(sys.argv[1:] if argv is None else argv))
        if argv and argv[0] in PROVIDERS:
            source=argv.pop(0)
            if selected and selected!=source:raise ValueError('Native command conflicts with --source')
            return _native(source,argv)
        if argv and argv[0] in ('auth','leaderboard','import-page','opportunities'):
            if selected not in (None,'cantina'):raise ValueError(argv[0]+' is currently a Cantina-only command')
            return _native('cantina',argv)
        args=build_parser().parse_args(argv)
        if args.command=='sources':
            result=[{'id':s,'url':'https://'+('immunefi.com' if s=='immunefi' else 'cantina.xyz')} for s in PROVIDERS]
            print(json.dumps(result,indent=2) if args.json else '\n'.join(r['id']+'  '+r['url'] for r in result));return 0
        if args.refresh and args.offline:raise ValueError('--refresh and --offline cannot be combined')
        if not math.isfinite(args.ttl) or args.ttl<0:raise ValueError('TTL must be finite and nonnegative')
        if getattr(args,'limit',None) is not None and args.limit<=0:raise ValueError('Limit must be positive')
        if hasattr(args,'min_bounty') and (not math.isfinite(args.min_bounty) or args.min_bounty<0):raise ValueError('Minimum bounty must be finite and nonnegative')
        root=(args.cache_dir or cache_root()).expanduser()
        if args.command=='cache':return _cache(args,root,selected)
        if args.command in ('program','bounty','competition','comp'):
            source,args.target=target_source(args.target,selected)
            return _detail(source,args,root)
        if args.command in ('contract','c'):
            from .adapters import make_evm_client
            source=selected or 'immunefi'
            if args.verb=='scope':source,args.program=target_source(args.program,selected)
            elif selected=='all':source='immunefi'  # standalone EVM reads are source-independent
            args.cache_dir=root/source;args.evm_cache_dir=root/'evm'
            args.make_client=lambda a:make_evm_client(a,source)
            return args.handler(args,render.Style(render.use_color(args.no_color)))
        kind='competition' if args.command in ('competitions','comps') else 'program'
        sources=list(PROVIDERS) if selected in (None,'all') else [selected]
        results=[]
        with ThreadPoolExecutor(max_workers=len(sources)) as pool:
            futures={source:pool.submit(listing,source,kind,args,root/source) for source in sources}
            for source,future in futures.items():
                try:results.extend(future.result())
                except (FetchError,CantinaError,ValueError) as exc:raise ValueError(f'{source}: {exc}; no partial combined listing was returned') from None
        total=len(results)
        def key(r):
            if args.sort in ('bounty','reward','pool'):return float(r['reward'] or 0)
            if args.sort=='name':return (r['name'] or '').casefold()
            if args.sort=='vault':return float(r['raw'].get('vaultBalance') or 0)
            return r.get({'launched':'start'}.get(args.sort,args.sort)) or ''
        results.sort(key=key,reverse=(args.sort!='name')!=args.reverse)
        if args.limit:results=results[:args.limit]
        if args.json:print(json.dumps(results,indent=2,ensure_ascii=False))
        else:
            style=render.Style(render.use_color(args.no_color))
            rows=[[r['source'],r['name'] or '(restricted)',r['id'],r['status'],render.money(float(r['reward']))+' '+str(r['currency'] or ''),'?' if r['kyc'] is None else 'yes' if r['kyc'] else 'no'] for r in results]
            print(render.table(['SOURCE','PROJECT','ID','STATUS','REWARD','KYC'],rows,style,flex=1))
            print(f'\n{len(results)} of {total} matching opportunities')
        return 0
    except BrokenPipeError:return 0
    except KeyboardInterrupt:return 130
    except (ValueError,FetchError,OSError) as exc:
        print('bb: '+str(exc),file=sys.stderr);return 1
