import hashlib
import json
from pathlib import Path

ROOT = Path('/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap-transition')
PLAN = ROOT / 'AUTHORITY-CHANGE-PLAN-2026-09-20-v8.json'
MD = ROOT / 'AUTHORITY-CHANGE-PLAN-2026-09-20-v8.md'
plan = json.loads(PLAN.read_text())
pub = plan['main_b_publisher']
checks = []
findings = []

def check(name, ok, detail):
    checks.append({'id': name, 'pass': bool(ok), 'detail': detail})
    if not ok:
        findings.append({'id': name, 'detail': detail})

check('schema-status', plan.get('schema') == 'velnor.authority-change-plan.v8' and plan.get('status') == 'successor_draft_external_blocked', plan.get('status'))
check('not-authorized', plan.get('execution_authorized') is False and plan.get('mutation_performed') is False, 'execution_authorized and mutation_performed must both be false')
check('markdown-hash', plan['plan_markdown']['sha256'] == hashlib.sha256(MD.read_bytes()).hexdigest(), plan['plan_markdown'])
check('current-main', plan['revision_bound_facts']['main_sha'] == '89f82dd8b287f46a3cf4c0920f341f6ca6c736db', plan['revision_bound_facts']['main_sha'])
check('current-tree', plan['revision_bound_facts']['tree_sha'] == '22ccc1daf9d55bd12d9a58e6652fe92cf3cb9416', plan['revision_bound_facts']['tree_sha'])

expected_jobs = ['build-linux-x64','artifact-verify','reserve-release','attest-binding','publish','attest-release','record-upload','verify-B']
check('job-graph', pub['job_graph'] == expected_jobs, pub['job_graph'])
expected_edges = [['artifact-verify','build-linux-x64'],['reserve-release','artifact-verify'],['attest-binding','reserve-release'],['publish','attest-binding'],['attest-release','publish'],['record-upload','attest-release'],['verify-B','record-upload']]
check('needs-edges', pub['needs_edges'] == expected_edges, pub['needs_edges'])
check('no-legacy-attest-node', 'attest' not in pub['jobs'] and 'attest' not in pub['job_graph'], list(pub['jobs']))

edge_map = {dst: src for dst, src in pub['needs_edges']}
for job in expected_jobs:
    declared = pub['jobs'][job]['needs']
    expected = [] if job == expected_jobs[0] else [edge_map[job]]
    check(f'needs:{job}', declared == expected, {'declared': declared, 'expected': expected})

stage_dag = pub['field_availability_dag']
stages = [x['stage'] for x in stage_dag]
check('stages', stages == [f'S{i}' for i in range(8)], stages)
stage_fields = {x['stage']: {v.split(':',1)[0] for v in x['produces']} for x in stage_dag}
all_fields = set().union(*stage_fields.values())
final_fields = {v.split(':',1)[0] for v in pub['transport_fields']}
check('field-output-coverage', final_fields <= all_fields, sorted(final_fields - all_fields))
prov = pub['field_provenance']
check('field-provenance-complete', set(prov) == final_fields and all(isinstance(v, dict) and {'stage','producer','source','required'} <= set(v) for v in prov.values()), {'missing': sorted(final_fields - set(prov)), 'nonobjects': [k for k,v in prov.items() if not isinstance(v,dict)]})
edge_fields = {(e['from_job'],e['to_job']): {v.split(':',1)[0] for v in e['fields']} for e in pub['field_lineage_edges']}
check('lineage-edge-count', len(edge_fields) == len(expected_edges), len(edge_fields))
check('lineage-no-empty', all(edge_fields.values()), 'every edge must transport typed fields')

schemas = pub['canonical_schemas']
check('strict-record-schema', schemas['record_provenance']['sha256'] == '3f2d72acda359e8ca0a31a40a6e4aa417bd49552acec050583a92601d0c851ef', schemas['record_provenance'])
check('canonical-path-map', len(schemas['canonical_leaf_paths']) >= 100 and all(isinstance(v,dict) and {'stage','producer','source','required'} <= set(v) for v in schemas['canonical_leaf_paths'].values()), len(schemas['canonical_leaf_paths']))
record = pub['record_contract']
check('record-raw-handoff', record['record_artifact_transport']['producer'] == 'record-upload' and record['record_artifact_transport']['upload_response'] == 'Actions artifact REST response' and record['record_artifact_transport']['not_in_binding_preimage'] is True, record['record_artifact_transport'])
check('record-no-self-preimage', record['record_ids_excluded_from_preimage'] is True, record)

parts = pub['attestation_contract']['stage_partition']
check('binding-preimage', set(parts['S4_binding']['preimage_stages']) == {'S0','S1','S2','S3'} and 'release_asset_id' in parts['S4_binding']['excludes'] and 'record_artifact_id' in parts['S4_binding']['excludes'], parts['S4_binding'])
check('release-preimage', set(parts['S6_release']['preimage_stages']) == {'S0','S1','S2','S3','S4','S5'} and 'release_asset_id' in parts['S6_release']['requires'] and 'release_attestation_id' in parts['S6_release']['excludes'], parts['S6_release'])
check('record-preimage', set(parts['S7_record']['preimage_stages']) == {'S0','S1','S2','S3','S4','S5','S6'} and 'record_artifact_id' in parts['S7_record']['excludes'], parts['S7_record'])
graph = pub['attestation_contract']['digest_graph']
idx = {x:i for i,x in enumerate(graph['nodes'])}
check('digest-graph-acyclic', all(idx[a] < idx[b] for a,b in graph['edges']) and graph['no_future_or_self_preimage'] is True, graph)
check('oidc-role-separation', pub['attestation_contract']['trust']['caller_claims'] == ['workflow_ref','workflow_sha'] and pub['attestation_contract']['trust']['called_claims'] == ['job_workflow_ref','job_workflow_sha'], pub['attestation_contract']['trust'])

check('event-guard', pub['event_guard']['publisher_trigger'] == 'workflow_call only' and pub['event_guard']['standalone_push'] is False and pub['event_guard']['caller_job_condition'] == "github.event_name == 'push' && github.ref == 'refs/heads/main'", pub['event_guard'])
check('caller-no-outputs', pub['ci_main_graph']['caller_job']['outputs_declared'] is False and pub['ci_main_graph']['caller_job']['shell_steps'] is False, pub['ci_main_graph']['caller_job'])
check('policy-direct-output', pub['called_workflow_outputs_source'].startswith('verify-B outputs directly'), pub['called_workflow_outputs_source'])
check('permissions-isolated', pub['jobs']['build-linux-x64']['permissions'] == ['contents:read','actions:read'] and pub['jobs']['publish']['permissions'] == ['contents:write'] and 'id-token:write' not in pub['jobs']['build-linux-x64']['permissions'], {k:v['permissions'] for k,v in pub['jobs'].items()})
check('terminal-census-excludes-self', set(pub['upstream_terminal_census']['excludes']) == {'verify-B own job/check','caller run wrapper','Policy job/check','external Policy-bootstrap-B check'}, pub['upstream_terminal_census'])

blockers = [x['id'] for x in plan['hard_blockers']]
check('provider-blocked', 'v8_freeze' in blockers and pub['external_checks_app']['status'] == 'unproven external capability blocker', blockers)
check('native-negative', plan['evidence']['current_runtime']['runner'] == 'macos-26' and plan['evidence']['current_runtime']['accepted'] is False, plan['evidence']['current_runtime'])
check('v7-preserved-input', plan['supersedes']['preserved'] is True and plan['evidence']['v7_canonical_dag']['status'].startswith('independent rejection'), plan['supersedes'])

def is_valid(p):
    q = p['main_b_publisher']
    s = q['attestation_contract']['stage_partition']
    return all([
        'release_asset_id' in s['S4_binding']['excludes'],
        'record_artifact_id' in s['S4_binding']['excludes'],
        'release_asset_id' in s['S6_release']['requires'],
        'release_attestation_id' in s['S6_release']['excludes'],
        all(isinstance(v, dict) for v in q['field_provenance'].values()),
        q['record_contract'].get('strict_schema') is True,
        bool(q['record_contract'].get('provider_handoff')),
        q['attestation_contract']['trust']['caller_claims'] == ['workflow_ref','workflow_sha'],
        'verify-B own job/check' in q['upstream_terminal_census']['excludes'],
        q['record_contract'].get('record_ids_excluded_from_preimage') is True,
        {'release_asset_field','immutable_tag_field'} <= set(q['product']),
        q['external_checks_app'].get('credential_source') != 'GITHUB_TOKEN',
        len(q['canonical_schemas']['canonical_leaf_paths']) >= 100,
    ])

def mutate(name, p):
    q = p['main_b_publisher']
    if name == 'binding-future-release-asset': q['attestation_contract']['stage_partition']['S4_binding']['excludes'].remove('release_asset_id')
    elif name == 'binding-record-id': q['attestation_contract']['stage_partition']['S4_binding']['excludes'].remove('record_artifact_id')
    elif name == 'release-before-asset': q['attestation_contract']['stage_partition']['S6_release']['requires'].remove('release_asset_id')
    elif name == 'generic-provenance': q['field_provenance']['source_sha'] = 'S1/S2/S3 or API'
    elif name == 'record-schema-omitted': q['record_contract']['strict_schema'] = False
    elif name == 'record-provider-omitted': q['record_contract']['provider_handoff'] = []
    elif name == 'oidc-role-reversal': q['attestation_contract']['trust']['caller_claims'] = ['job_workflow_ref','job_workflow_sha']
    elif name == 'terminal-self-cycle': q['upstream_terminal_census']['excludes'].remove('verify-B own job/check')
    elif name == 'record-artifact-self-preimage': q['record_contract']['record_ids_excluded_from_preimage'] = False
    elif name == 'product-asset-tag-omitted': q['product'].pop('release_asset_field', None)
    elif name == 'provider-token-impersonation': q['external_checks_app']['credential_source'] = 'GITHUB_TOKEN'
    elif name == 'canonical-path-unmapped': q['canonical_schemas']['canonical_leaf_paths'] = {}

negative_cases = ['binding-future-release-asset','binding-record-id','release-before-asset','generic-provenance','record-schema-omitted','record-provider-omitted','oidc-role-reversal','terminal-self-cycle','record-artifact-self-preimage','product-asset-tag-omitted','provider-token-impersonation','canonical-path-unmapped']
check('hostile-baseline-valid', is_valid(plan), 'baseline v8 contract must validate')
for name in negative_cases:
    mutated = json.loads(json.dumps(plan))
    mutate(name, mutated)
    check(f'negative:{name}', not is_valid(mutated), 'hostile mutation must be rejected')

external_blockers = [
    'no provider-enforced freeze/recovery excluding bypass actor 5',
    'target generator revision is null and B source/output are not current-main artifacts',
    'external B App/provider identities and credentials are unresolved',
    'real verifier and live record-artifact acceptance are unimplemented',
    'current native run is macos-26 and xcode-27 proof is absent',
]
result = {'schema':'velnor.authority-change-plan.v8.audit.v1','plan_sha256':hashlib.sha256(PLAN.read_bytes()).hexdigest(),'markdown_sha256':hashlib.sha256(MD.read_bytes()).hexdigest(),'checks':checks,'check_count':len(checks),'passed':sum(1 for x in checks if x['pass']),'findings':findings,'structural_pass':not findings,'external_blockers':external_blockers,'status':'structural_audit_pass_external_blocked' if not findings else 'structural_audit_failed','execution_authorized':False}
print(json.dumps(result, indent=2) + '\n')
