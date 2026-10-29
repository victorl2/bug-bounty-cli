import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch
from bb_cli.providers.cantina.client import Client, CantinaError, firefox_cookie, page_queries
from bb_cli.providers.cantina.cli import assets, identifier, main

class Tests(unittest.TestCase):
    def test_pagination(self):
        c=Client();calls=[]
        def get(path,query):
            calls.append(query.copy())
            return {'items':[{'id':len(calls)}], 'nextValue':'next' if len(calls)==1 else None}
        c.get=get
        self.assertEqual(len(c.opportunities()),2)
        self.assertEqual(calls[1]['next'],'next')
    def test_repeated_cursor(self):
        c=Client();c.get=lambda *a,**k:{'items':[],'nextValue':'x'}
        with self.assertRaises(CantinaError): c.opportunities()
    def test_scope_exclusions(self):
        self.assertTrue(assets({'assetGroups':[{'outOfScope':True,'subGroups':[{'assets':[{'name':'excluded'}]}]}]})[0]['outOfScope'])
        self.assertEqual(assets({'assetGroups':[{'hidden':True,'assets':[{}]}]}),[])
    def test_url(self):
        uid='f9df94db-c7b1-434b-bb06-d1360abdd1be'
        self.assertEqual(identifier('https://cantina.xyz/bounties/'+uid),uid)
        with self.assertRaises(CantinaError): identifier('https://evil.test/bounties/'+uid)
    def test_page(self):
        data={'queryKey':['infiniteQuery','opportunities.listOpportunities'], 'state':{'data':{'pages':[{'items':[]}]}}}
        flight='1:'+json.dumps(data)+'\n'
        html='<script>self.__next_f.push('+json.dumps([1,flight])+')</script>'
        self.assertIn('opportunities.listOpportunities',page_queries(html))
    def test_firefox_domain_filter(self):
        with tempfile.TemporaryDirectory() as d:
            db=sqlite3.connect(Path(d)/'cookies.sqlite')
            db.execute('CREATE TABLE moz_cookies(host,name,value,expiry)')
            db.executemany('INSERT INTO moz_cookies VALUES(?,?,?,?)',[('.cantina.xyz','session','test',9999999999),('evil.test','secret','exclude',9999999999)])
            db.commit();db.close()
            self.assertEqual(firefox_cookie(d),'session=test')
    def test_cache(self):
        with tempfile.TemporaryDirectory() as d:
            c=Client(d)
            import io
            with patch('bb_cli.providers.cantina.client.urlopen',return_value=io.BytesIO(b'{"items":[]}')):
                self.assertEqual(c.get('/api/v0/opportunities'),{'items':[]})
            self.assertEqual(Client(d,offline=True).get('/api/v0/opportunities'),{'items':[]})
            with self.assertRaises(CantinaError): Client(d,offline=True,cookie='test').get('/api/v0/opportunities')
    def test_auth_not_cached(self):
        with tempfile.TemporaryDirectory() as d:
            import io
            with patch('bb_cli.providers.cantina.client.urlopen',return_value=io.BytesIO(b'{}')): Client(d,cookie='session=test').get('/api/v0/auth/me')
            self.assertEqual(list(Path(d).iterdir()),[])

if __name__=='__main__': unittest.main()

class Regressions(unittest.TestCase):
    def test_api_kind_serialization(self):
        c=Client();calls=[]
        c.get=lambda path,q: calls.append(q.copy()) or {'items':[]}
        c.opportunities(['public_bounty','private_bounty'])
        self.assertEqual(calls[0]['kind'],'public_bounty,private_bounty')
    def test_redacted_names(self):
        import contextlib, io
        with patch.object(Client,'opportunities',return_value=[{'id':'private','name':None,'company':None,'totalRewardPot':None}]):
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(['programs','--sort','name','--json']),0)
    def test_authenticated_public_route(self):
        import contextlib, io
        with patch.object(Client,'detail',return_value={'id':'test'}) as get:
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(['program','f9df94db-c7b1-434b-bb06-d1360abdd1be','--json']),0)
            self.assertFalse(get.call_args.args[1])
    def test_contract_exclusions(self):
        from bb_cli.providers.cantina.contracts import scope
        url='https://etherscan.io/address/0x'+'1'*40
        record={'assetGroups':[{'outOfScope':True,'assets':[{'reference':url}]},{'assets':[{'reference':url},{'reference':url}]}]}
        self.assertEqual(len(scope(record)['contracts']),1)
    def test_redirect_origin(self):
        from bb_cli.providers.cantina.client import SameOriginRedirect
        from urllib.request import Request
        with self.assertRaises(CantinaError):
            SameOriginRedirect().redirect_request(Request('https://api.cantina.xyz/api/v0/auth/me'),None,302,'',{},'https://example.com')
