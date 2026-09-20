import copy, hashlib, json, re, subprocess
from pathlib import Path
ROOT=Path("/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap-transition")
P=ROOT/"AUTHORITY-CHANGE-PLAN-2026-09-20-v9.json"; M=ROOT/"AUTHORITY-CHANGE-PLAN-2026-09-20-v9.md"; V9=ROOT/"authority-contract-separation-2026-09-20/v9"
plan=json.loads(P.read_text()); pub=plan["main_b_publisher"]; checks=[]; findings=[]
def sha(p): return hashlib.sha256(p.read_bytes()).hexdigest()
def chk(i,ok,d):
    checks.append({"id":i,"pass":bool(ok),"detail":d})
    if not ok: findings.append({"id":i,"detail":d})
def leaves(n,p=""):
    props=n.get("properties") if isinstance(n,dict) else None
    if not isinstance(props,dict): return [p] if p else []
    out=[]
    for k,v in props.items():
        q=f"{p}.{k}" if p else k; out.extend(leaves(v,q) or [q])
    return out
chk("schema",plan["schema"]=="velnor.authority-change-plan.v9" and plan["status"]=="successor_draft_external_blocked",plan["status"])
chk("not-authorized",plan["execution_authorized"] is False and plan["mutation_performed"] is False,plan["execution_authorized"])
chk("markdown-hash",plan["plan_markdown"]["sha256"]==sha(M),plan["plan_markdown"])
cur=plan["evidence"]["current_main"]; facts=plan["revision_bound_facts"]
chk("current-main-one-tuple",facts["main_sha"]==cur["sha"]=="89f82dd8b287f46a3cf4c0920f341f6ca6c736db" and facts["tree_sha"]==cur["tree_sha"]=="22ccc1daf9d55bd12d9a58e6652fe92cf3cb9416",cur)
hist=plan["evidence"]["historical_base_snapshot"]
chk("historical-separated",hist["sha"]==facts["main_parent_sha"] and hist["role"].startswith("historical"),hist)
expected=["capture-caller-context","build-linux-x64","artifact-verify","reserve-release","attest-binding","publish","attest-release","pre-record-upload","verify-B"]
needs=[["build-linux-x64","capture-caller-context"],["artifact-verify","build-linux-x64"],["reserve-release","artifact-verify"],["attest-binding","reserve-release"],["publish","attest-binding"],["attest-release","publish"],["pre-record-upload","attest-release"],["verify-B","pre-record-upload"]]
chk("job-graph",pub["job_graph"]==expected,pub["job_graph"]); chk("needs",pub["needs_edges_consumer_producer"]==needs,pub["needs_edges_consumer_producer"])
chk("caller-upper-bound",set(pub["caller_permissions_upper_bound"])=={"actions:write","contents:write","attestations:write","id-token:write","checks:read"},pub["caller_permissions_upper_bound"])
chk("caller-no-reexport",pub["ci_main_graph"]["caller_job"]["outputs_declared"] is False and pub["ci_main_graph"]["caller_job"]["shell_steps"] is False,pub["ci_main_graph"]["caller_job"])
dag=pub["field_availability_dag"]; stages=[x["stage"] for x in dag]; outs={x["stage"]:{v.split(":",1)[0] for v in x["produces"]} for x in dag}
chk("stage-order",stages==["S0","S1","S2","S3","S4","S5","S6","S7a","S7b"],stages)
chk("s7-external-inputs","external_inputs" in dag[-1] and set(v.split(":",1)[0] for v in dag[-1]["external_inputs"])<=outs["S7b"],dag[-1])
prov=pub["field_provenance"]
chk("unique-provenance",all(isinstance(v,dict) and {"stage","producer","source","required"}<=set(v) and " or " not in v["producer"] for v in prov.values()),"every field has exactly one producer")
chk("canonical-146",len(pub["canonical_leaf_paths"]["permanent_binding"])==146,len(pub["canonical_leaf_paths"]["permanent_binding"]))
chk("canonical-typed",all({"stage","producer","source","type","required"}<=set(v) for v in pub["canonical_leaf_paths"]["permanent_binding"].values()),"146 mapped leaves")
jobs=pub["jobs"]; jout={k:{v.split(":",1)[0] for v in x["outputs"]} for k,x in jobs.items()}; jin={k:{v.split(":",1)[0] for v in x["consumes"]} for k,x in jobs.items()}
errs=[]
for e in pub["field_lineage_edges"]:
    p,c=e["producer"],e["consumer"]; fs={v.split(":",1)[0] for v in e["fields"]}
    if p in jout and not fs<=jout[p]: errs.append((p,c,"producer",sorted(fs-jout[p])))
    if c in jin and not fs<=jin[c]: errs.append((p,c,"consumer",sorted(fs-jin[c])))
    if e.get("orientation")!="producer_outputs_to_consumer_inputs": errs.append((p,c,"orientation"))
chk("lineage-oriented-typed",not errs,errs)
external={v.split(":",1)[0] for v in pub["field_lineage_edges"][-1]["fields"]}; inter=jout["verify-B"]&jin["verify-B"]
chk("verify-no-self-produced-input",inter<=external,{"intersection":sorted(inter),"external":sorted(external)})
chk("unique-s7-producers",all(" or " not in str(v["producer"]) for v in prov.values()),"no producer alternatives")
rm=json.loads((V9/"canonical-root-manifest.v2.json").read_text())
chk("root-current",rm["current_main"]["sha"]==facts["main_sha"] and rm["current_main"]["tree_sha"]==facts["tree_sha"],rm["current_main"])
for key,file in {"pre_record":"policy-validator-b-pre-record.v2.schema.json","provider_result":"policy-validator-b-provider-result.v1.schema.json","release_manifest":"policy-validator-b-release-manifest.v1.schema.json","tree_b_adoption":"validator-pin-adoption.v1.schema.json"}.items():
    f=V9/file; chk("schema-"+key,pub["canonical_schemas"][key]["sha256"]==sha(f),{"expected":pub["canonical_schemas"][key]["sha256"],"actual":sha(f)})
chk("historical-excluded",pub["canonical_schemas"]["historical_predicate_excluded"]["role"].startswith("historical") and "forbidden" in pub["canonical_schemas"]["historical_predicate_excluded"]["role"],pub["canonical_schemas"]["historical_predicate_excluded"])
chk("product",pub["product"]["product_id"]=="velnor-workflow-policy-validator" and pub["product"]["asset"]=="velnor-workflow-policy-validator-Linux-X64",pub["product"])
def validate(n,s,p=""):
    e=[]
    if "const" in s and n!=s["const"]: e.append((p,"const"))
    t=s.get("type")
    if t=="object":
        if not isinstance(n,dict): return [(p,"object")]
        e += [(p+"."+k,"required") for k in s.get("required",[]) if k not in n]
        if s.get("additionalProperties") is False: e += [(p+"."+k,"additional") for k in n if k not in s.get("properties",{})]
        for k,v in s.get("properties",{}).items():
            if k in n: e += validate(n[k],v,p+"."+k)
    elif t=="array":
        if not isinstance(n,list): return [(p,"array")]
        e += [(p,"minItems") for _ in [0] if len(n)<s.get("minItems",0)]
        for i,v in enumerate(n): e += validate(v,s.get("items",{}),f"{p}[{i}]")
    elif t=="string":
        if not isinstance(n,str): e.append((p,"string"))
        elif "pattern" in s and re.fullmatch(s["pattern"],n) is None: e.append((p,"pattern"))
    elif t=="integer":
        if not isinstance(n,int) or isinstance(n,bool) or n<s.get("minimum",-2**63): e.append((p,"integer"))
    elif t=="boolean" and not isinstance(n,bool): e.append((p,"boolean"))
    return e
pre=json.loads((V9/"positive-pre-record.json").read_text()); provider=json.loads((V9/"positive-provider-result.json").read_text())
pre_s=json.loads((V9/"policy-validator-b-pre-record.v2.schema.json").read_text()); provider_s=json.loads((V9/"policy-validator-b-provider-result.v1.schema.json").read_text())
chk("positive-pre-record",not validate(pre,pre_s),"strict pre-record")
chk("positive-provider-result",not validate(provider,provider_s),"strict provider result")
def norm(f):
    x=json.loads(f.read_text()); x["canonical_root_manifest_sha256"]="0"*64
    return hashlib.sha256((json.dumps(x,sort_keys=True,separators=(",",":"))+"\n").encode()).hexdigest()
bound={x["path"]:x["sha256"] for x in rm["bound_files"]}
chk("positive-root-binding",pre["canonical_root_manifest_sha256"]==pub["canonical_schemas"]["root_manifest"]["sha256"]==provider["canonical_root_manifest_sha256"],{"pre":pre["canonical_root_manifest_sha256"],"root":pub["canonical_schemas"]["root_manifest"]["sha256"]})
chk("positive-normalized-hashes",bound["G1/bootstrap-transition/authority-contract-separation-2026-09-20/v9/positive-pre-record.json"]==norm(V9/"positive-pre-record.json") and bound["G1/bootstrap-transition/authority-contract-separation-2026-09-20/v9/positive-provider-result.json"]==norm(V9/"positive-provider-result.json"),"root-bound normalized fixture hashes")
hostile=json.loads((V9/"hostile-transport-fixtures.json").read_text())
chk("hostile-fixture-set",len(hostile["fixtures"])>=15 and hostile["expected"]=="reject",len(hostile["fixtures"]))
fixture=[ROOT/"v9-executable-fixture/ci-main-caller.yml",ROOT/"v9-executable-fixture/ci-policy-validator-products.yml"]
act=subprocess.run(["actionlint",*map(str,fixture)],text=True,capture_output=True)
chk("actionlint",act.returncode==0,act.stderr.strip() or "exit 0")
imm=pub["release_protocol"]["immutable_setting"]
chk("immutable-release-contract",imm["get_endpoint"]=="/repos/{owner}/{repo}/immutable-releases" and imm["expected_status"]==200 and imm["expected_enabled"] is True,imm)
chk("provider-freeze-blocked",any(x["id"]=="provider_freeze" for x in plan["hard_blockers"]) and pub["external_checks_app"]["status"].startswith("unproven"),plan["hard_blockers"])
chk("native-negative",facts["current_checkpoint"]["runtime_runner"]=="macos-26" and facts["current_checkpoint"]["runtime_accepted"] is False,facts["current_checkpoint"])
result={"schema":"velnor.authority-change-plan.v9.audit.v1","plan_sha256":sha(P),"markdown_sha256":sha(M),"checks":checks,"check_count":len(checks),"passed":sum(1 for x in checks if x["pass"]),"findings":findings,"structural_pass":not findings,"external_blockers":["target generator/B source output absent","provider App identities/credential/verifier unresolved","provider freeze/recovery excluding actor 5 unproven","real verifier/native xcode-27 proof absent"],"status":"structural_audit_pass_external_blocked" if not findings else "structural_audit_failed","execution_authorized":False}
print(json.dumps(result,indent=2)+"\n")
