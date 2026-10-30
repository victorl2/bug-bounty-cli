//! Readable detail pages for Cantina bounties and competitions, in the shared document style.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::discovery::status as lifecycle;
use crate::jv::{self, at, opt, s, t};
use crate::render::{Doc, Style, date, datetime_utc, joined, relative, thousands};
use crate::util::capitalize;

const SEVERITY_ORDER: &[&str] = &["critical", "high", "medium", "low", "informational", "gas"];
static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>\)\]"']+"#).unwrap());

pub fn amount(value: &Value, currency: Option<&str>) -> String {
    let Some(number) = jv::num(value).filter(|n| *n > 0.0) else { return "-".into() };
    match currency {
        // Dollar-pegged: show as dollars.
        None | Some("") | Some("USD") => format!("${}", thousands(number)),
        Some(c @ ("USDC" | "USDT" | "DAI")) => format!("${} {c}", thousands(number)),
        Some(c) => format!("{} {c}", thousands(number)),
    }
}

pub fn is_competition(record: &Value) -> bool {
    s(record, "kind").contains("contest")
}

fn groups(record: &Value) -> Vec<&Value> {
    jv::arr(record, "assetGroups").iter().filter(|g| !t(g, "hidden")).collect()
}

fn rewards(group: &Value, currency: Option<&str>) -> String {
    let mut rows: Vec<&Value> = jv::arr(group, "rewards").iter().collect();
    rows.sort_by_key(|r| SEVERITY_ORDER.iter().position(|x| *x == s(r, "severity")).unwrap_or(99));
    rows.iter()
        .filter(|r| amount(at(r, "maxReward"), currency) != "-")
        .map(|r| format!("{} {}", capitalize(s(r, "severity")), amount(at(r, "maxReward"), currency)))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn repository(record: &Value) -> String {
    let url = s(record, "gitRepoUrl");
    let commit = s(record, "commitHash");
    if !url.is_empty() && !commit.is_empty() { format!("{url} @ {}", &commit[..commit.len().min(10)]) } else { url.into() }
}

fn header(doc: &mut Doc, record: &Value) {
    let line = format!("{}  {}", doc.style.bold(opt(record, "name").unwrap_or(s(record, "id"))), doc.style.link(s(record, "url")));
    doc.raw(line);
}

fn overview(doc: &mut Doc, record: &Value) {
    let st = doc.style;
    header(doc, record);
    let currency = opt(record, "currencyCode");
    let timeframe = at(record, "timeframe");
    let company = at(record, "company");
    let mut pairs: Vec<(&str, String)> = vec![("Status", st.status(&lifecycle(record).to_uppercase()))];
    if is_competition(record) {
        let (start, end) = (opt(timeframe, "start"), opt(timeframe, "end"));
        pairs.extend([
            ("Reward pool", amount(at(record, "totalRewardPot"), currency)),
            ("Starts", format!("{}  {}", datetime_utc(start), st.dim(&relative(start)))),
            ("Ends", format!("{}  {}", datetime_utc(end), st.dim(&relative(end)))),
        ]);
    } else {
        pairs.extend([("Max reward", amount(at(record, "totalRewardPot"), currency)), ("Launched", date(opt(timeframe, "start")))]);
    }
    let fee = at(record, "submissionFee");
    let kyc = match jv::tri(record, "kycRequired") {
        Some(true) => "required",
        Some(false) => "not required",
        None => "",
    };
    pairs.extend([
        ("KYC", kyc.into()),
        ("Submission fee", if jv::num(fee).is_some_and(|f| f > 0.0) { amount(fee, currency) } else { String::new() }),
        ("Triage", if t(record, "cantinaTriaged") { "platform managed".into() } else { String::new() }),
        ("Severities", joined(jv::strs(record, "consideredSeverities"), 0)),
        ("Findings", at(record, "totalFindings").as_i64().map(|n| format!("{} submitted", thousands(n as f64))).unwrap_or_default()),
        ("Company", s(company, "name").into()),
        ("Website", s(company, "website").into()),
        ("Repository", repository(record)),
    ]);
    doc.fields(&pairs);
}

fn information(doc: &mut Doc, record: &Value) {
    doc.section("Information");
    let currency = opt(record, "currencyCode");
    let rewards: Vec<(String, String)> = groups(record)
        .iter()
        .filter(|g| !t(g, "outOfScope"))
        .map(|g| (opt(g, "name").unwrap_or("-").to_string(), rewards(g, currency)))
        .filter(|(_, line)| !line.is_empty())
        .collect();
    doc.fields(&[
        ("Allowed severities", joined(jv::strs(record, "allowedSeverities"), 0)),
        ("Rewarded severities", joined(jv::strs(record, "consideredSeverities"), 0)),
    ]);
    if !rewards.is_empty() {
        doc.sub("Rewards by asset group");
        doc.fields(&rewards);
    }
    if t(record, "instructions") {
        doc.sub("Instructions");
        doc.text(opt(record, "instructions"));
    }
}

fn asset_rows(group: &Value) -> Vec<Vec<String>> {
    jv::arr(group, "assets")
        .iter()
        .filter(|a| !t(a, "hidden"))
        .map(|a| {
            let mut reference = s(a, "reference").trim().to_string();
            let mut description = s(a, "description").trim().to_string();
            if reference.is_empty() && LINK.find(&description).is_some_and(|m| m.as_str() == description) {
                reference = std::mem::take(&mut description);
            }
            let lines: Vec<String> = description
                .lines()
                .filter(|l| !matches!(l.trim(), "" | "-"))
                .map(|l| l.trim().trim_start_matches(['-', '*', ' ']).replace('`', ""))
                .collect();
            vec![opt(a, "name").unwrap_or("-").to_string(), if reference.is_empty() { "-".into() } else { reference }, lines.join(" ")]
        })
        .collect()
}

fn scope(doc: &mut Doc, record: &Value) {
    doc.section("Scope");
    let st = doc.style;
    let groups = groups(record);
    if groups.is_empty() {
        doc.fields(&[("Repository", repository(record)), ("Readme", s(record, "readmePath").to_string())]);
        doc.raw(st.dim("  No asset list published; the repository README describes the scope."));
        return;
    }
    let currency = opt(record, "currencyCode");
    for group in groups.iter().filter(|g| !t(g, "outOfScope")) {
        let rows = asset_rows(group);
        doc.sub(&format!("{} ({})", opt(group, "name").unwrap_or("Assets"), rows.len()));
        let line = rewards(group, currency);
        if !line.is_empty() {
            doc.raw(format!("  {}", st.dim(&line)));
        }
        doc.table(&["NAME", "TARGET", "NOTES"], &rows, 2, &[]);
    }
    let excluded: Vec<&&Value> = groups.iter().filter(|g| t(g, "outOfScope")).collect();
    if !excluded.is_empty() {
        doc.sub("Out of scope");
        for group in excluded {
            if !s(group, "name").trim().is_empty() {
                doc.raw(format!("  {}", st.bold(s(group, "name"))));
            }
            doc.text(opt(group, "description"));
            doc.table(&["NAME", "TARGET", "NOTES"], &asset_rows(group), 2, &[]);
        }
    }
}

fn resources(doc: &mut Doc, record: &Value) {
    doc.section("Resources");
    let company = at(record, "company");
    doc.fields(&[
        ("Repository", s(record, "gitRepoUrl").to_string()),
        ("Commit", s(record, "commitHash").to_string()),
        ("Readme", s(record, "readmePath").to_string()),
        ("Website", s(company, "website").to_string()),
        ("GitHub", opt(company, "github").map(|g| format!("https://github.com/{g}")).unwrap_or_default()),
        ("Twitter", opt(company, "twitter").map(|g| format!("https://x.com/{g}")).unwrap_or_default()),
    ]);
    let mut links: Vec<String> = Vec::new();
    for text in std::iter::once(s(record, "instructions")).chain(groups(record).iter().map(|g| s(g, "description"))) {
        for m in LINK.find_iter(text) {
            if !links.iter().any(|l| l == m.as_str()) {
                links.push(m.as_str().to_string());
            }
        }
    }
    if !links.is_empty() {
        doc.sub(&format!("Links ({})", links.len()));
        let lines: Vec<String> = links.iter().map(|l| format!("  {}", doc.style.link(l))).collect();
        doc.raw(lines.join("\n"));
    }
}

fn team(entry: &Value) -> String {
    let team = at(entry, "team");
    let names: Vec<&str> = jv::arr(team, "members").iter().filter_map(|m| opt(m, "username").or_else(|| opt(m, "name"))).collect();
    opt(team, "name")
        .or_else(|| opt(team, "handle"))
        .map(str::to_string)
        .unwrap_or_else(|| if names.is_empty() { "-".into() } else { names.join(", ") })
}

fn leaderboard(doc: &mut Doc, record: &Value) {
    doc.section("Leaderboard");
    let board = at(record, "leaderboard");
    if !is_competition(record) {
        doc.raw(doc.style.dim("  Bounties have no leaderboard."));
        return;
    }
    if !t(board, "available") {
        doc.raw(doc.style.dim(&format!("  {}", opt(board, "reason").unwrap_or("Not published yet."))));
        return;
    }
    let ranking = jv::arr(at(board, "data"), "ranking");
    let currency = opt(record, "currencyCode");
    // Finished competitions often publish rewards with zeroed finding counts; omit empty columns.
    let counted: Vec<&str> =
        ["critical", "high", "medium", "low"].into_iter().filter(|sev| ranking.iter().any(|e| t(e, &format!("{sev}Findings")))).collect();
    let rows: Vec<Vec<String>> = ranking
        .iter()
        .map(|e| {
            let position = if e.get("position").is_some() { jv::text(at(e, "position")) } else { "-".into() };
            let mut row = vec![position, team(e), amount(at(e, "reward"), currency)];
            row.extend(counted.iter().map(|sev| {
                let k = format!("{sev}Findings");
                if t(e, &k) { jv::text(at(e, &k)) } else { "0".into() }
            }));
            row
        })
        .collect();
    let upper: Vec<String> = counted.iter().map(|sev| sev.to_uppercase()[..4].to_string()).collect();
    let mut headers = vec!["#", "TEAM", "REWARD"];
    headers.extend(upper.iter().map(String::as_str));
    doc.table(&headers, &rows, 1, &[]);
}

/// Renders one section of a detail page.
type Section = fn(&mut Doc, &Value);

const SECTIONS: &[(&str, Section)] =
    &[("overview", overview), ("information", information), ("scope", scope), ("resources", resources), ("leaderboard", leaderboard)];

pub fn detail(record: &Value, sections: &[String], style: Style) -> String {
    let mut doc = Doc::new(style);
    let names: Vec<&str> = if sections.iter().any(|s| s == "all") {
        SECTIONS.iter().map(|(n, _)| *n).filter(|n| *n != "leaderboard" || is_competition(record)).collect()
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
    fn amounts() {
        assert_eq!(amount(&json!("1500000"), Some("USDC")), "$1,500,000 USDC");
        assert_eq!(amount(&json!(10), Some("USD")), "$10");
        assert_eq!(amount(&json!(10), Some("OP")), "10 OP");
        assert_eq!(amount(&json!(0), None), "-");
    }

    #[test]
    fn competition_page() {
        let record = json!({"id": "u", "name": "Rev", "kind": "public_contest", "status": "published", "url": "https://cantina.xyz/competitions/u",
            "totalRewardPot": "100000", "currencyCode": "USDC", "kycRequired": false,
            "assetGroups": [{"name": "Core", "rewards": [{"severity": "high", "maxReward": 500}, {"severity": "critical", "maxReward": 900}],
                             "assets": [{"name": "Repo", "description": "https://github.com/x/y"}]},
                            {"name": "Excluded", "outOfScope": true, "description": "tests"}],
            "leaderboard": {"available": true, "data": {"ranking": [{"position": 1, "team": {"name": "t1"}, "reward": 50000, "highFindings": 2}]}}});
        let out = detail(&record, &["all".to_string()], Style::new(false));
        assert!(out.contains("Status:      FINISHED"), "{out}");
        assert!(out.contains("Core: Critical $900 USDC · High $500 USDC"), "{out}");
        assert!(out.contains("Repo  https://github.com/x/y"), "{out}");
        assert!(out.contains("Out of scope") && out.contains("Excluded"), "{out}");
        assert!(out.contains("#  TEAM  REWARD        HIGH"), "{out}");
        assert!(out.contains("1  t1    $50,000 USDC  2"), "{out}");
    }
}
