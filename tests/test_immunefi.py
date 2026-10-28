import json
import sys
import tempfile
import time
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from immunefi_cli import cli, client, render, rsc  # noqa: E402
from immunefi_cli.cache import MIN_TTL_HOURS, Cache, resolve_ttl_hours  # noqa: E402


def page(flight: str) -> str:
    """Wrap a flight payload the way Next.js embeds it in HTML (split across two pushes)."""
    mid = len(flight) // 2
    pushes = "".join(
        f"<script>self.__next_f.push({json.dumps([1, part])})</script>"
        for part in (flight[:mid], flight[mid:])
    )
    return f"<html><body><script>self.__next_f.push([0])</script>{pushes}</body></html>"


def text_row(row_id: str, text: str) -> str:
    return f"{row_id}:T{len(text.encode()):x},{text}"


BOUNTY = {
    "slug": "acme",
    "project": "Acme",
    "maxBounty": 100000,
    "rewardsBody": "$a",
    "programOverview": "$$5 minimum",
    "launchDate": "$D2024-01-01T00:00:00.000Z",
    "assets": "$b:items",
    "missing": "$undefined",
}
FLIGHT = (
    '1:I[123,["x"],"default"]\n'
    + text_row("a", "Rewards in “USDC” — paid fast\nsecond line")
    + '\nb:{"items":[{"type":"smart_contract","url":"https://x"}]}\n'
    + f'2:["$","div",null,{{"bounty":{json.dumps(BOUNTY)},"vault":{{"vaultBalance":5}}}}]\n'
)


class RscTests(unittest.TestCase):
    def test_extract_and_resolve_bounty(self):
        b = rsc.Payload(page(FLIGHT)).find("bounty", dict)
        self.assertEqual(b["rewardsBody"], "Rewards in “USDC” — paid fast\nsecond line")
        self.assertEqual(b["programOverview"], "$5 minimum")
        self.assertEqual(b["launchDate"], "2024-01-01T00:00:00.000Z")
        self.assertEqual(b["assets"], [{"type": "smart_contract", "url": "https://x"}])
        self.assertIsNone(b["missing"])

    def test_raw_stream_matches_html(self):
        self.assertEqual(rsc.Payload(FLIGHT).rows, rsc.Payload(page(FLIGHT)).rows)

    def test_not_found_marker(self):
        self.assertTrue(rsc.Payload('c:E{"digest":"NEXT_HTTP_ERROR_FALLBACK;404"}\n').not_found)
        self.assertFalse(rsc.Payload(FLIGHT).not_found)

    def test_text_row_length_is_in_bytes(self):
        rows = rsc.parse_rows(text_row("a", "é€x") + '0:{"k":1}\n')
        self.assertEqual(rows["a"], "é€x")
        self.assertEqual(rows["0"], {"k": 1})

    def test_predicate_and_missing_key(self):
        with self.assertRaises(rsc.PayloadError):
            rsc.Payload(FLIGHT).find("bounty", dict, where=lambda b: b["slug"] == "other")
        with self.assertRaises(rsc.PayloadError):
            rsc.Payload("<html></html>")


class CacheTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_ttl_never_below_minimum(self):
        self.assertEqual(resolve_ttl_hours(1), MIN_TTL_HOURS)
        self.assertEqual(resolve_ttl_hours(24), 24)
        with mock.patch.dict("os.environ", {"IMMUNEFI_CACHE_TTL_HOURS": "0.5"}):
            self.assertEqual(resolve_ttl_hours(None), MIN_TTL_HOURS)

    def test_fresh_entries_are_not_refetched(self):
        c = client.Client(Cache(self.root, 10))
        with mock.patch.object(client, "_http_get", return_value=FLIGHT) as get:
            first = c.detail("program", "acme")
            second = c.detail("program", "acme")
        self.assertEqual(get.call_count, 1)
        self.assertFalse(first.from_cache)
        self.assertTrue(second.from_cache)
        self.assertEqual(second.data["vaults"], [{"vaultBalance": 5}])

    def test_expired_entry_refetches_and_falls_back_when_offline(self):
        cache = Cache(self.root, 10)
        cache.write("program/acme", {"slug": "acme"}, "u")
        path = self.root / "program" / "acme.json"
        raw = json.loads(path.read_text())
        raw["fetched_at"] = time.time() - 11 * 3600
        path.write_text(json.dumps(raw))

        c = client.Client(cache)
        with mock.patch.object(client, "_http_get", side_effect=client.FetchError("down")):
            result = c.detail("program", "acme")
        self.assertTrue(result.stale)

        with mock.patch.object(client, "_http_get", return_value=FLIGHT) as get:
            result = c.detail("program", "acme")
        self.assertEqual(get.call_count, 1)
        self.assertEqual(result.data["project"], "Acme")

    def test_refresh_bypasses_fresh_cache(self):
        Cache(self.root, 10).write("program/acme", {"slug": "acme"}, "u")
        c = client.Client(Cache(self.root, 10), refresh=True)
        with mock.patch.object(client, "_http_get", return_value=FLIGHT) as get:
            c.detail("program", "acme")
        self.assertEqual(get.call_count, 1)


class FallbackTests(unittest.TestCase):
    API = [
        {"slug": "acme", "project": "Acme", "endDate": None, "language": ["Rust"], "rewards": []},
        {"slug": "hidden", "project": "Hidden", "endDate": None, "inviteOnly": True},
        {"slug": "comp", "project": "Comp", "endDate": "2026-01-01T00:00:00Z", "inviteOnly": True},
    ]

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.client = client.Client(Cache(Path(self.tmp.name), 10))

    def tearDown(self):
        self.tmp.cleanup()

    def serve(self, url, headers=None):
        if url == client.PUBLIC_API_URL:
            return json.dumps(self.API)
        return "<html>redesigned site</html>"

    def test_unreadable_page_falls_back_to_public_api(self):
        with mock.patch.object(client, "_http_get", side_effect=self.serve):
            programs = self.client.listing("program")
            detail = self.client.detail("program", "acme")
            comps = self.client.listing("competition")
        self.assertTrue(programs.degraded)
        self.assertEqual([p["slug"] for p in programs.data], ["acme"])
        self.assertEqual(detail.data["tags"]["language"], ["Rust"])
        self.assertEqual([c["slug"] for c in comps.data], ["comp"])
        self.assertIsNone(self.client.cache.read("program/acme"))  # fallback data is not cached as a page

    def test_fallback_reports_unknown_slug_as_not_found(self):
        with mock.patch.object(client, "_http_get", side_effect=self.serve):
            with self.assertRaises(client.NotFoundError):
                self.client.detail("program", "missing")

    def test_rsc_404_marker_is_not_found(self):
        body = 'c:E{"digest":"NEXT_HTTP_ERROR_FALLBACK;404"}\n'
        with mock.patch.object(client, "_http_get", return_value=body) as get:
            with self.assertRaises(client.NotFoundError):
                self.client.detail("program", "missing")
        self.assertEqual(get.call_count, 1)  # no fallback request for a real 404


class StatusTests(unittest.TestCase):
    def iso(self, days: float) -> str:
        return (datetime.now(timezone.utc) + timedelta(days=days)).isoformat()

    def test_cached_status_advances_with_dates(self):
        item = {"auditCompetitionStatus": "UPCOMING", "launchDate": self.iso(-1), "endDate": self.iso(3)}
        self.assertEqual(render.competition_status(item), "LIVE")
        item = {"auditCompetitionStatus": "LIVE", "launchDate": self.iso(-9), "endDate": self.iso(-1)}
        self.assertEqual(render.competition_status(item), "EVALUATING")

    def test_reported_status_never_regresses(self):
        item = {"auditCompetitionStatus": "FINISHED", "launchDate": self.iso(-9), "endDate": self.iso(1)}
        self.assertEqual(render.competition_status(item), "FINISHED")


class CliTests(unittest.TestCase):
    def test_parse_target_accepts_urls_and_slugs(self):
        self.assertEqual(
            client.parse_target("https://immunefi.com/bug-bounty/ssvnetwork/information/#top"),
            ("program", "ssvnetwork"),
        )
        self.assertEqual(
            client.parse_target("https://immunefi.com/audit-competition/audit-comp-quantus/scope/"),
            ("competition", "audit-comp-quantus"),
        )
        self.assertEqual(client.parse_target("lido"), (None, "lido"))
        for bad in ("../etc", "https://immunefi.com/bug-bounty/../x", "a b"):
            with self.assertRaises(ValueError):
                client.parse_target(bad)

    def test_program_filters(self):
        items = [
            {"slug": "a", "project": "A", "maxBounty": 10, "tags": {"ecosystem": ["ETH"], "language": ["Solidity"]}},
            {"slug": "b", "project": "B", "maxBounty": 500, "tags": {"ecosystem": ["Solana"], "language": ["Rust"]}},
            {"slug": "c", "project": "C", "maxBounty": 900, "pausedAt": "2025-01-01", "tags": {}},
        ]
        args = cli.build_parser().parse_args(["programs"])
        self.assertEqual([i["slug"] for i in cli._filter(items, args)], ["b", "a"])
        args = cli.build_parser().parse_args(["programs", "-e", "eth", "--include-paused"])
        self.assertEqual([i["slug"] for i in cli._filter(items, args)], ["a"])
        args = cli.build_parser().parse_args(["programs", "--min-bounty", "100", "--include-paused"])
        self.assertEqual([i["slug"] for i in cli._filter(items, args)], ["c", "b"])

    def test_detail_renders_every_section(self):
        bounty = rsc.Payload(FLIGHT).find("bounty", dict)
        out = render.detail("competition", bounty, cli.COMPETITION_SECTIONS, render.Style(False))
        for heading in ("INFORMATION", "SCOPE", "RESOURCES", "LEADERBOARD"):
            self.assertIn(heading, out)


if __name__ == "__main__":
    unittest.main()
