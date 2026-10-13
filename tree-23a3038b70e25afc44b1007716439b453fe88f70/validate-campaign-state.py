#!/usr/bin/env python3
"""Validate coordination records locally; never query or mutate live systems.

Requires Python >= 3.10 and jsonschema >= 4.22, < 5.
Exit 0: consistent records, including honest pending/blocked records.
Exit 1: invalid records. Exit 2: unavailable dependency or input.
"""

import argparse
import hashlib
import json
import re
import sys
from datetime import date, datetime
from pathlib import Path
from urllib.parse import urlsplit

try:
    from jsonschema import Draft202012Validator, FormatChecker
    from referencing import Registry, Resource
except ImportError:
    print("ERROR: install jsonschema>=4.22,<5 in an isolated Python environment.", file=sys.stderr)
    raise SystemExit(2)


STATE_NAMES = (
    "campaign-state", "preview-lock", "hosts", "repository-rollout", "evidence-index",
)
SCHEMA_BASE = "https://velnor.invalid/schemas/"
REPOSITORIES = (
    "donbeave/essential-mac", "ChainArgos/jackin-agent-brown",
    "ChainArgos/cloudflare-tofu", "ChainArgos/github-terraform",
    "ChainArgos/java-monorepo",
)
COMPONENTS = (
    "product-source", "generator", "runtime-linux-amd64", "runtime-linux-arm64",
    "homebrew-macos-arm64", "apt-debian-amd64", "native-job-image",
    "scaleset-reference", "official-runner-image", "dind-image",
)
FORMATS = FormatChecker()
SECRET_VALUE = re.compile(
    r"-----BEGIN (?:[A-Z]+ )*PRIVATE KEY-----|"
    r"\bgh[pousr]_[A-Za-z0-9]{20,}|\bgithub_pat_[A-Za-z0-9_]{20,}|"
    r"\bxox[baprs]-[A-Za-z0-9-]{15,}|\bBearer\s+[A-Za-z0-9._~+/-]{12,}",
    re.IGNORECASE,
)
SECRET_KEY = re.compile(
    r"(?:access_?token|refresh_?token|github_?token|password|private_?key|"
    r"client_?secret|authorization|secret|token)", re.IGNORECASE,
)


@FORMATS.checks("date-time", raises=(ValueError, TypeError))
def valid_timestamp(value):
    if not isinstance(value, str):
        return True
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    return parsed.tzinfo is not None


@FORMATS.checks("date", raises=(ValueError, TypeError))
def valid_date(value):
    return not isinstance(value, str) or bool(date.fromisoformat(value))


def strict_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON object key")
        result[key] = value
    return result


def reject_constant(_value):
    raise ValueError("non-finite JSON number")


def read_json(path):
    return json.loads(
        path.read_text(encoding="utf-8"),
        object_pairs_hook=strict_object,
        parse_constant=reject_constant,
    )


def walk(value, path=""):
    yield path, value
    if isinstance(value, dict):
        for key, child in value.items():
            yield from walk(child, f"{path}/{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from walk(child, f"{path}/{index}")


def secret_errors(value, name):
    errors = []
    for path, item in walk(value):
        if isinstance(item, dict) and any(SECRET_KEY.fullmatch(key) for key in item):
            errors.append(f"{name}{path}: credential-value field is forbidden")
        if isinstance(item, str):
            suspect = bool(SECRET_VALUE.search(item))
            for url in re.findall(r"https?://[^\s]+", item):
                parsed = urlsplit(url)
                suspect |= bool(parsed.username or parsed.password)
                suspect |= any(
                    SECRET_KEY.fullmatch(part.split("=", 1)[0])
                    for part in parsed.query.split("&")
                )
            if suspect:
                errors.append(f"{name}{path}: possible secret; value suppressed")
    return errors


def load_bundle(root):
    schemas = {
        name: read_json(root / "schemas" / f"{name}.schema.json")
        for name in ("common", *STATE_NAMES)
    }
    documents = {
        name: read_json(root / "state" / f"{name}.json") for name in STATE_NAMES
    }
    return documents, schemas


def schema_errors(documents, schemas):
    errors = []
    resources = []
    for name, schema in schemas.items():
        expected_id = f"{SCHEMA_BASE}{name}.schema.json"
        if not isinstance(schema, dict) or schema.get("$id") != expected_id:
            errors.append(f"schemas/{name}: unexpected schema identity")
            continue
        try:
            Draft202012Validator.check_schema(schema)
            resources.append((expected_id, Resource.from_contents(schema)))
        except Exception:
            errors.append(f"schemas/{name}: invalid Draft 2020-12 schema")
    if errors:
        return errors
    # Default Registry retrieval refuses unknown resources. No network resolver.
    registry = Registry().with_resources(resources)
    for name, document in documents.items():
        validator = Draft202012Validator(
            schemas[name], registry=registry, format_checker=FORMATS,
        )
        try:
            for error in validator.iter_errors(document):
                location = "/".join(map(str, error.absolute_path))
                errors.append(f"state/{name}.json/{location}: violates {error.validator}")
        except Exception:
            errors.append(f"state/{name}.json: unresolved or invalid local schema reference")
    return errors


def semantic_errors(documents, root):
    errors = []
    campaign = documents["campaign-state"]
    preview = documents["preview-lock"]
    rollout = documents["repository-rollout"]
    index = documents["evidence-index"]

    def check(condition, message):
        if not condition:
            errors.append(message)

    def keyed(rows, label, field="id"):
        result = {}
        for row in rows:
            key = row[field]
            check(key not in result, f"{label}: duplicate {field}")
            result[key] = row
        return result

    sources = keyed(index["sources"], "sources")
    evidence = keyed(index["evidence"], "evidence")
    gates = keyed(campaign["gates"], "gates")
    blockers = keyed(campaign["blockers"], "blockers")
    hosts = keyed(documents["hosts"]["hosts"], "hosts")
    repos = keyed(rollout["repositories"], "repositories")
    components = keyed(preview["components"], "preview components")
    source_lines = {}
    recorded_at = campaign["recorded_at"]
    for name, document in documents.items():
        check(document["recorded_at"] == recorded_at, f"{name}: inconsistent recorded_at")
        errors.extend(secret_errors(document, name))

    root = root.resolve()
    for source in sources.values():
        try:
            path = (root / source["path"]).resolve(strict=True)
            check(path.is_relative_to(root), "source: path escapes repository")
            if not path.is_relative_to(root):
                continue
            raw = path.read_bytes()
            check(hashlib.sha256(raw).hexdigest() == source["sha256"],
                  f"{source['id']}: source digest changed; reconcile evidence before updating")
            source_lines[source["id"]] = len(raw.decode("utf-8").splitlines())
        except (OSError, UnicodeError, ValueError):
            errors.append(f"{source['id']}: source unavailable")
        if source["captured_at"]:
            check(datetime.fromisoformat(source["captured_at"].replace("Z", "+00:00"))
                  <= datetime.fromisoformat(recorded_at.replace("Z", "+00:00")),
                  f"{source['id']}: capture is after record time")

    for item in evidence.values():
        source = sources.get(item["source_id"])
        check(source is not None, f"{item['id']}: unknown source")
        if source:
            check(item["status"] == source["status"],
                  f"{item['id']}: evidence/source status mismatch")
            check(1 <= item["line_start"] <= item["line_end"]
                  <= source_lines.get(item["source_id"], 0),
                  f"{item['id']}: invalid source line range")
        check(not (item["observed_at"] and item["observed_on"]),
              f"{item['id']}: conflicting observation precision")
        if item["observed_on"]:
            check(item["timezone"] is not None, f"{item['id']}: date requires timezone")
        if item["observed_at"]:
            check(datetime.fromisoformat(item["observed_at"].replace("Z", "+00:00"))
                  <= datetime.fromisoformat(recorded_at.replace("Z", "+00:00")),
                  f"{item['id']}: observation is after record time")

    for name, document in documents.items():
        for path, item in walk(document):
            if not isinstance(item, dict):
                continue
            for field in ("evidence_refs", "historical_evidence_refs", "known_evidence_refs"):
                for evidence_id in item.get(field, []):
                    check(evidence_id in evidence, f"{name}{path}: unknown evidence reference")
                    if field == "historical_evidence_refs" and evidence_id in evidence:
                        check(evidence[evidence_id]["status"] == "historical",
                              f"{name}{path}: historical reference is not historical")
            if "evidence_refs" in item and item.get("status") in ("observed", "historical"):
                check(bool(item["evidence_refs"]), f"{name}{path}: missing evidence")
                check(all(evidence.get(ref, {}).get("status") == item["status"]
                          for ref in item["evidence_refs"]),
                      f"{name}{path}: historical/observed evidence cannot be promoted or relabeled")

    def qualified_record(record, subject):
        if record["status"] != "observed":
            return
        check(any(evidence.get(ref, {}).get("kind") == "qualification"
                  and evidence[ref]["subject"] == subject
                  and evidence[ref]["status"] == "observed"
                  for ref in record.get("evidence_refs", [])),
              f"{subject}: observed qualification requires matching qualification evidence")

    check(campaign["objective_evidence_ref"] in evidence, "campaign: missing objective source")
    for field in ("canonical_plan", "resumption_handoff"):
        check((root / campaign[field]).is_file(), f"campaign: missing {field}")
    gate_order = [f"G{i}" for i in range(9)]
    check(list(gates) == gate_order, "gates: require ordered G0 through G8")
    for i, gate in enumerate(campaign["gates"]):
        expected = [] if i == 0 else [f"G{i - 1}"]
        check(gate["depends_on"] == expected, f"{gate['id']}: invalid gate dependencies")
        for blocker_id in gate["blocker_ids"]:
            check(blocker_id in blockers, f"{gate['id']}: unknown blocker")
            if blocker_id in blockers:
                check(gate["id"] in blockers[blocker_id]["gate_ids"],
                      f"{gate['id']}: blocker link is not reciprocal")
        qualified_record(gate, gate["id"])
        if gate["status"] == "observed":
            check(bool(gate["author"]) and bool(gate["verifier"])
                  and gate["author"] != gate["verifier"],
                  f"{gate['id']}: separate actual author and verifier required")
            check(not gate["blocker_ids"], f"{gate['id']}: unresolved blockers")
            check(all(gates.get(dep, {}).get("status") == "observed"
                      for dep in gate["depends_on"]), f"{gate['id']}: unmet dependency")
    for blocker in blockers.values():
        for gate_id in blocker["gate_ids"]:
            check(gate_id in gates and blocker["id"] in gates[gate_id]["blocker_ids"],
                  f"{blocker['id']}: missing reciprocal gate link")
    remaining = [gate["id"] for gate in campaign["gates"] if gate["status"] != "observed"]
    check(bool(remaining) and campaign["next_gate"] == remaining[0],
          "campaign: next_gate must be earliest unverified gate")
    if campaign["status"] == "observed":
        check(not remaining and not blockers, "campaign: observed completion has unresolved gates")

    check(set(hosts) == {"local-mac", "bastion"}, "hosts: expected both execution hosts")
    for host in hosts.values():
        keyed(host["facts"], f"{host['id']} facts", "key")
        qualified_record(host["readiness"], f"host:{host['id']}")
    check(set(components) == set(COMPONENTS), "preview: missing required component")
    for component in components.values():
        source = component["source_commit"]
        check(source is None or not any(source.startswith(prefix)
              for prefix in preview["excluded_source_prefixes"]),
              f"preview/{component['id']}: forbidden stale source")
        qualified_record(component, f"preview:{component['id']}")
        if component["status"] == "observed":
            check(all(component[field] for field in ("version", "source_commit", "sha256")),
                  f"preview/{component['id']}: incomplete immutable identity")
    if preview["status"] == "observed":
        check(all(item["status"] == "observed" for item in components.values()),
              "preview: unresolved component")
        inputs = preview["observed_inputs"]
        check(inputs["preview_run"]["run_state"] == "completed"
              and inputs["preview_run"]["conclusion"] == "success"
              and inputs["preview_run"]["attempt"] is not None,
              "preview: successful completed run and attempt are required")
        check(inputs["preview_run"]["source_commit"] == inputs["main"]["source_commit"]
              == components.get("product-source", {}).get("source_commit"),
              "preview: run, main and selected source identities differ")
        digest = hashlib.sha256(json.dumps(
            preview["components"], sort_keys=True, separators=(",", ":"),
        ).encode()).hexdigest()
        check(preview["lock_id"] == digest, "preview: lock digest does not match components")
    else:
        check(preview["lock_id"] is None, "preview: unresolved lock must not have a lock identity")
    run = preview["observed_inputs"]["preview_run"]
    check((run["run_state"] == "completed") == (run["conclusion"] is not None),
          "preview: run state and conclusion disagree")

    check(list(repos) == list(REPOSITORIES), "rollout: incorrect consumer order")
    for i, repo in enumerate(rollout["repositories"]):
        check(repo["order"] == i + 1, "rollout: incorrect ordinal")
        check(repo["depends_on"] == ([] if i == 0 else [REPOSITORIES[i - 1]]),
              f"{repo['id']}: invalid consumer dependency")
        for stage, record in repo["lifecycle"].items():
            qualified_record(record, f"repo:{repo['id']}:{stage}")
            if record["status"] == "observed":
                check(repo["source_commit"] is not None, f"{repo['id']}: missing source identity")
        for host_id, record in repo["qualification"].items():
            qualified_record(record, f"repo:{repo['id']}:{host_id}")
            if record["status"] != "observed":
                continue
            check(repo["source_commit"] is not None, f"{repo['id']}: missing source identity")
            check(preview["status"] == "observed", f"{repo['id']}: preview is not locked")
            check(hosts.get(host_id, {}).get("readiness", {}).get("status") == "observed",
                  f"{repo['id']}: host readiness is not qualified")
            if i:
                check(rollout["repositories"][i - 1]["qualification"][host_id]["status"]
                      == "observed", f"{repo['id']}: previous consumer is not qualified")
            if host_id == "bastion":
                check(all(r["qualification"]["local-mac"]["status"] == "observed"
                          for r in repos.values()), "bastion replay: incomplete Mac rollout")
                check(gates.get("G6", {}).get("status") == "observed",
                      "bastion replay: G6 is not qualified")
        if repo["lifecycle"]["qualified"]["status"] == "observed":
            check(all(r["status"] == "observed" for r in repo["qualification"].values()),
                  f"{repo['id']}: both hosts must qualify")
    return errors


def validate(documents, schemas, root):
    errors = schema_errors(documents, schemas)
    if not errors:
        errors = semantic_errors(documents, root)
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1],
                        help="coordination checkout (default: script's repository)")
    args = parser.parse_args()
    try:
        documents, schemas = load_bundle(args.root)
        errors = validate(documents, schemas, args.root)
    except (OSError, ValueError, UnicodeError):
        print("ERROR: missing, unreadable or malformed JSON input; values suppressed.",
              file=sys.stderr)
        return 2
    if errors:
        for error in errors:
            print(f"ERROR: {error}", file=sys.stderr)
        return 1
    print("PASS: 5 state files; 6 local Draft 2020-12 schemas; evidence, order and lock checks.")
    print("Operational rollout remains blocked. Validation is not live readiness or deployment approval.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
