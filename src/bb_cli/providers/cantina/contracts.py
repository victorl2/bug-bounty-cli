"""Adapt Cantina metadata to the bundled MIT-licensed EVM inspection engine."""
from pathlib import Path
import re
from types import SimpleNamespace
from urllib.parse import urlparse
from .client import Client, CantinaError, firefox_cookie


def parse_target(value):
    kind = 'competition' if '/competitions/' in value else None
    return kind, value


def scope(record):
    from .cli import assets
    refs, skipped, seen = [], [], set()
    for asset in assets(record):
        if asset['outOfScope']: continue
        ref = asset.get('reference') or ''
        address = re.search(r'0x[0-9a-fA-F]{40}(?![0-9a-fA-F])', ref)
        if not address: continue
        parsed = urlparse(ref)
        if parsed.scheme not in ('https','http') or not re.search(r'/(address|token)/0x[0-9a-fA-F]{40}(?:[/?#]|$)',parsed.path):
            # Commit hashes in source paths are not deployment addresses.
            if parsed.hostname not in ('github.com','raw.githubusercontent.com'):
                skipped.append({'reference':ref,'reason':'Address has no explorer chain context'})
            continue
        key=(parsed.hostname,address[0].lower())
        if key in seen: continue
        seen.add(key)
        refs.append({'address':address[0],'url':ref,'name':asset.get('name'),'group':asset.get('group')})
    return {'id':record.get('id'),'name':record.get('name'),'contracts':refs,'unresolved':skipped}


def make_client(args):
    from bb_cli.adapters import make_evm_client
    return make_evm_client(args,'cantina')
