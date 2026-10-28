"""Fetches Immunefi data, going through the cache first.

Primary source: each page's React Server Components stream (``RSC: 1``), which is the exact
data the site renders, including the Resources tab and vault. Fallback, when a page cannot be
fetched or decoded and nothing is cached: ``/public-api/bounties.json``, Immunefi's JSON API.
It holds every program and competition but no codebase/documentation links or vault data.
"""

from __future__ import annotations

import gzip
import json
import re
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Any, Callable, Literal

from immunefi_cli import __version__, rsc
from immunefi_cli.cache import Cache, Entry

BASE_URL = "https://immunefi.com"
PUBLIC_API_URL = f"{BASE_URL}/public-api/bounties.json"
RSC_CACHE_KEY = "immunefi-cli"
USER_AGENT = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 "
    f"(KHTML, like Gecko) Chrome/130.0 Safari/537.36 immunefi-cli/{__version__}"
)
TIMEOUT_SECONDS = 30

Kind = Literal["program", "competition"]
KIND_PATH: dict[Kind, str] = {"program": "bug-bounty", "competition": "audit-competition"}
_SLUG_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
_URL_RE = re.compile(r"(bug-bounty|audit-competition)/([^/?#]+)")


class FetchError(RuntimeError):
    pass


class NotFoundError(FetchError):
    pass


@dataclass
class Result:
    data: Any
    entry: Entry
    from_cache: bool
    stale: bool = False
    degraded: bool = False  # served from the public API fallback, which lacks some fields


def parse_target(value: str) -> tuple[Kind | None, str]:
    """Accept a bare slug or any immunefi.com program/competition URL."""
    kind: Kind | None = None
    slug = value.strip().strip("/")
    if m := _URL_RE.search(value):
        kind = "program" if m.group(1) == "bug-bounty" else "competition"
        slug = m.group(2)
    if not _SLUG_RE.match(slug):
        raise ValueError(f"invalid slug: {value!r}")
    return kind, slug


def _http_get(url: str, headers: dict[str, str] | None = None) -> str:
    req = urllib.request.Request(
        url,
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
            raise NotFoundError(f"{url} returned 404") from exc
        raise FetchError(f"{url} returned HTTP {exc.code}") from exc
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        raise FetchError(f"could not reach {url}: {exc}") from exc


def _fetch_page(url: str) -> rsc.Payload:
    """Fetch a page's RSC stream directly instead of the HTML that wraps it."""
    # _rsc is the cache-busting parameter Next.js itself adds, so shared caches never
    # hand back the HTML variant for this URL.
    body = _http_get(f"{url}?_rsc={RSC_CACHE_KEY}", {"RSC": "1", "Accept": "text/x-component"})
    payload = rsc.Payload(body)
    if payload.not_found:
        raise NotFoundError(f"{url} returned 404")
    return payload


def _api_kind(item: dict) -> Kind:
    return "competition" if item.get("endDate") else "program"


def from_public_api(item: dict) -> dict:
    """Shape a public-api/bounties.json entry like the ``bounty`` object pages carry."""
    tags = {k: item.get(k) or [] for k in ("language", "ecosystem", "productType", "programType", "projectType")}
    return {**item, "tags": tags, "vaults": [], "source": "public-api"}


class Client:
    def __init__(self, cache: Cache, refresh: bool = False, offline: bool = False):
        self.cache = cache
        self.refresh = refresh
        self.offline = offline

    def _cached(
        self,
        key: str,
        url: str,
        fetch: Callable[[str], Any],
        fallback: Callable[[], Result] | None = None,
    ) -> Result:
        entry = self.cache.read(key)
        if entry and (self.offline or (not self.refresh and self.cache.is_fresh(entry))):
            return Result(entry.data, entry, from_cache=True)
        try:
            if self.offline:
                raise FetchError(f"no cached data for '{key}' (offline mode)")
            data = fetch(url)
        except NotFoundError:
            raise
        except (FetchError, rsc.PayloadError) as exc:
            if entry is not None:
                print(f"warning: {exc}; using stale cache", file=sys.stderr)
                return Result(entry.data, entry, from_cache=True, stale=True)
            if fallback is None:
                raise
            try:
                result = fallback()
            except NotFoundError:
                raise
            except (FetchError, rsc.PayloadError):
                raise exc from None
            print(f"warning: {exc}; using {PUBLIC_API_URL} instead", file=sys.stderr)
            return result
        return Result(data, self.cache.write(key, data, url), from_cache=False)

    def dataset(self) -> Result:
        """Every program and competition from Immunefi's public JSON API (one ~6 MB document)."""

        def fetch(url: str) -> list[dict]:
            try:
                data = json.loads(_http_get(url, {"Accept": "application/json"}))
            except ValueError as exc:
                raise rsc.PayloadError(f"invalid JSON from {url}") from exc
            if not isinstance(data, list):
                raise rsc.PayloadError(f"unexpected JSON shape from {url}")
            return data

        return self._cached("public-api", PUBLIC_API_URL, fetch)

    def _listing_from_api(self, kind: Kind) -> Result:
        res = self.dataset()
        items = [
            from_public_api(x)
            for x in res.data
            # The site lists invite-only competitions but hides invite-only programs.
            if _api_kind(x) == kind and (kind == "competition" or not x.get("inviteOnly"))
        ]
        return Result(items, res.entry, res.from_cache, res.stale, degraded=True)

    def _detail_from_api(self, kind: Kind, slug: str) -> Result:
        res = self.dataset()
        for x in res.data:
            if x.get("slug") == slug and _api_kind(x) == kind:
                return Result(from_public_api(x), res.entry, res.from_cache, res.stale, degraded=True)
        raise NotFoundError(f"'{slug}' is not in {PUBLIC_API_URL}")

    def listing(self, kind: Kind) -> Result:
        def fetch(url: str) -> list[dict]:
            return _fetch_page(url).find("bounties", list)

        return self._cached(
            f"{kind}s",
            f"{BASE_URL}/{KIND_PATH[kind]}/",
            fetch,
            fallback=lambda: self._listing_from_api(kind),
        )

    def detail(self, kind: Kind, slug: str) -> Result:
        def fetch(url: str) -> dict:
            page = _fetch_page(url)
            bounty = page.find("bounty", dict, where=lambda b: b.get("slug") == slug)
            try:
                bounty["vaults"] = [page.find("vault", dict)]
            except rsc.PayloadError:
                bounty["vaults"] = []
            return bounty

        return self._cached(
            f"{kind}/{slug}",
            f"{BASE_URL}/{KIND_PATH[kind]}/{slug}/information/",
            fetch,
            fallback=lambda: self._detail_from_api(kind, slug),
        )
