"""On-disk JSON cache with a long, enforced minimum TTL.

Each entry is stored as ``{"fetched_at": <epoch>, "source": <url>, "data": <payload>}`` so the
parsed page data (not the raw HTML) is what persists between runs.
"""

from __future__ import annotations

import json
import os
import shutil
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

MIN_TTL_HOURS = 10.0
DEFAULT_TTL_HOURS = 12.0


def default_cache_dir() -> Path:
    if env := os.environ.get("IMMUNEFI_CACHE_DIR"):
        return Path(env).expanduser()
    base = os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache"
    return Path(base).expanduser() / "immunefi-cli"


def resolve_ttl_hours(requested: float | None) -> float:
    """Return the effective TTL, never below ``MIN_TTL_HOURS``."""
    if requested is None:
        env = os.environ.get("IMMUNEFI_CACHE_TTL_HOURS")
        try:
            requested = float(env) if env else DEFAULT_TTL_HOURS
        except ValueError:
            requested = DEFAULT_TTL_HOURS
    return max(requested, MIN_TTL_HOURS)


@dataclass
class Entry:
    data: Any
    fetched_at: float
    source: str

    @property
    def age_seconds(self) -> float:
        return max(0.0, time.time() - self.fetched_at)


class Cache:
    def __init__(self, root: Path, ttl_hours: float):
        self.root = root
        self.ttl_seconds = ttl_hours * 3600

    def _path(self, key: str) -> Path:
        return self.root / f"{key}.json"

    def read(self, key: str) -> Entry | None:
        try:
            raw = json.loads(self._path(key).read_text(encoding="utf-8"))
            return Entry(raw["data"], float(raw["fetched_at"]), raw.get("source", ""))
        except (OSError, ValueError, KeyError, TypeError):
            return None

    def is_fresh(self, entry: Entry) -> bool:
        return entry.age_seconds < self.ttl_seconds

    def write(self, key: str, data: Any, source: str) -> Entry:
        entry = Entry(data, time.time(), source)
        path = self._path(key)
        path.parent.mkdir(parents=True, exist_ok=True)
        payload = {"fetched_at": entry.fetched_at, "source": source, "data": data}
        fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=".tmp-", suffix=".json")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as fh:
                json.dump(payload, fh, ensure_ascii=False)
            os.replace(tmp, path)
        except BaseException:
            Path(tmp).unlink(missing_ok=True)
            raise
        return entry

    def entries(self) -> list[tuple[str, Entry]]:
        out = []
        if not self.root.is_dir():
            return out
        for path in sorted(self.root.rglob("*.json")):
            if path.name.startswith(".tmp-"):
                continue
            key = path.relative_to(self.root).with_suffix("").as_posix()
            if entry := self.read(key):
                out.append((key, entry))
        return out

    def clear(self) -> None:
        if self.root.is_dir():
            shutil.rmtree(self.root)
