#!/usr/bin/env python3
import pathlib,json,yaml,sys,argparse
from jsonschema import Draft202012Validator, FormatChecker
r=pathlib.Path(__file__).resolve().parents[1]
reg=yaml.safe_load((r/'contracts/api/operation-registry.yaml').read_text())['operations']
mat=yaml.safe_load((r/'contracts/matrices/api-contract-matrix.yaml').read_text())['operations']
evs=yaml.safe_load((r/'contracts/events/event-registry.yaml').read_text())['events']
mcp=json.loads((r/'contracts/mcp/tool-manifest.json').read_text())['tools']
assert len(reg)==len(mat)==len(mcp)==65
ids={o['operation_id'] for o in reg}
assert len(ids)==65
assert ids=={o['operation_id'] for o in mat}=={o['operation_id'] for o in mcp}
assert len(evs)==len({e['event_type'] for e in evs})
shared=json.loads((r/'contracts/schema/v1/shared.schema.json').read_text())
for f in (r/'contracts/schema/v1').glob('*.json'):
 Draft202012Validator.check_schema(json.loads(f.read_text()))
for f in (r/'contracts/events').glob('*.json'):
 Draft202012Validator.check_schema(json.loads(f.read_text()))
ref=shared['$defs']['Ref']
validator=Draft202012Validator({'$defs':shared['$defs'],**ref},format_checker=FormatChecker())
assert validator.is_valid(json.loads((r/'contracts/fixtures/shared-ref.valid.json').read_text()))
assert not validator.is_valid(json.loads((r/'contracts/fixtures/shared-ref.invalid-missing-revision.json').read_text()))
assert all(not t['schema_closed'] for t in mcp)
assert all(e['payload_status']=='UNRESOLVED' for e in evs)
not_closed=[o for o in reg if o['closure_status']!='closed' or o['blockers']]
print(f'PASS: 65 operations, {len(evs)} event types, {len(mcp)} MCP mappings, shared Ref fixtures validated')
print(f'FULL CONTRACT CLOSURE: {65-len(not_closed)}/65; event payloads 0/{len(evs)} closed')
parser=argparse.ArgumentParser();parser.add_argument('--strict',action='store_true');args=parser.parse_args()
if args.strict and (not_closed or any(e['payload_status']!='CLOSED' for e in evs)):
 print('RELEASE BLOCKED: unresolved operation or event payload contracts',file=sys.stderr)
 sys.exit(2)
