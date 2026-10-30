//! Immunefi page decoding, caching and fallbacks against a stubbed network.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{Duration, Utc};
use serde_json::{Value, json};

use super::cli::{Native, NativeFilter, filter_items, make_client};
use super::client::{Client, PUBLIC_API_URL, parse_target};
use super::render::{COMPETITION_SECTIONS, competition_status, detail};
use crate::cache::{Cache, MIN_TTL_HOURS};
use crate::error::Error;
use crate::http::stub::{reply, serve};
use crate::opts::Kind;
use crate::render::Style;
use crate::rsc::{Payload, parse_rows};

/// Wrap a flight payload the way Next.js embeds it in HTML (split across two pushes).
fn page(flight: &str) -> String {
    let chars: Vec<char> = flight.chars().collect();
    let mid = chars.len() / 2;
    let parts = [chars[..mid].iter().collect::<String>(), chars[mid..].iter().collect::<String>()];
    let pushes: String = parts.iter().map(|p| format!("<script>self.__next_f.push({})</script>", json!([1, p]))).collect();
    format!("<html><body><script>self.__next_f.push([0])</script>{pushes}</body></html>")
}

fn text_row(id: &str, text: &str) -> String {
    format!("{id}:T{:x},{text}", text.len())
}

fn flight() -> String {
    let bounty = json!({"slug": "acme", "project": "Acme", "maxBounty": 100000, "rewardsBody": "$a", "programOverview": "$$5 minimum",
        "launchDate": "$D2024-01-01T00:00:00.000Z", "assets": "$b:items", "missing": "$undefined"});
    format!(
        "1:I[123,[\"x\"],\"default\"]\n{}\nb:{{\"items\":[{{\"type\":\"smart_contract\",\"url\":\"https://x\"}}]}}\n2:[\"$\",\"div\",null,{{\"bounty\":{bounty},\"vault\":{{\"vaultBalance\":5}}}}]\n",
        text_row("a", "Rewards in “USDC” — paid fast\nsecond line")
    )
}

#[test]
fn extract_and_resolve_bounty() {
    let b = Payload::new(&page(&flight())).unwrap().find("bounty", Value::is_object).unwrap();
    assert_eq!(b["rewardsBody"], "Rewards in “USDC” — paid fast\nsecond line");
    assert_eq!(b["programOverview"], "$5 minimum");
    assert_eq!(b["launchDate"], "2024-01-01T00:00:00.000Z");
    assert_eq!(b["assets"], json!([{"type": "smart_contract", "url": "https://x"}]));
    assert_eq!(b["missing"], Value::Null);
}

#[test]
fn raw_stream_matches_html() {
    assert_eq!(Payload::new(&flight()).unwrap().rows, Payload::new(&page(&flight())).unwrap().rows);
}

#[test]
fn not_found_marker_and_byte_lengths() {
    assert!(Payload::new("c:E{\"digest\":\"NEXT_HTTP_ERROR_FALLBACK;404\"}\n").unwrap().not_found);
    assert!(!Payload::new(&flight()).unwrap().not_found);
    let rows = parse_rows(&format!("{}0:{{\"k\":1}}\n", text_row("a", "é€x")));
    assert_eq!(rows["a"], "é€x");
    assert_eq!(rows["0"], json!({"k": 1}));
}

#[test]
fn predicate_and_missing_key() {
    assert!(Payload::new(&flight()).unwrap().find("bounty", |b| b["slug"] == "other").is_err());
    assert!(Payload::new("<html></html>").is_err());
}

#[test]
fn ttl_never_below_minimum() {
    let ttl = |t: Option<f64>| make_client(&Native { ttl: t, ..Default::default() }).cache.ttl_seconds / 3600.0;
    assert_eq!(ttl(Some(1.0)), MIN_TTL_HOURS);
    assert_eq!(ttl(Some(24.0)), 24.0);
}

fn counting(body: String) -> (Rc<RefCell<usize>>, crate::http::stub::Guard) {
    let count = Rc::new(RefCell::new(0));
    let seen = count.clone();
    let guard = serve(move |_, _, _| {
        *seen.borrow_mut() += 1;
        reply(200, &body)
    });
    (count, guard)
}

#[test]
fn fresh_entries_are_not_refetched() {
    let dir = tempfile::tempdir().unwrap();
    let c = Client::new(Cache::new(dir.path(), 10.0), false, false);
    let (count, _net) = counting(flight());
    let first = c.detail(Kind::Program, "acme").unwrap();
    let second = c.detail(Kind::Program, "acme").unwrap();
    assert_eq!(*count.borrow(), 1);
    assert!(!first.from_cache && second.from_cache);
    assert_eq!(second.data["vaults"], json!([{"vaultBalance": 5}]));
}

#[test]
fn expired_entries_refetch_and_fall_back_to_stale() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path(), 10.0);
    cache.write("program/acme", json!({"slug": "acme"}), "u").unwrap();
    let path = dir.path().join("program/acme.json");
    let mut raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    raw["fetched_at"] = json!(crate::cache::now() - 11.0 * 3600.0);
    std::fs::write(&path, raw.to_string()).unwrap();
    let c = Client::new(cache, false, false);
    {
        let _down = serve(|_, _, _| Err(Error::fetch("down", None)));
        assert!(c.detail(Kind::Program, "acme").unwrap().stale);
    }
    let (count, _net) = counting(flight());
    assert_eq!(c.detail(Kind::Program, "acme").unwrap().data["project"], "Acme");
    assert_eq!(*count.borrow(), 1);
}

#[test]
fn refresh_bypasses_fresh_cache() {
    let dir = tempfile::tempdir().unwrap();
    Cache::new(dir.path(), 10.0).write("program/acme", json!({"slug": "acme"}), "u").unwrap();
    let c = Client::new(Cache::new(dir.path(), 10.0), true, false);
    let (count, _net) = counting(flight());
    c.detail(Kind::Program, "acme").unwrap();
    assert_eq!(*count.borrow(), 1);
}

fn public_api() -> crate::http::stub::Guard {
    let api = json!([
        {"slug": "acme", "project": "Acme", "endDate": null, "language": ["Rust"], "rewards": []},
        {"slug": "hidden", "project": "Hidden", "endDate": null, "inviteOnly": true},
        {"slug": "comp", "project": "Comp", "endDate": "2026-01-01T00:00:00Z", "inviteOnly": true}]);
    serve(move |url, _, _| if url == PUBLIC_API_URL { reply(200, &api.to_string()) } else { reply(200, "<html>redesigned site</html>") })
}

#[test]
fn unreadable_pages_fall_back_to_the_public_api() {
    let dir = tempfile::tempdir().unwrap();
    let c = Client::new(Cache::new(dir.path(), 10.0), false, false);
    let _net = public_api();
    let programs = c.listing(Kind::Program).unwrap();
    assert!(programs.degraded);
    assert_eq!(programs.data.as_array().unwrap().iter().map(|p| p["slug"].clone()).collect::<Vec<_>>(), vec![json!("acme")]);
    assert_eq!(c.detail(Kind::Program, "acme").unwrap().data["tags"]["language"], json!(["Rust"]));
    assert_eq!(c.listing(Kind::Competition).unwrap().data[0]["slug"], "comp");
    assert!(c.cache.read("program/acme").is_none(), "fallback data is not cached as a page");
    assert!(matches!(c.detail(Kind::Program, "missing"), Err(Error::NotFound(_))));
}

#[test]
fn rsc_404_marker_is_not_found_without_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let c = Client::new(Cache::new(dir.path(), 10.0), false, false);
    let (count, _net) = counting("c:E{\"digest\":\"NEXT_HTTP_ERROR_FALLBACK;404\"}\n".into());
    assert!(matches!(c.detail(Kind::Program, "missing"), Err(Error::NotFound(_))));
    assert_eq!(*count.borrow(), 1, "no fallback request for a real 404");
}

fn days(n: i64) -> String {
    (Utc::now() + Duration::days(n)).to_rfc3339()
}

#[test]
fn cached_status_advances_but_never_regresses() {
    assert_eq!(competition_status(&json!({"auditCompetitionStatus": "UPCOMING", "launchDate": days(-1), "endDate": days(3)})), "LIVE");
    assert_eq!(competition_status(&json!({"auditCompetitionStatus": "LIVE", "launchDate": days(-9), "endDate": days(-1)})), "EVALUATING");
    assert_eq!(competition_status(&json!({"auditCompetitionStatus": "FINISHED", "launchDate": days(-9), "endDate": days(1)})), "FINISHED");
}

#[test]
fn targets_accept_urls_and_slugs() {
    assert_eq!(
        parse_target("https://immunefi.com/bug-bounty/ssvnetwork/information/#top").unwrap(),
        (Some(Kind::Program), "ssvnetwork".into())
    );
    assert_eq!(
        parse_target("https://immunefi.com/audit-competition/audit-comp-quantus/scope/").unwrap(),
        (Some(Kind::Competition), "audit-comp-quantus".into())
    );
    assert_eq!(parse_target("lido").unwrap(), (None, "lido".into()));
    for bad in ["../etc", "https://immunefi.com/bug-bounty/../x", "a b"] {
        assert!(parse_target(bad).is_err(), "{bad}");
    }
}

#[test]
fn program_filters() {
    let items = vec![
        json!({"slug": "a", "project": "A", "maxBounty": 10, "tags": {"ecosystem": ["ETH"], "language": ["Solidity"]}}),
        json!({"slug": "b", "project": "B", "maxBounty": 500, "tags": {"ecosystem": ["Solana"], "language": ["Rust"]}}),
        json!({"slug": "c", "project": "C", "maxBounty": 900, "pausedAt": "2025-01-01", "tags": {}}),
    ];
    let slugs = |f: NativeFilter| filter_items(&items, Kind::Program, &f).iter().map(|i| i["slug"].clone()).collect::<Vec<_>>();
    let base = NativeFilter { sort: "bounty".into(), ..Default::default() };
    assert_eq!(slugs(base.clone()), vec![json!("b"), json!("a")]);
    assert_eq!(slugs(NativeFilter { ecosystem: Some("eth".into()), include_paused: true, ..base.clone() }), vec![json!("a")]);
    assert_eq!(slugs(NativeFilter { min_bounty: Some(100.0), include_paused: true, ..base }), vec![json!("c"), json!("b")]);
}

#[test]
fn detail_renders_every_section() {
    let bounty = Payload::new(&flight()).unwrap().find("bounty", Value::is_object).unwrap();
    let out = detail(Kind::Competition, &bounty, COMPETITION_SECTIONS, Style::new(false));
    for heading in ["INFORMATION", "SCOPE", "RESOURCES", "LEADERBOARD"] {
        assert!(out.contains(heading), "{heading}");
    }
}
