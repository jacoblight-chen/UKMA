import sys,unittest,tempfile
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'scripts'))
from reference_adapter import Adapter,Error
class TestPhase6(unittest.TestCase):
 def setUp(self):
  self.temp=tempfile.TemporaryDirectory();self.path=str(Path(self.temp.name)/'state.json');self.store=Adapter(self.path);self.scope={'tenant':'t','project':'p'};self.item={'id':'x','tenant':'t','project':'p','revision':'commit1','name':'run'}
 def tearDown(self):self.temp.cleanup()
 def test_persist_restart(self):
  self.assertEqual(self.store.put(self.scope,self.item,'k')['etag'],1)
  self.assertEqual(Adapter(self.path).get(self.scope,'x')['name'],'run')
 def test_idempotency(self):
  self.store.put(self.scope,self.item,'k');self.store.put(self.scope,self.item,'k');self.assertEqual(len(self.store.pending()),1)
 def test_idempotency_payload_conflict(self):
  self.store.put(self.scope,self.item,'k')
  with self.assertRaisesRegex(Error,'IDEMPOTENCY_CONFLICT'):self.store.put(self.scope,dict(self.item,name='changed'),'k')
 def test_etag(self):
  self.store.put(self.scope,self.item,'k')
  with self.assertRaisesRegex(Error,'ETAG_CONFLICT'):self.store.put(self.scope,dict(self.item,name='changed'),'k2',etag=7)
  self.assertEqual(self.store.put(self.scope,dict(self.item,name='changed'),'k2',etag=1)['etag'],2)
 def test_tenant_read_isolation(self):
  self.store.put(self.scope,self.item,'k')
  with self.assertRaisesRegex(Error,'NOT_FOUND'):self.store.get({'tenant':'other','project':'p'},'x')
 def test_tenant_write_isolation(self):
  with self.assertRaisesRegex(Error,'DENIED'):self.store.put({'tenant':'other','project':'p'},self.item,'k')
 def test_outbox_replay(self):
  self.store.put(self.scope,self.item,'k');self.assertEqual(len(Adapter(self.path).pending()),1)
  self.store.ack(1);self.assertEqual(len(Adapter(self.path).pending()),0)
 def test_repeated_ack(self):
  self.store.put(self.scope,self.item,'k');self.store.ack(1);self.store.ack(1);self.assertEqual(len(self.store.pending()),0)
if __name__=='__main__':unittest.main()
