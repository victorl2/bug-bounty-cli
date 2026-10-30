//! Lenient reads of platform JSON, mirroring how loosely the platforms type their fields.

use serde_json::Value;

static NULL: Value = Value::Null;

/// `v[key]`, or null when `v` is not an object or lacks the key.
pub fn at<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(&NULL)
}

/// Follow a path of object keys.
pub fn path<'a>(v: &'a Value, keys: &[&str]) -> &'a Value {
    keys.iter().fold(v, |cur, k| at(cur, k))
}

/// A string field, `""` when missing or not a string.
pub fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    at(v, key).as_str().unwrap_or("")
}

/// A string field, `None` when missing, empty or not a string.
pub fn opt<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    at(v, key).as_str().filter(|s| !s.is_empty())
}

/// A number, also accepting numeric strings (several APIs send amounts as text).
pub fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn f(v: &Value, key: &str) -> Option<f64> {
    num(at(v, key))
}

/// Python truthiness.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

pub fn t(v: &Value, key: &str) -> bool {
    truthy(at(v, key))
}

/// An array field, empty when missing.
pub fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    at(v, key).as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// The strings in an array field.
pub fn strs<'a>(v: &'a Value, key: &str) -> Vec<&'a str> {
    arr(v, key).iter().filter_map(Value::as_str).collect()
}

/// Python's `str(value)` for scalars: `None` → "", floats with integral values keep `.0`.
pub fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// The first non-empty value among `keys`, as `or` chains do in the Python original.
pub fn first<'a>(v: &'a Value, keys: &[&str]) -> &'a Value {
    keys.iter().map(|k| at(v, k)).find(|x| truthy(x)).unwrap_or(&NULL)
}

/// `true` / `false` / `None` for a nullable boolean.
pub fn tri(v: &Value, key: &str) -> Option<bool> {
    match at(v, key) {
        Value::Null => None,
        other => Some(truthy(other)),
    }
}

/// Merge objects left to right (`{**a, **b}`).
pub fn merge(base: &Value, over: &Value) -> Value {
    let mut out = base.as_object().cloned().unwrap_or_default();
    if let Some(o) = over.as_object() {
        for (k, v) in o {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}
