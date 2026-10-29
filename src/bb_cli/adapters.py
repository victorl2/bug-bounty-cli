"""Provider-neutral EVM client with provider-specific scope metadata adapters."""
from types import SimpleNamespace
from pathlib import Path
from .cache import Cache
from .paths import cache_root
from .evm.client import Client as EvmClient, FetchError

def make_evm_client(args, source='immunefi'):
    from .providers.immunefi.cli import _make_client
    from .providers.cantina.client import Client as CantinaClient, CantinaError, firefox_cookie
    from .providers.cantina.cli import resolve_target
    from .providers.cantina.contracts import scope
    class Adapter(EvmClient):
        def detail(self,kind,target):
            if source=='immunefi': return _make_client(args).detail(kind,target)
            try:
                profile=getattr(args,'firefox_profile',None)
                api=CantinaClient(args.cache_dir,args.refresh,args.offline,args.ttl,firefox_cookie(profile) if profile else None)
                uid=resolve_target(api,target,kind)
                record=api.detail(uid)
                scoped=scope(record)
                entries=[{'url':r['url'],'type':'smart_contract','description':r.get('name')} for r in scoped['contracts']]
                entries += [{'url':r['reference'],'type':'smart_contract','description':r['reason']} for r in scoped['unresolved']]
                for warning in api.warnings:
                    import sys
                    print('Warning: '+warning,file=sys.stderr)
                return SimpleNamespace(data={'slug':uid,'project':record.get('name'),'assets':entries})
            except CantinaError as exc: raise FetchError(str(exc)) from None
    ttl=args.ttl if args.ttl is not None else 12
    return Adapter(Cache(Path(getattr(args,'evm_cache_dir',None) or cache_root()/'evm'),ttl),refresh=args.refresh,offline=args.offline)
