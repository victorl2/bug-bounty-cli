//! Readable detail pages for Sherlock bug bounties and contests, in the shared document style.

use serde_json::Value;

use super::super::common::iso;
use crate::jv::{self, at, opt, s};
use crate::opts::Kind;
use crate::render::{Doc, Style, date, datetime_utc, joined, money, relative, thousands};
use crate::util::{dedupe, links, title};

fn amount(value: &Value, currency: Option<&str>) -> String {
    let Some(number) = jv::num(value) else { return String::new() };
    let suffix = match currency {
        Some(c) if !c.is_empty() && number > 0.0 => format!(" {c}"),
        _ => String::new(),
    };
    money(Some(number), false) + &suffix
}

fn contest(record: &Value) -> bool {
    s(record, "kind") == "competition"
}

struct Repo {
    repo: String,
    commit: Option<String>,
    files: Vec<(String, Option<i64>)>,
    nsloc: Option<i64>,
}

/// Repositories in scope with their commit and file count, from either record shape.
fn repos(record: &Value) -> Vec<Repo> {
    if contest(record) {
        return jv::arr(record, "scope")
            .iter()
            .map(|sc| Repo {
                repo: s(sc, "repo").into(),
                commit: opt(sc, "commit_hash").map(str::to_string),
                files: jv::arr(sc, "files").iter().map(|f| (s(f, "name").to_string(), at(f, "nsloc").as_i64())).collect(),
                nsloc: at(sc, "total_nsloc").as_i64(),
            })
            .collect();
    }
    let mut out: Vec<Repo> = Vec::new();
    for sc in jv::arr(record, "covered_scope") {
        let Some(name) = opt(sc, "repo_name") else { continue };
        if !sc.get("selected").is_none_or(jv::truthy) {
            continue;
        }
        let commit = opt(sc, "commit_hash").map(str::to_string);
        let file = (s(sc, "file_path").to_string(), None);
        match out.iter_mut().find(|r| r.repo == name && r.commit == commit) {
            Some(r) => r.files.push(file),
            None => out.push(Repo { repo: name.into(), commit, files: vec![file], nsloc: None }),
        }
    }
    out
}

fn repo_url(repo: &Repo) -> String {
    let base = format!("https://github.com/{}", repo.repo);
    match &repo.commit {
        Some(c) => format!("{base}/tree/{c}"),
        None => base,
    }
}

fn contracts(record: &Value) -> Vec<&Value> {
    jv::arr(record, "covered_scope").iter().filter(|c| opt(c, "address").is_some() && c.get("selected").is_none_or(jv::truthy)).collect()
}

fn header(doc: &mut Doc, record: &Value) {
    let name = opt(record, "title").map(str::to_string).unwrap_or_else(|| jv::text(at(record, "id")));
    let line = format!("{}  {}", doc.style.bold(&name), doc.style.link(s(record, "url")));
    doc.raw(line);
}

fn int(v: &Value) -> String {
    v.as_f64().map(thousands).unwrap_or_default()
}

fn overview(doc: &mut Doc, record: &Value) {
    let st = doc.style;
    header(doc, record);
    let kind = if contest(record) { Kind::Competition } else { Kind::Program };
    let mut pairs: Vec<(&str, String)> = vec![("Status", st.status(&super::status(kind, record).to_uppercase()))];
    if contest(record) {
        let token = opt(record, "token");
        let start = iso(at(record, "starts_at"));
        let end = iso(at(record, "ends_at"));
        let kyc = match at(record, "requires_kyc") {
            Value::Bool(true) => "required",
            Value::Bool(false) => "not required",
            _ => "",
        };
        pairs.extend([
            ("Type", s(record, "type_label").to_string()),
            ("Reward pool", amount(at(record, "rewards"), token)),
            ("Auditor pool", amount(at(record, "prize_pool"), token)),
            (
                "Judging pool",
                if jv::t(record, "judging_prize_pool") { amount(at(record, "judging_prize_pool"), token) } else { String::new() },
            ),
            ("Starts", format!("{}  {}", datetime_utc(start.as_deref()), st.dim(&relative(start.as_deref())))),
            ("Ends", format!("{}  {}", datetime_utc(end.as_deref()), st.dim(&relative(end.as_deref())))),
            ("KYC", kyc.to_string()),
            ("Languages", joined(jv::strs(record, "languages"), 0)),
            ("nSLOC", if jv::t(record, "nsloc") { int(at(record, "nsloc")) } else { String::new() }),
            (
                "Issues",
                if jv::t(record, "num_competition_issues") {
                    format!("{} submitted", int(at(record, "num_competition_issues")))
                } else {
                    String::new()
                },
            ),
            ("Lead judge", s(record, "lead_judge_handle").to_string()),
            ("Lead auditor", s(record, "lead_senior_auditor_handle").to_string()),
            ("Repository", opt(record, "template_repo_name").map(|r| format!("https://github.com/{r}")).unwrap_or_default()),
        ]);
    } else {
        let currency = opt(record, "display_currency");
        let mut tiers: Vec<&Value> = jv::arr(record, "severities").iter().collect();
        tiers.sort_by_key(|d| at(d, "display_order").as_i64().unwrap_or(0));
        let deposits: Vec<String> =
            tiers.iter().map(|d| format!("{} {}", s(d, "severity_name"), amount(at(d, "deposit_amount"), currency))).collect();
        let found = contracts(record).len();
        pairs.extend([
            ("Max payout", amount(at(record, "payout"), currency)),
            ("Live since", date(iso(at(record, "live_since")).as_deref())),
            ("Updated", date(iso(at(record, "last_updated")).as_deref())),
            ("Categories", joined(jv::arr(record, "categories").iter().map(|c| s(c, "name")), 0)),
            ("Submission deposit", deposits.join(" · ")),
            ("Contracts", if found > 0 { found.to_string() } else { String::new() }),
            ("Repositories", joined(repos(record).iter().map(|r| r.repo.clone()), 0)),
        ]);
    }
    doc.fields(&pairs);
}

fn information(doc: &mut Doc, record: &Value) {
    doc.section("Information");
    if jv::t(record, "short_description") {
        doc.text(opt(record, "short_description"));
    }
    let tiers: Vec<(String, String)> = jv::arr(record, "rewards_tiers")
        .iter()
        .map(|t| {
            (
                s(at(t, "severity"), "name").trim().to_string(),
                format!(
                    "{:.0}% of the pool unlocks at {} valid",
                    jv::f(t, "unlock_percentage").unwrap_or(0.0),
                    jv::text(at(t, "min_count"))
                ),
            )
        })
        .collect();
    if !tiers.is_empty() {
        doc.sub("Reward unlocks");
        doc.fields(&tiers);
    }
    for (heading, key) in [("Details", "description"), ("Submission notice", "submission_severity_notice")] {
        if jv::t(record, key) {
            doc.sub(heading);
            doc.text(opt(record, key));
        }
    }
}

fn scope(doc: &mut Doc, record: &Value) {
    doc.section("Scope");
    let found = contracts(record);
    let repos = repos(record);
    if !found.is_empty() {
        doc.sub(&format!("Contracts ({})", found.len()));
        let rows: Vec<Vec<String>> = found
            .iter()
            .map(|c| {
                let network = opt(c, "network").map(title).unwrap_or_else(|| "-".into());
                vec![opt(c, "display_name").unwrap_or("-").to_string(), network, s(c, "address").to_string()]
            })
            .collect();
        doc.table(&["NAME", "NETWORK", "ADDRESS"], &rows, 0, &[]);
    }
    for repo in &repos {
        let size = match repo.nsloc {
            Some(n) if n != 0 => format!(", {} nSLOC)", thousands(n as f64)),
            _ => ")".into(),
        };
        doc.sub(&format!("{} ({} files{size}", repo.repo, repo.files.len()));
        let link = format!("  {}", doc.style.link(&repo_url(repo)));
        doc.raw(link);
        let rows: Vec<Vec<String>> = repo
            .files
            .iter()
            .map(|(name, nsloc)| {
                let name = if name.is_empty() { "-".to_string() } else { name.clone() };
                vec![name, nsloc.filter(|n| *n != 0).map(|n| n.to_string()).unwrap_or_else(|| "-".into())]
            })
            .collect();
        doc.table(&["FILE", "NSLOC"], &rows, 0, &[]);
    }
    let extra: Vec<String> = jv::arr(record, "url_scope")
        .iter()
        .chain(jv::arr(record, "extra_scope"))
        .filter(|e| jv::truthy(e))
        .map(|e| match e {
            Value::String(text) => text.clone(),
            other => opt(other, "url").or_else(|| opt(other, "name")).map(str::to_string).unwrap_or_else(|| other.to_string()),
        })
        .collect();
    if !extra.is_empty() {
        doc.sub("Other assets");
        doc.raw(extra.iter().map(|e| format!("  {e}")).collect::<Vec<_>>().join("\n"));
    }
    if found.is_empty() && repos.is_empty() && extra.is_empty() {
        let note = doc.style.dim("  No asset list published; the information section describes the scope.");
        doc.raw(note);
    }
}

fn resources(doc: &mut Doc, record: &Value) {
    doc.section("Resources");
    doc.fields(&[
        ("Repository", opt(record, "template_repo_name").map(|r| format!("https://github.com/{r}")).unwrap_or_default()),
        ("Judging", opt(record, "judging_repo_name").map(|r| format!("https://github.com/{r}")).unwrap_or_default()),
    ]);
    let found = dedupe(repos(record).iter().map(repo_url).chain(links([opt(record, "description"), opt(record, "short_description")])));
    if !found.is_empty() {
        doc.sub(&format!("Links ({})", found.len()));
        let lines: Vec<String> = found.iter().map(|l| format!("  {}", doc.style.link(l))).collect();
        doc.raw(lines.join("\n"));
    }
}

fn leaderboard(doc: &mut Doc, _record: &Value) {
    doc.section("Leaderboard");
    let note = doc.style.dim("  Not published through the public API; see the page on the site.");
    doc.raw(note);
}

/// Renders one section of a detail page.
type Section = fn(&mut Doc, &Value);

const SECTIONS: &[(&str, Section)] =
    &[("overview", overview), ("information", information), ("scope", scope), ("resources", resources), ("leaderboard", leaderboard)];

pub fn render(record: &Value, sections: &[String], style: Style) -> String {
    let mut doc = Doc::new(style);
    let names: Vec<&str> = if sections.iter().any(|s| s == "all") {
        SECTIONS.iter().map(|(n, _)| *n).filter(|n| *n != "leaderboard").collect()
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
    fn renders_bounty_sections() {
        let record = json!({"id": 56, "title": "Usual", "kind": "program", "url": "https://audits.sherlock.xyz/bug-bounties/56",
            "payout": 250000, "display_currency": "USDC", "is_ready": true,
            "covered_scope": [{"address": "0xe1DeE60c516a8350704Ec24a6E856c9F533d1c1b", "network": "ethereum", "display_name": "Oracle"},
                              {"repo_name": "usual/core", "commit_hash": "abc", "file_path": "src/A.sol"}],
            "description": "See https://docs.usual.money."});
        let all = vec!["all".to_string()];
        let out = render(&record, &all, Style::new(false));
        assert!(out.starts_with("Usual  https://audits.sherlock.xyz/bug-bounties/56\n"), "{out}");
        assert!(out.contains("Max payout:   $250,000 USDC"), "{out}");
        assert!(out.contains("Oracle  Ethereum  0xe1DeE60c516a8350704Ec24a6E856c9F533d1c1b"), "{out}");
        assert!(out.contains("usual/core (1 files)"), "{out}");
        assert!(out.contains("https://github.com/usual/core/tree/abc"), "{out}");
        assert!(out.contains("https://docs.usual.money"), "{out}");
        assert!(!out.contains("LEADERBOARD"), "{out}");
        let board = render(&record, &["leaderboard".to_string()], Style::new(false));
        assert!(board.contains("Not published through the public API"), "{board}");
    }
}
