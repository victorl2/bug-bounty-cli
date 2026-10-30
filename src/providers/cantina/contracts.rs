//! Cantina scope adapted to the EVM inspection engine: deployed contracts with explorer links.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use super::discovery::assets;
use crate::jv::{self, s};

static ADDRESS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"0x[0-9a-fA-F]{40}").unwrap());
static EXPLORER_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/(address|token)/0x[0-9a-fA-F]{40}(?:[/?#]|$)").unwrap());

/// The first 40-hex address not followed by more hex digits.
fn address(text: &str) -> Option<&str> {
    ADDRESS.find_iter(text).find(|m| !text[m.end()..].starts_with(|c: char| c.is_ascii_hexdigit())).map(|m| m.as_str())
}

/// `{"id", "name", "contracts": [{address, url, name, group}], "unresolved": [{reference, reason}]}`.
pub fn scope(record: &Value) -> Value {
    let mut refs: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    for asset in assets(record) {
        if jv::t(&asset, "outOfScope") {
            continue;
        }
        let reference = s(&asset, "reference");
        let Some(addr) = address(reference) else { continue };
        let parsed = url::Url::parse(reference).ok();
        let host = parsed.as_ref().and_then(|u| u.host_str()).unwrap_or("").to_string();
        let explorer = parsed.as_ref().is_some_and(|u| matches!(u.scheme(), "https" | "http") && EXPLORER_PATH.is_match(u.path()));
        if !explorer {
            // Commit hashes in source paths are not deployment addresses.
            if !matches!(host.as_str(), "github.com" | "raw.githubusercontent.com") {
                skipped.push(json!({"reference": reference, "reason": "Address has no explorer chain context"}));
            }
            continue;
        }
        let key = (host, addr.to_lowercase());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        refs.push(json!({"address": addr, "url": reference, "name": jv::at(&asset, "name"), "group": jv::at(&asset, "group")}));
    }
    json!({"id": jv::at(record, "id"), "name": jv::at(record, "name"), "contracts": refs, "unresolved": skipped})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_links_only() {
        let a = "0x1111111111111111111111111111111111111111";
        let record = json!({"assetGroups": [{"name": "G", "assets": [
            {"name": "Vault", "reference": format!("https://etherscan.io/address/{a}")},
            {"name": "Dup", "reference": format!("https://etherscan.io/address/{a}#code")},
            {"name": "Src", "reference": format!("https://github.com/x/y/blob/{a}/A.sol")},
            {"name": "Bare", "reference": a},
            {"name": "Long", "reference": format!("https://etherscan.io/tx/{a}ff")}]}]});
        let out = scope(&record);
        assert_eq!(out["contracts"].as_array().unwrap().len(), 1);
        assert_eq!(out["contracts"][0]["name"], "Vault");
        assert_eq!(out["unresolved"].as_array().unwrap().len(), 1);
    }
}
