#!/usr/bin/env python3
import pathlib,yaml,json,sys,subprocess
from jsonschema import Draft202012Validator
root=pathlib.Path(__file__).resolve().parents[1]
ops=yaml.safe_load((root/'contracts/api/operation-registry.yaml').read_text())['operations']
policies=yaml.safe_load((root/'contracts/runtime/code-runtime-policy.yaml').read_text())['operations']
assert len(ops)==65 and len(policies)==13
assert {x['operation_id'] for x in policies}=={x['operation_id'] for x in ops if x['domain']=='code'}
for p in (root/'contracts/events/code').glob('*.schema.json'):Draft202012Validator.check_schema(json.loads(p.read_text()))
assert len(list((root/'contracts/events/code').glob('*.schema.json')))==3
res=subprocess.run([sys.executable,'-m','unittest','discover','-s',str(root/'tests'),'-p','test_phase5_runtime.py','-v'],cwd=root)
if res.returncode:sys.exit(res.returncode)
print('PASS Phase 5 semantic reference model; NOT production closure')
