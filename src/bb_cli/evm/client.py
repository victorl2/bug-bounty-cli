"""Shared cached HTTP support for the bundled explorer and EVM readers.

No platform requests are made here; providers adapt their scope data to this client.
"""
from __future__ import annotations
import gzip
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Any, Callable, Literal
from bb_cli import __version__
from bb_cli.cache import Cache, Entry

USER_AGENT = f'bug-bounty-cli/{__version__}'
TIMEOUT_SECONDS = 30
Kind = Literal['program','competition']

class FetchError(RuntimeError):
    def __init__(self, message, status=None):
        super().__init__(message)
        self.status=status

class NotFoundError(FetchError): pass

@dataclass
class Result:
    data: Any
    entry: Entry
    from_cache: bool
    stale: bool = False

def http_request(url: str, headers: dict[str, str] | None = None, data: bytes | None = None) -> str:
    """GET (or POST when ``data`` is given) returning the decoded body."""
    req = urllib.request.Request(
        url,
        data=data,
        headers={
            "User-Agent": USER_AGENT,
            "Accept-Encoding": "gzip",
            **(headers or {}),
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT_SECONDS) as resp:
            body = resp.read()
            if resp.headers.get("Content-Encoding") == "gzip":
                body = gzip.decompress(body)
            return body.decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            raise NotFoundError(f"{url} returned 404", 404) from exc
        raise FetchError(f"{url} returned HTTP {exc.code}", exc.code) from exc
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        raise FetchError(f"could not reach {url}: {exc}") from exc


class Client:
    def __init__(self,cache,refresh=False,offline=False):
        self.cache,self.refresh,self.offline=cache,refresh,offline

    def cached(self,key,url,fetch,fallback=None):
        entry=self.cache.read(key)
        if entry and (self.offline or (not self.refresh and self.cache.is_fresh(entry))):
            return Result(entry.data,entry,True)
        if self.offline: raise FetchError('No cached EVM data for '+key+' (offline mode)')
        try:
            data=fetch(url)
        except NotFoundError: raise
        except FetchError as exc:
            if entry:
                print('Warning: provider unavailable; using stale EVM cache',file=sys.stderr)
                return Result(entry.data,entry,True,True)
            raise
        return Result(data,self.cache.write(key,data,url),False)
