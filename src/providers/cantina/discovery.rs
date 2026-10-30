//! Lifecycle status and listing filters. Tag filters use explicit metadata and file/explorer
//! references, never guessed chain names.

use std::sync::LazyLock;

use chrono::Utc;
use regex::Regex;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::evm::explorers::host_of;
use crate::jv::{self, s};
use crate::render::parse_date;

const EVALUATING: &[&str] = &["judging", "escalations", "escalations_ended"];
const FINISHED: &[&str] = &["complete", "published", "migrated"];

pub fn status(record: &Value) -> String {
    let raw = s(record, "status");
    if EVALUATING.contains(&raw) {
        return "evaluating".into();
    }
    if FINISHED.contains(&raw) {
        return "finished".into();
    }
    if raw == "live" {
        let timeframe = jv::at(record, "timeframe");
        if let Some(start) = jv::opt(timeframe, "start").and_then(parse_date) {
            let now = Utc::now();
            if start > now {
                return "upcoming".into();
            }
            if jv::opt(timeframe, "end").and_then(parse_date).is_some_and(|end| end <= now) {
                return "evaluating".into();
            }
        }
    }
    raw.to_string()
}

/// In-scope and out-of-scope assets, flattened from nested asset groups.
pub fn assets(record: &Value) -> Vec<Value> {
    fn group(g: &Value, excluded: bool, out: &mut Vec<Value>) {
        let excluded = excluded || jv::t(g, "outOfScope");
        if jv::t(g, "hidden") {
            return;
        }
        for a in jv::arr(g, "assets") {
            if !jv::t(a, "hidden") {
                out.push(jv::merge(a, &json!({"outOfScope": excluded, "group": jv::at(g, "name")})));
            }
        }
        for sub in jv::arr(g, "subGroups") {
            group(sub, excluded, out);
        }
    }
    let mut out = Vec::new();
    for g in jv::arr(record, "assetGroups") {
        group(g, false, &mut out);
    }
    out
}

static EXTENSIONS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [("sol", "Solidity"), ("rs", "Rust"), ("move", "Move"), ("vy", "Vyper"), ("cairo", "Cairo"), ("go", "Go")]
        .into_iter()
        .map(|(ext, name)| (Regex::new(&format!(r"\.{ext}\b")).unwrap(), name))
        .collect()
});

const EXPLORER_ECOSYSTEMS: &[(&str, &str)] = &[
    ("etherscan.io", "Ethereum"),
    ("arbiscan.io", "Arbitrum"),
    ("basescan.org", "Base"),
    ("optimistic.etherscan.io", "Optimism"),
    ("bscscan.com", "BSC"),
    ("polygonscan.com", "Polygon"),
    ("snowtrace.io", "Avalanche"),
    ("solscan.io", "Solana"),
    ("suivision.xyz", "Sui"),
    ("starkscan.co", "Starknet"),
];

fn sorted_unique(mut items: Vec<String>) -> Vec<String> {
    items.sort();
    items.dedup();
    items
}

/// Tags backed by evidence: explicit tags, scope group names, explorer URLs and file extensions.
pub fn evidence(record: &Value) -> Value {
    let refs: Vec<String> = assets(record).iter().filter(|a| !jv::t(a, "outOfScope")).map(|a| s(a, "reference").to_string()).collect();
    let text = format!("{}\n{}", refs.join("\n"), s(record, "instructions"));
    let tags = jv::at(record, "tags");
    let explicit = |k: &str| jv::strs(tags, k).into_iter().map(str::to_string).collect::<Vec<_>>();
    let mut languages: Vec<String> = EXTENSIONS.iter().filter(|(re, _)| re.is_match(&text)).map(|(_, n)| n.to_string()).collect();
    languages.extend(explicit("language"));
    let mut ecosystems: Vec<String> =
        refs.iter().filter_map(|r| EXPLORER_ECOSYSTEMS.iter().find(|(h, _)| *h == host_of(r)).map(|(_, n)| n.to_string())).collect();
    ecosystems.extend(explicit("ecosystem"));
    let mut types: Vec<String> =
        jv::arr(record, "assetGroups").iter().filter(|g| !jv::t(g, "outOfScope")).map(|g| s(g, "name").to_string()).collect();
    types.extend(explicit("programType"));
    json!({
        "language": sorted_unique(languages),
        "ecosystem": sorted_unique(ecosystems),
        "programType": types,
        "projectType": explicit("projectType"),
        "source": "explicit tags, scope group names, explorer URLs and file extensions in instructions/references",
    })
}

/// Listing options Cantina understands.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub status: Option<String>,
    pub search: String,
    pub min_bounty: f64,
    pub kyc: Option<bool>,
    pub language: Option<String>,
    pub ecosystem: Option<String>,
    pub program_type: Option<String>,
    pub project_type: Option<String>,
    pub sort: String,
    pub reverse: bool,
    pub limit: Option<usize>,
}

pub fn apply(records: Vec<Value>, o: &Options) -> Result<Vec<Value>> {
    if o.sort == "vault" {
        return Err(Error::value("Cantina does not publish an Immunefi-style vault balance; sort by bounty/reward instead"));
    }
    if o.project_type.is_some() && !records.iter().any(|r| jv::t(jv::at(r, "tags"), "projectType")) {
        return Err(Error::value("Cantina does not expose project-type tags; this filter cannot be applied reliably"));
    }
    let tag_filters = o.language.is_some() || o.ecosystem.is_some() || o.program_type.is_some() || o.project_type.is_some();
    let mut out = Vec::new();
    for r in records {
        let actual = status(&r);
        match o.status.as_deref() {
            None | Some("all") => {}
            Some("ended") => {
                if !(EVALUATING.contains(&actual.as_str())
                    || FINISHED.contains(&actual.as_str())
                    || matches!(actual.as_str(), "evaluating" | "finished"))
                {
                    continue;
                }
            }
            Some("active") => {
                if !matches!(actual.as_str(), "upcoming" | "live") {
                    continue;
                }
            }
            Some(wanted) => {
                if actual != wanted {
                    continue;
                }
            }
        }
        let company = jv::at(&r, "company");
        let hay = [s(&r, "id"), s(&r, "name"), s(company, "name"), s(company, "handle")].join(" ").to_lowercase();
        if !hay.contains(&o.search.to_lowercase()) {
            continue;
        }
        if jv::f(&r, "totalRewardPot").unwrap_or(0.0) < o.min_bounty {
            continue;
        }
        if let Some(k) = o.kyc {
            if jv::tri(&r, "kycRequired") != Some(k) {
                continue;
            }
        }
        let mut r = r;
        if tag_filters {
            let ev = evidence(&r);
            let pairs = [
                (&o.language, "language"),
                (&o.ecosystem, "ecosystem"),
                (&o.program_type, "programType"),
                (&o.project_type, "projectType"),
            ];
            let missing = pairs.iter().any(|(needle, key)| {
                needle.as_ref().is_some_and(|n| !jv::strs(&ev, key).iter().any(|tag| tag.to_lowercase().contains(&n.to_lowercase())))
            });
            if missing {
                continue;
            }
            r["filterEvidence"] = ev;
        }
        out.push(r);
    }
    let text_key = |r: &Value| -> String {
        let tf = jv::at(r, "timeframe");
        match o.sort.as_str() {
            "name" => s(r, "name").to_lowercase(),
            "start" | "launched" => s(tf, "start").into(),
            "end" => s(tf, "end").into(),
            _ => jv::first(r, &["updatedAt", "createdAt"]).as_str().unwrap_or("").into(),
        }
    };
    let numeric = matches!(o.sort.as_str(), "reward" | "bounty" | "pool");
    out.sort_by(|a, b| {
        let ord = if numeric {
            jv::f(a, "totalRewardPot").unwrap_or(0.0).total_cmp(&jv::f(b, "totalRewardPot").unwrap_or(0.0))
        } else {
            text_key(a).cmp(&text_key(b))
        };
        if (o.sort != "name") != o.reverse { ord.reverse() } else { ord }
    });
    if let Some(n) = o.limit.filter(|n| *n > 0) {
        out.truncate(n);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle() {
        assert_eq!(status(&json!({"status": "judging"})), "evaluating");
        assert_eq!(status(&json!({"status": "published"})), "finished");
        assert_eq!(status(&json!({"status": "live", "timeframe": {"start": "2999-01-01T00:00:00Z"}})), "upcoming");
        assert_eq!(
            status(&json!({"status": "live", "timeframe": {"start": "2020-01-01T00:00:00Z", "end": "2020-02-01T00:00:00Z"}})),
            "evaluating"
        );
        assert_eq!(status(&json!({"status": "live", "timeframe": {"start": "2020-01-01T00:00:00Z"}})), "live");
    }

    #[test]
    fn evidence_from_references() {
        let record = json!({"instructions": "see src/Vault.sol", "assetGroups": [
            {"name": "Contracts", "assets": [{"reference": "https://etherscan.io/address/0x1"}]},
            {"name": "Old", "outOfScope": true, "assets": [{"reference": "https://solscan.io/x"}]}]});
        let ev = evidence(&record);
        assert_eq!(ev["language"], json!(["Solidity"]));
        assert_eq!(ev["ecosystem"], json!(["Ethereum"]));
        assert_eq!(ev["programType"], json!(["Contracts"]));
    }

    #[test]
    fn apply_filters_and_sorts() {
        let rows = vec![
            json!({"id": "a", "name": "Alpha", "status": "live", "totalRewardPot": "100", "kycRequired": false}),
            json!({"id": "b", "name": "Beta", "status": "live", "totalRewardPot": "300", "kycRequired": true}),
        ];
        let o = Options { sort: "bounty".into(), ..Default::default() };
        let ids = |v: Vec<Value>| v.iter().map(|r| s(r, "id").to_string()).collect::<Vec<_>>();
        assert_eq!(ids(apply(rows.clone(), &o).unwrap()), ["b", "a"]);
        assert_eq!(ids(apply(rows.clone(), &Options { kyc: Some(false), ..o.clone() }).unwrap()), ["a"]);
        assert!(apply(rows.clone(), &Options { sort: "vault".into(), ..o.clone() }).is_err());
        assert!(apply(rows, &Options { project_type: Some("defi".into()), ..o }).is_err());
    }
}
