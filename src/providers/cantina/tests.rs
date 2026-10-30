//! Cantina behaviour against a stubbed network.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};

use super::cli::{ListArgs, resolve_target};
use super::client::{Client, firefox_cookie};
use super::detail::detail;
use super::discovery::{Options, apply, assets};
use crate::error::Error;
use crate::http::stub::{reply, serve};
use crate::render::Style;

const UID: &str = "11111111-1111-1111-1111-111111111111";

fn client() -> Client {
    let dir = tempfile::tempdir().unwrap().keep();
    Client::new(Some(dir), false, false, 12.0, Some(String::new()))
}

/// Serve `routes` (URL substring → status and body); anything else is a 500.
fn routes(routes: Vec<(&'static str, u16, String)>) -> crate::http::stub::Guard {
    serve(move |url, _, _| match routes.iter().find(|(part, _, _)| url.contains(part)) {
        Some((_, status, body)) => reply(*status, body),
        None => reply(500, ""),
    })
}

#[test]
fn pagination_follows_cursors() {
    let calls = Rc::new(RefCell::new(Vec::<String>::new()));
    let seen = calls.clone();
    let _net = serve(move |url, _, _| {
        seen.borrow_mut().push(url.to_string());
        let next = if seen.borrow().len() == 1 { json!("next") } else { Value::Null };
        reply(200, &json!({"items": [{"id": seen.borrow().len()}], "nextValue": next}).to_string())
    });
    assert_eq!(client().opportunities(None, "live").unwrap().len(), 2);
    assert!(calls.borrow()[1].contains("next=next"));
}

#[test]
fn repeated_cursor_is_an_error() {
    let _net = routes(vec![("opportunities", 200, json!({"items": [], "nextValue": "x"}).to_string())]);
    assert!(client().opportunities(None, "live").is_err());
}

#[test]
fn kinds_are_sent_comma_separated() {
    let calls = Rc::new(RefCell::new(Vec::<String>::new()));
    let seen = calls.clone();
    let _net = serve(move |url, _, _| {
        seen.borrow_mut().push(url.to_string());
        reply(200, r#"{"items": []}"#)
    });
    client().opportunities(Some(&["public_bounty", "private_bounty"]), "live").unwrap();
    assert!(calls.borrow()[0].contains("kind=public_bounty%2Cprivate_bounty"));
}

#[test]
fn scope_exclusions() {
    let excluded = assets(&json!({"assetGroups": [{"outOfScope": true, "subGroups": [{"assets": [{"name": "excluded"}]}]}]}));
    assert_eq!(excluded[0]["outOfScope"], true);
    assert!(assets(&json!({"assetGroups": [{"hidden": true, "assets": [{}]}]})).is_empty());
}

#[test]
fn firefox_cookies_are_limited_to_cantina() {
    let dir = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(dir.path().join("cookies.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TABLE moz_cookies(host,name,value,expiry);
         INSERT INTO moz_cookies VALUES('.cantina.xyz','session','test',9999999999);
         INSERT INTO moz_cookies VALUES('evil.test','secret','exclude',9999999999);
         INSERT INTO moz_cookies VALUES('cantina.xyz','old','expired',1);",
    )
    .unwrap();
    drop(db);
    assert_eq!(firefox_cookie(dir.path().to_str().unwrap()).unwrap(), "session=test");
}

#[test]
fn responses_are_cached_but_not_for_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    {
        let _net = routes(vec![("opportunities", 200, r#"{"items": []}"#.into())]);
        let c = Client::new(Some(path.clone()), false, false, 12.0, Some(String::new()));
        assert_eq!(c.api("/api/v0/opportunities").unwrap(), json!({"items": []}));
    }
    let offline = Client::new(Some(path.clone()), false, true, 12.0, Some(String::new()));
    assert_eq!(offline.api("/api/v0/opportunities").unwrap(), json!({"items": []}));
    let session = Client::new(Some(path), false, true, 12.0, Some("session=test".into()));
    assert!(session.api("/api/v0/opportunities").is_err());
}

#[test]
fn authenticated_responses_are_never_written() {
    let dir = tempfile::tempdir().unwrap();
    let _net = routes(vec![("auth/me", 200, "{}".into())]);
    Client::new(Some(dir.path().to_path_buf()), false, false, 12.0, Some("session=test".into())).api("/api/v0/auth/me").unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn redirects_to_other_origins_are_refused() {
    let _net = serve(|_, _, _| {
        Ok(crate::http::Reply { status: 302, body: String::new(), location: Some("https://example.com/x".into()), retry_after: None })
    });
    let err = client().api("/api/v0/auth/me").unwrap_err();
    assert!(err.to_string().contains("Refusing API redirect"), "{err}");
}

#[test]
fn same_origin_redirects_are_followed() {
    let _net = serve(|url, _, _| {
        if url.ends_with("/moved") {
            return reply(200, r#"{"ok": true}"#);
        }
        Ok(crate::http::Reply { status: 301, body: String::new(), location: Some("/api/v0/moved".into()), retry_after: None })
    });
    assert_eq!(client().api("/api/v0/leaderboard").unwrap(), json!({"ok": true}));
}

#[test]
fn transient_errors_are_retried() {
    let attempts = Rc::new(RefCell::new(0));
    let count = attempts.clone();
    let _net = serve(move |_, _, _| {
        *count.borrow_mut() += 1;
        if *count.borrow() == 1 { reply(429, "") } else { reply(200, "{}") }
    });
    assert_eq!(client().api("/api/v0/leaderboard").unwrap(), json!({}));
    assert_eq!(*attempts.borrow(), 2);
}

#[test]
fn listing_falls_back_to_the_catalog() {
    let _net = routes(vec![
        ("opportunities", 503, String::new()),
        ("repositories/public", 200, json!([{"id": UID, "status": "live", "kind": "public_bounty"}]).to_string()),
    ]);
    let c = client();
    assert_eq!(c.opportunities(Some(&["public_bounty"]), "live").unwrap()[0]["id"], UID);
    assert!(!c.take_warnings().is_empty());
}

#[test]
fn malformed_listing_uses_the_catalog() {
    let _net = routes(vec![
        ("opportunities", 200, r#"{"changed": "schema"}"#.into()),
        ("repositories/public", 200, json!([{"id": UID, "kind": "public_bounty", "status": "live"}]).to_string()),
    ]);
    assert_eq!(client().opportunities(None, "live").unwrap().len(), 1);
}

#[test]
fn access_denied_never_falls_back() {
    let catalog = Rc::new(RefCell::new(false));
    let hit = catalog.clone();
    let _net = serve(move |url, _, _| {
        if url.contains("repositories/public") {
            *hit.borrow_mut() = true;
        }
        reply(403, "")
    });
    let err = client().opportunities(None, "live").unwrap_err();
    assert_eq!(err.status(), Some(403));
    assert!(!*catalog.borrow());
}

#[test]
fn detail_is_enriched_from_the_catalog() {
    let _net = routes(vec![
        (UID, 200, json!({"id": UID, "name": "Current"}).to_string()),
        ("repositories/public", 200, json!([{"id": UID, "name": "Old", "commitHash": "pinned"}]).to_string()),
    ]);
    let r = client().detail(UID, false).unwrap();
    assert_eq!((r["name"].as_str(), r["commitHash"].as_str()), (Some("Current"), Some("pinned")));
}

#[test]
fn detail_falls_back_to_the_catalog() {
    let _net = routes(vec![("repositories/public", 200, json!([{"id": UID, "name": "Fallback"}]).to_string()), (UID, 503, String::new())]);
    assert_eq!(client().detail(UID, false).unwrap()["name"], "Fallback");
}

#[test]
fn detail_falls_back_to_the_page() {
    let flight = format!("1:{}\n", json!({"competition": {"id": UID, "name": "Page fallback"}}));
    let html = format!("<script>self.__next_f.push({})</script>", json!([1, flight]));
    let _net = routes(vec![(".xyz/competitions/", 200, html), ("api.cantina.xyz", 503, String::new())]);
    assert_eq!(client().detail(UID, false).unwrap()["name"], "Page fallback");
}

#[test]
fn ambiguous_names_need_a_uuid() {
    let _net =
        routes(vec![("repositories/public", 200, json!([{"id": UID, "name": "same"}, {"id": "other", "name": "same"}]).to_string())]);
    let err = resolve_target(&client(), "same", None).unwrap_err();
    assert!(err.to_string().contains("Ambiguous"), "{err}");
}

#[test]
fn unpublished_and_denied_leaderboards_differ() {
    {
        let _net = routes(vec![("leaderboard", 404, String::new())]);
        assert_eq!(client().leaderboard(UID).unwrap()["available"], false);
    }
    let _net = routes(vec![("leaderboard", 403, String::new())]);
    assert!(matches!(client().leaderboard(UID), Err(Error::Fetch { status: Some(403), .. })));
}

fn options(sort: &str) -> Options {
    Options { sort: sort.into(), ..Default::default() }
}

#[test]
fn language_needs_evidence_and_kyc_must_match() {
    let rows = vec![
        json!({"id": UID, "instructions": "Scope: src/Vault.sol", "kycRequired": true}),
        json!({"id": "unknown", "instructions": "hello", "kycRequired": true}),
    ];
    let o = Options { language: Some("Solidity".into()), kyc: Some(true), ..options("bounty") };
    let ids: Vec<Value> = apply(rows, &o).unwrap().iter().map(|r| r["id"].clone()).collect();
    assert_eq!(ids, vec![json!(UID)]);
}

#[test]
fn unsupported_filters_and_reverse_sorting() {
    let err = apply(vec![json!({})], &Options { project_type: Some("Defi".into()), ..options("bounty") }).unwrap_err();
    assert!(err.to_string().contains("does not expose"));
    let rows = vec![json!({"id": "later", "timeframe": {"end": "2030"}}), json!({"id": "earlier", "timeframe": {"end": "2020"}})];
    assert_eq!(apply(rows, &Options { reverse: true, ..options("end") }).unwrap()[0]["id"], "earlier");
}

#[test]
fn redacted_names_still_list() {
    let rows = vec![json!({"id": "private", "name": null, "company": null, "totalRewardPot": null})];
    assert_eq!(apply(rows, &options("name")).unwrap().len(), 1);
    let _ = std::mem::size_of::<ListArgs>();
}

#[test]
fn bounty_page() {
    let bounty = json!({
        "id": "b1", "name": "Example", "kind": "public_bounty", "status": "live", "url": "https://cantina.xyz/bounties/b1",
        "currencyCode": "USDC", "totalRewardPot": "1000000", "submissionFee": "0.00", "kycRequired": true,
        "timeframe": {"start": "2024-01-01T00:00:00Z", "end": null}, "consideredSeverities": ["critical", "high"],
        "gitRepoUrl": "https://github.com/example/repo", "commitHash": "abcdef0123456789",
        "instructions": "Read https://docs.example.com first.",
        "assetGroups": [
            {"name": "Contracts", "outOfScope": false, "rewards": [
                {"severity": "high", "maxReward": "50000.00"}, {"severity": "critical", "maxReward": "1000000.00"},
                {"severity": "low", "maxReward": "0"}],
             "assets": [{"name": "Core", "description": "https://github.com/example/core", "reference": ""},
                        {"name": "Vault", "description": "- Mainnet: `0x00000000219ab540356cBB839Cbe05303d7705Fa`", "reference": ""},
                        {"name": "Hidden", "hidden": true, "reference": "x"}]},
            {"name": "Excluded", "outOfScope": true, "description": "Third-party code", "assets": []}]});
    let text = detail(&bounty, &["all".into()], Style::new(false));
    assert!(regex::Regex::new(r"Max reward:\s+\$1,000,000 USDC").unwrap().is_match(&text), "{text}");
    assert!(!text.contains("Submission fee"), "zero fee is omitted");
    assert!(text.contains("Critical $1,000,000 USDC · High $50,000 USDC"));
    assert!(text.contains("https://github.com/example/core"));
    assert!(text.contains("Mainnet: 0x00000000219ab540356cBB839Cbe05303d7705Fa"));
    assert!(!text.contains("Hidden"));
    assert!(text.contains("Out of scope"));
    assert!(text.contains("https://docs.example.com"));
    assert!(!text.contains("LEADERBOARD"), "bounties have none");
}

#[test]
fn competition_without_assets_and_with_leaderboard() {
    let competition = json!({
        "id": "c1", "name": "Contest", "kind": "public_contest", "status": "complete", "currencyCode": "USDC",
        "totalRewardPot": "400000", "timeframe": {"start": "2026-05-29T12:00:00Z", "end": "2026-06-12T20:00:00Z"},
        "gitRepoUrl": "https://github.com/example/contest", "commitHash": "7538c438513622",
        "leaderboard": {"available": true, "data": {"ranking": [
            {"position": 1, "team": {"members": [{"username": "alice"}]}, "reward": "6500.00", "highFindings": 0},
            {"position": 2, "team": {"name": "Team B"}, "reward": "5000.00", "highFindings": 0}]}}});
    let text = detail(&competition, &["scope".into(), "leaderboard".into()], Style::new(false));
    assert!(text.contains("Contest  "), "header still shown without overview");
    assert!(text.contains("repository README describes the scope"));
    assert!(text.contains("alice") && text.contains("Team B"));
    assert!(!text.contains("HIGH"), "all-zero finding columns are dropped");
}

#[test]
fn contract_scope_skips_excluded_groups() {
    let url = format!("https://etherscan.io/address/0x{}", "1".repeat(40));
    let record = json!({"assetGroups": [{"outOfScope": true, "assets": [{"reference": url}]}, {"assets": [{"reference": url}, {"reference": url}]}]});
    assert_eq!(super::contracts::scope(&record)["contracts"].as_array().unwrap().len(), 1);
}
