"""Plain-text rendering of program lists and detail pages."""

from __future__ import annotations

import os
import re
import shutil
import sys
from datetime import datetime, timezone
from typing import Any, Callable, Iterable

from bb_cli.providers.immunefi.client import BASE_URL

SEVERITY_ORDER = ["critical", "high", "medium", "low", "none", "insight"]
STATUS_ORDER = ["UPCOMING", "LIVE", "EVALUATING", "FINISHED"]
ASSET_TYPE_LABEL = {
    "smart_contract": "Smart Contract",
    "websites_and_applications": "Websites and Applications",
    "blockchain_dlt": "Blockchain/DLT",
}
ELIGIBILITY_LABEL = {
    "no_ofac_sdn": "Not on OFAC SDN list",
    "no_employee": "Not an employee or contractor of the project",
    "no_sanctioned_country": "Not resident in a sanctioned country",
    "age_18": "At least 18 years old",
}


from bb_cli.evm.render import (Style, use_color, money, parse_date, date, datetime_utc, relative, duration, joined, markdown, indent, table, Doc)

def _normalize_status(raw: Any) -> str | None:
    if not isinstance(raw, str) or not raw:
        return None
    s = raw.upper()
    for keywords, status in (
        (("EVAL", "JUDG", "REVIEW"), "EVALUATING"),
        (("FINISH", "END", "CLOSE", "COMPLET"), "FINISHED"),
        (("UPCOMING", "SCHEDUL", "SOON", "START"), "UPCOMING"),
        (("LIVE", "ACTIVE", "ONGOING", "OPEN"), "LIVE"),
    ):
        if any(k in s for k in keywords):
            return status
    return s


def competition_status(item: dict) -> str:
    """Status reported by Immunefi, advanced by dates so a cached listing never goes stale."""
    now = datetime.now(timezone.utc)
    start, end = parse_date(item.get("launchDate")), parse_date(item.get("endDate"))
    eval_end = parse_date(item.get("evaluationEndDate"))
    derived = None
    if start and now < start:
        derived = "UPCOMING"
    elif end and now < end:
        derived = "LIVE"
    elif end:
        derived = "FINISHED" if eval_end and now >= eval_end else "EVALUATING"
    reported = _normalize_status(item.get("auditCompetitionStatus"))
    if reported not in STATUS_ORDER:
        return reported or derived or "UNKNOWN"
    if derived and STATUS_ORDER.index(derived) > STATUS_ORDER.index(reported):
        return derived
    return reported


def program_status(item: dict) -> str:
    return "PAUSED" if item.get("pausedAt") or item.get("isPaused") else "ACTIVE"


def program_table(items: list[dict], style: Style) -> str:
    headers = ["SLUG", "PROJECT", "MAX BOUNTY", "VAULT", "TYPE", "ECOSYSTEM", "LANGUAGE", "KYC", "UPDATED"]
    rows = []
    for it in items:
        tags = it.get("tags") or {}
        name = it.get("project") or ""
        if program_status(it) == "PAUSED":
            name += " (paused)"
        rows.append([
            it.get("slug", ""),
            name,
            money(it.get("maxBounty"), compact=True),
            money(it.get("vaultBalance"), compact=True),
            joined(tags.get("programType"), 1),
            joined(tags.get("ecosystem"), 2),
            joined(tags.get("language"), 2),
            "yes" if it.get("kyc") else "no",
            date(it.get("updatedDate")),
        ])
    return table(headers, rows, style, flex=1)


def competition_table(items: list[dict], style: Style) -> str:
    headers = ["SLUG", "PROJECT", "STATUS", "POOL", "START", "END", "TYPE", "LANGUAGE", "KYC"]
    rows = []
    for it in items:
        tags = it.get("tags") or {}
        rows.append([
            it.get("slug", ""),
            it.get("project") or "",
            competition_status(it),
            money(it.get("rewardsPool") or it.get("maxBounty"), compact=True),
            date(it.get("launchDate")),
            date(it.get("endDate")),
            joined(tags.get("programType"), 1),
            joined(tags.get("language"), 2),
            "yes" if it.get("kyc") else "no",
        ])
    return table(headers, rows, style, flex=1, styler={2: style.status})


# --- detail sections --------------------------------------------------------------------------


def detail_url(kind: str, slug: str, tab: str = "information") -> str:
    path = "bug-bounty" if kind == "program" else "audit-competition"
    return f"{BASE_URL}/{path}/{slug}/{tab}/"


def _reward_line(r: dict) -> tuple[str, str, str]:
    severity = str(r.get("severity") or r.get("level") or "-").lower()
    asset = ASSET_TYPE_LABEL.get(r.get("assetType"), r.get("assetType") or "-")
    model = r.get("rewardModel")
    if r.get("payout"):
        payout = str(r["payout"])
    elif model == "range" or ("minReward" in r and "maxReward" in r):
        payout = f"{money(r.get('minReward'))} - {money(r.get('maxReward'))}"
    elif model == "fixed" or "fixedReward" in r:
        payout = money(r.get("fixedReward"))
    else:
        payout = f"up to {money(r.get('maxReward'))}"
    if pct := r.get("rewardCalculationPercentage"):
        payout += f" ({pct}% of funds directly affected)"
    if other := r.get("otherImpactMaxReward"):
        payout += f" (other impacts up to {money(other)})"
    if r.get("pocRequired"):
        payout += " · PoC required"
    if r.get("primacy"):
        payout += f" · {str(r['primacy']).replace('_', ' ')}"
    return severity, asset, payout


def _rewards(b: dict) -> list[dict]:
    if b.get("programRewards"):
        return b["programRewards"]
    if b.get("rewards"):
        return b["rewards"]
    legacy = b.get("legacy") or {}
    out = []
    for key, asset in (
        ("smartcontract_rewards", "smart_contract"),
        ("web_rewards", "websites_and_applications"),
        ("blockchain_rewards", "blockchain_dlt"),
    ):
        out += [{**r, "assetType": asset} for r in legacy.get(key) or []]
    return out


def _sev_key(severity: str) -> int:
    s = severity.lower()
    return SEVERITY_ORDER.index(s) if s in SEVERITY_ORDER else len(SEVERITY_ORDER)


def section_overview(doc: Doc, kind: str, b: dict) -> None:
    s = doc.style
    tags = b.get("tags") or {}
    doc.raw(s.bold(b.get("project") or b.get("slug", "")) + "  " + s.link(detail_url(kind, b["slug"])))
    pairs: list[tuple[str, str]] = []
    if kind == "competition":
        status = competition_status(b)
        pairs += [
            ("Status", s.status(status)),
            ("Reward pool", money(b.get("rewardsPool") or b.get("maxBounty"))),
            ("Pools", ", ".join(
                f"{label} {money(b.get(key))}"
                for label, key in (("primary", "primaryPool"), ("all-stars", "allStarsPool"), ("podium", "podiumPool"))
                if b.get(key)
            )),
            ("Starts", f"{datetime_utc(b.get('launchDate'))}  {s.dim(relative(b.get('launchDate')))}"),
            ("Ends", f"{datetime_utc(b.get('endDate'))}  {s.dim(relative(b.get('endDate')))}"),
            ("Evaluation end", datetime_utc(b.get("evaluationEndDate"))),
            ("Lines of code", f"{b['sourceLinesOfCode']:,}" if b.get("sourceLinesOfCode") else ""),
        ]
    else:
        pairs += [
            ("Status", s.status(program_status(b))),
            ("Max bounty", money(b.get("maxBounty"))),
            ("Launched", date(b.get("launchDate"))),
        ]
    vault_total = sum(v.get("vaultBalance") or 0 for v in b.get("vaults") or [])
    pairs += [
        ("Vault balance", money(vault_total or b.get("vaultBalance"))),
        ("Updated", date(b.get("updatedDate"))),
        ("Rewards token", " on ".join(x for x in (b.get("rewardsToken"), b.get("rewardsTokenNetwork")) if x)),
        ("KYC", "required" if b.get("kyc") else "not required"),
        ("PoC", str(b.get("proofOfConceptType") or "").replace("_", " ")),
        ("Primacy", str(b.get("primacy") or "").replace("primacy_of_", "primacy of ")),
        ("Triage", "Immunefi managed" if b.get("premiumTriaging") else "project"),
        ("Program type", joined(tags.get("programType"))),
        ("Project type", joined(tags.get("projectType"))),
        ("Product type", joined(tags.get("productType"))),
        ("Ecosystem", joined(tags.get("ecosystem"), 12)),
        ("Language", joined(tags.get("language"))),
        ("Features", joined(b.get("features"))),
        ("Website", b.get("websiteUrl") or ""),
        ("GitHub", b.get("githubUrl") or ""),
    ]
    if b.get("pausedMessage"):
        pairs.append(("Paused", b["pausedMessage"]))
    doc.fields(pairs)
    if b.get("description"):
        doc.sub("Description")
        doc.text(b["description"])


def section_information(doc: Doc, kind: str, b: dict) -> None:
    doc.section("Information")
    if kind == "competition":
        status = competition_status(b).capitalize()
        intro = b.get(f"boostedIntro{status}") or b.get("boostedIntroLive")
        intro = re.sub(r"^\s*competition timeline\s*\n", "", markdown(intro), flags=re.I)
        if intro:
            doc.sub("Competition timeline")
            doc.text(intro)
        if b.get("boostedSummaryReport"):
            doc.fields([("Summary report", b["boostedSummaryReport"])])

    vaults = b.get("vaults") or []
    if vaults:
        doc.sub("Immunefi vault")
        for v in vaults:
            assets = ", ".join(f"{a.get('balance')} {a.get('symbol')}" for a in v.get("assets") or [])
            doc.fields([
                ("Address", f"{v.get('address')} (chain {v.get('chainId')})"),
                ("Balance", money(v.get("vaultBalance"))),
                ("30d average", money(v.get("averageBalance"))),
                ("Holdings", assets),
            ])

    doc.sub("Rewards by threat level")
    rewards = sorted((_reward_line(r) for r in _rewards(b)), key=lambda x: (x[1], _sev_key(x[0])))
    if rewards:
        doc.raw(indent(table(
            ["SEVERITY", "ASSET", "REWARD"],
            [list(r) for r in rewards],
            doc.style,
            flex=2,
            styler={0: doc.style.severity},
        )))
    else:
        doc.raw("  -")
    if markdown(b.get("rewardsBody")):
        doc.raw("")
        doc.text(b["rewardsBody"])

    for title, key in (
        ("Program overview", "programOverview"),
        ("Prioritized vulnerabilities", "prioritizedVulnerabilities"),
    ):
        if markdown(b.get(key)):
            doc.sub(title)
            doc.text(b[key])

    if b.get("audits"):
        doc.sub("Audits")
        for a in b["audits"]:
            doc.raw(f"  - {a.get('auditor') or '?'} ({date(a.get('date'))}): {doc.style.link(a.get('url') or '')}")

    if b.get("knownIssues"):
        doc.sub(f"Known issues ({len(b['knownIssues'])})")
        for k in b["knownIssues"]:
            body = markdown(k.get("description")) or "-"
            first, *rest = body.splitlines()
            extra = f"  [{ASSET_TYPE_LABEL.get(k.get('relatedImpactInScope'), k.get('relatedImpactInScope'))}]" if k.get("relatedImpactInScope") else ""
            doc.raw(f"  - {first}{doc.style.dim(extra)}")
            if rest:
                doc.raw(indent("\n".join(rest), "    "))
            if k.get("link"):
                doc.raw(f"    {doc.style.link(k['link'])}")

    doc.sub("KYC " + ("required" if b.get("kyc") else "not required"))
    if b.get("kycLevel"):
        doc.fields([("Level", str(b["kycLevel"]))])
    criteria = [ELIGIBILITY_LABEL.get(c, c.replace("_", " ")) for c in b.get("eligibilityCriteria") or []]
    if criteria:
        doc.sub("Eligibility")
        doc.raw("\n".join(f"  - {c}" for c in criteria))
    doc.text(b.get("eligibilityCriteriaAdditionalInformation"))

    doc.sub("Proof of concept")
    doc.fields([("Requirement", str(b.get("proofOfConceptType") or "-").replace("_", " "))])

    if b.get("responsiblePublicationCategory"):
        doc.sub("Responsible publication")
        category = b["responsiblePublicationCategory"].replace("category_", "Category ")
        doc.raw(f"  {category} - see {doc.style.link(BASE_URL + '/responsible-publication/')}")

    prohibited = b.get("prohibitedActivites") or b.get("defaultProhibitedActivities")
    custom = [c if isinstance(c, str) else c.get("description", "") for c in b.get("customProhibitedActivities") or []]
    if markdown(prohibited) or custom:
        doc.sub("Prohibited activities")
        doc.text(prohibited)
        doc.raw("\n".join(f"  - {c}" for c in custom if c))

    feasibility = b.get("feasibilityLimitations") or b.get("defaultFeasibilityLimitations")
    if markdown(feasibility):
        doc.sub("Feasibility limitations")
        doc.text(feasibility)


def section_scope(doc: Doc, kind: str, b: dict) -> None:
    doc.section("Scope")
    assets = b.get("assets") or []
    doc.sub(f"Assets in scope ({len(assets)})")
    if b.get("hideAssetsInScope"):
        doc.raw("  (assets hidden by the project)")
    rows = []
    for a in assets:
        notes = [n for n in (
            "primacy of impact" if a.get("isPrimacyOfImpact") else "",
            "safe harbor" if a.get("isSafeHarbor") else "",
        ) if n]
        rows.append([
            ASSET_TYPE_LABEL.get(a.get("type"), a.get("type") or "-"),
            a.get("url") or "-",
            (a.get("description") or "") + (f" [{', '.join(notes)}]" if notes else ""),
            date(a.get("addedAt")),
        ])
    if rows:
        doc.raw(indent(table(["TYPE", "TARGET", "DESCRIPTION", "ADDED"], rows, doc.style, flex=2)))
    doc.text(b.get("assetsBodyV2"))

    impacts = b.get("programImpacts") or [
        {**i, "assetType": i.get("type"), "description": i.get("title")} for i in b.get("impacts") or []
    ]
    doc.sub(f"Impacts in scope ({len(impacts)})")
    by_type: dict[str, list[dict]] = {}
    for i in impacts:
        by_type.setdefault(i.get("assetType") or "other", []).append(i)
    for asset_type, group in by_type.items():
        doc.raw(f"  {doc.style.bold(ASSET_TYPE_LABEL.get(asset_type, asset_type))}")
        for i in sorted(group, key=lambda x: _sev_key(str(x.get("severity") or ""))):
            sev = str(i.get("severity") or "-").capitalize()
            label = doc.style.severity(sev.ljust(10))
            extra = f"  (discretionary {money(i['discretionaryRewardAmount'])})" if i.get("discretionaryRewardAmount") else ""
            doc.raw(f"    {label}{i.get('description') or '-'}{extra}")
    doc.text(b.get("impactsBody"))

    doc.sub("Out of scope")
    if markdown(b.get("customOutOfScopeInformation")):
        doc.text(b["customOutOfScopeInformation"])
    else:
        in_scope = {a.get("type") for a in assets} | set(by_type)
        for title, key, asset_type in (
            ("General", "defaultOutOfScopeGeneral", None),
            ("Smart contracts", "defaultOutOfScopeSmartContract", "smart_contract"),
            ("Blockchain/DLT", "defaultOutOfScopeBlockchain", "blockchain_dlt"),
            ("Websites and applications", "defaultOutOfScopeWebAndApplications", "websites_and_applications"),
        ):
            if markdown(b.get(key)) and (asset_type is None or asset_type in in_scope):
                doc.raw(f"  {doc.style.bold(title)}")
                doc.text(b[key])
    if markdown(b.get("outOfScopeAndRules")):
        doc.sub("Additional rules")
        doc.text(b["outOfScopeAndRules"])


def section_resources(doc: Doc, kind: str, b: dict) -> None:
    doc.section("Resources")
    if b.get("source") == "public-api":
        doc.raw(doc.style.dim(f"  Codebase and documentation links are only on {detail_url(kind, b['slug'], 'resources')}"))
    for title, key in (("Codebase", "programCodebases"), ("Documentation", "programDocumentations")):
        entries = b.get(key) or []
        doc.sub(title)
        if not entries:
            doc.raw("  -")
        for e in entries:
            desc = e.get("description") or ""
            desc = f" - {desc}" if desc and desc != "-" else ""
            doc.raw(f"  - {e.get('title') or e.get('url')}{doc.style.dim(desc)}\n    {doc.style.link(e.get('url') or '')}")
    links = [("Website", b.get("websiteUrl")), ("GitHub", b.get("githubUrl"))]
    if any(v for _, v in links):
        doc.sub("Links")
        doc.fields([(k, v) for k, v in links if v])
    if b.get("audits"):
        doc.sub("Audits")
        for a in b["audits"]:
            doc.raw(f"  - {a.get('auditor') or '?'} ({date(a.get('date'))}): {doc.style.link(a.get('url') or '')}")


def section_leaderboard(doc: Doc, kind: str, b: dict) -> None:
    doc.section("Leaderboard")
    board = b.get("boostedLeaderboard") or []
    if not board:
        doc.raw("  No leaderboard published yet.")
        return
    def rank_key(r: dict) -> tuple[int, float]:
        rank = r.get("aspRank")
        return (rank if isinstance(rank, int) else 10**9, -(r.get("totalEarnings") or 0))

    board = sorted(board, key=rank_key)
    rows = [[
        str(r.get("aspRank") or "-"),
        r.get("name") or "-",
        money(r.get("totalEarnings")),
        str(r.get("totalValidBugs") or 0),
        str(r.get("critical") or 0),
        str(r.get("high") or 0),
        str(r.get("medium") or 0),
        str(r.get("low") or 0),
        str(r.get("insights") or 0),
        str(r.get("duplicateReports") or 0),
    ] for r in board]
    doc.raw(indent(table(
        ["RANK", "WHITEHAT", "EARNINGS", "VALID", "CRIT", "HIGH", "MED", "LOW", "INSIGHT", "DUPES"],
        rows, doc.style, flex=1,
    )))


SECTIONS: dict[str, Callable[[Doc, str, dict], None]] = {
    "overview": section_overview,
    "information": section_information,
    "scope": section_scope,
    "resources": section_resources,
    "leaderboard": section_leaderboard,
}


def detail(kind: str, b: dict, sections: list[str], style: Style) -> str:
    doc = Doc(style)
    if "overview" not in sections:
        doc.raw(style.bold(b.get("project") or b["slug"]) + "  " + style.link(detail_url(kind, b["slug"])))
    for name in sections:
        SECTIONS[name](doc, kind, b)
    return doc.render()
