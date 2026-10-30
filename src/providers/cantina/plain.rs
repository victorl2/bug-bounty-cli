//! Plain terminal rendering of arbitrary results; machine output remains lossless JSON.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

static CAMEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([a-z])([A-Z])").unwrap());

fn label(key: &str) -> String {
    crate::util::capitalize(&CAMEL.replace_all(key, "$1 $2").replace('_', " "))
}

fn scalar(v: &Value) -> String {
    match v {
        Value::Bool(true) => "yes".into(),
        Value::Bool(false) => "no".into(),
        other => crate::jv::text(other),
    }
}

pub fn render(value: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                match item {
                    Value::Null => {}
                    Value::Object(_) | Value::Array(_) => {
                        out.push_str(&format!("{pad}{}:\n", label(key)));
                        render(item, indent + 2, out);
                    }
                    Value::String(s) if s.contains('\n') => out.push_str(&format!("{pad}{}:\n{s}\n\n", label(key))),
                    other => out.push_str(&format!("{pad}{}: {}\n", label(key), scalar(other))),
                }
            }
        }
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str(&format!("{pad}(none)\n"));
            }
            for item in items {
                if item.is_object() || item.is_array() {
                    render(item, indent, out);
                    out.push('\n');
                } else {
                    out.push_str(&format!("{pad}- {}\n", crate::jv::text(item)));
                }
            }
        }
        other => out.push_str(&format!("{pad}{}\n", crate::jv::text(other))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn labels_and_nesting() {
        let mut out = String::new();
        render(&json!({"authenticated": true, "teamName": "x", "skip": null, "items": []}), 0, &mut out);
        assert_eq!(out, "Authenticated: yes\nTeam name: x\nItems:\n  (none)\n");
    }
}
