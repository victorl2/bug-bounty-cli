import contextlib
import io
import json
import tempfile
import unittest
from unittest.mock import patch
from bb_cli.providers.cantina.client import Client, CantinaError
from bb_cli.providers.cantina.cli import main, parser, resolve_target, section
from bb_cli.providers.cantina.discovery import apply, status
from bb_cli.providers.cantina.contracts import make_client

UID='11111111-1111-1111-1111-111111111111'
class ParityTests(unittest.TestCase):
    def test_catalog_fallback_for_listing(self):
        c=Client()
        with patch.object(c,'get',side_effect=CantinaError('down')),patch.object(c,'catalog',return_value=[{'id':UID,'status':'live','kind':'public_bounty'}]):
            self.assertEqual(c.opportunities(['public_bounty'])[0]['id'],UID)
            self.assertTrue(c.warnings)
    def test_no_auth_denial_fallback(self):
        c=Client()
        with patch.object(c,'get',side_effect=CantinaError('denied',403)),patch.object(c,'catalog') as catalog:
            with self.assertRaises(CantinaError): c.opportunities()
            catalog.assert_not_called()
    def test_detail_enrichment(self):
        c=Client()
        with patch.object(c,'get',return_value={'id':UID,'name':'Current'}),patch.object(c,'catalog',return_value=[{'id':UID,'name':'Old','commitHash':'pinned'}]):
            r=c.detail(UID)
            self.assertEqual(r['name'],'Current');self.assertEqual(r['commitHash'],'pinned')
    def test_detail_fallback(self):
        c=Client()
        with patch.object(c,'get',side_effect=CantinaError('down')),patch.object(c,'catalog',return_value=[{'id':UID,'name':'Fallback'}]):
            self.assertEqual(c.detail(UID)['name'],'Fallback')
    def test_name_ambiguity(self):
        c=Client()
        with patch.object(c,'catalog',return_value=[{'id':UID,'name':'same'},{'id':'other','name':'same'}]):
            with self.assertRaisesRegex(CantinaError,'Ambiguous'): resolve_target(c,'same')
    def test_unpublished_vs_access_denied_leaderboard(self):
        c=Client()
        with patch.object(c,'get',side_effect=CantinaError('unpublished',404)):
            self.assertFalse(c.leaderboard(UID)['available'])
        with patch.object(c,'get',side_effect=CantinaError('denied',403)):
            with self.assertRaises(CantinaError): c.leaderboard(UID)
    def test_date_advancement(self):
        self.assertEqual(status({'status':'live','timeframe':{'start':'2000-01-01T00:00:00Z','end':'2001-01-01T00:00:00Z'}}),'evaluating')
    def test_language_evidence_and_unknowns(self):
        args=parser().parse_args(['programs','-l','Solidity','--kyc','yes'])
        rows=[{'id':UID,'instructions':'Scope: src/Vault.sol','kycRequired':True},{'id':'unknown','instructions':'hello','kycRequired':True}]
        self.assertEqual([r['id'] for r in apply(rows,args)],[UID])
    def test_unsupported_domain_filters(self):
        args=parser().parse_args(['programs','--project-type','Defi'])
        with self.assertRaisesRegex(CantinaError,'does not expose'): apply([{}],args)
    def test_sort_and_reverse(self):
        args=parser().parse_args(['competitions','--sort','end','--reverse'])
        rows=[{'id':'later','timeframe':{'end':'2030'}},{'id':'earlier','timeframe':{'end':'2020'}}]
        self.assertEqual(apply(rows,args)[0]['id'],'earlier')
    def test_contract_cli_is_native(self):
        args=parser().parse_args(['contract','scope',UID,'--no-live','--download','/tmp/example','--limit','1','--offline'])
        self.assertTrue(args.no_live);self.assertEqual(args.limit,1)
        with tempfile.TemporaryDirectory() as d:
            args.cache_dir=d
            with patch('bb_cli.providers.cantina.client.Client.detail',return_value={'id':UID,'name':'N','assetGroups':[{'outOfScope':True,'assets':[{'reference':'https://etherscan.io/address/0x'+'1'*40}]}]}):
                self.assertEqual(make_client(args).detail('program',UID).data['assets'],[])
    def test_competition_resources_and_scope_keep_pinned_source(self):
        r={'gitRepoUrl':'https://github.com/a/b','commitHash':'123','instructions':'Scope: Vault.sol; docs https://example.org/docs'}
        self.assertEqual(section(r,'scope')['commitHash'],'123')
        self.assertIn('https://example.org/docs',section(r,'resources')['instructionLinks'])

if __name__=='__main__': unittest.main()

class ResilienceTests(unittest.TestCase):
    def test_transient_api_retry(self):
        from urllib.error import HTTPError
        with tempfile.TemporaryDirectory() as d:
            with patch('bb_cli.providers.cantina.client.urlopen',side_effect=[HTTPError('https://api.cantina.xyz',429,'rate limit',{},None),io.BytesIO(b'{}')]),patch('bb_cli.providers.cantina.client.time.sleep') as sleep:
                self.assertEqual(Client(d).get('/api/v0/leaderboard'),{})
                sleep.assert_called_once()
    def test_malformed_response_uses_catalog(self):
        c=Client()
        with patch.object(c,'get',return_value={'changed':'schema'}),patch.object(c,'catalog',return_value=[{'id':UID,'kind':'public_bounty','status':'live'}]):
            self.assertEqual(len(c.opportunities()),1)
    def test_source_page_fallback(self):
        c=Client()
        flight='1:'+json.dumps({'competition':{'id':UID,'name':'Page fallback'}})+'\n'
        html='<script>self.__next_f.push('+json.dumps([1,flight])+')</script>'
        def get(path,*a,**kwargs):
            if kwargs.get('site'): return html
            raise CantinaError('down')
        with patch.object(c,'get',side_effect=get),patch.object(c,'catalog',side_effect=CantinaError('down')):
            self.assertEqual(c.detail(UID)['name'],'Page fallback')
    def test_scope_block_forwarded_to_rpc_batch(self):
        from bb_cli.evm.onchain import OnChain
        from bb_cli.evm.explorers import Chain
        from unittest.mock import Mock
        engine=OnChain(Mock())
        node=Mock();node.batch.return_value=['0x1234']
        with patch.object(engine,'node',return_value=node) as getnode:
            self.assertEqual(engine.code_sizes(Mock(),['0x'+'1'*40],21000000)['0x'+'1'*40],2)
            self.assertEqual(getnode.call_args.args[1],21000000)
            self.assertEqual(node.batch.call_args.args[0][0][1][1],hex(21000000))
