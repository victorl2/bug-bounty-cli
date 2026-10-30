import contextlib
import io
import json
import time
import unittest
from unittest.mock import patch

from bb_cli import cli
from bb_cli.evm.render import Style
from bb_cli.providers import common
from bb_cli.providers.hackenproof import api as hackenproof
from bb_cli.providers.sherlock import api as sherlock

NOW = int(time.time())
BOUNTY = {'id': 56, 'title': 'Usual Labs', 'payout': 16000000, 'display_currency': 'USDC', 'is_ready': True, 'live_since': 1744113073,
          'last_updated': 1744113073, 'categories': [{'name': 'Stablecoins'}],
          'severities': [{'severity_name': 'Critical', 'deposit_amount': 250, 'display_order': 1}],
          'covered_scope': [{'address': '0xe1DeE60c516a8350704Ec24a6E856c9F533d1c1b', 'network': 'ETHEREUM', 'display_name': 'EulerOracle', 'selected': True},
                            {'address': None, 'repo_name': 'usual-dao/pegasus', 'file_path': 'a.sol', 'commit_hash': 'f567a2f6', 'selected': False}]}
CONTEST = {'id': 1209, 'title': 'Aave V4', 'status': 'RUNNING', 'starts_at': NOW + 86400, 'ends_at': NOW + 864000, 'rewards': 365000, 'token': 'USDC'}
PROGRAM = {'slug': 'cetus-smart-contracts', 'title': 'Cetus Smart Contracts', 'state': 'published', 'status': {'name': 'Active'},
           'start_date': '07 Jul 2025', 'end_date': '07 Jul 2027', 'unending_program': True, 'max_bounty': '1000000.0', 'kyc_required': True,
           'labels': {'platforms': ['SUI'], 'languages': ['Move'], 'types': ['smart contract'], 'project_types': ['DEX']},
           'rewards': {'critical_min': 10000, 'critical_max': 1000000},
           'scopes': [{'title': 'Vault', 'target': 'https://etherscan.io/address/0x8e02d37b6cad86039bdd11095b8c879b907f7d10', 'criticality': 'Critical', 'out_of_scope': False},
                      {'title': 'Repo', 'target': 'https://github.com/x/y', 'criticality': 'Critical', 'out_of_scope': False},
                      {'title': 'Old', 'target': 'https://etherscan.io/address/0x0000000000000000000000000000000000000001', 'out_of_scope': True}],
           'hacker_ranks': [{'rank': 1, 'reports_amount': 10, 'reputation': 13, 'user': {'nickname': 'kafir'}}], 'hacker_ranks_count': 207}


def args(*argv):
    return cli.build_parser().parse_args(list(argv) or ['programs'])


class NormalizationTests(unittest.TestCase):
    def test_sherlock_statuses_and_dates(self):
        self.assertEqual(sherlock.status('competition', CONTEST), 'upcoming')
        self.assertEqual(sherlock.status('competition', dict(CONTEST, status='ESCALATING')), 'evaluating')
        self.assertEqual(sherlock.status('competition', dict(CONTEST, status='FINISHED')), 'finished')
        self.assertEqual(sherlock.status('program', dict(BOUNTY, is_ready=False)), 'upcoming')
        row = sherlock.normalize('program', BOUNTY)
        self.assertEqual((row['target'], row['status'], row['reward'], row['start']), ('sherlock:56', 'live', 16000000.0, '2025-04-08T11:51:13Z'))
        self.assertEqual(row['url'], 'https://audits.sherlock.xyz/bug-bounties/56')

    def test_hackenproof_statuses_and_dates(self):
        row = hackenproof.normalize('program', PROGRAM)
        self.assertEqual((row['status'], row['reward'], row['kyc'], row['start'], row['end']), ('live', 1000000.0, True, '2025-07-07T00:00:00Z', None))
        self.assertEqual(hackenproof.status(dict(PROGRAM, state='paused', status={'name': None})), 'paused')
        self.assertEqual(hackenproof.status(dict(PROGRAM, status={'name': 'Completed'})), 'finished')
        self.assertEqual(hackenproof.normalize('competition', PROGRAM)['url'], 'https://hackenproof.com/audit-programs/cetus-smart-contracts')

    def test_urls_identify_kind(self):
        self.assertEqual(sherlock.identify('competition', 'https://audits.sherlock.xyz/contests/1209', None, None), '1209')
        with self.assertRaisesRegex(LookupError, 'is a competition: bb competition sherlock:1209'):
            sherlock.identify('program', 'https://audits.sherlock.xyz/contests/1209', None, None)
        with self.assertRaisesRegex(LookupError, 'is a program: bb program hackenproof:okx'):
            hackenproof.identify('competition', 'https://hackenproof.com/programs/okx', None, None)


class FilterTests(unittest.TestCase):
    rows = [hackenproof.normalize('program', PROGRAM), hackenproof.normalize('program', dict(PROGRAM, slug='done', status={'name': 'Completed'})),
            hackenproof.normalize('program', dict(PROGRAM, slug='paused', state='paused'))]

    def ids(self, *argv):
        return [r['id'] for r in common.apply_filters(self.rows, args('programs', *argv), 'program', hackenproof.tags, 'HackenProof')]

    def test_program_status_defaults(self):
        self.assertEqual(self.ids(), ['cetus-smart-contracts'])
        self.assertEqual(self.ids('--include-paused'), ['cetus-smart-contracts', 'paused'])
        self.assertEqual(self.ids('--status', 'all'), ['cetus-smart-contracts', 'done', 'paused'])
        self.assertEqual(self.ids('--status', 'ended'), ['done'])

    def test_tag_filters_and_missing_tags(self):
        self.assertEqual(self.ids('-e', 'sui', '-l', 'move', '--project-type', 'dex'), ['cetus-smart-contracts'])
        self.assertEqual(self.ids('-l', 'solidity'), [])
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            rows = common.apply_filters([sherlock.normalize('program', BOUNTY)], args('programs', '-l', 'rust'), 'program', sherlock.tags, 'Sherlock')
        self.assertEqual(rows, [])
        self.assertIn('Sherlock has no language tags; its programs are left out', err.getvalue())

    def test_pick_by_name_with_hints(self):
        rows = [('56', 'Usual Labs', {'56', 'Usual Labs'}), ('300', 'Aave V4', {'300', 'Aave V4'})]
        self.assertEqual(common.pick('aave v4', rows, 'program', 'Sherlock'), '300')
        with self.assertRaisesRegex(ValueError, r'did you mean: Usual Labs \(56\)'):
            common.pick('usual lab', rows, 'program', 'Sherlock')


class RenderAndScopeTests(unittest.TestCase):
    def test_sherlock_page_and_network_scope(self):
        record = dict(BOUNTY, kind='program', url=sherlock.url('program', 56))
        text = sherlock.render(record, ['all'], Style(False))
        for expected in ('Max payout:', '$16,000,000 USDC', 'Critical $250 USDC', 'EulerOracle', 'Ethereum'):
            self.assertIn(expected, text)
        self.assertNotIn('pegasus', text)  # unselected repository files are not in scope
        self.assertNotIn('LEADERBOARD', text)
        self.assertEqual(sherlock.scope_assets(record), [{'url': '', 'type': 'smart_contract', 'address': '0xe1DeE60c516a8350704Ec24a6E856c9F533d1c1b',
                                                          'chain': 'ethereum', 'description': 'EulerOracle'}])

    def test_hackenproof_page_and_explorer_scope(self):
        record = dict(PROGRAM, kind='program', url=hackenproof.url('program', PROGRAM['slug']))
        text = hackenproof.render(record, ['all'], Style(False))
        for expected in ('Ends:', 'ongoing', 'Critical:', '$10,000 – $1,000,000', 'In scope (2)', 'Out of scope (1)', 'kafir', 'top 1 of 207'):
            self.assertIn(expected, text)
        self.assertEqual([a['description'] for a in hackenproof.scope_assets(record)], ['Vault'])


class CommandTests(unittest.TestCase):
    def run_bb(self, argv):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = cli.main(argv)
        return code, out.getvalue(), err.getvalue()

    def test_sites_lists_every_platform_listing(self):
        code, out, _ = self.run_bb(['sites', '--json'])
        rows = json.loads(out)
        self.assertEqual(code, 0)
        self.assertEqual({r['source'] for r in rows}, set(cli.SOURCES))
        self.assertIn({'source': 'sherlock', 'name': 'Sherlock', 'kind': 'competition', 'url': 'https://audits.sherlock.xyz/contests'}, rows)
        code, out, _ = self.run_bb(['links', 'hackenproof', '-k', 'programs'])
        self.assertIn('https://hackenproof.com/programs', out)
        self.assertNotIn('audit-programs', out)
        code, _, err = self.run_bb(['sites', 'nowhere'])
        self.assertEqual(code, 1)
        self.assertIn("Unknown platform 'nowhere'", err)

    def test_sites_open_uses_the_browser(self):
        with patch('webbrowser.open_new_tab') as opened:
            code, out, _ = self.run_bb(['sites', 'sherlock', '--open'])
        self.assertEqual(code, 0)
        self.assertEqual([c.args[0] for c in opened.call_args_list], ['https://audits.sherlock.xyz/bug-bounties', 'https://audits.sherlock.xyz/contests'])

    def test_platform_prefix_without_native_interface_selects_it(self):
        with patch('bb_cli.cli.listing', return_value=[]) as listing:
            code, out, _ = self.run_bb(['sherlock', 'programs'])
        self.assertEqual(code, 0)
        self.assertEqual([c.args[0] for c in listing.call_args_list], ['sherlock'])

    def test_served_detail_json_and_section_checks(self):
        record = dict(BOUNTY, kind='program', url='u')
        with patch.object(sherlock, 'detail', return_value=record):
            code, out, err = self.run_bb(['program', 'sherlock:56', '--json'])
            self.assertEqual(code, 0, err)
            self.assertEqual(json.loads(out)['target'], 'sherlock:56')
            code, _, err = self.run_bb(['program', 'sherlock:56', 'rules'])
            self.assertIn("Unknown section 'rules'", err)
            code, _, err = self.run_bb(['program', 'sherlock:56', '--private'])
            self.assertIn('--private is not available on Sherlock', err)

    def test_vault_sort_lists_only_platforms_with_vaults(self):
        with patch('bb_cli.cli.listing', return_value=[]) as listing:
            code, _, err = self.run_bb(['programs', '--sort', 'vault'])
        self.assertEqual(code, 0)
        self.assertEqual([c.args[0] for c in listing.call_args_list], ['immunefi'])
        self.assertIn('only Immunefi publishes vault balances', err)
        code, _, err = self.run_bb(['--source', 'sherlock', 'programs', '--sort', 'vault'])
        self.assertIn('Sherlock publishes no vault balances', err)
        code, _, err = self.run_bb(['competitions', '--sort', 'vault'])
        self.assertIn('programs only', err)

    def test_default_limit_and_hint(self):
        rows = [sherlock.normalize('program', dict(BOUNTY, id=i, payout=i)) for i in range(1, 31)]
        with patch('bb_cli.cli.listing', side_effect=lambda source, *a: rows if source == 'sherlock' else []):
            _, out, _ = self.run_bb(['programs'])
            self.assertEqual(out.count('sherlock:'), cli.DEFAULT_ROWS)
            self.assertIn('25 of 30 matching programs  (-n N or --all for more)', out)
            _, out, _ = self.run_bb(['programs', '--all'])
            self.assertEqual(out.count('sherlock:'), 30)
            self.assertNotIn('--all for more', out)
            _, out, _ = self.run_bb(['programs', '--json'])
            self.assertEqual(len(json.loads(out)), 30)

    def test_competitions_default_to_live_first(self):
        contests = [sherlock.normalize('competition', dict(CONTEST, id=1, status='FINISHED', starts_at=NOW - 10)),
                    sherlock.normalize('competition', dict(CONTEST, id=2, status='RUNNING', starts_at=NOW - 99999))]
        with patch('bb_cli.cli.listing', side_effect=lambda source, *a: contests if source == 'sherlock' else []):
            _, out, _ = self.run_bb(['competitions', '--json'])
            self.assertEqual([r['id'] for r in json.loads(out)], ['2', '1'])
            _, out, _ = self.run_bb(['competitions', '--sort', 'start', '--json'])
            self.assertEqual([r['id'] for r in json.loads(out)], ['1', '2'])

    def test_short_uuid_targets_round_trip(self):
        uid = 'f9df94db-c7b1-434b-bb06-d1360abdd1be'
        self.assertEqual(cli.short('cantina', uid), 'cantina:f9df94db')
        self.assertEqual(cli.short('immunefi', 'sky'), 'immunefi:sky')
        rows = {'program': [(uid, 'Uniswap', set()), ('f9df94aa-0000-0000-0000-000000000000', 'Other', set())], 'competition': []}
        expand = lambda value: cli.expand_prefix('cantina', value, 'program', args(), cli.Path('/tmp'))
        with patch('bb_cli.cli.candidates', side_effect=lambda source, kind, *a: rows[kind]):
            self.assertEqual(expand('f9df94db'), uid)
            with self.assertRaisesRegex(ValueError, 'matches several ids'):
                expand('f9df94')
        self.assertEqual(cli.expand_prefix('immunefi', 'sky', 'program', args(), cli.Path('/tmp')), 'sky')


class LookupTests(unittest.TestCase):
    def test_wrong_kind_names_the_right_command(self):
        rows = {'program': [('56', 'Usual Labs', {'56', 'Usual Labs'})], 'competition': [('1209', 'Aave V4', {'1209', 'Aave V4'})]}
        self.assertEqual(common.lookup('sherlock', 'Sherlock', 'program', '56', rows.get), '56')
        with self.assertRaisesRegex(LookupError, "'1209' is a Sherlock competition: bb competition sherlock:1209"):
            common.lookup('sherlock', 'Sherlock', 'program', '1209', rows.get)
        with self.assertRaisesRegex(ValueError, 'No Sherlock competition named'):
            common.lookup('sherlock', 'Sherlock', 'competition', 'nothing', rows.get)

    def test_scope_explorers_for_new_platform_chains(self):
        from bb_cli.evm.explorers import Registry
        registry = Registry([])
        for url, chain in (('https://explorer.cronos.com/address/0x5C7F8A570d578ED84E63fdFA7b1eE72dEae1AE23', 25),
                           ('https://cronos.org/explorer/address/0x5C7F8A570d578ED84E63fdFA7b1eE72dEae1AE23', 25),
                           ('https://explorer.zkevm.cronos.org/address/0xDA59Cc2Ee08ce5F830B1190bfed34415772Ddc57', 388),
                           ('https://explorer.mainnet.citrea.xyz/address/0x3100000000000000000000000000000000000002', 4114)):
            self.assertEqual(registry.for_url(url).id, chain)
            self.assertTrue(registry.for_url(url).public_rpcs)


if __name__ == '__main__':
    unittest.main()
