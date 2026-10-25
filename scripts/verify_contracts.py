#!/usr/bin/env python3
import argparse, pathlib, yaml, json, sys
from jsonschema import Draft202012Validator
root=pathlib.Path(__file__).resolve().parents[1]
reg=yaml.safe_load((root/'contracts/api/operation-registry.yaml').read_text())
ops=reg['operations']; ids=[o['operation_id'] for o in ops]
counts={d:sum(o['domain']==d for o in ops) for d in ['code','document','memory','traceability']}
assert counts=={'code':13,'document':14,'memory':25,'traceability':13},counts
assert len(ids)==len(set(ids))==65
assert len(set(o['test_id'] for o in ops))==65
schemas=list((root/'contracts/schema/v1').glob('*.json'))
assert len(schemas)==131,len(schemas)
for f in schemas:
 Draft202012Validator.check_schema(json.loads(f.read_text()))
for o in ops:
 for k in ['request_schema','response_schema']:
  assert (root/o[k]).exists(),(o['operation_id'],k)
assert all(o['source_section'] in ('32.2','33.2','34.2','35.2') for o in ops)
print('PASS: catalog=65, test IDs=65 unique, 131 JSON Schemas valid as Draft 2020-12')
print('Domain counts:',counts)
not_closed=[o['operation_id'] for o in ops if o['closure_status']!='closed' or o['blockers']]
print('Fully closed:',65-len(not_closed),'of 65')
parser=argparse.ArgumentParser(); parser.add_argument('--strict',action='store_true');args=parser.parse_args()
if args.strict and not_closed:
 print('RELEASE BLOCKED: provisional operation contracts:',len(not_closed),file=sys.stderr)
 sys.exit(2)
