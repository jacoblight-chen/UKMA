import unittest,sys,pathlib,json,yaml
from jsonschema import Draft202012Validator
ROOT=pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'scripts'))
from code_runtime_model import Model,ContractError

class TestRuntimeContract(unittest.TestCase):
    def setUp(self):self.m=Model()
    def error(self,code,fn):
        with self.assertRaises(ContractError) as cm:fn()
        self.assertEqual(cm.exception.code,code)
    def test_policy_all_13_and_scope(self):
        ops=yaml.safe_load((ROOT/'contracts/runtime/code-runtime-policy.yaml').read_text())['operations']
        self.assertEqual(len(ops),13)
        self.assertEqual(len({o['operation_id'] for o in ops}),13)
        for o in ops:
            self.m.authorize({o['required_scope']},o['required_scope'],'projectA','projectA')
            self.error('FORBIDDEN',lambda o=o:self.m.authorize(set(),o['required_scope'],'projectA','projectA'))
            self.error('NOT_FOUND',lambda o=o:self.m.authorize({o['required_scope']},o['required_scope'],'projectA','projectB'))
    def test_replay_and_accept_event(self):
        a=self.m.accept('j1','idempotency1','alice','projectA')
        self.assertIs(a,self.m.accept('j999','idempotency1','alice','projectA'))
        self.assertEqual(len(self.m.events),1)
    def test_cancel_before_commit(self):
        self.m.accept('j1','k','alice','p')
        self.assertEqual(self.m.cancel('j1','p','alice').commit,'not_committed')
        self.assertEqual(len(self.m.events),2)
        self.error('JOB_NOT_CANCELLABLE',lambda:self.m.cancel('j1','p','alice'))
    def test_cancel_after_commit_denied(self):
        self.m.accept('j1','k','alice','p');self.m.commit('j1','receipt')
        self.error('JOB_NOT_CANCELLABLE',lambda:self.m.cancel('j1','p','alice'))
        self.assertEqual(self.m.jobs['j1'].commit,'committed')
    def test_projection_failure_retains_commit(self):
        self.m.accept('j1','k','alice','p');self.m.commit('j1','r');j=self.m.finish_projection('j1',False)
        self.assertEqual((j.execution,j.commit,j.projection),('completed','committed','failed'))
    def test_unknown_commit_not_cancelled(self):
        self.m.accept('j1','k','alice','p');self.m.jobs['j1'].commit='unknown'
        self.error('JOB_NOT_CANCELLABLE',lambda:self.m.cancel('j1','p','alice'))
    def test_index_generation_fail_closed(self):
        self.error('INDEX_NOT_READY',lambda:self.m.require_generation('p',2))
        self.m.projections['p']=2;self.m.require_generation('p',2)
    def test_etag_and_idempotent_stale(self):
        self.error('ETAG_MISMATCH',lambda:self.m.invalidate('sha','7','key'))
        self.assertEqual(self.m.invalidate('sha','0','key'),'1')
        self.assertEqual(self.m.invalidate('sha','0','key'),'1')
        self.assertEqual(len(self.m.events),1)
    def test_graph_acl_no_hidden_relationship(self):
        edges=[{'id':'e1','src':'A','dst':'B'},{'id':'e2','src':'A','dst':'C'}]
        self.assertEqual([e['id'] for e in self.m.filter_edges(edges,{'A','B'},{'e1','e2'})],['e1'])
    def test_payload_positive_negative_for_all_3(self):
        root=ROOT/'contracts/events/code'
        for p in sorted(root.glob('*.schema.json')):
            schema=json.loads(p.read_text());Draft202012Validator.check_schema(schema)
            data={k:'value' for k in schema['required']}
            validator=Draft202012Validator(schema)
            self.assertTrue(validator.is_valid(data),p.name)
            for missing in schema['required']:
                invalid={k:v for k,v in data.items() if k!=missing}
                self.assertFalse(validator.is_valid(invalid),(p.name,missing))
            self.assertFalse(validator.is_valid({**data,'unexpected':1}))
    def test_event_dedup(self):
        payload={'operation_id':'op1'}
        self.assertTrue(self.m.publish('event','entity',payload))
        self.assertFalse(self.m.publish('event','entity',payload))
if __name__=='__main__':unittest.main()
