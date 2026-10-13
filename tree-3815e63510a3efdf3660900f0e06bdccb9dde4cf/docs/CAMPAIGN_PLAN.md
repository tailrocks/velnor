# Canonical Velnor campaign plan

## Scope and authority

This is the single continuation plan for the five-consumer, two-host campaign.
The current implementation workstream changes this coordination repository only.
It does not authorize deployment, publication, host changes, consumer/product
edits, commits or pushes. This workstream makes no edits to the source checkout.

The rollout objective was recovered from section B.2 of the
[archived handoff](goal-handoffs/velnor-rollout-bastion--20260921T220213Z--antigravity--930b8384.md).
It is a requirements source, not current execution authority or completion proof.
The current user's narrower scope takes precedence over its operational commands.
No separate original transcript or newer full rollout objective was available.

[CURRENT_STATE.md](CURRENT_STATE.md) remains the preserved detailed inspection.
[CAMPAIGN_LEDGER.md](../CAMPAIGN_LEDGER.md) and the archived handoff remain historical
records. Neither is a competing plan. The later
[user-supplied integration refresh](evidence/integration-context-20260923.md)
updates only the facts it actually reports. Sources are hashed and indexed in
[evidence-index.json](../state/evidence-index.json).

## Status semantics and evidence

| Status | Meaning |
| --- | --- |
| observed | A fact was recorded by a cited inspection or user report, at the supplied time/precision. It does not mean ready, accepted or independently rechecked here. |
| historical | An archived claim or recovered requirement. It cannot satisfy current qualification. |
| pending | Required evidence or an immutable identity has not been collected. Unknown values stay null. |
| blocked | A known defect, missing prerequisite or unresolved dependency prevents progress through a gate. |

Observation time and transcription time are separate. A date-only Mac report
does not acquire an invented exact timestamp. Source file hashes prove which
record was transcribed; they do not authenticate external events.

The state files separate implemented, tested, integrated, published, installed
and qualified evidence. None is inferred from another. Source compilation,
registration, a green dispatcher, an installed package or a historical green run
cannot establish complete installed operation.

## Observed starting point

The campaign remains blocked at G0. No gate has current qualification evidence.

| Scope | Latest supplied evidence | Consequence |
| --- | --- | --- |
| Product main | User reports `tailrocks/velnor@6391a0ce53b7e666d1bfb391083011622158f4f3`. | This is an observed source HEAD, not a selected release. |
| Preview | Run `35797775098` is `in_progress`; its source, attempt and products are unknown. Rolling preview `f7bc191` is stale. | No usable preview lock. Reject every selected source beginning `f7bc191`. |
| Bastion, 2026-09-22T23:44Z | Docker 29.8.1, installed stable 0.1.274, 16 slots, zero ready, GitHub unreachable, CPUQuota/MemoryHigh/MemoryMax present, inactive Velnor daemon. | Installation is not fleet readiness. G6 and replay remain blocked. |
| Mac, 2026-09-23 MYT | M5 Max arm64, OrbStack arm64, no live supervisor, state N=1 versus monitor N=4. | Supervision and capacity authority need diagnosis. Neither N is a desired or measured accepted value. |

These fresh facts came from the user. This workstream did not query GitHub or
either host. Earlier bastion details remain attributable only to the
2026-09-23T00:52:43+02:00 inspection (2026-09-22T22:52:43Z): zero registered
and executor-ready slots, placeholder GitHub URL, empty credential variable,
absent secret files, invalid group/routing, and a policy selecting only
`tailrocks/velnor` with untrusted scope.

That earlier record also reports controller health live versus unavailable CLI
control API, desired slots 16 versus CLI 4, disabled controller/guardian units,
inactive timers, and exact slice limits CPUQuota=9120%, MemoryHigh=90%,
MemoryMax=95%, MemorySwapMax=0 and TasksMax=4096. The latest report confirms quota
presence, not those exact values. Empty reconciled permits prove no leak at that
capture, not FIFO behavior under load or job execution.

The installed 0.1.274 source commit, binary hash, image index digest and APT public
fingerprint remain in `hosts.json` with their original evidence. They are not
preview targets. The secondary NVMe had no partitions, filesystem or mount at
the earlier inspection; the fresh report does not renew that observation.

Known contradictions remain explicit:

- The ledger calls G0-G8 complete; the paused handoff calls G4 in progress and
  G5-G8 not started. The objective, ledger and handoff also assign different
  meanings to some gate IDs. The objective's definitions below govern this plan.
- The ledger lists two essential-mac main-run triplets. Both remain historical;
  neither is silently preferred or reused as fresh proof.
- The ledger's enabled `velnor-runner.service`, root partition p3 and no-quota
  claims conflict with the inspection's missing unit, root partition p4 and
  effective slice ceilings.
- Historical Mac max_jobs=4 does not resolve the new state/monitor mismatch.

## Rollout contract

The exact order on each host is:

1. `donbeave/essential-mac`
2. `ChainArgos/jackin-agent-brown`
3. `ChainArgos/cloudflare-tofu`
4. `ChainArgos/github-terraform`
5. `ChainArgos/java-monorepo`

On the actual Mac, essential-mac must prove Scale Set first, native second,
then both together with hosted verification. Each consumer qualifies before
the next activates. All five Mac qualifications precede operational bastion
installation/activation. The existing bastion installation does not waive that
gate. Bastion uses the same consumer order, with essential-mac native first,
then Scale Set and combined operation. Java remains last on both hosts.

`tailrocks/velnor`, `tailrocks/velnor-apt` and `tailrocks/homebrew-velnor`
are product/delivery dependencies, not preceding consumers. A product self-dogfood
fleet is not a prerequisite to the essential-mac pilot. Generic product work and
later-repository research can be parallel in a separately authorized workstream;
consumer activation stays sequential.

Canonical providers are `github-hosted`, `github-self-hosted` (unmodified
official ephemeral runner managed by Velnor's Rust Scale Set adapter), and
`velnor` (existing native Docker backend). Provider, execution host, daemon
architecture, workload target, trust and publication role remain distinct.

## Gates, ownership and exit evidence

Gate dependencies form G0 through G8 in order. All currently have status
`blocked`. Actual authors and verifiers are unassigned; role names are
requirements, not evidence that reviewers worked. The machine-readable task
graph, role assignments, blocker links and detailed exit criteria live in
[campaign-state.json](../state/campaign-state.json).

| Gate | Required outcome | Author / separate verifier role |
| --- | --- | --- |
| G0 | Fresh repository/access/workload and both-host inventory; reconcile conflicting history. | Evidence/access author / independent evidence reviewer |
| G1 | Published installed Mac product, locked tools/images, generated hosted recovery, scoped credentials, protocol/Docker tests; resolve Mac supervision and N. | Product/macOS/delivery authors / independent protocol, generator, host and supply-chain reviewers |
| G2 | First essential-mac live proof on actual Mac through Scale Set, private DinD and substantive portable checks. | Official-engine author / independent Mac integration reviewer |
| G3 | Native proof after G2, then combined three-provider PR and three consecutive full green main runs without manual reruns. | Native author / independent native, capacity and result reviewers |
| G4 | Completely qualify jackin-agent-brown, cloudflare-tofu, github-terraform in that order. | Repository authors / independent stack and shared-runtime reviewers |
| G5 | Java last on Mac, complete workload and architecture coverage, PR/main, performance and recovery proof. | Java/Rust/Docker/frontend authors / independent coverage reviewers |
| G6 | Fresh bastion inventory, signed Docker/APT delivery, repaired registration/routing/control/supervision, no quotas, untouched NVMe. | Debian/Docker/APT author / independent deployment reviewer |
| G7 | Native-first bastion pilot, then Scale Set/combined, same five-consumer replay with forced host placement and Java last. | Placement/consumer authors / independent host and parity reviewers |
| G8 | Final identities, both-host acceptance, trust/recovery/watchdog/no-quota proof, operating procedures and future onboarding. | Integration/documentation authors / independent final reviewer |

Current blockers B-FRESH, B-HISTORY, B-PINS, B-MAC, B-GITHUB, B-ROUTING,
B-CONTROL, B-SUPERVISION and B-QUOTAS each have an evidence link, affected gates
and a next action. Downstream gates are also blocked by their preceding gate.
There is no delegation facility in this runtime; no independent verifier is
claimed. An author-run local validator is not independent operational acceptance.

## Invariants retained from the objective

- Native control processes run on Darwin/Linux; ordinary local jobs execute in
  Linux Docker containers. Preserve real native Mac tests on explicit hosted
  macOS lanes. Do not run essential-mac apply/bootstrap against personal state,
  change global Docker context or invent Velnor worker VMs.
- One durable FIFO capacity authority per physical host covers both engines,
  all scopes, and reserved through uncertain/cleanup states. Hold permits until
  owned cleanup completes; reconcile before advertising. Measure N under mixed
  cold/warm workloads. No per-repository budgets or CPU/memory/ancestor ceilings.
  Do not silently choose between the Mac's observed N values.
- Use Ubuntu 26.04 job images, immutable platform digests and reviewed Scale Set/
  runner revisions. Prove private DinD workspace/network/socket behavior for the
  official lane and job-owned Docker mediation for native Velnor. No unrestricted
  host Docker socket, broad HOME, SSH agent or desktop credential-store mounts.
- Test acquire/JIT/provision/acknowledgement failure windows, fencing, redelivery,
  cancellation, partial cleanup, worker adoption, mode drain, restart and next-job
  recovery. Keep one fenced controller owner per set; do not promise exactly-once
  external effects or OOM isolation without evidence.
- `velnor-workflow` alone generates workflows from typed inputs. Publish verified
  immutable tooling before consumer repinning; update the pin and generated tree
  together. Keep hosted recovery independent of local runners. No handwritten
  workflow escape hatches, source-install fallback or fake/skipped coverage.
- Mac acceptance uses coherent published Homebrew installation. Bastion acceptance
  uses exact verified signed APT packages under the product transaction lock,
  release activation verification and independent origin/key checks. No copied
  binaries, local-deb installation or signature bypass. Product fixes precede
  published delivery and installed promotion.
- Enforce trusted-code admission outside PR-editable YAML; keep privileged keys
  in management and short-lived job credentials scoped. Preserve DCO, required
  checks and one publication/deployment writer. Test trust denial.
- Keep OpenTofu in cloudflare-tofu; inspect github-terraform and jackin-agent-brown
  without guessing their actual workload. No production apply/destroy/import to
  prove a runner. Java's historical baseline is 71 units: 37 Gradle, 17 Rust,
  11 Docker, 4 Bun, 1 Node and 1 Docs, or a reviewed coverage-equivalent inventory.
  Preserve database/codegen identity, Testcontainers, native tools, browser tests,
  amd64 requirements and explicit emulation labeling.
- Leave secondary NVMe untouched, preserve SSH, and keep management endpoints
  private. No formatting, broad prune, blanket runner deletion or unrelated
  destructive cleanup. Use owned disposable canaries for fault tests.

## Preview lock and qualification evidence

[preview-lock.json](../state/preview-lock.json) is deliberately unresolved:
`status=blocked`, `lock_id=null`, every selected component identity null.
Its `observed_inputs` reports current main and the running preview only.
`excluded_source_prefixes` is a rejection list, never a pin. The validator
rejects selected component sources beginning `f7bc191`, even when other
fields claim success. No floating rolling tag can stand in for an immutable
release identity.

In a later authorized workstream, independently reconcile run source, attempt,
successful conclusion, published manifests/provenance, all required runtime
architectures, generator, Mac product, APT package, native image, upstream
reference, official runner and DinD identities. Keep the lock blocked until
every component is evidenced. A resolved lock ID is SHA-256 of the UTF-8 JSON
component array using sorted keys and compact separators. Resolution still
does not authorize promotion: `promotion_authorized=false` is fixed here.

Future qualification evidence must bind repository + exact source SHA +
run/job/attempt + plan digest + unit + provider + required host +
OS/architecture + command/features/profile/fixture identity. Correlate GitHub
runner/job data with installed binary/image and management container/daemon/host
records, actual test counts and cleanup. Missing, skipped, failed, cancelled,
stale-attempt and wrong-host results fail; another provider cannot substitute.
Historical run IDs in `repository-rollout.json` are investigation anchors only.

The generated hosted watchdog must start independently of local completion.
Initial targets from the objective are reserve-to-connected 180 seconds,
cleanup 120 seconds, free-capacity stall diagnosis 5 minutes, and total local
outage reported failed/incomplete within 10 minutes. These are pending targets,
not observed performance. Later generic fixes invalidate only affected evidence;
publish before repinning and requalify affected earlier consumers.

## Local validation and resumption

[validate-campaign-state.py](../scripts/validate-campaign-state.py) validates all
five JSON files against six local Draft 2020-12 schemas. It checks strict fields,
statuses, timestamps, source hashes/line ranges, references, gate and consumer
order, separate reviewer identities, rejection of historical proof and unresolved
or stale preview selection. Schema resolution is offline; no host, GitHub or
deployment commands are executed.

It also rejects credential-value fields, common token/private-key signatures
and credential-bearing URLs without printing the values. This is a conservative
guard, not a guarantee that arbitrary prose cannot contain an unknown secret.
Only safe version, identity, presence/absence and empty-state facts belong here.

Use an isolated Python environment; do not alter the user's global interpreter:

```sh
campaign_check_dir="$(rtk mktemp -d)"
rtk python3 -m venv "$campaign_check_dir/venv"
rtk "$campaign_check_dir/venv/bin/python" -m pip install 'jsonschema>=4.22,<5'
rtk "$campaign_check_dir/venv/bin/python" scripts/validate-campaign-state.py
rtk "$campaign_check_dir/venv/bin/python" -B -m unittest discover -s scripts -p 'test_validate_campaign_state.py'
```

Python 3.10 or newer is required. Dependencies are installed explicitly; the
validator itself never installs them. A successful exit proves local consistency,
including honest blocked states, not live campaign completion. Exit 1 means
invalid records; exit 2 means missing dependencies or unreadable/malformed input.

Resume from [RESUMPTION_HANDOFF.md](RESUMPTION_HANDOFF.md), reconcile source
observations and pending reviews, then take the earliest authorized dependency.
Do not resume the archived operational commands merely because this plan exists.
