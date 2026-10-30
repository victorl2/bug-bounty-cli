//! Readable detail pages for HackenProof programs and audit programs, in the shared document style.

use serde_json::Value;

use super::super::common::iso;
use crate::jv::{self, at, opt, s};
use crate::render::{Doc, Style, date, joined, money, relative, thousands};
use crate::util::{capitalize, links};

const SEVERITIES: &[&str] = &["critical", "high", "medium", "low"];

fn header(doc: &mut Doc, record: &Value) {
    let name = opt(record, "title").unwrap_or(s(record, "slug"));
    let line = format!("{}  {}", doc.style.bold(name), doc.style.link(s(record, "url")));
    doc.raw(line);
}

fn number(v: &Value) -> f64 {
    jv::num(v).unwrap_or(0.0)
}

fn range(low: &Value, high: &Value) -> String {
    let (low, high) = (number(low), number(high));
    if high <= 0.0 {
        return String::new();
    }
    if low > 0.0 && low != high {
        format!("{} – {}", money(Some(low), false), money(Some(high), false))
    } else {
        money(Some(high), false)
    }
}

fn overview(doc: &mut Doc, record: &Value) {
    let st = doc.style;
    header(doc, record);
    let labels = at(record, "labels");
    let end = if jv::t(record, "unending_program") { None } else { iso(at(record, "end_date")) };
    let sla = at(record, "sla");
    let kyc = if jv::t(record, "kyc_required") {
        "required"
    } else if jv::t(record, "kyc_required_for_payment") {
        "required for payment"
    } else {
        "not required"
    };
    let reports = match at(record, "submitted_reports").as_i64() {
        Some(n) => {
            let valid = at(record, "valid_reports_count").as_i64().filter(|v| *v != 0);
            format!("{} submitted{}", thousands(n as f64), valid.map(|v| format!(", {} valid", thousands(v as f64))).unwrap_or_default())
        }
        None => String::new(),
    };
    let sla_line = match (at(sla, "time_to_triage"), at(sla, "time_to_reward"), at(sla, "resolution_time")) {
        (a, b, c) if !a.is_null() && !b.is_null() && !c.is_null() => {
            format!("triage {}d · reward {}d · resolution {}d", jv::text(a), jv::text(b), jv::text(c))
        }
        _ => String::new(),
    };
    let ends = match &end {
        Some(e) => format!("{}  {}", date(Some(e)), st.dim(&relative(Some(e)))),
        None if jv::t(record, "unending_program") => "ongoing".into(),
        None => String::new(),
    };
    doc.fields(&[
        ("Status", st.status(&super::status(record).to_uppercase())),
        ("Max bounty", money(Some(number(at(record, "max_bounty"))), false)),
        ("Min bounty", money(Some(number(at(record, "min_bounty"))), false)),
        ("Starts", date(iso(at(record, "start_date")).as_deref())),
        ("Ends", ends),
        ("KYC", kyc.into()),
        ("PoC", if jv::t(record, "poc_required") { "required".into() } else { String::new() }),
        ("Company", s(at(record, "company"), "company_name").into()),
        ("Triage", s(record, "managed_by_company_name").into()),
        ("Reports", reports),
        ("Paid out", money(Some(number(at(record, "total_rewards"))), false)),
        ("Ecosystems", joined(jv::strs(labels, "platforms"), 0)),
        ("Languages", joined(jv::strs(labels, "languages"), 0)),
        ("Asset types", joined(jv::strs(labels, "types"), 0)),
        ("Project types", joined(jv::strs(labels, "project_types"), 0)),
        ("Response SLA", sla_line),
        ("Repository", s(record, "repo_url").into()),
    ]);
}

fn information(doc: &mut Doc, record: &Value) {
    doc.section("Information");
    doc.text(opt(record, "desc"));
    let rewards = at(record, "rewards");
    let rows: Vec<(String, String)> = SEVERITIES
        .iter()
        .map(|sev| (capitalize(sev), range(at(rewards, &format!("{sev}_min")), at(rewards, &format!("{sev}_max")))))
        .collect();
    if rows.iter().any(|(_, v)| !v.is_empty()) {
        doc.sub("Rewards by severity");
        doc.fields(&rows);
    }
    for (heading, key) in [
        ("Focus area", "focus_area"),
        ("Program rules", "program_rules"),
        ("Disclosure guidelines", "disclosure_guidelines"),
        ("Eligibility", "eligibility_and_coordinate_disclosure"),
    ] {
        if jv::t(record, key) {
            doc.sub(heading);
            doc.text(opt(record, key));
        }
    }
    for block in jv::arr(record, "custom_blocks") {
        doc.sub(opt(block, "title").unwrap_or("Notes"));
        doc.text(opt(block, "description"));
    }
}

fn scope_rows(scopes: &[&Value]) -> Vec<Vec<String>> {
    let mut sorted = scopes.to_vec();
    sorted.sort_by_key(|sc| at(sc, "position").as_i64().unwrap_or(0));
    sorted
        .iter()
        .map(|sc| {
            let notes: Vec<String> = s(sc, "target_description")
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.trim().trim_start_matches(['-', '*', ' ']).replace("**", ""))
                .collect();
            let or_dash = |k: &str| opt(sc, k).unwrap_or("-").to_string();
            vec![or_dash("title"), or_dash("target"), or_dash("criticality"), notes.join(" ")]
        })
        .collect()
}

fn scope(doc: &mut Doc, record: &Value) {
    doc.section("Scope");
    let scopes = jv::arr(record, "scopes");
    let (excluded, included): (Vec<&Value>, Vec<&Value>) = scopes.iter().partition(|sc| jv::t(sc, "out_of_scope"));
    let headers = ["NAME", "TARGET", "SEVERITY", "NOTES"];
    if !included.is_empty() {
        doc.sub(&format!("In scope ({})", included.len()));
        doc.table(&headers, &scope_rows(&included), 3, &[]);
    }
    if !excluded.is_empty() {
        doc.sub(&format!("Out of scope ({})", excluded.len()));
        doc.table(&headers, &scope_rows(&excluded), 3, &[]);
    }
    if scopes.is_empty() {
        let note = doc.style.dim("  No asset list published; the information section describes the scope.");
        doc.raw(note);
    }
}

fn resources(doc: &mut Doc, record: &Value) {
    doc.section("Resources");
    doc.fields(&[("Repository", s(record, "repo_url")), ("Pool", s(record, "pool_url")), ("Final report", s(record, "final_report_url"))]);
    let texts = [opt(record, "desc"), opt(record, "focus_area"), opt(record, "program_rules")]
        .into_iter()
        .chain(jv::arr(record, "custom_blocks").iter().map(|b| opt(b, "description")))
        .chain(jv::arr(record, "scopes").iter().filter(|sc| !jv::t(sc, "out_of_scope")).map(|sc| opt(sc, "target")));
    let found = links(texts);
    if !found.is_empty() {
        doc.sub(&format!("Links ({})", found.len()));
        let lines: Vec<String> = found.iter().map(|l| format!("  {}", doc.style.link(l))).collect();
        doc.raw(lines.join("\n"));
    }
}

fn leaderboard(doc: &mut Doc, record: &Value) {
    doc.section("Leaderboard");
    let ranks = jv::arr(record, "hacker_ranks");
    if ranks.is_empty() {
        let note = doc.style.dim("  No ranked hackers yet.");
        doc.raw(note);
        return;
    }
    let count = |r: &Value, k: &str| if jv::t(r, k) { jv::text(at(r, k)) } else { "0".into() };
    let rows: Vec<Vec<String>> = ranks
        .iter()
        .map(|r| {
            let rank = if r.get("rank").is_some() { jv::text(at(r, "rank")) } else { "-".into() };
            vec![rank, opt(at(r, "user"), "nickname").unwrap_or("-").to_string(), count(r, "reports_amount"), count(r, "reputation")]
        })
        .collect();
    doc.table(&["#", "HACKER", "REPORTS", "REPUTATION"], &rows, 1, &[]);
    if let Some(total) = at(record, "hacker_ranks_count").as_u64().filter(|t| *t as usize > ranks.len()) {
        let note = doc.style.dim(&format!("  top {} of {total} ranked hackers", ranks.len()));
        doc.raw(note);
    }
}

/// Renders one section of a detail page.
type Section = fn(&mut Doc, &Value);

const SECTIONS: &[(&str, Section)] =
    &[("overview", overview), ("information", information), ("scope", scope), ("resources", resources), ("leaderboard", leaderboard)];

pub fn render(record: &Value, sections: &[String], style: Style) -> String {
    let mut doc = Doc::new(style);
    let names: Vec<&str> = if sections.iter().any(|s| s == "all") {
        SECTIONS.iter().map(|(n, _)| *n).collect()
    } else {
        sections.iter().map(String::as_str).collect()
    };
    if !names.contains(&"overview") {
        header(&mut doc, record);
    }
    for name in names {
        if let Some((_, section)) = SECTIONS.iter().find(|(n, _)| *n == name) {
            section(&mut doc, record);
        }
    }
    doc.render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_program() {
        let record = json!({"slug": "layer3", "title": "Layer3", "url": "https://hackenproof.com/programs/layer3",
            "max_bounty": "50000", "min_bounty": 100, "status": {"name": "Active"}, "unending_program": true,
            "rewards": {"critical_min": 10000, "critical_max": 50000, "high_max": 5000},
            "sla": {"time_to_triage": 2, "time_to_reward": 14, "resolution_time": 30},
            "scopes": [{"title": "Token", "target": "https://etherscan.io/address/0x1", "criticality": "Critical", "position": 2,
                        "target_description": "- **main** token\n"},
                       {"title": "Old", "target": "x", "out_of_scope": true}],
            "hacker_ranks": [{"rank": 1, "user": {"nickname": "neo"}, "reports_amount": 3, "reputation": 90}],
            "hacker_ranks_count": 40});
        let out = render(&record, &["all".to_string()], Style::new(false));
        assert!(out.contains("Max bounty:   $50,000"), "{out}");
        assert!(out.contains("Ends:         ongoing"), "{out}");
        assert!(out.contains("Critical: $10,000 – $50,000"), "{out}");
        assert!(out.contains("High:     $5,000"), "{out}");
        assert!(out.contains("triage 2d · reward 14d · resolution 30d"), "{out}");
        assert!(out.contains("In scope (1)") && out.contains("Out of scope (1)"), "{out}");
        assert!(out.contains("main token"), "{out}");
        assert!(out.contains("top 1 of 40 ranked hackers"), "{out}");
    }
}
