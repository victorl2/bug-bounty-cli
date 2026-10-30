//! Solidity ABI types as the CLI reads and prints them, on top of alloy's codec.
//!
//! alloy encodes, decodes and hashes; this module keeps what alloy has no notion of: parameter
//! names on tuples (for JSON output), command-line value parsing (`1.5ether`, `[a,(b,c)]`) and
//! the text and JSON formats the Python releases printed.

use std::sync::LazyLock;

use alloy_dyn_abi::{DynSolType, DynSolValue};
use alloy_primitives::{Address, B256, I256, U256, keccak256};
use regex::Regex;
use serde_json::{Value, json};

use crate::error::{Error, Result};

fn abi_err(message: impl Into<String>) -> Error {
    Error::Value(message.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BasicKind {
    Uint,
    Int,
    Address,
    Bool,
    /// dynamic `bytes`
    Bytes,
    FixedBytes,
    String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiType {
    /// `size` is bits for uint/int, bytes for fixedbytes.
    Basic(BasicKind, usize),
    /// `None` length = dynamic.
    Array(Box<AbiType>, Option<usize>),
    Tuple(Vec<AbiType>, Vec<String>),
}

impl AbiType {
    pub fn uint(bits: usize) -> Self {
        AbiType::Basic(BasicKind::Uint, bits)
    }

    pub fn string() -> Self {
        AbiType::Basic(BasicKind::String, 0)
    }

    pub fn canonical(&self) -> String {
        match self {
            AbiType::Basic(kind, size) => match kind {
                BasicKind::Uint => format!("uint{size}"),
                BasicKind::Int => format!("int{size}"),
                BasicKind::FixedBytes => format!("bytes{size}"),
                BasicKind::Address => "address".into(),
                BasicKind::Bool => "bool".into(),
                BasicKind::Bytes => "bytes".into(),
                BasicKind::String => "string".into(),
            },
            AbiType::Array(elem, len) => format!("{}[{}]", elem.canonical(), len.map(|l| l.to_string()).unwrap_or_default()),
            AbiType::Tuple(components, _) => {
                format!("({})", components.iter().map(AbiType::canonical).collect::<Vec<_>>().join(","))
            }
        }
    }

    pub fn to_dyn(&self) -> DynSolType {
        match self {
            AbiType::Basic(kind, size) => match kind {
                BasicKind::Uint => DynSolType::Uint(*size),
                BasicKind::Int => DynSolType::Int(*size),
                BasicKind::Address => DynSolType::Address,
                BasicKind::Bool => DynSolType::Bool,
                BasicKind::Bytes => DynSolType::Bytes,
                BasicKind::FixedBytes => DynSolType::FixedBytes(*size),
                BasicKind::String => DynSolType::String,
            },
            AbiType::Array(elem, None) => DynSolType::Array(Box::new(elem.to_dyn())),
            AbiType::Array(elem, Some(n)) => DynSolType::FixedArray(Box::new(elem.to_dyn()), *n),
            AbiType::Tuple(components, _) => DynSolType::Tuple(components.iter().map(AbiType::to_dyn).collect()),
        }
    }

    fn components(&self) -> Option<&[AbiType]> {
        match self {
            AbiType::Tuple(c, _) => Some(c),
            _ => None,
        }
    }
}

static BASIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(uint|int)(\d*)$|^bytes(\d+)$|^(address|bool|string|bytes)$").unwrap());

/// Split on `sep` outside of brackets, parentheses and quotes.
pub fn split_top_level(text: &str, sep: char) -> Vec<String> {
    let (mut parts, mut current) = (Vec::new(), String::new());
    let (mut depth, mut quote) = (0i32, None::<char>);
    for ch in text.chars() {
        if let Some(q) = quote {
            current.push(ch);
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(current.trim().to_string());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(ch);
    }
    let tail = current.trim().to_string();
    if !tail.is_empty() || !parts.is_empty() {
        parts.push(tail);
    }
    parts
}

/// Parse a canonical type string such as `(address,uint256)[]`; parameter names are ignored.
pub fn parse_type(text: &str) -> Result<AbiType> {
    let text = text.trim();
    if text.ends_with(']') {
        let i = text.rfind('[').ok_or_else(|| abi_err(format!("bad array dimension in {}", py_repr(text))))?;
        let dim = &text[i + 1..text.len() - 1];
        if !dim.is_empty() && !dim.chars().all(|c| c.is_ascii_digit()) {
            return Err(abi_err(format!("bad array dimension in {}", py_repr(text))));
        }
        let len = if dim.is_empty() { None } else { Some(dim.parse().map_err(|_| abi_err("bad array dimension"))?) };
        return Ok(AbiType::Array(Box::new(parse_type(&text[..i])?), len));
    }
    if text.starts_with('(') && text.ends_with(')') {
        let inner = &text[1..text.len() - 1];
        let components = if inner.trim().is_empty() {
            Vec::new()
        } else {
            split_top_level(inner, ',').iter().map(|p| parse_type(p)).collect::<Result<_>>()?
        };
        return Ok(AbiType::Tuple(components, Vec::new()));
    }
    let word = text.split_whitespace().next().unwrap_or("");
    let caps = BASIC.captures(word).ok_or_else(|| abi_err(format!("unsupported ABI type {}", py_repr(word))))?;
    if let Some(kind) = caps.get(1) {
        let size: usize = caps
            .get(2)
            .map(|m| m.as_str())
            .filter(|s| !s.is_empty())
            .map_or(Ok(256), str::parse)
            .map_err(|_| abi_err(format!("bad integer size in {}", py_repr(word))))?;
        if size % 8 != 0 || !(8..=256).contains(&size) {
            return Err(abi_err(format!("bad integer size in {}", py_repr(word))));
        }
        let kind = if kind.as_str() == "uint" { BasicKind::Uint } else { BasicKind::Int };
        return Ok(AbiType::Basic(kind, size));
    }
    if let Some(size) = caps.get(3) {
        let size: usize = size.as_str().parse().unwrap_or(0);
        if !(1..=32).contains(&size) {
            return Err(abi_err(format!("bad bytes size in {}", py_repr(word))));
        }
        return Ok(AbiType::Basic(BasicKind::FixedBytes, size));
    }
    let kind = match &caps[4] {
        "address" => BasicKind::Address,
        "bool" => BasicKind::Bool,
        "string" => BasicKind::String,
        _ => BasicKind::Bytes,
    };
    Ok(AbiType::Basic(kind, 0))
}

static DIMS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[(\d*)\]").unwrap());

/// Parse one entry of a JSON ABI `inputs`/`outputs` list.
pub fn from_json(param: &Value) -> Result<AbiType> {
    let ty = param.get("type").and_then(Value::as_str).unwrap_or("");
    if let Some(suffix) = ty.strip_prefix("tuple") {
        let comps = param.get("components").and_then(Value::as_array).cloned().unwrap_or_default();
        let names = comps.iter().map(|c| c.get("name").and_then(Value::as_str).unwrap_or("").to_string()).collect();
        let mut base = AbiType::Tuple(comps.iter().map(from_json).collect::<Result<_>>()?, names);
        for dim in DIMS.captures_iter(suffix) {
            base = AbiType::Array(Box::new(base), if dim[1].is_empty() { None } else { dim[1].parse().ok() });
        }
        return Ok(base);
    }
    parse_type(ty)
}

// --- addresses ----------------------------------------------------------------------------------

static ADDRESS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^0x[0-9a-fA-F]{40}$").unwrap());

pub fn is_address(text: &str) -> bool {
    ADDRESS.is_match(text)
}

/// EIP-55 mixed-case checksum of a 40-digit hex address (with or without `0x`).
pub fn checksum_address(addr: &str) -> String {
    let hex_part = addr.strip_prefix("0x").or_else(|| addr.strip_prefix("0X")).unwrap_or(addr);
    match hex::decode(hex_part) {
        Ok(bytes) if bytes.len() == 20 => Address::from_slice(&bytes).to_checksum(None),
        _ => addr.to_string(),
    }
}

/// The address in the low 20 bytes of a 32-byte word.
pub fn word_address(word: &[u8]) -> String {
    Address::from_slice(&word[word.len() - 20..]).to_checksum(None)
}

// --- functions ----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub inputs: Vec<AbiType>,
    /// `None` = unknown: return raw bytes.
    pub outputs: Option<Vec<AbiType>>,
    pub input_names: Vec<String>,
    pub output_names: Vec<String>,
    pub mutability: String,
}

impl Function {
    pub fn signature(&self) -> String {
        format!("{}({})", self.name, self.inputs.iter().map(AbiType::canonical).collect::<Vec<_>>().join(","))
    }

    pub fn selector(&self) -> [u8; 4] {
        let hash = keccak256(self.signature().as_bytes());
        [hash[0], hash[1], hash[2], hash[3]]
    }

    pub fn is_read(&self) -> bool {
        matches!(self.mutability.as_str(), "view" | "pure")
    }

    pub fn describe(&self) -> String {
        let params = |types: &[AbiType], names: &[String]| {
            types
                .iter()
                .enumerate()
                .map(|(i, t)| format!("{} {}", t.canonical(), names.get(i).map(String::as_str).unwrap_or("")).trim().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let rets = match &self.outputs {
            Some(outs) if !outs.is_empty() => format!(" returns ({})", params(outs, &self.output_names)),
            _ => String::new(),
        };
        format!("{}({}){rets}", self.name, params(&self.inputs, &self.input_names))
    }

    pub fn encode_call(&self, args: &[DynSolValue]) -> Vec<u8> {
        let mut out = self.selector().to_vec();
        out.extend(DynSolValue::Tuple(args.to_vec()).abi_encode_params());
        out
    }
}

pub fn functions_from_abi(abi: &[Value]) -> Result<Vec<Function>> {
    let mut out = Vec::new();
    for item in abi {
        if item.get("type").and_then(Value::as_str) != Some("function") {
            continue;
        }
        let list = |key: &str| item.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
        let (inputs, outputs) = (list("inputs"), list("outputs"));
        let names = |params: &[Value]| params.iter().map(|p| p.get("name").and_then(Value::as_str).unwrap_or("").to_string()).collect();
        let mutability =
            item.get("stateMutability").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| {
                if item.get("constant").and_then(Value::as_bool).unwrap_or(false) { "view" } else { "nonpayable" }.into()
            });
        out.push(Function {
            name: item.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            inputs: inputs.iter().map(from_json).collect::<Result<_>>()?,
            outputs: Some(outputs.iter().map(from_json).collect::<Result<_>>()?),
            input_names: names(&inputs),
            output_names: names(&outputs),
            mutability,
        });
    }
    Ok(out)
}

static SIG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)^\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*\((.*)\)\s*$").unwrap());

/// Parse `name(types)` or cast-style `name(types)(return types)`.
pub fn parse_signature(text: &str) -> Result<Function> {
    let caps = SIG.captures(text).ok_or_else(|| abi_err(format!("not a function signature: {}", py_repr(text))))?;
    let rest = format!("({})", &caps[2]).replace(")(", ")\0(");
    let groups = split_top_level(&rest, '\0');
    let params = parse_type(&groups[0])?;
    let outputs = match groups.get(1) {
        Some(g) => Some(parse_type(g)?.components().map(<[AbiType]>::to_vec).unwrap_or_default()),
        None => None,
    };
    Ok(Function {
        name: caps[1].to_string(),
        inputs: params.components().map(<[AbiType]>::to_vec).unwrap_or_default(),
        outputs,
        input_names: Vec::new(),
        output_names: Vec::new(),
        mutability: "view".into(),
    })
}

// --- codec --------------------------------------------------------------------------------------

/// Encode `values` as function parameters.
pub fn encode(values: &[DynSolValue]) -> Vec<u8> {
    DynSolValue::Tuple(values.to_vec()).abi_encode_params()
}

/// Decode `data` as the parameter list `types`.
pub fn decode(types: &[AbiType], data: &[u8]) -> Result<Vec<DynSolValue>> {
    let ty = DynSolType::Tuple(types.iter().map(AbiType::to_dyn).collect());
    match ty.abi_decode_params(data) {
        Ok(DynSolValue::Tuple(values)) => Ok(values),
        Ok(other) => Ok(vec![other]),
        Err(e) => Err(abi_err(format!("return data does not match the expected types ({e})"))),
    }
}

// --- command-line values ------------------------------------------------------------------------

/// A signed integer of any size an ABI value can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BigInt {
    pub negative: bool,
    pub magnitude: U256,
}

impl BigInt {
    pub fn unsigned(&self) -> Result<U256> {
        if self.negative && !self.magnitude.is_zero() {
            return Err(abi_err(format!("-{} must not be negative", self.magnitude)));
        }
        Ok(self.magnitude)
    }

    fn display(&self) -> String {
        format!("{}{}", if self.negative && !self.magnitude.is_zero() { "-" } else { "" }, self.magnitude)
    }
}

fn pow10(exp: usize) -> Option<U256> {
    U256::from(10u64).checked_pow(U256::from(exp))
}

/// Accepts 123, 0x7b, 1_000, 1e18, 1.5e18 and 1.5ether/gwei suffixes.
pub fn parse_int(text: &str) -> Result<BigInt> {
    let invalid = || abi_err(format!("invalid integer {}", py_repr(text)));
    let mut raw = text.replace('_', "").trim().to_lowercase();
    for (unit, exp) in [("ether", 18), ("gwei", 9), ("wei", 0)] {
        if let Some(head) = raw.strip_suffix(unit) {
            raw = format!("{}e{exp}", head.trim());
            break;
        }
    }
    let (negative, body) = match raw.as_bytes().first() {
        Some(b'-') => (true, &raw[1..]),
        Some(b'+') => (false, &raw[1..]),
        _ => (false, raw.as_str()),
    };
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(digits) = body.strip_prefix(prefix) {
            let magnitude = U256::from_str_radix(digits, radix).map_err(|_| invalid())?;
            return Ok(BigInt { negative, magnitude });
        }
    }
    // Decimal notation, possibly with a fraction and an exponent.
    let (mantissa, exponent) = match body.split_once('e') {
        Some((m, e)) => (m, e.parse::<i64>().map_err(|_| invalid())?),
        None => (body, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if (whole.is_empty() && fraction.is_empty()) || !whole.chars().chain(fraction.chars()).all(|c| c.is_ascii_digit()) {
        return Err(invalid());
    }
    let digits = format!("{whole}{fraction}");
    let shift = exponent - fraction.len() as i64;
    let digits = if shift < 0 {
        let cut = (-shift) as usize;
        let keep = digits.len().saturating_sub(cut);
        if digits[keep..].chars().any(|c| c != '0') {
            return Err(abi_err(format!("{} is not a whole number", py_repr(text))));
        }
        digits[..keep].to_string()
    } else {
        digits
    };
    let base = if digits.is_empty() { U256::ZERO } else { U256::from_str_radix(&digits, 10).map_err(|_| invalid())? };
    let scale = pow10(shift.max(0) as usize).ok_or_else(invalid)?;
    let magnitude = base.checked_mul(scale).ok_or_else(invalid)?;
    Ok(BigInt { negative, magnitude })
}

/// Convert a command-line string into a value of type `t`.
pub fn parse_value(t: &AbiType, text: &str) -> Result<DynSolValue> {
    let text = text.trim();
    match t {
        AbiType::Array(..) | AbiType::Tuple(..) => {
            let open = text.starts_with('[') || text.starts_with('(');
            let close = text.ends_with(']') || text.ends_with(')');
            if !(open && close && text.len() >= 2) {
                return Err(abi_err(format!("expected [...] for {}, got {}", t.canonical(), py_repr(text))));
            }
            let inner = &text[1..text.len() - 1];
            let items = if inner.trim().is_empty() { Vec::new() } else { split_top_level(inner, ',') };
            match t {
                AbiType::Array(elem, len) => {
                    let values = items.iter().map(|i| parse_value(elem, i)).collect::<Result<Vec<_>>>()?;
                    match len {
                        Some(n) if *n != values.len() => {
                            Err(abi_err(format!("expected {n} elements for {}, got {}", t.canonical(), values.len())))
                        }
                        Some(_) => Ok(DynSolValue::FixedArray(values)),
                        None => Ok(DynSolValue::Array(values)),
                    }
                }
                AbiType::Tuple(components, _) => {
                    if items.len() != components.len() {
                        return Err(abi_err(format!("expected {} fields for {}", components.len(), t.canonical())));
                    }
                    Ok(DynSolValue::Tuple(components.iter().zip(&items).map(|(c, i)| parse_value(c, i)).collect::<Result<_>>()?))
                }
                AbiType::Basic(..) => unreachable!(),
            }
        }
        AbiType::Basic(kind, size) => {
            let unquoted = unquote(text);
            match kind {
                BasicKind::Uint => {
                    let v = parse_int(unquoted)?;
                    let limit = if *size == 256 { None } else { Some(U256::from(1u8) << *size) };
                    if (v.negative && !v.magnitude.is_zero()) || limit.is_some_and(|l| v.magnitude >= l) {
                        return Err(abi_err(format!("{} out of range for uint{size}", v.display())));
                    }
                    Ok(DynSolValue::Uint(v.magnitude, *size))
                }
                BasicKind::Int => {
                    let v = parse_int(unquoted)?;
                    let half = U256::from(1u8) << (*size - 1);
                    let fits = if v.negative { v.magnitude <= half } else { v.magnitude < half };
                    if !fits {
                        return Err(abi_err(format!("{} out of range for int{size}", v.display())));
                    }
                    let value = I256::from_raw(v.magnitude);
                    Ok(DynSolValue::Int(if v.negative { value.wrapping_neg() } else { value }, *size))
                }
                BasicKind::Address => {
                    if !is_address(unquoted) {
                        return Err(abi_err(format!("invalid address {}", py_repr(unquoted))));
                    }
                    let bytes = hex::decode(&unquoted[2..]).map_err(|_| abi_err("invalid address"))?;
                    Ok(DynSolValue::Address(Address::from_slice(&bytes)))
                }
                BasicKind::Bool => match unquoted.to_lowercase().as_str() {
                    "true" | "1" | "yes" => Ok(DynSolValue::Bool(true)),
                    "false" | "0" | "no" => Ok(DynSolValue::Bool(false)),
                    _ => Err(abi_err(format!("invalid bool {}", py_repr(unquoted)))),
                },
                BasicKind::Bytes | BasicKind::FixedBytes => {
                    let bytes = hex::decode(unquoted.strip_prefix("0x").unwrap_or(unquoted))
                        .map_err(|_| abi_err(format!("invalid hex bytes {}", py_repr(unquoted))))?;
                    if *kind == BasicKind::Bytes {
                        return Ok(DynSolValue::Bytes(bytes));
                    }
                    if bytes.len() > *size {
                        return Err(abi_err(format!("value longer than bytes{size}")));
                    }
                    let mut word = B256::ZERO;
                    word[..bytes.len()].copy_from_slice(&bytes);
                    Ok(DynSolValue::FixedBytes(word, *size))
                }
                BasicKind::String => Ok(DynSolValue::String(unquoted.to_string())),
            }
        }
    }
}

fn unquote(text: &str) -> &str {
    let b = text.as_bytes();
    if b.len() >= 2 && b[0] == b[b.len() - 1] && (b[0] == b'"' || b[0] == b'\'') { &text[1..text.len() - 1] } else { text }
}

// --- output -------------------------------------------------------------------------------------

/// Python's `repr()` of a string.
pub fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn int_text(value: &DynSolValue) -> Option<String> {
    match value {
        DynSolValue::Uint(v, _) => Some(v.to_string()),
        DynSolValue::Int(v, _) => Some(v.to_string()),
        _ => None,
    }
}

fn hex0x(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

pub fn format_value(t: &AbiType, v: &DynSolValue) -> String {
    match (t, v) {
        (AbiType::Tuple(components, names), DynSolValue::Tuple(items)) => {
            let inner: Vec<String> = components
                .iter()
                .zip(items)
                .enumerate()
                .map(|(i, (c, x))| {
                    let name = names.get(i).filter(|n| !n.is_empty());
                    format!("{}{}", name.map(|n| format!("{n}: ")).unwrap_or_default(), format_value(c, x))
                })
                .collect();
            format!("({})", inner.join(", "))
        }
        (AbiType::Array(elem, _), DynSolValue::Array(items) | DynSolValue::FixedArray(items)) => {
            format!("[{}]", items.iter().map(|x| format_value(elem, x)).collect::<Vec<_>>().join(", "))
        }
        (_, DynSolValue::Bytes(b)) => hex0x(b),
        (_, DynSolValue::FixedBytes(w, size)) => hex0x(&w[..*size]),
        (_, DynSolValue::Bool(b)) => if *b { "true" } else { "false" }.into(),
        (_, DynSolValue::String(s)) => py_repr(s),
        (_, DynSolValue::Address(a)) => a.to_checksum(None),
        (_, other) => int_text(other).unwrap_or_default(),
    }
}

/// JSON-safe value; integers beyond 2^53 become strings so no consumer loses precision.
pub fn to_json(t: &AbiType, v: &DynSolValue) -> Value {
    match (t, v) {
        (AbiType::Tuple(components, names), DynSolValue::Tuple(items)) => {
            if !names.is_empty() && names.iter().all(|n| !n.is_empty()) {
                Value::Object(names.iter().zip(components.iter().zip(items)).map(|(n, (c, x))| (n.clone(), to_json(c, x))).collect())
            } else {
                Value::Array(components.iter().zip(items).map(|(c, x)| to_json(c, x)).collect())
            }
        }
        (AbiType::Array(elem, _), DynSolValue::Array(items) | DynSolValue::FixedArray(items)) => {
            Value::Array(items.iter().map(|x| to_json(elem, x)).collect())
        }
        (_, DynSolValue::Bytes(b)) => json!(hex0x(b)),
        (_, DynSolValue::FixedBytes(w, size)) => json!(hex0x(&w[..*size])),
        (_, DynSolValue::Bool(b)) => json!(b),
        (_, DynSolValue::String(s)) => json!(s),
        (_, DynSolValue::Address(a)) => json!(a.to_checksum(None)),
        (_, DynSolValue::Uint(v, _)) => {
            if *v >= U256::from(1u64 << 53) {
                json!(v.to_string())
            } else {
                json!(v.to::<u64>())
            }
        }
        (_, DynSolValue::Int(v, _)) => {
            if v.unsigned_abs() >= U256::from(1u64 << 53) {
                json!(v.to_string())
            } else {
                json!(v.as_i64())
            }
        }
        (_, other) => json!(format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference values produced with Foundry's `cast calldata`.
    const TUPLE_ARRAY_CALLDATA: &str = concat!(
        "73942f08",
        "0000000000000000000000000000000000000000000000000000000000000040",
        "00000000000000000000000000000000000000000000000000000000000000e0",
        "0000000000000000000000000000000000000000000000000000000000000002",
        "00000000000000000000000000000000219ab540356cbb839cbe05303d7705fa",
        "0000000000000000000000000000000000000000000000000000000000000007",
        "0000000000000000000000000000000000000000000000000000000000000001",
        "0000000000000000000000000000000000000000000000000000000000000008",
        "0000000000000000000000000000000000000000000000000000000000000040",
        "0000000000000000000000000000000000000000000000000000000000000080",
        "0000000000000000000000000000000000000000000000000000000000000001",
        "6100000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000002",
        "6263000000000000000000000000000000000000000000000000000000000000",
    );
    const NEG_INT_CALLDATA: &str = concat!(
        "f6cf013f",
        "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffb",
        "0000000000000000000000000000000000000000000000000000000000000060",
        "0000000000000000000000000000000000000000000000000000000000000001",
        "0000000000000000000000000000000000000000000000000000000000000004",
        "deadbeef00000000000000000000000000000000000000000000000000000000",
    );

    fn encode_sig(sig: &str, args: &[&str]) -> String {
        let f = parse_signature(sig).unwrap();
        let values: Vec<DynSolValue> = f.inputs.iter().zip(args).map(|(t, a)| parse_value(t, a).unwrap()).collect();
        hex::encode(f.encode_call(&values))
    }

    #[test]
    fn keccak_vectors() {
        assert_eq!(hex::encode(keccak256(b"")), "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470");
        assert_eq!(hex::encode(&keccak256(b"transfer(address,uint256)")[..4]), "a9059cbb");
        assert_eq!(hex::encode(keccak256("ab".repeat(137).as_bytes())), "f5ae4363565d60c5fd94d59e528d9ab1e8f209dbf9c8c70aa0ffafa5e9f40e76");
    }

    #[test]
    fn encoding_matches_cast() {
        assert_eq!(
            encode_sig(
                "f((address,uint256)[],string[2])",
                &["[(0x00000000219ab540356cBB839Cbe05303d7705Fa,7),(0x0000000000000000000000000000000000000001,8)]", r#"["a","bc"]"#]
            ),
            TUPLE_ARRAY_CALLDATA
        );
        assert_eq!(encode_sig("f(int8,bytes,bool)", &["-5", "0xdeadbeef", "true"]), NEG_INT_CALLDATA);
    }

    #[test]
    fn decode_round_trip() {
        let f = parse_signature("f((address,uint256)[],string[2])").unwrap();
        let values = decode(&f.inputs, &hex::decode(&TUPLE_ARRAY_CALLDATA[8..]).unwrap()).unwrap();
        assert_eq!(to_json(&f.inputs[0], &values[0])[0], json!(["0x00000000219ab540356cBB839Cbe05303d7705Fa", 7]));
        assert_eq!(to_json(&f.inputs[1], &values[1]), json!(["a", "bc"]));
        assert_eq!(hex::encode(f.encode_call(&values)), TUPLE_ARRAY_CALLDATA);
        let neg = parse_signature("f(int8,bytes,bool)").unwrap();
        let values = decode(&neg.inputs, &hex::decode(&NEG_INT_CALLDATA[8..]).unwrap()).unwrap();
        let shown: Vec<String> = neg.inputs.iter().zip(&values).map(|(t, v)| format_value(t, v)).collect();
        assert_eq!(shown, ["-5", "0xdeadbeef", "true"]);
    }

    #[test]
    fn decode_rejects_truncated_data() {
        let word = (U256::from(1u8) << 200usize).to_be_bytes::<32>();
        assert!(decode(&[parse_type("uint256[]").unwrap()], &word).is_err());
    }

    #[test]
    fn signatures_and_json_abi() {
        let f = parse_signature("balanceOf(address)(uint256)").unwrap();
        assert_eq!(hex::encode(f.selector()), "70a08231");
        assert_eq!(f.outputs, Some(vec![AbiType::uint(256)]));
        assert_eq!(parse_signature("balanceOf(address)").unwrap().outputs, None);
        let abi = json!([{
            "type": "function", "name": "getReserves", "stateMutability": "view", "inputs": [],
            "outputs": [{"type": "tuple[]", "name": "r", "components": [
                {"type": "uint112", "name": "a"}, {"type": "address", "name": "b"},
            ]}],
        }]);
        let fns = functions_from_abi(abi.as_array().unwrap()).unwrap();
        assert_eq!(fns[0].outputs.as_ref().unwrap()[0].canonical(), "(uint112,address)[]");
        assert!(fns[0].is_read());
        assert_eq!(fns[0].describe(), "getReserves() returns ((uint112,address)[] r)");
    }

    #[test]
    fn cli_values() {
        let int = |t: &str| parse_int(t).unwrap().magnitude;
        assert_eq!(int("1.5ether"), U256::from(1_500_000_000_000_000_000u128));
        assert_eq!(int("0x10"), U256::from(16));
        assert_eq!(int("1e6"), U256::from(1_000_000));
        assert_eq!(int("1_000"), U256::from(1000));
        assert_eq!(int("1.5e18"), U256::from(1_500_000_000_000_000_000u128));
        assert!(parse_int("1.5").is_err());
        assert!(parse_int("abc").is_err());
        assert!(parse_value(&AbiType::Basic(BasicKind::Address, 0), "0x123").is_err());
        assert!(parse_value(&AbiType::uint(8), "256").is_err());
        assert!(parse_value(&AbiType::Basic(BasicKind::Int, 8), "-128").is_ok());
        assert!(parse_value(&AbiType::Basic(BasicKind::Int, 8), "128").is_err());
    }

    #[test]
    fn checksum_and_json() {
        assert_eq!(checksum_address("0xdd9bc35ae942ef0cfa76930954a156b3ff30a4e1"), "0xDD9BC35aE942eF0cFa76930954a156B3fF30a4E1");
        let big = DynSolValue::Uint(U256::from(1u64 << 60), 256);
        assert_eq!(to_json(&AbiType::uint(256), &big), json!((1u64 << 60).to_string()));
        assert_eq!(to_json(&AbiType::uint(256), &DynSolValue::Uint(U256::from(5), 256)), json!(5));
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(format_value(&AbiType::string(), &DynSolValue::String("hi".into())), "'hi'");
    }
}
