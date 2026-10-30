//! End-to-end checks of the `bb` binary, offline, against a cache holding every platform.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const UNISWAP: &str = "f9df94db-c7b1-434b-bb06-d1360abdd1be";
const SAME: &str = "c1c1c1c1-0000-4000-8000-000000000001";
const REVERT: &str = "e27e27e2-0000-4000-8000-000000000002";

fn write(path: &Path, data: Value, source: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, json!({"fetched_at": 4102444800.0, "source": source, "url": source, "data": data}).to_string()).unwrap();
}

fn cantina(root: &Path, url: &str, data: Value) {
    let key = hex::encode(Sha256::digest(url.as_bytes()));
    write(&root.join("cantina").join(format!("{key}.json")), data, url);
}

/// A cache root with a few opportunities on every platform.
fn fixture() -> PathBuf {
    let root = tempfile::tempdir().unwrap().keep();
    let mut programs: Vec<Value> = (0..28)
        .map(|i| {
            json!({"slug": format!("p{i:02}"), "project": format!("Project {i:02}"), "maxBounty": 1000 + i, "kyc": false,
                        "vaultBalance": i, "tags": {"language": ["Solidity"]}})
        })
        .collect();
    programs.push(json!({"slug": "same", "project": "Same", "maxBounty": 100, "kyc": false}));
    programs.push(json!({"slug": "ssvnetwork", "project": "SSV Network", "maxBounty": 1000000, "kyc": false}));
    write(&root.join("immunefi/programs.json"), json!(programs), "u");
    write(
        &root.join("immunefi/competitions.json"),
        json!([
            {"slug": "old", "project": "Old", "launchDate": "2020-01-01T00:00:00Z", "endDate": "2020-02-01T00:00:00Z",
             "evaluationEndDate": "2020-03-01T00:00:00Z", "rewardsPool": 5000},
            {"slug": "now", "project": "Now", "launchDate": "2020-01-01T00:00:00Z", "endDate": "2999-01-01T00:00:00Z", "rewardsPool": 1000},
        ]),
        "u",
    );
    let bounty = json!({"id": UNISWAP, "name": "Uniswap", "kind": "public_bounty", "status": "live", "totalRewardPot": "15500000",
                        "currencyCode": "USDC", "kycRequired": true, "company": {"handle": "uniswap"}});
    let same =
        json!({"id": SAME, "name": "Same", "kind": "public_bounty", "status": "live", "totalRewardPot": "200", "currencyCode": "USDC"});
    let revert = json!({"id": REVERT, "name": "Revert v4lend", "kind": "public_contest", "status": "live", "totalRewardPot": "90000",
                        "currencyCode": "USDC", "timeframe": {"start": "2020-01-01T00:00:00Z", "end": "2999-01-01T00:00:00Z"}});
    let api = "https://api.cantina.xyz/api/v0";
    cantina(&root, &format!("{api}/repositories/public"), json!([bounty, same, revert]));
    cantina(
        &root,
        &format!("{api}/opportunities?limit=50&status=live&kind=public_bounty%2Cprivate_bounty"),
        json!({"items": [bounty, same]}),
    );
    for status in ["live", "ended"] {
        let items = if status == "live" { json!([revert]) } else { json!([]) };
        cantina(
            &root,
            &format!("{api}/opportunities?limit=50&status={status}&kind=public_contest%2Cprivate_contest"),
            json!({"items": items}),
        );
    }
    cantina(&root, &format!("{api}/repositories/{UNISWAP}/public"), bounty.clone());
    let usual = json!({"id": 56, "title": "Usual Labs", "payout": 16000000, "display_currency": "USDC", "is_ready": true,
                       "covered_scope": [{"address": "0xe1DeE60c516a8350704Ec24a6E856c9F533d1c1b", "network": "ethereum", "display_name": "Oracle"}]});
    write(&root.join("sherlock/bug_bounties-1.json"), json!({"items": [usual], "has_next": false}), "u");
    write(&root.join("sherlock/program-56.json"), usual, "u");
    write(
        &root.join("sherlock/contests-1.json"),
        json!({"items": [{"id": 7, "title": "Metric", "status": "FINISHED", "rewards": 150000, "starts_at": 1700000000, "ends_at": 1700500000}], "has_next": false}),
        "u",
    );
    write(
        &root.join("hackenproof/programs-1.json"),
        json!({"programs": [{"slug": "layer3", "title": "Layer3", "max_bounty": 50000, "status": {"name": "Active"}, "start_date": "01 Jan 2024"}], "next_page": null}),
        "u",
    );
    write(&root.join("hackenproof/audits-1.json"), json!({"audits": [], "next_page": null}), "u");
    root
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

fn bb(root: &Path, args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(args)
        .args(["--cache-dir", root.to_str().unwrap(), "--offline"])
        .env("NO_COLOR", "1")
        .env("COLUMNS", "200")
        .env_remove("BB_CACHE_TTL_HOURS")
        .output()
        .unwrap();
    Run {
        code: output.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&output.stdout).into(),
        err: String::from_utf8_lossy(&output.stderr).into(),
    }
}

fn json_rows(run: &Run) -> Vec<Value> {
    assert_eq!(run.code, 0, "{}", run.err);
    serde_json::from_str::<Value>(&run.out).unwrap().as_array().unwrap().clone()
}

#[test]
fn combined_listing_sorts_and_limits_globally() {
    let root = fixture();
    let rows = json_rows(&bb(&root, &["programs", "--limit", "2", "--json"]));
    assert_eq!(rows[0]["target"], "sherlock:56");
    assert_eq!(rows[1]["source"], "cantina");
    let all = json_rows(&bb(&root, &["programs", "--json"]));
    let same: Vec<&Value> = all.iter().filter(|r| r["name"] == "Same").collect();
    assert_eq!(same.len(), 2, "same names on two platforms stay separate rows");
}

#[test]
fn default_limit_shows_a_hint() {
    let root = fixture();
    let run = bb(&root, &["programs"]);
    assert_eq!(run.code, 0, "{}", run.err);
    assert!(run.out.contains("25 of 34 matching programs  (-n N or --all for more)"), "{}", run.out);
    assert!(run.out.contains("cantina:f9df94db") && !run.out.contains(UNISWAP), "{}", run.out);
    let all = bb(&root, &["programs", "--all"]);
    assert!(all.out.contains("34 of 34 matching programs\n"), "{}", all.out);
}

#[test]
fn competitions_list_live_first() {
    let root = fixture();
    let rows = json_rows(&bb(&root, &["competitions", "--json"]));
    let statuses: Vec<&str> = rows.iter().map(|r| r["status"].as_str().unwrap()).collect();
    assert_eq!(statuses, ["live", "live", "finished", "finished"]);
}

#[test]
fn source_selection_and_platform_prefix() {
    let root = fixture();
    for args in [&["--source", "cantina", "programs", "--json"][..], &["programs", "--source=cantina", "--json"]] {
        assert!(json_rows(&bb(&root, args)).iter().all(|r| r["source"] == "cantina"));
    }
    let run = bb(&root, &["sherlock", "programs"]);
    assert!(run.out.contains("sherlock:56") && !run.out.contains("immunefi:"), "{}", run.out);
}

#[test]
fn failed_platform_never_prints_a_partial_listing() {
    let root = fixture();
    std::fs::remove_dir_all(root.join("hackenproof")).unwrap();
    let run = bb(&root, &["programs", "--json"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.out, "");
    assert!(run.err.contains("HackenProof") && run.err.contains("no partial combined listing"), "{}", run.err);
}

#[test]
fn invalid_sources_and_flags() {
    let root = fixture();
    for args in [&["--source", "other", "programs"][..], &["programs", "--refresh"], &["programs", "--limit", "0"]] {
        let run = bb(&root, args);
        assert_eq!(run.code, 1, "{args:?}: {}", run.err);
        assert_eq!(run.out, "");
    }
}

#[test]
fn vault_sort_lists_only_platforms_with_vaults() {
    let root = fixture();
    let run = bb(&root, &["programs", "--sort", "vault", "--json"]);
    assert!(json_rows(&run).iter().all(|r| r["source"] == "immunefi"));
    assert!(run.err.contains("only Immunefi publishes vault balances"), "{}", run.err);
    let run = bb(&root, &["programs", "--sort", "vault", "--source", "sherlock"]);
    assert!(run.err.contains("Sherlock publishes no vault balances"));
}

#[test]
fn tag_filters_note_platforms_without_tags() {
    let root = fixture();
    let run = bb(&root, &["programs", "-l", "solidity", "--json"]);
    assert!(run.err.contains("Note: Sherlock has no language tags; its programs are left out"), "{}", run.err);
    assert!(json_rows(&run).iter().all(|r| r["source"] == "immunefi"));
}

#[test]
fn details_keep_their_provenance() {
    let root = fixture();
    let run = bb(&root, &["program", "sherlock:56", "--json"]);
    let doc: Value = serde_json::from_str(&run.out).unwrap();
    assert_eq!((doc["source"].as_str(), doc["target"].as_str()), (Some("sherlock"), Some("sherlock:56")));
    assert_eq!(doc["data"]["title"], "Usual Labs");
    let run = bb(&root, &["program", "cantina:f9df94db", "--json"]);
    let doc: Value = serde_json::from_str(&run.out).unwrap_or_else(|_| panic!("{} {}", run.out, run.err));
    assert_eq!(doc["source"], "cantina");
    assert_eq!(doc["target"], format!("cantina:{UNISWAP}"));
    assert_eq!(doc["data"]["name"], "Uniswap");
    let run = bb(&root, &["program", "sherlock:56", "bogus"]);
    assert!(run.err.contains("Unknown section 'bogus'"), "{}", run.err);
}

#[test]
fn bare_names_resolve_across_platforms() {
    let root = fixture();
    let run = bb(&root, &["program", "uniswap"]);
    assert!(run.out.starts_with("Uniswap  ") && run.out.contains("$15,500,000 USDC"), "{} {}", run.out, run.err);
    let run = bb(&root, &["program", "Same"]);
    assert!(
        run.err.contains("matches several programs") && run.err.contains("immunefi:same") && run.err.contains("cantina:c1c1c1c1"),
        "{}",
        run.err
    );
    let run = bb(&root, &["program", "revert v4lend"]);
    assert!(run.err.contains("is a competition: bb competition cantina:e27e27e2"), "{}", run.err);
    let run = bb(&root, &["program", "Uniswapp"]);
    assert!(run.err.contains("did you mean: Uniswap (cantina:f9df94db)"), "{}", run.err);
    let run = bb(&root, &["program", "https://audits.sherlock.xyz/contests/7"]);
    assert!(run.err.contains("is a competition: bb competition sherlock:7"), "{}", run.err);
}

#[test]
fn platform_only_commands_and_features() {
    let root = fixture();
    let run = bb(&root, &["--source", "immunefi", "auth"]);
    assert!(run.err.contains("auth is not available on Immunefi"), "{}", run.err);
    let run = bb(&root, &["program", "immunefi:same", "--private"]);
    assert!(run.err.contains("--private is not available on Immunefi"), "{}", run.err);
    let native = root.join("immunefi");
    let output = Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(["immunefi", "programs", "-n", "1", "--offline", "--cache-dir", native.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("1 of 30 programs"), "{text} {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn cache_report_and_selective_clear() {
    let root = fixture();
    write(&root.join("evm/test.json"), json!({}), "https://example.com");
    let report: Value = serde_json::from_str(&bb(&root, &["cache", "--json"]).out).unwrap();
    assert!(report["entries"].as_array().unwrap().len() >= 10);
    let cleared: Value = serde_json::from_str(&bb(&root, &["cache", "clear", "--source", "sherlock", "--json"]).out).unwrap();
    assert_eq!(cleared["removed"], 3);
    assert!(root.join("evm/test.json").exists() && !root.join("sherlock/contests-1.json").exists());
}

#[test]
fn sites_list_every_platform_listing() {
    let root = fixture();
    let output = Command::new(env!("CARGO_BIN_EXE_bb")).args(["sites", "--json"]).output().unwrap();
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.len(), 8);
    let output = Command::new(env!("CARGO_BIN_EXE_bb")).args(["links", "sherlock", "-k", "programs"]).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("https://audits.sherlock.xyz/bug-bounties") && !text.contains("contests"), "{text}");
    let _ = root;
}

#[test]
fn help_names_no_platform() {
    for args in [
        &["--help"][..],
        &["programs", "--help"],
        &["competitions", "--help"],
        &["program", "--help"],
        &["contract", "--help"],
        &["cache", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_bb")).args(args).output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout).to_lowercase();
        for name in ["immunefi", "cantina", "sherlock", "hackenproof"] {
            assert!(!text.contains(name), "{args:?} names {name}");
        }
    }
}

#[test]
fn scope_routes_to_the_platform() {
    let root = fixture();
    let chains = json!([{"chainid": "1", "chainname": "Ethereum Mainnet", "blockexplorer": "https://etherscan.io", "comment": ""}]);
    write(&root.join("evm/etherscan/chainlist.json"), chains, "u");
    let run = bb(&root, &["contract", "scope", "sherlock:56", "--no-live", "--json"]);
    // Offline with no explorer metadata cached: the contract is listed with the lookup error.
    let doc: Value = serde_json::from_str(&run.out).unwrap_or_else(|_| panic!("{} {}", run.out, run.err));
    assert_eq!(doc["program"], 56);
    assert_eq!(doc["contracts"][0]["address"], "0xe1DeE60c516a8350704Ec24a6E856c9F533d1c1b");
}
