#!/usr/bin/env python3
"""Phase 3: verify schema reference resolution, positive/negative fixtures, and semantic release gate."""
import pathlib,json,yaml,sys,argparse
from jsonschema import Draft202012Validator,FormatChecker
from referencing import Registry,Resource
r=pathlib.Path(__file__).resolve().parents[1]
schema_dir=r/'contracts/schema/v1'
schemas={p.name:json.loads(p.read_text()) for p in schema_dir.glob('*.json')}
registry=Registry()
for s in schemas.values():registry=registry.with_resource(s['$id'],Resource.from_contents(s))
passed=0
for op in ['code.get_symbol','code.get_analysis_job']:
 schema=schemas[op+'.request.schema.json']
 v=Draft202012Validator(schema,registry=registry,format_checker=FormatChecker())
 for valid in [True,False]:
  suffix='valid' if valid else 'invalid'
  case=json.loads((r/f'tests/fixtures/{op}.{suffix}.json').read_text())
  assert v.is_valid(case)==valid,(op,suffix,list(v.iter_errors(case)))
  passed+=1
schema=json.loads((r/'contracts/events/event-envelope.schema.json').read_text())
v=Draft202012Validator(schema,format_checker=FormatChecker())
for valid in [True,False]:
 suffix='valid' if valid else 'invalid'
 case=json.loads((r/f'tests/fixtures/event-envelope.{suffix}.json').read_text())
 assert v.is_valid(case)==valid,(suffix,list(v.iter_errors(case)))
 passed+=1
report=json.loads((r/'reports/semantic-closure-report.json').read_text())
ops=yaml.safe_load((r/'contracts/api/operation-registry.yaml').read_text())['operations']
evs=yaml.safe_load((r/'contracts/events/event-registry.yaml').read_text())['events']
assert report['operations']==len(ops)==65
assert len(report['rows'])==65
assert report['events_unclosed']==len(evs)
assert {x['operation_id'] for x in report['rows']}=={x['operation_id'] for x in ops}
assert all(x['status']=='NOT_CLOSED' for x in report['rows'])
print(f'PASS: {passed} positive/negative JSON Schema fixtures; 65 operation audit rows; {len(evs)} event records')
print(f"SEMANTIC CLOSURE: {report['fully_closed']}/65, request provisional {report['request_provisional_count']}, response provisional {report['response_provisional_count']}")
p=argparse.ArgumentParser();p.add_argument('--strict',action='store_true');args=p.parse_args()
if args.strict and (report['fully_closed']<65 or report['events_unclosed']):
 print('RELEASE BLOCKED: field-level schema/event/authorization/transaction closure incomplete',file=sys.stderr)
 sys.exit(2)
