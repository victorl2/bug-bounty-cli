//! Plain-text rendering of Immunefi program lists and detail pages.

use std::sync::LazyLock;

use chrono::Utc;
use regex::Regex;
use serde_json::Value;

use super::client::{BASE_URL, kind_path};
use crate::jv::{self, at, opt, s, t};
use crate::opts::Kind;
use crate::render::{Doc, Style, Styler, date, datetime_utc, indent, joined, markdown, money, parse_date, relative, table, thousands};
use crate::util::capitalize;

const SEVERITY_ORDER: &[&str] = &["critical", "high", "medium", "low", "none", "insight"];
const STATUS_ORDER: &[&str] = &["UPCOMING", "LIVE", "EVALUATING", "FINISHED"];

fn asset_label(kind: &str) -> String {
    match kind {
        "smart_contract" => "Smart Contract".into(),
        "websites_and_applications" => "Websites and Applications".into(),
        "blockchain_dlt" => "Blockchain/DLT".into(),
        other => other.into(),
    }
}

fn eligibility_label(code: &str) -> String {
    match code {
        "no_ofac_sdn" => "Not on OFAC SDN list".into(),
        "no_employee" => "Not an employee or contractor of the project".into(),
        "no_sanctioned_country" => "Not resident in a sanctioned country".into(),
        "age_18" => "At least 18 years old".into(),
        other => other.replace('_', " "),
    }
}

fn amount(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn normalize_status(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let upper = raw.to_uppercase();
    for (keywords, status) in [
        (&["EVAL", "JUDG", "REVIEW"][..], "EVALUATING"),
        (&["FINISH", "END", "CLOSE", "COMPLET"][..], "FINISHED"),
        (&["UPCOMING", "SCHEDUL", "SOON", "START"][..], "UPCOMING"),
        (&["LIVE", "ACTIVE", "ONGOING", "OPEN"][..], "LIVE"),
    ] {
        if keywords.iter().any(|k| upper.contains(k)) {
            return Some(status.into());
        }
    }
    Some(upper)
}

/// Status reported by Immunefi, advanced by dates so a cached listing never goes stale.
pub fn competition_status(item: &Value) -> String {
    let now = Utc::now();
    let start = opt(item, "launchDate").and_then(parse_date);
    let end = opt(item, "endDate").and_then(parse_date);
    let eval_end = opt(item, "evaluationEndDate").and_then(parse_date);
    let derived = match (start, end) {
        (Some(s), _) if now < s => Some("UPCOMING"),
        (_, Some(e)) if now < e => Some("LIVE"),
        (_, Some(_)) => Some(if eval_end.is_some_and(|ev| now >= ev) { "FINISHED" } else { "EVALUATING" }),
        _ => None,
    };
    let reported = normalize_status(s(item, "auditCompetitionStatus"));
    let rank = |x: &str| STATUS_ORDER.iter().position(|s| *s == x);
    match reported {
        Some(r) if rank(&r).is_some() => match derived {
            Some(d) if rank(d) > rank(&r) => d.into(),
            _ => r,
        },
        Some(r) => r,
        None => derived.unwrap_or("UNKNOWN").into(),
    }
}

pub fn program_status(item: &Value) -> &'static str {
    if t(item, "pausedAt") || t(item, "isPaused") { "PAUSED" } else { "LIVE" }
}

fn tag_list<'a>(item: &'a Value, key: &str) -> Vec<&'a str> {
    jv::strs(at(item, "tags"), key)
}

fn yes_no(v: bool) -> String {
    if v { "yes" } else { "no" }.into()
}

pub fn program_table(items: &[Value], style: &Style) -> String {
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|it| {
            let mut name = s(it, "project").to_string();
            if program_status(it) == "PAUSED" {
                name.push_str(" (paused)");
            }
            vec![
                s(it, "slug").into(),
                name,
                money(amount(at(it, "maxBounty")), true),
                money(amount(at(it, "vaultBalance")), true),
                joined(tag_list(it, "programType"), 1),
                joined(tag_list(it, "ecosystem"), 2),
                joined(tag_list(it, "language"), 2),
                yes_no(t(it, "kyc")),
                date(opt(it, "updatedDate")),
            ]
        })
        .collect();
    table(&["SLUG", "PROJECT", "MAX BOUNTY", "VAULT", "TYPE", "ECOSYSTEM", "LANGUAGE", "KYC", "UPDATED"], &rows, style, 1, &[])
}

pub fn competition_table(items: &[Value], style: &Style) -> String {
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|it| {
            vec![
                s(it, "slug").into(),
                s(it, "project").into(),
                competition_status(it),
                money(amount(jv::first(it, &["rewardsPool", "maxBounty"])), true),
                date(opt(it, "launchDate")),
                date(opt(it, "endDate")),
                joined(tag_list(it, "programType"), 1),
                joined(tag_list(it, "language"), 2),
                yes_no(t(it, "kyc")),
            ]
        })
        .collect();
    table(&["SLUG", "PROJECT", "STATUS", "POOL", "START", "END", "TYPE", "LANGUAGE", "KYC"], &rows, style, 1, &[(2, Styler::Status)])
}

// --- detail sections --------------------------------------------------------------------------

pub fn detail_url(kind: Kind, slug: &str, tab: &str) -> String {
    format!("{BASE_URL}/{}/{slug}/{tab}/", kind_path(kind))
}

fn reward_line(r: &Value) -> (String, String, String) {
    let severity = jv::first(r, &["severity", "level"]).as_str().unwrap_or("-").to_lowercase();
    let asset = opt(r, "assetType").map(asset_label).unwrap_or_else(|| "-".into());
    let model = s(r, "rewardModel");
    let mut payout = if t(r, "payout") {
        jv::text(at(r, "payout"))
    } else if model == "range" || (r.get("minReward").is_some() && r.get("maxReward").is_some()) {
        format!("{} - {}", money(amount(at(r, "minReward")), false), money(amount(at(r, "maxReward")), false))
    } else if model == "fixed" || r.get("fixedReward").is_some() {
        money(amount(at(r, "fixedReward")), false)
    } else {
        format!("up to {}", money(amount(at(r, "maxReward")), false))
    };
    if t(r, "rewardCalculationPercentage") {
        payout += &format!(" ({}% of funds directly affected)", jv::text(at(r, "rewardCalculationPercentage")));
    }
    if t(r, "otherImpactMaxReward") {
        payout += &format!(" (other impacts up to {})", money(amount(at(r, "otherImpactMaxReward")), false));
    }
    if t(r, "pocRequired") {
        payout += " · PoC required";
    }
    if t(r, "primacy") {
        payout += &format!(" · {}", jv::text(at(r, "primacy")).replace('_', " "));
    }
    (severity, asset, payout)
}

fn rewards(b: &Value) -> Vec<Value> {
    for key in ["programRewards", "rewards"] {
        if t(b, key) {
            return jv::arr(b, key).to_vec();
        }
    }
    let legacy = at(b, "legacy");
    let mut out = Vec::new();
    for (key, asset) in [
        ("smartcontract_rewards", "smart_contract"),
        ("web_rewards", "websites_and_applications"),
        ("blockchain_rewards", "blockchain_dlt"),
    ] {
        out.extend(jv::arr(legacy, key).iter().map(|r| jv::merge(r, &serde_json::json!({"assetType": asset}))));
    }
    out
}

fn sev_key(severity: &str) -> usize {
    let lower = severity.to_lowercase();
    SEVERITY_ORDER.iter().position(|s| *s == lower).unwrap_or(SEVERITY_ORDER.len())
}

fn title(b: &Value) -> &str {
    opt(b, "project").unwrap_or(s(b, "slug"))
}

fn overview(doc: &mut Doc, kind: Kind, b: &Value) {
    let st = doc.style;
    let head = format!("{}  {}", st.bold(title(b)), st.link(&detail_url(kind, s(b, "slug"), "information")));
    doc.raw(head);
    let mut pairs: Vec<(&str, String)> = Vec::new();
    if kind == Kind::Competition {
        let pools: Vec<String> = [("primary", "primaryPool"), ("all-stars", "allStarsPool"), ("podium", "podiumPool")]
            .iter()
            .filter(|(_, k)| t(b, k))
            .map(|(label, k)| format!("{label} {}", money(amount(at(b, k)), false)))
            .collect();
        pairs.extend([
            ("Status", st.status(&competition_status(b))),
            ("Reward pool", money(amount(jv::first(b, &["rewardsPool", "maxBounty"])), false)),
            ("Pools", pools.join(", ")),
            ("Starts", format!("{}  {}", datetime_utc(opt(b, "launchDate")), st.dim(&relative(opt(b, "launchDate"))))),
            ("Ends", format!("{}  {}", datetime_utc(opt(b, "endDate")), st.dim(&relative(opt(b, "endDate"))))),
            ("Evaluation end", datetime_utc(opt(b, "evaluationEndDate"))),
            (
                "Lines of code",
                if t(b, "sourceLinesOfCode") { thousands(jv::f(b, "sourceLinesOfCode").unwrap_or(0.0)) } else { String::new() },
            ),
        ]);
    } else {
        pairs.extend([
            ("Status", st.status(program_status(b))),
            ("Max bounty", money(amount(at(b, "maxBounty")), false)),
            ("Launched", date(opt(b, "launchDate"))),
        ]);
    }
    let vault_total: f64 = jv::arr(b, "vaults").iter().filter_map(|v| amount(at(v, "vaultBalance"))).sum();
    let vault = if vault_total != 0.0 { Some(vault_total) } else { amount(at(b, "vaultBalance")) };
    let token: Vec<&str> = [opt(b, "rewardsToken"), opt(b, "rewardsTokenNetwork")].into_iter().flatten().collect();
    let tags = at(b, "tags");
    pairs.extend([
        ("Vault balance", money(vault, false)),
        ("Updated", date(opt(b, "updatedDate"))),
        ("Rewards token", token.join(" on ")),
        ("KYC", if t(b, "kyc") { "required" } else { "not required" }.into()),
        ("PoC", jv::text(at(b, "proofOfConceptType")).replace('_', " ")),
        ("Primacy", jv::text(at(b, "primacy")).replace("primacy_of_", "primacy of ")),
        ("Triage", if t(b, "premiumTriaging") { "Immunefi managed" } else { "project" }.into()),
        ("Program type", joined(jv::strs(tags, "programType"), 0)),
        ("Project type", joined(jv::strs(tags, "projectType"), 0)),
        ("Product type", joined(jv::strs(tags, "productType"), 0)),
        ("Ecosystem", joined(jv::strs(tags, "ecosystem"), 12)),
        ("Language", joined(jv::strs(tags, "language"), 0)),
        ("Website", s(b, "websiteUrl").into()),
        ("GitHub", s(b, "githubUrl").into()),
    ]);
    if t(b, "pausedMessage") {
        pairs.push(("Paused", s(b, "pausedMessage").into()));
    }
    doc.fields(&pairs);
    if t(b, "description") {
        doc.sub("Description");
        doc.text(opt(b, "description"));
    }
}

static TIMELINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*competition timeline\s*\n").unwrap());

fn audits(doc: &mut Doc, b: &Value) {
    if t(b, "audits") {
        doc.sub("Audits");
        for a in jv::arr(b, "audits") {
            let line = format!("  - {} ({}): {}", opt(a, "auditor").unwrap_or("?"), date(opt(a, "date")), doc.style.link(s(a, "url")));
            doc.raw(line);
        }
    }
}

fn information(doc: &mut Doc, kind: Kind, b: &Value) {
    doc.section("Information");
    if kind == Kind::Competition {
        let status = capitalize(&competition_status(b));
        let intro = opt(b, &format!("boostedIntro{status}")).or_else(|| opt(b, "boostedIntroLive"));
        let intro = TIMELINE.replace(&markdown(intro), "").to_string();
        if !intro.is_empty() {
            doc.sub("Competition timeline");
            doc.text(Some(&intro));
        }
        if t(b, "boostedSummaryReport") {
            doc.fields(&[("Summary report", s(b, "boostedSummaryReport"))]);
        }
    }
    let vaults = jv::arr(b, "vaults");
    if !vaults.is_empty() {
        doc.sub("Immunefi vault");
        for v in vaults {
            let assets: Vec<String> =
                jv::arr(v, "assets").iter().map(|a| format!("{} {}", jv::text(at(a, "balance")), jv::text(at(a, "symbol")))).collect();
            doc.fields(&[
                ("Address", format!("{} (chain {})", jv::text(at(v, "address")), jv::text(at(v, "chainId")))),
                ("Balance", money(amount(at(v, "vaultBalance")), false)),
                ("30d average", money(amount(at(v, "averageBalance")), false)),
                ("Holdings", assets.join(", ")),
            ]);
        }
    }
    doc.sub("Rewards by threat level");
    let mut lines: Vec<(String, String, String)> = rewards(b).iter().map(reward_line).collect();
    lines.sort_by(|x, y| (x.1.as_str(), sev_key(&x.0)).cmp(&(y.1.as_str(), sev_key(&y.0))));
    if lines.is_empty() {
        doc.raw("  -");
    } else {
        let rows: Vec<Vec<String>> = lines.into_iter().map(|(a, b, c)| vec![a, b, c]).collect();
        doc.table(&["SEVERITY", "ASSET", "REWARD"], &rows, 2, &[(0, Styler::Severity)]);
    }
    if !markdown(opt(b, "rewardsBody")).is_empty() {
        doc.raw("");
        doc.text(opt(b, "rewardsBody"));
    }
    for (heading, key) in [("Program overview", "programOverview"), ("Prioritized vulnerabilities", "prioritizedVulnerabilities")] {
        if !markdown(opt(b, key)).is_empty() {
            doc.sub(heading);
            doc.text(opt(b, key));
        }
    }
    audits(doc, b);
    let known = jv::arr(b, "knownIssues");
    if !known.is_empty() {
        doc.sub(&format!("Known issues ({})", known.len()));
        for k in known {
            let body = markdown(opt(k, "description"));
            let body = if body.is_empty() { "-".to_string() } else { body };
            let mut lines = body.lines();
            let first = lines.next().unwrap_or("-");
            let rest: Vec<&str> = lines.collect();
            let extra = opt(k, "relatedImpactInScope").map(|i| format!("  [{}]", asset_label(i))).unwrap_or_default();
            let line = format!("  - {first}{}", doc.style.dim(&extra));
            doc.raw(line);
            if !rest.is_empty() {
                doc.raw(indent(&rest.join("\n"), "    "));
            }
            if t(k, "link") {
                let link = format!("    {}", doc.style.link(s(k, "link")));
                doc.raw(link);
            }
        }
    }
    doc.sub(&format!("KYC {}", if t(b, "kyc") { "required" } else { "not required" }));
    if t(b, "kycLevel") {
        doc.fields(&[("Level", jv::text(at(b, "kycLevel")))]);
    }
    let criteria: Vec<String> = jv::strs(b, "eligibilityCriteria").into_iter().map(eligibility_label).collect();
    if !criteria.is_empty() {
        doc.sub("Eligibility");
        doc.raw(criteria.iter().map(|c| format!("  - {c}")).collect::<Vec<_>>().join("\n"));
    }
    doc.text(opt(b, "eligibilityCriteriaAdditionalInformation"));
    doc.sub("Proof of concept");
    let poc = if t(b, "proofOfConceptType") { jv::text(at(b, "proofOfConceptType")) } else { "-".into() };
    doc.fields(&[("Requirement", poc.replace('_', " "))]);
    if t(b, "responsiblePublicationCategory") {
        doc.sub("Responsible publication");
        let category = s(b, "responsiblePublicationCategory").replace("category_", "Category ");
        let line = format!("  {category} - see {}", doc.style.link(&format!("{BASE_URL}/responsible-publication/")));
        doc.raw(line);
    }
    let prohibited = jv::first(b, &["prohibitedActivites", "defaultProhibitedActivities"]).as_str();
    let custom: Vec<String> = jv::arr(b, "customProhibitedActivities")
        .iter()
        .map(|c| c.as_str().map(str::to_string).unwrap_or_else(|| s(c, "description").to_string()))
        .collect();
    if !markdown(prohibited).is_empty() || !custom.is_empty() {
        doc.sub("Prohibited activities");
        doc.text(prohibited);
        doc.raw(custom.iter().filter(|c| !c.is_empty()).map(|c| format!("  - {c}")).collect::<Vec<_>>().join("\n"));
    }
    let feasibility = jv::first(b, &["feasibilityLimitations", "defaultFeasibilityLimitations"]).as_str();
    if !markdown(feasibility).is_empty() {
        doc.sub("Feasibility limitations");
        doc.text(feasibility);
    }
}

fn scope(doc: &mut Doc, _kind: Kind, b: &Value) {
    doc.section("Scope");
    let assets = jv::arr(b, "assets");
    doc.sub(&format!("Assets in scope ({})", assets.len()));
    if t(b, "hideAssetsInScope") {
        doc.raw("  (assets hidden by the project)");
    }
    let rows: Vec<Vec<String>> = assets
        .iter()
        .map(|a| {
            let notes: Vec<&str> = [(t(a, "isPrimacyOfImpact"), "primacy of impact"), (t(a, "isSafeHarbor"), "safe harbor")]
                .into_iter()
                .filter(|(on, _)| *on)
                .map(|(_, n)| n)
                .collect();
            let suffix = if notes.is_empty() { String::new() } else { format!(" [{}]", notes.join(", ")) };
            vec![
                opt(a, "type").map(asset_label).unwrap_or_else(|| "-".into()),
                opt(a, "url").unwrap_or("-").into(),
                format!("{}{suffix}", s(a, "description")),
                date(opt(a, "addedAt")),
            ]
        })
        .collect();
    doc.table(&["TYPE", "TARGET", "DESCRIPTION", "ADDED"], &rows, 2, &[]);
    doc.text(opt(b, "assetsBodyV2"));

    let impacts: Vec<Value> = if t(b, "programImpacts") {
        jv::arr(b, "programImpacts").to_vec()
    } else {
        jv::arr(b, "impacts")
            .iter()
            .map(|i| jv::merge(i, &serde_json::json!({"assetType": at(i, "type"), "description": at(i, "title")})))
            .collect()
    };
    doc.sub(&format!("Impacts in scope ({})", impacts.len()));
    let mut by_type: Vec<(String, Vec<&Value>)> = Vec::new();
    for i in &impacts {
        let key = opt(i, "assetType").unwrap_or("other").to_string();
        match by_type.iter_mut().find(|(k, _)| *k == key) {
            Some((_, group)) => group.push(i),
            None => by_type.push((key, vec![i])),
        }
    }
    for (asset_type, group) in &by_type {
        let heading = format!("  {}", doc.style.bold(&asset_label(asset_type)));
        doc.raw(heading);
        let mut group = group.clone();
        group.sort_by_key(|x| sev_key(&jv::text(at(x, "severity"))));
        for i in group {
            let sev = if t(i, "severity") { capitalize(&jv::text(at(i, "severity"))) } else { "-".into() };
            let label = doc.style.severity(&format!("{sev:<10}"));
            let extra = if t(i, "discretionaryRewardAmount") {
                format!("  (discretionary {})", money(amount(at(i, "discretionaryRewardAmount")), false))
            } else {
                String::new()
            };
            let line = format!("    {label}{}{extra}", opt(i, "description").unwrap_or("-"));
            doc.raw(line);
        }
    }
    doc.text(opt(b, "impactsBody"));

    doc.sub("Out of scope");
    if !markdown(opt(b, "customOutOfScopeInformation")).is_empty() {
        doc.text(opt(b, "customOutOfScopeInformation"));
    } else {
        let in_scope: Vec<String> = assets.iter().map(|a| s(a, "type").to_string()).chain(by_type.iter().map(|(k, _)| k.clone())).collect();
        for (heading, key, asset_type) in [
            ("General", "defaultOutOfScopeGeneral", None),
            ("Smart contracts", "defaultOutOfScopeSmartContract", Some("smart_contract")),
            ("Blockchain/DLT", "defaultOutOfScopeBlockchain", Some("blockchain_dlt")),
            ("Websites and applications", "defaultOutOfScopeWebAndApplications", Some("websites_and_applications")),
        ] {
            if !markdown(opt(b, key)).is_empty() && asset_type.is_none_or(|a| in_scope.iter().any(|x| x == a)) {
                let line = format!("  {}", doc.style.bold(heading));
                doc.raw(line);
                doc.text(opt(b, key));
            }
        }
    }
    if !markdown(opt(b, "outOfScopeAndRules")).is_empty() {
        doc.sub("Additional rules");
        doc.text(opt(b, "outOfScopeAndRules"));
    }
}

fn resources(doc: &mut Doc, kind: Kind, b: &Value) {
    doc.section("Resources");
    if s(b, "source") == "public-api" {
        let note =
            doc.style.dim(&format!("  Codebase and documentation links are only on {}", detail_url(kind, s(b, "slug"), "resources")));
        doc.raw(note);
    }
    for (heading, key) in [("Codebase", "programCodebases"), ("Documentation", "programDocumentations")] {
        let entries = jv::arr(b, key);
        doc.sub(heading);
        if entries.is_empty() {
            doc.raw("  -");
        }
        for e in entries {
            let desc = s(e, "description");
            let desc = if !desc.is_empty() && desc != "-" { format!(" - {desc}") } else { String::new() };
            let line =
                format!("  - {}{}\n    {}", opt(e, "title").unwrap_or(s(e, "url")), doc.style.dim(&desc), doc.style.link(s(e, "url")));
            doc.raw(line);
        }
    }
    let links: Vec<(&str, &str)> =
        [("Website", s(b, "websiteUrl")), ("GitHub", s(b, "githubUrl"))].into_iter().filter(|(_, v)| !v.is_empty()).collect();
    if !links.is_empty() {
        doc.sub("Links");
        doc.fields(&links);
    }
    audits(doc, b);
}

fn leaderboard(doc: &mut Doc, _kind: Kind, b: &Value) {
    doc.section("Leaderboard");
    let mut board: Vec<&Value> = jv::arr(b, "boostedLeaderboard").iter().collect();
    if board.is_empty() {
        doc.raw("  No leaderboard published yet.");
        return;
    }
    board.sort_by(|x, y| {
        let rank = |r: &Value| at(r, "aspRank").as_i64().unwrap_or(1_000_000_000);
        let earned = |r: &Value| -jv::f(r, "totalEarnings").unwrap_or(0.0);
        rank(x).cmp(&rank(y)).then(earned(x).total_cmp(&earned(y)))
    });
    let count = |r: &Value, k: &str| if t(r, k) { jv::text(at(r, k)) } else { "0".into() };
    let rows: Vec<Vec<String>> = board
        .iter()
        .map(|r| {
            vec![
                if t(r, "aspRank") { jv::text(at(r, "aspRank")) } else { "-".into() },
                opt(r, "name").unwrap_or("-").into(),
                money(amount(at(r, "totalEarnings")), false),
                count(r, "totalValidBugs"),
                count(r, "critical"),
                count(r, "high"),
                count(r, "medium"),
                count(r, "low"),
                count(r, "insights"),
                count(r, "duplicateReports"),
            ]
        })
        .collect();
    doc.table(&["RANK", "WHITEHAT", "EARNINGS", "VALID", "CRIT", "HIGH", "MED", "LOW", "INSIGHT", "DUPES"], &rows, 1, &[]);
}

pub const PROGRAM_SECTIONS: &[&str] = &["overview", "information", "scope", "resources"];
pub const COMPETITION_SECTIONS: &[&str] = &["overview", "information", "scope", "resources", "leaderboard"];

pub fn detail(kind: Kind, b: &Value, sections: &[&str], style: Style) -> String {
    let mut doc = Doc::new(style);
    if !sections.contains(&"overview") {
        let head = format!("{}  {}", style.bold(title(b)), style.link(&detail_url(kind, s(b, "slug"), "information")));
        doc.raw(head);
    }
    for name in sections {
        match *name {
            "overview" => overview(&mut doc, kind, b),
            "information" => information(&mut doc, kind, b),
            "scope" => scope(&mut doc, kind, b),
            "resources" => resources(&mut doc, kind, b),
            "leaderboard" => leaderboard(&mut doc, kind, b),
            _ => {}
        }
    }
    doc.render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn statuses_advance_by_date() {
        assert_eq!(
            competition_status(&json!({"auditCompetitionStatus": "live", "launchDate": "2020-01-01", "endDate": "2020-02-01",
            "evaluationEndDate": "2020-03-01"})),
            "FINISHED"
        );
        assert_eq!(competition_status(&json!({"launchDate": "2999-01-01"})), "UPCOMING");
        assert_eq!(
            competition_status(&json!({"auditCompetitionStatus": "Judging", "launchDate": "2020-01-01", "endDate": "2999-02-01"})),
            "EVALUATING"
        );
        assert_eq!(competition_status(&json!({})), "UNKNOWN");
        assert_eq!(program_status(&json!({"isPaused": true})), "PAUSED");
        assert_eq!(program_status(&json!({})), "LIVE");
    }

    #[test]
    fn reward_lines() {
        assert_eq!(
            reward_line(&json!({"severity": "Critical", "assetType": "smart_contract", "minReward": 1000, "maxReward": 5000})),
            ("critical".into(), "Smart Contract".into(), "$1,000 - $5,000".into())
        );
        let (_, _, payout) =
            reward_line(&json!({"level": "high", "maxReward": 10, "rewardCalculationPercentage": 10, "pocRequired": true}));
        assert_eq!(payout, "up to $10 (10% of funds directly affected) · PoC required");
    }

    #[test]
    fn detail_page() {
        let b = json!({"slug": "x", "project": "X", "maxBounty": 100000, "kyc": true, "tags": {"language": ["Solidity"]},
            "programRewards": [{"severity": "high", "assetType": "smart_contract", "fixedReward": 1000}],
            "assets": [{"type": "smart_contract", "url": "https://etherscan.io/address/0x1", "description": "Vault", "isSafeHarbor": true}],
            "impacts": [{"type": "smart_contract", "severity": "critical", "title": "Theft"}]});
        let out = detail(Kind::Program, &b, PROGRAM_SECTIONS, Style::new(false));
        assert!(out.starts_with("X  https://immunefi.com/bug-bounty/x/information/"), "{out}");
        assert!(out.contains("Max bounty: $100,000"), "{out}");
        assert!(out.contains("high      Smart Contract  $1,000"), "{out}");
        assert!(out.contains("Vault [safe harbor]"), "{out}");
        assert!(out.contains("    Critical  Theft"), "{out}");
        assert!(out.contains("CODEBASE") || out.contains("Codebase"), "{out}");
    }
}
