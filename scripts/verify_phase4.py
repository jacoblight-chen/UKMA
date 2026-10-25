#!/usr/bin/env python3
import json,pathlib,yaml
from jsonschema import Draft202012Validator,FormatChecker
from referencing import Registry,Resource
root=pathlib.Path(__file__).resolve().parents[1]
sdir=root/'contracts/schema/v1'
schemas={p.name:json.loads(p.read_text()) for p in sdir.glob('*.json')}
reg=Registry()
for s in schemas.values():
 Draft202012Validator.check_schema(s)
 reg=reg.with_resource(s['$id'],Resource.from_contents(s))
ops=yaml.safe_load((root/'contracts/api/operation-registry.yaml').read_text())['operations']
code=[x for x in ops if x['domain']=='code'];assert len(code)==13 and len(ops)==65
for op in code:
 name=op['operation_id'];s=schemas[name+'.request.schema.json']
 v=Draft202012Validator(s,registry=reg,format_checker=FormatChecker())
 for valid in (True,False):
  f=root/'tests/phase4'/f"{name}.{'valid' if valid else 'invalid'}.json"
  assert v.is_valid(json.loads(f.read_text()))==valid,(name,valid)
 response=schemas[name+'.response.schema.json'];assert '$ref' in response['properties']['data'] or response['properties']['data']['type']=='object'
report=json.loads((root/'reports/phase4-code-closure.json').read_text())
assert report['fully_closed']==0 and len(report['rows'])==13
print('PASS Phase 4: 65 API retained; 13 Code API typed request/response candidates; 26 positive/negative request fixtures; 0 Code APIs runtime-closed')
