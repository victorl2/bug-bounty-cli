import unittest
from bb_cli.evm.render import Style
from bb_cli.providers.cantina.detail import detail

BOUNTY = {
    'id': 'b1', 'name': 'Example', 'kind': 'public_bounty', 'status': 'live', 'url': 'https://cantina.xyz/bounties/b1',
    'currencyCode': 'USDC', 'totalRewardPot': '1000000', 'submissionFee': '0.00', 'kycRequired': True,
    'timeframe': {'start': '2024-01-01T00:00:00Z', 'end': None}, 'consideredSeverities': ['critical', 'high'],
    'gitRepoUrl': 'https://github.com/example/repo', 'commitHash': 'abcdef0123456789',
    'instructions': 'Read https://docs.example.com first.',
    'assetGroups': [
        {'name': 'Contracts', 'outOfScope': False, 'rewards': [
            {'severity': 'high', 'maxReward': '50000.00'}, {'severity': 'critical', 'maxReward': '1000000.00'},
            {'severity': 'low', 'maxReward': '0'}],
         'assets': [{'name': 'Core', 'description': 'https://github.com/example/core', 'reference': ''},
                    {'name': 'Vault', 'description': '- Mainnet: `0x00000000219ab540356cBB839Cbe05303d7705Fa`', 'reference': ''},
                    {'name': 'Hidden', 'hidden': True, 'reference': 'x'}]},
        {'name': 'Excluded', 'outOfScope': True, 'description': 'Third-party code', 'assets': []},
    ],
}
COMPETITION = {
    'id': 'c1', 'name': 'Contest', 'kind': 'public_contest', 'status': 'complete', 'currencyCode': 'USDC',
    'totalRewardPot': '400000', 'timeframe': {'start': '2026-05-29T12:00:00Z', 'end': '2026-06-12T20:00:00Z'},
    'gitRepoUrl': 'https://github.com/example/contest', 'commitHash': '7538c438513622',
    'leaderboard': {'available': True, 'data': {'ranking': [
        {'position': 1, 'team': {'members': [{'username': 'alice'}]}, 'reward': '6500.00', 'highFindings': 0},
        {'position': 2, 'team': {'name': 'Team B'}, 'reward': '5000.00', 'highFindings': 0}]}},
}


class CantinaDetailTests(unittest.TestCase):
    def test_overview_scope_and_resources(self):
        text = detail(BOUNTY, ['all'], Style(False))
        self.assertRegex(text, r'Max reward:\s+\$1,000,000 USDC')
        self.assertNotIn('Submission fee', text)  # zero fee is omitted
        self.assertIn('Critical $1,000,000 USDC · High $50,000 USDC', text)  # severity order, zero rewards dropped
        self.assertIn('https://github.com/example/core', text)  # link-only description becomes the target
        self.assertIn('Mainnet: 0x00000000219ab540356cBB839Cbe05303d7705Fa', text)
        self.assertNotIn('Hidden', text)
        self.assertIn('Out of scope', text)
        self.assertIn('https://docs.example.com', text)
        self.assertNotIn('LEADERBOARD', text)  # bounties have none

    def test_competition_without_assets_and_leaderboard(self):
        text = detail(COMPETITION, ['scope', 'leaderboard'], Style(False))
        self.assertIn('Contest  ', text)  # header still shown without overview
        self.assertIn('repository README describes the scope', text)
        self.assertIn('alice', text)
        self.assertIn('Team B', text)
        self.assertNotIn('HIGH', text)  # all-zero finding columns are dropped


if __name__ == '__main__':
    unittest.main()
