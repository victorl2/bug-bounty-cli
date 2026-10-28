"""Minimal decoder for the React Server Components payload embedded in Next.js pages.

Immunefi pages are rendered with the Next.js App Router. The page data is not exposed as a
standalone JSON document; instead it is streamed into the HTML as
``self.__next_f.push([1, "<chunk>"])`` script tags. Concatenated, the chunks form a "flight"
stream of rows::

    <hex id>:<json>\n          regular row
    <hex id>:T<hex len>,<text>  text row (length in UTF-8 bytes, no trailing newline)
    <hex id>:I[...]\n          module import (ignored)

Values reference other rows with ``"$<id>"`` strings, which ``resolve`` follows.

The same stream is served directly (``text/x-component``, no HTML around it) when the page is
requested with an ``RSC: 1`` header, which is how the site's client-side navigation fetches it.
"""

from __future__ import annotations

import json
import re
from typing import Any, Callable

_PUSH_RE = re.compile(r"self\.__next_f\.push\((\[.*?\])\)</script>", re.S)
_ROW_ID_RE = re.compile(rb"([0-9a-f]*):")
_SKIPPED_ROW_TAGS = b"IHXEDW"
_MAX_DEPTH = 64
_NOT_FOUND_MARKER = "NEXT_HTTP_ERROR_FALLBACK;404"


class PayloadError(ValueError):
    """The page did not contain the expected Next.js payload."""


def extract_flight(html: str) -> str:
    chunks = []
    for raw in _PUSH_RE.findall(html):
        try:
            item = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if len(item) > 1 and isinstance(item[1], str):
            chunks.append(item[1])
    if not chunks:
        raise PayloadError("no Next.js flight data found in page")
    return "".join(chunks)


def parse_rows(flight: str) -> dict[str, Any]:
    data = flight.encode("utf-8")
    rows: dict[str, Any] = {}
    i, n = 0, len(data)
    while i < n:
        m = _ROW_ID_RE.match(data, i)
        if not m:
            j = data.find(b"\n", i)
            i = n if j < 0 else j + 1
            continue
        row_id = m.group(1).decode()
        i = m.end()
        if data[i : i + 1] == b"T":
            comma = data.index(b",", i)
            length = int(data[i + 1 : comma], 16)
            start = comma + 1
            rows[row_id] = data[start : start + length].decode("utf-8", "replace")
            i = start + length
            continue
        j = data.find(b"\n", i)
        j = n if j < 0 else j
        line = data[i:j]
        i = j + 1
        if line[:1] and line[:1] in _SKIPPED_ROW_TAGS:
            continue
        try:
            rows[row_id] = json.loads(line)
        except json.JSONDecodeError:
            pass
    return rows


def resolve(value: Any, rows: dict[str, Any], depth: int = 0) -> Any:
    if depth > _MAX_DEPTH:
        return value
    if isinstance(value, str) and value.startswith("$") and len(value) > 1:
        if value == "$undefined":
            return None
        if value.startswith("$$"):
            return value[1:]
        ref = value[1:]
        if ref[0] == "D":  # serialized Date
            return ref[1:]
        if ref[0] in "L@SFQWKBnNI":  # lazy/promise/symbol/etc. - not plain data
            return value
        row_id, *path = ref.split(":")
        if row_id not in rows:
            return value
        target = rows[row_id]
        for key in path:
            if isinstance(target, dict):
                target = target.get(key)
            elif isinstance(target, list) and key.isdigit() and int(key) < len(target):
                target = target[int(key)]
            else:
                return value
        return resolve(target, rows, depth + 1)
    if isinstance(value, list):
        return [resolve(v, rows, depth + 1) for v in value]
    if isinstance(value, dict):
        return {k: resolve(v, rows, depth + 1) for k, v in value.items()}
    return value


def _find_key(node: Any, key: str, out: list[Any]) -> None:
    if isinstance(node, dict):
        if key in node:
            out.append(node[key])
        for v in node.values():
            _find_key(v, key, out)
    elif isinstance(node, list):
        for v in node:
            _find_key(v, key, out)


class Payload:
    """A decoded page, given either its HTML or its raw RSC stream."""

    def __init__(self, body: str):
        flight = extract_flight(body) if body.lstrip().startswith("<") else body
        # RSC requests answer 200 for missing pages; the 404 is signalled inside the stream.
        self.not_found = _NOT_FOUND_MARKER in flight
        self.rows = parse_rows(flight)

    def find(
        self, key: str, expect: type, where: Callable[[Any], bool] = lambda _: True
    ) -> Any:
        """Return the first value stored under ``key`` anywhere in the payload."""
        for row in self.rows.values():
            found: list[Any] = []
            _find_key(row, key, found)
            for candidate in found:
                value = resolve(candidate, self.rows)
                if isinstance(value, expect) and where(value):
                    return value
        raise PayloadError(f"page payload has no {expect.__name__} under '{key}'")
