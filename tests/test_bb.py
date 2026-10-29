import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from bb_cli import cli, sources
from bb_cli.adapters import make_evm_client
from bb_cli.paths import cache_root
from bb_cli.evm.client import FetchError

IMM={'slug':'same','project':'Same','maxBounty':100,'kyc':False}
CAN={'id':'same','name':'Same','status':'live','totalRewardPot':'200','currencyCode':'USDC','kycRequired':True}

class UnifiedTests(unittest.TestCase):
    def run_bb(self,argv):
        output=io.StringIO();err=io.StringIO()
        with contextlib.redirect_stdout(output),contextlib.redirect_stderr(err):code=cli.main(argv)
        return code,output.getvalue(),err.getvalue()
    def test_default_aggregate_globally_limits(self):
        def listing(source,kind,args,path):
            return [sources.normalize(source,kind,IMM if source=='immunefi' else CAN)]
        with patch('bb_cli.cli.listing',side_effect=listing):
            code,out,err=self.run_bb(['programs','--limit','1','--json'])
        self.assertEqual(code,0,err);rows=json.loads(out)
        self.assertEqual(rows[0]['source'],'cantina');self.assertEqual(rows[0]['raw'],CAN)
    def test_same_ids_keep_both_sources(self):
        def listing(source,kind,args,path):return [sources.normalize(source,kind,IMM if source=='immunefi' else CAN)]
        with patch('bb_cli.cli.listing',side_effect=listing):
            code,out,_=self.run_bb(['programs','--json'])
        self.assertEqual(len(json.loads(out)),2)
    def test_explicit_source_and_namespaced_cache(self):
        with tempfile.TemporaryDirectory() as root,patch('bb_cli.cli.listing',return_value=[]) as listing:
            code,out,err=self.run_bb(['--source','cantina','programs','--cache-dir',root,'--json'])
            self.assertEqual(code,0,err);self.assertEqual(listing.call_args.args[3],Path(root)/'cantina')
            self.assertEqual(listing.call_count,1)
    def test_source_selection_anywhere(self):
        for argv in [['--source','cantina','programs'],['programs','--source=cantina']]:
            self.assertEqual(cli.extract_source(argv),('cantina',['programs']))
    def test_target_prefix_and_url_inference(self):
        self.assertEqual(cli.target_source('cantina:Uniswap'),('cantina','Uniswap'))
        self.assertEqual(cli.target_source('https://immunefi.com/bug-bounty/sky/information/')[0],'immunefi')
        with self.assertRaises(ValueError):cli.target_source('cantina:Uniswap','immunefi')
    def test_failed_provider_never_prints_partial_success(self):
        def listing(source,*args):
            if source=='cantina':raise FetchError('down')
            return [sources.normalize('immunefi','program',IMM)]
        with patch('bb_cli.cli.listing',side_effect=listing):
            code,out,err=self.run_bb(['programs','--json'])
        self.assertEqual(code,1);self.assertEqual(out,'');self.assertIn('no partial',err)
    def test_native_provider_passthrough(self):
        with patch.object(cli.PROVIDERS['cantina'],'main',return_value=0) as native:
            self.assertEqual(cli.main(['cantina','auth','--firefox-profile','auto']),0)
            native.assert_called_once_with(['auth','--firefox-profile','auto'])
    def test_json_detail_keeps_provider_provenance(self):
        def native(argv):print(json.dumps({'id':'uuid','instructions':'rules'}));return 0
        with patch.object(cli.PROVIDERS['cantina'],'main',side_effect=native):
            code,out,err=self.run_bb(['program','cantina:Uniswap','all','--json'])
        self.assertEqual(code,0,err);self.assertEqual(json.loads(out)['source'],'cantina')
        self.assertEqual(json.loads(out)['data']['instructions'],'rules')
    def test_shared_scope_routing_and_cache(self):
        with tempfile.TemporaryDirectory() as root,patch('bb_cli.cli.contract_cli.cmd_scope',return_value=0) as scope:
            code,out,err=self.run_bb(['contract','scope','cantina:Uniswap','--no-live','--cache-dir',root])
            self.assertEqual(code,0,err);args=scope.call_args.args[0]
            self.assertEqual(args.program,'Uniswap');self.assertEqual(args.cache_dir,Path(root)/'cantina')
            self.assertEqual(args.evm_cache_dir,Path(root)/'evm')
    def test_shared_engine_has_one_module_identity(self):
        import immunefi_cli.onchain
        import bb_cli.evm.onchain
        self.assertIs(immunefi_cli.onchain,bb_cli.evm.onchain)
    def test_cache_report_and_selective_clear(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)
            for source in ['cantina','immunefi','evm']:
                (root/source).mkdir();(root/source/'test.json').write_text(json.dumps({'fetched_at':0,'data':{},'source':'https://example.com'}))
            code,out,err=self.run_bb(['cache','--cache-dir',d,'--json'])
            self.assertEqual(len(json.loads(out)['entries']),3)
            code,out,err=self.run_bb(['cache','clear','--source','cantina','--cache-dir',d,'--json'])
            self.assertEqual(json.loads(out)['removed'],1)
            self.assertTrue((root/'evm/test.json').exists())
    def test_invalid_source_and_flags(self):
        for argv in [['--source','other','programs'],['programs','--refresh','--offline'],['programs','--limit','0']]:
            code,out,_=self.run_bb(argv);self.assertEqual(code,1);self.assertEqual(out,'')
    def test_active_program_filter_uses_normalized_live_status(self):
        args=cli.build_parser().parse_args(['programs','--status','active'])
        with patch('bb_cli.sources.ImmunefiClient') as client:
            client.return_value.listing.return_value.data=[IMM]
            rows=sources.listing('immunefi','program',args,Path('/tmp/test-bb-no-writes'))
        self.assertEqual(len(rows),1);self.assertEqual(rows[0]['status'],'live')
    def test_legacy_cantina_engine_is_same_module(self):
        import cantina_cli._evm.onchain as legacy
        from bb_cli.evm import onchain
        self.assertIs(legacy,onchain)
    def test_normalization_distinguishes_unknown_from_false_kyc(self):
        self.assertIs(sources.normalize('immunefi','program',IMM)['kyc'],False)
        self.assertIsNone(sources.normalize('cantina','program',{'id':'x'})['kyc'])
        self.assertEqual(sources.normalize('immunefi','program',IMM)['status'],'live')

if __name__=='__main__':unittest.main()
