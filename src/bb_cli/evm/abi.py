"""Minimal Solidity ABI codec: type parsing, call encoding, return decoding and formatting.

Covers every ABI type (uintN/intN, address, bool, bytesN, bytes, string, fixed and dynamic
arrays, tuples) except the rarely used fixed-point and function types.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from decimal import Decimal, InvalidOperation
from typing import Any, Union

from bb_cli.evm.keccak import keccak256


class AbiError(ValueError):
    pass


@dataclass(frozen=True)
class Basic:
    name: str  # uint, int, address, bool, bytes (dynamic), fixedbytes, string
    size: int = 0  # bits for uint/int, bytes for fixedbytes

    def canonical(self) -> str:
        if self.name in ("uint", "int"):
            return f"{self.name}{self.size}"
        if self.name == "fixedbytes":
            return f"bytes{self.size}"
        return self.name


@dataclass(frozen=True)
class Array:
    elem: "AbiType"
    length: int | None  # None = dynamic

    def canonical(self) -> str:
        return f"{self.elem.canonical()}[{'' if self.length is None else self.length}]"


@dataclass(frozen=True)
class Tuple:
    components: tuple["AbiType", ...]
    names: tuple[str, ...] = field(default=())

    def canonical(self) -> str:
        return "(" + ",".join(c.canonical() for c in self.components) + ")"


AbiType = Union[Basic, Array, Tuple]

_BASIC_RE = re.compile(r"^(uint|int)(\d*)$|^bytes(\d+)$|^(address|bool|string|bytes)$")


def split_top_level(text: str, sep: str = ",") -> list[str]:
    """Split on ``sep`` outside of brackets/parentheses/quotes."""
    parts, depth, quote, current = [], 0, "", []
    for ch in text:
        if quote:
            current.append(ch)
            if ch == quote:
                quote = ""
            continue
        if ch in "\"'":
            quote = ch
        elif ch in "([":
            depth += 1
        elif ch in ")]":
            depth -= 1
        elif ch == sep and depth == 0:
            parts.append("".join(current).strip())
            current = []
            continue
        current.append(ch)
    tail = "".join(current).strip()
    if tail or parts:
        parts.append(tail)
    return parts


def parse_type(text: str) -> AbiType:
    """Parse a canonical type string such as ``(address,uint256)[]``."""
    text = text.strip()
    if text.endswith("]"):
        i = text.rindex("[")
        dim = text[i + 1 : -1]
        if dim and not dim.isdigit():
            raise AbiError(f"bad array dimension in {text!r}")
        return Array(parse_type(text[:i]), int(dim) if dim else None)
    if text.startswith("(") and text.endswith(")"):
        inner = text[1:-1]
        return Tuple(tuple(parse_type(p) for p in split_top_level(inner)) if inner.strip() else ())
    # parameter names in signatures ("address owner") are ignored
    text = text.split()[0] if text else text
    m = _BASIC_RE.match(text)
    if not m:
        raise AbiError(f"unsupported ABI type {text!r}")
    if m.group(1):
        size = int(m.group(2) or 256)
        if size % 8 or not 8 <= size <= 256:
            raise AbiError(f"bad integer size in {text!r}")
        return Basic(m.group(1), size)
    if m.group(3):
        size = int(m.group(3))
        if not 1 <= size <= 32:
            raise AbiError(f"bad bytes size in {text!r}")
        return Basic("fixedbytes", size)
    return Basic(m.group(4))


def from_json(param: dict) -> AbiType:
    """Parse one entry of a JSON ABI ``inputs``/``outputs`` list."""
    typ = param.get("type", "")
    if typ.startswith("tuple"):
        comps = param.get("components") or []
        base: AbiType = Tuple(
            tuple(from_json(c) for c in comps), tuple(c.get("name") or "" for c in comps)
        )
        for dim in re.findall(r"\[(\d*)\]", typ[len("tuple") :]):
            base = Array(base, int(dim) if dim else None)
        return base
    return parse_type(typ)


def is_dynamic(t: AbiType) -> bool:
    if isinstance(t, Basic):
        return t.name in ("bytes", "string")
    if isinstance(t, Array):
        return t.length is None or is_dynamic(t.elem)
    return any(is_dynamic(c) for c in t.components)


def head_size(t: AbiType) -> int:
    if is_dynamic(t):
        return 32
    if isinstance(t, Array):
        return t.length * head_size(t.elem)  # type: ignore[operator]
    if isinstance(t, Tuple):
        return sum(head_size(c) for c in t.components)
    return 32


# --- encoding -----------------------------------------------------------------------------------


def _word(n: int) -> bytes:
    return n.to_bytes(32, "big")


def _pad_right(data: bytes) -> bytes:
    return data + b"\x00" * (-len(data) % 32)


def _encode_one(t: AbiType, v: Any) -> bytes:
    if isinstance(t, Basic):
        if t.name == "uint":
            if not 0 <= v < 1 << t.size:
                raise AbiError(f"{v} out of range for uint{t.size}")
            return _word(v)
        if t.name == "int":
            if not -(1 << (t.size - 1)) <= v < 1 << (t.size - 1):
                raise AbiError(f"{v} out of range for int{t.size}")
            return _word(v % (1 << 256))
        if t.name == "address":
            return _word(int(v, 16))
        if t.name == "bool":
            return _word(1 if v else 0)
        if t.name == "fixedbytes":
            if len(v) > t.size:
                raise AbiError(f"value longer than bytes{t.size}")
            return _pad_right(v)
        data = v.encode() if t.name == "string" else v
        return _word(len(data)) + _pad_right(data)
    if isinstance(t, Array):
        if t.length is not None and len(v) != t.length:
            raise AbiError(f"expected {t.length} elements for {t.canonical()}, got {len(v)}")
        body = encode([t.elem] * len(v), v)
        return body if t.length is not None else _word(len(v)) + body
    if len(v) != len(t.components):
        raise AbiError(f"expected {len(t.components)} tuple fields, got {len(v)}")
    return encode(list(t.components), v)


def encode(types: list[AbiType], values: list[Any]) -> bytes:
    heads, tails = [], []
    offset = sum(head_size(t) for t in types)
    for t, v in zip(types, values, strict=True):
        enc = _encode_one(t, v)
        if is_dynamic(t):
            heads.append(_word(offset))
            tails.append(enc)
            offset += len(enc)
        else:
            heads.append(enc)
    return b"".join(heads + tails)


# --- decoding -----------------------------------------------------------------------------------


def _read_word(data: bytes, at: int) -> int:
    if at < 0 or at + 32 > len(data):
        raise AbiError("return data too short for the expected types")
    return int.from_bytes(data[at : at + 32], "big")


def _decode_one(t: AbiType, data: bytes, at: int) -> Any:
    if isinstance(t, Basic):
        word = _read_word(data, at)
        if t.name == "uint":
            return word
        if t.name == "int":
            return word - (1 << 256) if word >> 255 else word
        if t.name == "address":
            return checksum_address(f"{word & ((1 << 160) - 1):040x}")
        if t.name == "bool":
            return bool(word)
        if t.name == "fixedbytes":
            return data[at : at + t.size]
        end = at + 32 + word
        if end > len(data):
            raise AbiError("dynamic value runs past the end of return data")
        raw = data[at + 32 : end]
        return raw.decode("utf-8", "replace") if t.name == "string" else raw
    if isinstance(t, Array):
        if t.length is None:
            n = _read_word(data, at)
            if n * 32 > len(data):
                raise AbiError("array length exceeds return data")
            return _decode_seq([t.elem] * n, data, at + 32)
        return _decode_seq([t.elem] * t.length, data, at)
    return _decode_seq(list(t.components), data, at)


def _decode_seq(types: list[AbiType], data: bytes, base: int) -> list[Any]:
    out, pos = [], base
    for t in types:
        if is_dynamic(t):
            out.append(_decode_one(t, data, base + _read_word(data, pos)))
        else:
            out.append(_decode_one(t, data, pos))
        pos += head_size(t)
    return out


def decode(types: list[AbiType], data: bytes) -> list[Any]:
    return _decode_seq(types, data, 0)


# --- addresses, functions, CLI values --------------------------------------------------------------

_ADDRESS_RE = re.compile(r"^0x[0-9a-fA-F]{40}$")


def checksum_address(addr: str) -> str:
    """EIP-55 mixed-case checksum."""
    hex_addr = addr.lower().removeprefix("0x")
    digest = keccak256(hex_addr.encode()).hex()
    return "0x" + "".join(c.upper() if int(digest[i], 16) >= 8 else c for i, c in enumerate(hex_addr))


def is_address(text: str) -> bool:
    return bool(_ADDRESS_RE.match(text))


@dataclass
class Function:
    name: str
    inputs: list[AbiType]
    outputs: list[AbiType] | None  # None = unknown, return raw bytes
    input_names: list[str] = field(default_factory=list)
    output_names: list[str] = field(default_factory=list)
    mutability: str = ""

    @property
    def signature(self) -> str:
        return f"{self.name}({','.join(t.canonical() for t in self.inputs)})"

    @property
    def selector(self) -> bytes:
        return keccak256(self.signature.encode())[:4]

    @property
    def is_read(self) -> bool:
        return self.mutability in ("view", "pure")

    def describe(self) -> str:
        args = ", ".join(
            f"{t.canonical()} {n}".strip() for t, n in zip(self.inputs, self.input_names or [""] * len(self.inputs))
        )
        rets = ""
        if self.outputs:
            rets = " returns (" + ", ".join(
                f"{t.canonical()} {n}".strip() for t, n in zip(self.outputs, self.output_names or [""] * len(self.outputs))
            ) + ")"
        return f"{self.name}({args}){rets}"

    def encode_call(self, args: list[Any]) -> bytes:
        return self.selector + encode(self.inputs, args)


def functions_from_abi(abi: list[dict]) -> list[Function]:
    out = []
    for item in abi:
        if item.get("type") != "function":
            continue
        inputs, outputs = item.get("inputs") or [], item.get("outputs") or []
        mutability = item.get("stateMutability") or ("view" if item.get("constant") else "nonpayable")
        out.append(Function(
            item["name"],
            [from_json(p) for p in inputs],
            [from_json(p) for p in outputs],
            [p.get("name") or "" for p in inputs],
            [p.get("name") or "" for p in outputs],
            mutability,
        ))
    return out


_SIG_RE = re.compile(r"^\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*\((.*)\)\s*$", re.S)


def parse_signature(text: str) -> Function:
    """Parse ``name(types)`` or cast-style ``name(types)(return types)``."""
    m = _SIG_RE.match(text)
    if not m:
        raise AbiError(f"not a function signature: {text!r}")
    name, rest = m.group(1), "(" + m.group(2) + ")"
    groups = split_top_level(rest.replace(")(", ")\x00("), "\x00")
    params = parse_type(groups[0])
    outputs = list(parse_type(groups[1]).components) if len(groups) > 1 else None  # type: ignore[union-attr]
    return Function(name, list(params.components), outputs, mutability="view")  # type: ignore[union-attr]


def parse_value(t: AbiType, text: str) -> Any:
    """Convert a command-line string into a Python value for ``t``."""
    text = text.strip()
    if isinstance(t, (Array, Tuple)):
        if not (text[:1] in "[(" and text[-1:] in "])"):
            raise AbiError(f"expected [...] for {t.canonical()}, got {text!r}")
        items = split_top_level(text[1:-1]) if text[1:-1].strip() else []
        if isinstance(t, Array):
            return [parse_value(t.elem, i) for i in items]
        if len(items) != len(t.components):
            raise AbiError(f"expected {len(t.components)} fields for {t.canonical()}")
        return [parse_value(c, i) for c, i in zip(t.components, items)]
    if len(text) >= 2 and text[0] == text[-1] and text[0] in "\"'":
        text = text[1:-1]
    if t.name in ("uint", "int"):
        return parse_int(text)
    if t.name == "address":
        if not is_address(text):
            raise AbiError(f"invalid address {text!r}")
        return text
    if t.name == "bool":
        if text.lower() in ("true", "1", "yes"):
            return True
        if text.lower() in ("false", "0", "no"):
            return False
        raise AbiError(f"invalid bool {text!r}")
    if t.name in ("bytes", "fixedbytes"):
        try:
            return bytes.fromhex(text.removeprefix("0x"))
        except ValueError as exc:
            raise AbiError(f"invalid hex bytes {text!r}") from exc
    return text


def parse_int(text: str) -> int:
    """Accepts 123, 0x7b, 1_000, 1e18, 1.5e18 and 1.5ether/gwei suffixes."""
    raw = text.replace("_", "").strip().lower()
    for unit, exp in (("ether", 18), ("gwei", 9), ("wei", 0)):
        if raw.endswith(unit):
            raw = f"{raw[: -len(unit)].strip()}e{exp}"
            break
    try:
        return int(raw, 0)
    except ValueError:
        pass
    try:
        value = Decimal(raw)
    except InvalidOperation as exc:
        raise AbiError(f"invalid integer {text!r}") from exc
    if value != value.to_integral_value():
        raise AbiError(f"{text!r} is not a whole number")
    return int(value)


def format_value(t: AbiType, v: Any, name: str = "") -> str:
    if isinstance(t, Tuple):
        names = t.names or ("",) * len(t.components)
        inner = ", ".join(
            (f"{n}: " if n else "") + format_value(c, x) for c, x, n in zip(t.components, v, names)
        )
        return f"({inner})"
    if isinstance(t, Array):
        return "[" + ", ".join(format_value(t.elem, x) for x in v) + "]"
    if isinstance(v, bytes):
        return "0x" + v.hex()
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, str) and t.name == "string":
        return repr(v)
    return str(v)


def to_json(t: AbiType, v: Any) -> Any:
    """JSON-safe value; integers beyond 2^53 become strings so no consumer loses precision."""
    if isinstance(t, Tuple):
        if t.names and all(t.names):
            return {n: to_json(c, x) for n, c, x in zip(t.names, t.components, v)}
        return [to_json(c, x) for c, x in zip(t.components, v)]
    if isinstance(t, Array):
        return [to_json(t.elem, x) for x in v]
    if isinstance(v, bytes):
        return "0x" + v.hex()
    if isinstance(v, int) and not isinstance(v, bool) and abs(v) >= 1 << 53:
        return str(v)
    return v
