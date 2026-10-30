//! Decoder for the React Server Components ("flight") payload embedded in Next.js pages.
//!
//! The page data is streamed into the HTML as `self.__next_f.push([1, "<chunk>"])` script tags.
//! Concatenated, the chunks form rows:
//!
//! ```text
//! <hex id>:<json>\n          regular row
//! <hex id>:T<hex len>,<text>  text row (length in UTF-8 bytes, no trailing newline)
//! <hex id>:I[...]\n          module import (ignored)
//! ```
//!
//! Values reference other rows with `"$<id>"` strings, which [`resolve`] follows. The same
//! stream is served bare (`text/x-component`) when a page is requested with an `RSC: 1` header.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::error::{Error, Result};

static PUSH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)self\.__next_f\.push\((\[.*?\])\)</script>").unwrap());
const SKIPPED_ROW_TAGS: &[u8] = b"IHXEDW";
const MAX_DEPTH: usize = 64;
const NOT_FOUND_MARKER: &str = "NEXT_HTTP_ERROR_FALLBACK;404";

pub type Rows = HashMap<String, Value>;

pub fn extract_flight(html: &str) -> Result<String> {
    let chunks: Vec<String> = PUSH
        .captures_iter(html)
        .filter_map(|c| serde_json::from_str::<Value>(&c[1]).ok())
        .filter_map(|item| item.get(1).and_then(Value::as_str).map(str::to_string))
        .collect();
    if chunks.is_empty() {
        return Err(Error::Payload("no Next.js flight data found in page".into()));
    }
    Ok(chunks.concat())
}

pub fn parse_rows(flight: &str) -> Rows {
    parse_ordered(flight).0
}

/// The rows, plus their ids in stream order (first appearance).
fn parse_ordered(flight: &str) -> (Rows, Vec<String>) {
    let data = flight.as_bytes();
    let n = data.len();
    let mut rows = Rows::new();
    let mut order = Vec::new();
    let mut insert = |rows: &mut Rows, id: String, value: Value| {
        if rows.insert(id.clone(), value).is_none() {
            order.push(id);
        }
    };
    let mut i = 0;
    while i < n {
        let id_end = data[i..].iter().position(|b| !b.is_ascii_hexdigit() || b.is_ascii_uppercase()).map(|p| i + p);
        let Some(colon) = id_end.filter(|&p| data[p] == b':') else {
            i = data[i..].iter().position(|&b| b == b'\n').map(|p| i + p + 1).unwrap_or(n);
            continue;
        };
        let row_id = String::from_utf8_lossy(&data[i..colon]).into_owned();
        i = colon + 1;
        if data.get(i) == Some(&b'T') {
            let Some(comma) = data[i..].iter().position(|&b| b == b',').map(|p| i + p) else { break };
            let length = usize::from_str_radix(&String::from_utf8_lossy(&data[i + 1..comma]), 16).unwrap_or(0);
            let start = comma + 1;
            let end = (start + length).min(n);
            insert(&mut rows, row_id, Value::String(String::from_utf8_lossy(&data[start..end]).into_owned()));
            i = end;
            continue;
        }
        let j = data[i..].iter().position(|&b| b == b'\n').map(|p| i + p).unwrap_or(n);
        let line = &data[i..j];
        i = j + 1;
        if line.first().is_some_and(|b| SKIPPED_ROW_TAGS.contains(b)) {
            continue;
        }
        if let Ok(value) = serde_json::from_slice(line) {
            insert(&mut rows, row_id, value);
        }
    }
    (rows, order)
}

pub fn resolve(value: &Value, rows: &Rows) -> Value {
    resolve_at(value, rows, 0)
}

fn resolve_at(value: &Value, rows: &Rows, depth: usize) -> Value {
    if depth > MAX_DEPTH {
        return value.clone();
    }
    match value {
        Value::String(s) if s.starts_with('$') && s.len() > 1 => {
            if s == "$undefined" {
                return Value::Null;
            }
            if let Some(rest) = s.strip_prefix("$$") {
                return Value::String(format!("${rest}"));
            }
            let reference = &s[1..];
            let tag = reference.chars().next().unwrap_or(' ');
            if tag == 'D' {
                return Value::String(reference[1..].to_string()); // serialized Date
            }
            if "L@SFQWKBnNI".contains(tag) {
                return value.clone(); // lazy/promise/symbol/etc. - not plain data
            }
            let mut parts = reference.split(':');
            let row_id = parts.next().unwrap_or("");
            let Some(mut target) = rows.get(row_id) else { return value.clone() };
            for key in parts {
                target = match target {
                    Value::Object(o) => o.get(key).unwrap_or(&Value::Null),
                    Value::Array(a) => match key.parse::<usize>().ok().and_then(|i| a.get(i)) {
                        Some(v) => v,
                        None => return value.clone(),
                    },
                    _ => return value.clone(),
                };
            }
            resolve_at(target, rows, depth + 1)
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| resolve_at(v, rows, depth + 1)).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), resolve_at(v, rows, depth + 1))).collect()),
        other => other.clone(),
    }
}

fn find_key<'a>(node: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match node {
        Value::Object(map) => {
            if let Some(v) = map.get(key) {
                out.push(v);
            }
            for v in map.values() {
                find_key(v, key, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| find_key(v, key, out)),
        _ => {}
    }
}

/// A decoded page, given either its HTML or its raw RSC stream.
pub struct Payload {
    pub rows: Rows,
    /// RSC requests answer 200 for missing pages; the 404 is signalled inside the stream.
    pub not_found: bool,
    order: Vec<String>,
}

impl Payload {
    pub fn new(body: &str) -> Result<Self> {
        let flight = if body.trim_start().starts_with('<') { extract_flight(body)? } else { body.to_string() };
        let (rows, order) = parse_ordered(&flight);
        Ok(Payload { not_found: flight.contains(NOT_FOUND_MARKER), rows, order })
    }

    /// The first value stored under `key` anywhere in the payload that satisfies `accept`.
    pub fn find(&self, key: &str, accept: impl Fn(&Value) -> bool) -> Result<Value> {
        for id in &self.order {
            let mut found = Vec::new();
            find_key(&self.rows[id], key, &mut found);
            for candidate in found {
                let value = resolve(candidate, &self.rows);
                if accept(&value) {
                    return Ok(value);
                }
            }
        }
        Err(Error::Payload(format!("page payload has no value under '{key}'")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_text_and_references() {
        let flight = "0:{\"bounty\":\"$1\",\"d\":\"$D2024-01-01\"}\n1:{\"slug\":\"x\",\"body\":\"$2\"}\n2:T5,hello3:I[\"mod\"]\n";
        let page = Payload::new(flight).unwrap();
        let bounty = page.find("bounty", Value::is_object).unwrap();
        assert_eq!(bounty, json!({"slug": "x", "body": "hello"}));
        assert!(!page.rows.contains_key("3"));
        assert_eq!(resolve(&json!("$D2024-01-01"), &page.rows), json!("2024-01-01"));
        assert_eq!(resolve(&json!("$undefined"), &page.rows), Value::Null);
        assert_eq!(resolve(&json!("$$x"), &page.rows), json!("$x"));
    }

    #[test]
    fn html_wrapper_and_not_found() {
        let html = r#"<html><script>self.__next_f.push([1,"0:{\"a\":1}\n"])</script><script>self.__next_f.push([1,"NEXT_HTTP_ERROR_FALLBACK;404"])</script>"#;
        let page = Payload::new(html).unwrap();
        assert!(page.not_found);
        assert_eq!(page.find("a", Value::is_number).unwrap(), json!(1));
        assert!(Payload::new("<html></html>").is_err());
    }
}
