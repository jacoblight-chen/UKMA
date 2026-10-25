import pathlib, json, yaml
from jsonschema import Draft202012Validator, RefResolver
ROOT=pathlib.Path(__file__).resolve().parents[1]
REG=yaml.safe_load((ROOT/'contracts/api/operation-registry.yaml').read_text())
SHARED=json.loads((ROOT/'contracts/schema/v1/shared.schema.json').read_text())
STORE={SHARED['$id']: SHARED}
def test_exact_operation_set():
 assert len(REG['operations'])==65
 assert {d:sum(o['domain']==d for o in REG['operations']) for d in ['code','document','memory','traceability']}=={'code':13,'document':14,'memory':25,'traceability':13}
def test_schema_structure():
 for o in REG['operations']:
  for suffix in ('request','response'):
   sch=json.loads((ROOT/f"contracts/schema/v1/{o['operation_id']}.{suffix}.schema.json").read_text())
   Draft202012Validator.check_schema(sch)
def test_request_envelope_invalid_operation():
 op=REG['operations'][0]
 sch=json.loads((ROOT/op['request_schema']).read_text())
 ctx={'request_id':'a','trace_id':'b','scope':{'tenant_id':'t','project_id':'p'},'principal_id':'u','scopes':['code:read'],'consistency':'eventual'}
 valid={'api_version':'1','operation':'ukma.'+op['operation_id']+'.v1','context':ctx,'input':{'revision':{},'parser_profile':'x','include_globs':[],'exclude_globs':[],'async':True}}
 resolver=RefResolver(base_uri=sch['$id'],referrer=sch,store=STORE)
 v=Draft202012Validator(sch,resolver=resolver)
 assert v.is_valid(valid) # structural ONLY; nested semantic typing is not yet closed
 assert not v.is_valid({**valid,'operation':'ukma.code.WRONG.v1'})
 assert not v.is_valid({**valid,'bad_field':True})
 assert not v.is_valid({**valid,'context':{**ctx,'consistency':'impossible'}})
def test_no_false_look_of_closure():
 assert all(o['closure_status']=='provisional' and o['blockers'] for o in REG['operations'])
