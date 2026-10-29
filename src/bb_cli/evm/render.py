"""Shared terminal typography, tables, dates and document rendering."""
from __future__ import annotations
import os
import re
import shutil
import sys
from datetime import datetime, timezone
from typing import Any, Callable, Iterable

class Style:
    def __init__(self, enabled: bool):
        self.enabled = enabled

    def _wrap(self, code: str, text: str) -> str:
        return f"\033[{code}m{text}\033[0m" if self.enabled and text else text

    def bold(self, t: str) -> str:
        return self._wrap("1", t)

    def dim(self, t: str) -> str:
        return self._wrap("2", t)

    def heading(self, t: str) -> str:
        return self._wrap("1;36", t)

    def link(self, t: str) -> str:
        return self._wrap("4;34", t)

    def severity(self, t: str) -> str:
        code = {"critical": "1;31", "high": "31", "medium": "33", "low": "34"}.get(t.lower())
        return self._wrap(code, t) if code else t

    def status(self, t: str) -> str:
        code = {"LIVE": "1;32", "UPCOMING": "36", "EVALUATING": "33", "PAUSED": "33"}.get(t)
        return self._wrap(code, t) if code else self.dim(t)


def use_color(flag_disabled: bool) -> bool:
    return not flag_disabled and "NO_COLOR" not in os.environ and sys.stdout.isatty()


# --- value formatting -----------------------------------------------------------------------


def money(value: Any, compact: bool = False) -> str:
    if not isinstance(value, (int, float)) or value <= 0:
        return "-"
    if compact:
        for div, suffix in ((1e6, "M"), (1e3, "K")):
            if value >= div:
                return f"${value / div:.3g}{suffix}"
    return f"${value:,.0f}"


def parse_date(value: Any) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        dt = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return dt if dt.tzinfo else dt.replace(tzinfo=timezone.utc)


def date(value: Any) -> str:
    dt = parse_date(value)
    return dt.strftime("%Y-%m-%d") if dt else "-"


def datetime_utc(value: Any) -> str:
    dt = parse_date(value)
    return dt.strftime("%Y-%m-%d %H:%M UTC") if dt else "-"


def relative(value: Any) -> str:
    dt = parse_date(value)
    if not dt:
        return ""
    delta = (dt - datetime.now(timezone.utc)).total_seconds()
    span = abs(delta)
    if span < 3600:
        amount = f"{span / 60:.0f}m"
    elif span < 86400 * 2:
        amount = f"{span / 3600:.0f}h"
    else:
        amount = f"{span / 86400:.0f}d"
    return f"in {amount}" if delta > 0 else f"{amount} ago"


def duration(seconds: float) -> str:
    hours, rem = divmod(int(seconds), 3600)
    return f"{hours}h{rem // 60:02d}m"


def joined(values: Iterable[str] | None, limit: int = 0) -> str:
    items = [v for v in (values or []) if v]
    if not items:
        return "-"
    if limit and len(items) > limit:
        return ", ".join(items[:limit]) + f" +{len(items) - limit}"
    return ", ".join(items)


def markdown(text: Any) -> str:
    """Light cleanup of the markdown bodies Immunefi stores; keeps them readable as text."""
    if not isinstance(text, str) or not text.strip() or text.strip() == "_blank_":
        return ""
    text = text.replace("\r\n", "\n").replace("\\\n", "\n").replace("&nbsp;", " ")
    text = re.sub(r"[ \t]+\n", "\n", text)
    text = re.sub(r"\n{3,}", "\n\n", text)
    return text.strip()


def indent(text: str, prefix: str = "  ") -> str:
    return "\n".join(prefix + line if line else line for line in text.splitlines())


# --- competition status -----------------------------------------------------------------------


# --- tables -----------------------------------------------------------------------------------

_ANSI_RE = re.compile(r"\033\[[0-9;]*m")


def _visible_len(text: str) -> int:
    return len(_ANSI_RE.sub("", text))


def _truncate(text: str, width: int) -> str:
    if len(text) <= width:
        return text
    return text[: max(width - 1, 0)] + "…"


def table(
    headers: list[str],
    rows: list[list[str]],
    style: Style,
    flex: int = 1,
    styler: dict[int, Callable[[str], str]] | None = None,
) -> str:
    """Render rows as aligned columns, shrinking column ``flex`` to fit the terminal."""
    styler = styler or {}
    widths = [max([len(h)] + [len(r[i]) for r in rows]) for i, h in enumerate(headers)]
    term = shutil.get_terminal_size((160, 24)).columns
    overflow = sum(widths) + 2 * (len(widths) - 1) - term
    if overflow > 0:
        widths[flex] = max(len(headers[flex]), widths[flex] - overflow, 12)
    lines = ["  ".join(style.bold(h.ljust(w)) for h, w in zip(headers, widths)).rstrip()]
    for row in rows:
        cells = []
        for i, (cell, w) in enumerate(zip(row, widths)):
            text = _truncate(cell, w)
            padded = text + " " * (w - len(text))
            cells.append(styler[i](text) + padded[len(text) :] if i in styler else padded)
        lines.append("  ".join(cells).rstrip())
    return "\n".join(lines)


class Doc:
    """Accumulates the text of a detail page."""

    def __init__(self, style: Style):
        self.style = style
        self.parts: list[str] = []

    def section(self, title: str) -> None:
        bar = "─" * min(len(title) + 4, 80)
        self.parts.append(f"\n{self.style.heading(title.upper())}\n{self.style.dim(bar)}")

    def sub(self, title: str) -> None:
        self.parts.append(f"\n{self.style.bold(title)}")

    def text(self, body: Any) -> None:
        if body := markdown(body):
            self.parts.append(indent(body))

    def fields(self, pairs: list[tuple[str, str]]) -> None:
        pairs = [(k, v) for k, v in pairs if v and v != "-"]
        if not pairs:
            return
        width = max(len(k) for k, _ in pairs) + 1
        self.parts.append("\n".join(f"  {self.style.dim((k + ':').ljust(width))} {v}" for k, v in pairs))

    def raw(self, text: str) -> None:
        if text:
            self.parts.append(text)

    def render(self) -> str:
        return "\n".join(self.parts).strip("\n") + "\n"
