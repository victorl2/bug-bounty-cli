"""One cache root, namespaced provider responses and shared explorer/EVM metadata."""
import os
from pathlib import Path

def cache_root():
    return Path(os.environ.get('BB_CACHE_DIR') or Path(os.environ.get('XDG_CACHE_HOME',Path.home()/'.cache'))/'bug-bounty-cli').expanduser()

def provider_cache(source):
    legacy=os.environ.get(source.upper()+'_CACHE_DIR')
    return Path(legacy).expanduser() if legacy else cache_root()/source
