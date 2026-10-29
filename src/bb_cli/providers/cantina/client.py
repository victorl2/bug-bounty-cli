"""Cantina's web application API. Only GET requests are supported."""
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, build_opener, HTTPRedirectHandler

class SameOriginRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        from urllib.parse import urlparse
        if urlparse(newurl).scheme != "https" or urlparse(newurl).netloc != urlparse(req.full_url).netloc:
            raise CantinaError("Refusing API redirect to another origin")
        return super().redirect_request(req, fp, code, msg, headers, newurl)

def urlopen(request, timeout=30):
    return build_opener(SameOriginRedirect()).open(request, timeout=timeout)
from bb_cli.rsc import extract_flight, parse_rows, resolve

class CantinaError(ValueError):
    def __init__(self, message, status=None):
        super().__init__(message)
        self.status = status

def page_queries(body):
    rows = parse_rows(extract_flight(body))
    found = {}
    def walk(v):
        if isinstance(v, dict):
            key = v.get('queryKey')
            if isinstance(key, list) and len(key) > 1 and isinstance(key[1], str):
                data = v.get('state', {}).get('data')
                if data is not None:
                    found[key[1]] = data
            for child in v.values(): walk(child)
        elif isinstance(v, list):
            for child in v: walk(child)
    for value in rows.values(): walk(resolve(value, rows))
    if not found: raise CantinaError('Page has no recognized Cantina query data')
    return found

def firefox_cookie(profile):
    """Query a private temporary snapshot when Firefox exclusively locks SQLite."""
    import shutil
    if profile == 'auto':
        import configparser
        import sys
        root = Path.home() / ('Library/Application Support/Firefox' if sys.platform=='darwin' else '.mozilla/firefox')
        cfg = configparser.ConfigParser()
        cfg.read(root/'profiles.ini')
        paths = [cfg[s].get('Default') for s in cfg.sections() if s.startswith('Install') and cfg[s].get('Default')]
        if not paths:
            paths = [cfg[s].get('Path') for s in cfg.sections() if s.startswith('Profile') and cfg[s].get('Default')=='1']
        if len(set(paths)) != 1:
            raise CantinaError('Cannot select a unique default Firefox profile; provide its path')
        profile = root / paths[0]
    source = Path(profile).expanduser().resolve() / 'cookies.sqlite'
    if not source.is_file(): raise CantinaError('Firefox profile has no cookies.sqlite')
    # Copy the DB and committed WAL; never change Firefox's own files. The temporary
    # directory is mode 0700 and removed even if querying or authentication fails.
    with tempfile.TemporaryDirectory(prefix='cantina-session-') as tmp:
        dest = Path(tmp) / 'cookies.sqlite'
        for suffix in ('', '-wal'):
            candidate = Path(str(source) + suffix)
            if candidate.exists():
                shutil.copyfile(candidate, Path(str(dest) + suffix))
                os.chmod(Path(str(dest) + suffix), 0o600)
        db = sqlite3.connect(dest)
        try:
            cols = {r[1] for r in db.execute('PRAGMA table_info(moz_cookies)')}
            container_filter = " AND originAttributes=''" if 'originAttributes' in cols else ''
            rows = db.execute('SELECT name,value FROM moz_cookies WHERE host IN (?, ?, ?, ?) AND (expiry=0 OR expiry>?)' + container_filter,
                               ('cantina.xyz', '.cantina.xyz', 'api.cantina.xyz', '.api.cantina.xyz', int(time.time())))
            return '; '.join(f'{name}={value}' for name, value in rows)
        finally:
            db.close()

class Client:
    def __init__(self, cache_dir=None, refresh=False, offline=False, ttl=12, cookie=None):
        from bb_cli.paths import provider_cache
        self.root = Path(cache_dir).expanduser() if cache_dir else provider_cache('cantina')
        self.refresh, self.offline, self.ttl = refresh, offline, ttl*3600
        self.cookie = cookie or os.getenv('CANTINA_COOKIE', '')
        self.warnings = []

    def get(self, path, query=None, cache=True, site=False):
        import re
        if site:
            if not re.fullmatch(r'/(bounties|competitions)/[a-fA-F0-9-]+(?:/leaderboard)?',path): raise CantinaError('Invalid site path')
        elif not path.startswith('/api/v0/') or '..' in path: raise CantinaError('Invalid API path')
        url = ('https://cantina.xyz' if site else 'https://api.cantina.xyz') + path
        if query: url += '?' + urlencode(query, doseq=True)
        # Authenticated records are never persisted in the public cache.
        cache = cache and not self.cookie
        dest = self.root / (hashlib.sha256(url.encode()).hexdigest() + '.json')
        entry = None
        if cache and dest.exists():
            try: entry = json.loads(dest.read_text())
            except (ValueError, OSError): pass
            if not isinstance(entry,dict) or not isinstance(entry.get('fetched_at'),(int,float)) or 'data' not in entry: entry=None
        if entry and (self.offline or (not self.refresh and time.time()-entry['fetched_at'] < self.ttl)):
            return entry['data']
        if self.offline: raise CantinaError('No cached response available for this request')
        headers = {'Accept':'text/html' if site else 'application/json', 'User-Agent':'cantina-cli/0.2.0'}
        if self.cookie: headers['Cookie'] = self.cookie
        try:
            for attempt in range(3):
                try:
                    with urlopen(Request(url, headers=headers), timeout=30) as response:
                        data = response.read().decode('utf-8') if site else json.load(response)
                    break
                except HTTPError as exc:
                    if exc.code not in (429,500,502,503,504) or attempt==2: raise
                    try: delay=float(exc.headers.get('Retry-After',2**attempt))
                    except (ValueError,AttributeError): delay=2**attempt
                    time.sleep(min(max(delay,0),10))
        except HTTPError as exc:
            if exc.code in (401,403): raise CantinaError(f'Cantina denied access (HTTP {exc.code}); authenticate or check eligibility', exc.code) from None
            if exc.code == 404: raise CantinaError('Opportunity or endpoint not found',404) from None
            raise CantinaError(f'Cantina API returned HTTP {exc.code}',exc.code) from None
        except (URLError, TimeoutError, ValueError) as exc:
            if entry:
                self.warnings.append('Network failed; using stale cache fetched at ' + time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(entry['fetched_at'])))
                return entry['data']
            raise CantinaError('Cannot read Cantina API; check network access and API availability') from None
        if cache:
            self.root.mkdir(parents=True, exist_ok=True)
            fd, tmp = tempfile.mkstemp(dir=self.root)
            try:
                with os.fdopen(fd,'w') as f: json.dump({'url':url,'fetched_at':time.time(),'data':data}, f)
                os.replace(tmp,dest)
            finally:
                if os.path.exists(tmp): os.unlink(tmp)
        return data

    def opportunities(self, kind=None, status='live', limit=None):
        result, cursor, seen = [], None, set()
        while True:
            query = {'limit':50, 'status':status}
            if kind: query['kind'] = ','.join(kind)
            if cursor: query['next'] = cursor
            try:
                page = self.get('/api/v0/opportunities', query)
                if not isinstance(page,dict) or not isinstance(page.get('items'),list): raise CantinaError('Unexpected opportunity response schema')
            except CantinaError as exc:
                if exc.status in (401,403): raise
                if cursor: raise  # never mix a partial listing with a fallback
                self.warnings.append('Opportunity API unavailable; using public repository catalog')
                rows=self.catalog()
                ended={'complete','published','migrated','judging','escalations','escalations_ended'}
                return [r for r in rows if (not kind or r.get('kind') in kind) and ((r.get('status') in ended) if status=='ended' else r.get('status') in ('live','upcoming'))]
            if not isinstance(page,dict) or not isinstance(page.get('items'),list): raise CantinaError('Unexpected opportunity response schema')
            result.extend(page['items'])
            cursor = page.get('nextValue')
            if limit and len(result)>=limit: return result[:limit]
            if not cursor: return result
            if cursor in seen: raise CantinaError('API repeated a pagination cursor')
            seen.add(cursor)

    def catalog(self):
        data=self.get('/api/v0/repositories/public')
        if not isinstance(data,list) or not all(isinstance(r,dict) and 'id' in r for r in data):
            raise CantinaError('Unexpected public repository catalog schema')
        return data

    def detail(self, uid, private=False):
        if private:
            return self.get('/api/v0/repositories/'+uid)
        record=None
        try:
            record=self.get('/api/v0/repositories/'+uid+'/public')
            if not isinstance(record,dict) or record.get('id')!=uid:
                raise CantinaError('Unexpected repository detail schema')
        except CantinaError as exc:
            if exc.status in (401,403): raise
            self.warnings.append('Detail API unavailable; trying public catalog and page data')
        try:
            extra=next((r for r in self.catalog() if r['id']==uid),None)
            if extra:
                record={**extra,**(record or {})}
        except CantinaError as exc:
            if exc.status in (401,403): raise
            self.warnings.append('Public catalog enrichment unavailable')
        if record is None:
            from bb_cli.rsc import Payload
            for kind,key in [('bounties','bounty'),('competitions','competition')]:
                try:
                    payload=Payload(self.get('/'+kind+'/'+uid,site=True))
                    record=payload.find(key,dict,lambda r:r.get('id')==uid)
                    self.warnings.append('Using page fallback; repository metadata may be incomplete')
                    break
                except (CantinaError,ValueError): continue
        if record is None: raise CantinaError('Opportunity not found in public API, catalog or page',404)
        return record

    def leaderboard(self, uid):
        try:
            data=self.get('/api/v0/repositories/'+uid+'/leaderboard/teams')
            return {'available':True,'data':data}
        except CantinaError as exc:
            if exc.status!=404: raise
            # Cantina returns 404 while scores are unpublished. Do not invent ranks.
            return {'available':False,'reason':'Cantina has not published a leaderboard for this competition'}
