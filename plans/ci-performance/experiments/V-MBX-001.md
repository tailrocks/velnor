# V-MBX-001: MBX directory transport and candidate runtime bootstrap

Status: MBX transport evidence collected; transport fix accepted only as a
functional hypothesis. Performance acceptance is pending at least five
controlled cohorts. Candidate runtime bootstrap is a separate unresolved
correctness defect.

## Question and baseline

PR [#967](https://github.com/tailrocks/velnor/pull/967) changes the MBX
transport after hosted jobs restored a large object bundle and failed with
`EDQUOT`. Its head at observation time was
`d3c95ec3f4fafdedc38b4d4a88858df6361331ab`. The implementation pins MBX
1.12.0, `jdx/mr-boxington-action` `867fc530102eec5b756075d70d850dc8330d2272`
(v1.4.0), uses directory-form object transport, stages import in the local
store, and changes the MBX compatibility digest in generated consumers.

The observations below are manual `workflow_dispatch` runs of that head. They
are not controlled baseline/candidate timing samples and do not establish a
10x result.

## Successful run: 35481387575

Run: [35481387575](https://github.com/tailrocks/velnor/actions/runs/35481387575).
Relevant jobs:

| Job | Evidence |
| --- | --- |
| Rust workflow unit, [105999752499](https://github.com/tailrocks/velnor/actions/runs/35481387575/job/105999752499) | 01:26:36--01:28:29 UTC; MBX action v1.4.0 and MBX 1.12.0. |
| Rust runner unit, [105999752413](https://github.com/tailrocks/velnor/actions/runs/35481387575/job/105999752413) | 01:26:35--01:36:35 UTC; successful 2,516-test nextest job. |
| Docker unit, [105999752229](https://github.com/tailrocks/velnor/actions/runs/35481387575/job/105999752229) | 01:26:36--01:27:02 UTC; successful BuildKit `ci` target. |

The workflow runtime artifact downloaded by these jobs was still the old
published pin `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`, with digest
`1a666ba09f475b550d69e0fd2d8cec483683687ac503048f4cd756e22db64b1d`.
The old runtime is therefore the only runtime product proven to execute in
the plan and consumer setup path in this run.

The Rust workflow unit did exercise the new directory path. Its log records:

```text
Cache hit for: velnor-mbx-v3-13a355ab0f86-...-rust-velnor-workflow-...
Cache Size: ~984 MB (1031291619 B)
Set up mbx 1.12.0
mbx cache import /home/runner/.cache/mbx/actions/github-actions-cache-v1
imported 3698 actions and 13064 objects ... (4.5 GiB)
restored Cargo workspace state (2039 referenced files, 4.3 GiB)
mbx[cache]: object cache: 3 hits, 0 misses, 1 bypassed; 0 B downloaded, 0 B uploaded
mbx cache export --group github-actions-35481387575-1-... --format directory .../github-actions-cache-v1
```

The same job's generator check ran the checkout's `target/debug/velnor-workflow`
and reported that the tree matched candidate render
`ee54e59efde29dba1d63467fec9e932ad1b70c263367e356701e327d742ec848`, not the
declared old pin. This proves candidate source execution inside the unit
check, not candidate artifact handoff to privileged policy.

The runner unit had an independent, cold MBX layer even though rustup, mise,
mold, and Cargo target caches restored:

```text
No mbx cache found
rustup cache: ~568 MB (595467383 B)
mise cache: ~49 MB (51565988 B)
target cache: ~166 MB (174146795 B)
Finished `test` profile ... in 4m 14s
Finished `test` profile ... in 2m 46s
mbx[cache]: object cache: 3 hits, 3 misses, 1808 not looked up, 131 bypassed
0 B downloaded, 0 B uploaded, 353.7 MiB stored locally
0ns estimated compiler time avoided
```

This is evidence that an outer cache hit or the new transport does not imply
compiler reuse. It also explains why this run cannot support a warm-build
speedup claim.

The Docker job had an explicitly cold seed cache:

```text
Cache not found for input keys: velnor-docker-seed-v3-...
Docker build seed empty: the build starts from cold cache mounts
docker buildx build ... --cache-from type=gha,scope=docker,mode=max ...
#9 importing cache manifest from gha:16114672505471616594
#10 CACHED ... #40 CACHED
#15 ... cargo fetch ...
#15 CACHED
```

No `Downloaded <crate>` lines appeared. Immutable BuildKit layers were warm,
so this is not a cold container dependency benchmark. BuildKit exported its
cache in 3.0 seconds; the whole Docker job report was 19 seconds.

## Failed runner run: 35480920648

Run: [35480920648](https://github.com/tailrocks/velnor/actions/runs/35480920648).
The runner job was [105998484852](https://github.com/tailrocks/velnor/actions/runs/35480920648/job/105998484852).
It used the same MBX/action pins, had no MBX cache, and reported:

```text
mbx[cache]: object cache: 3 hits, 0 misses, 1878 not looked up, 131 bypassed
0 B downloaded, 0 B uploaded, 7.0 GiB stored locally
```

The failure was a test failure, not quota exhaustion:

```text
FAIL velnor-runner::scaleset_allocator occupancy_never_exceeds_n_under_churn
Summary: 2470 passed (1 leaky), 1 failed, 5 skipped
```

No `EDQUOT`, no no-space error, and no MBX transfer error appeared. The run is
retained as a failed observation and cannot count as a successful warm cohort.

## Policy bootstrap failure: 35480811779

Run: [35480811779](https://github.com/tailrocks/velnor/actions/runs/35480811779),
policy job [105998176337](https://github.com/tailrocks/velnor/actions/runs/35480811779/job/105998176337).
The job was `pull_request_target` for the same head. It installed and ran the
old pin `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`, closure
`8b96d5108550dfa61a57ff65c6c357b4119493c660417bf3beb4af2b03742269`, detected
the generated tree was candidate-rendered, then waited 15 minutes for:

```text
velnor-workflow-candidate-ee54e59efde29dba1-Linux-X64
no candidate product ... was published within 15 minutes
```

The generated candidate producer is gated on
`github.event_name == 'pull_request'` and a same-repository head. Both PR967
manual runs were `workflow_dispatch`, so `candidate_publish: true` was present
but the producer and artifact upload were skipped. No candidate artifact was
published or consumed in these runs. This is a coordination/correctness gap,
not evidence that policy validation passed.

## Root cause and bootstrap design

The architectural cause is that the generated plan and the candidate runtime
are produced by different stages, while the pinned runtime cannot parse a
schema-changing generator configuration. A candidate product must exist before
the first plan can be trusted. The current producer is after checkout and unit
checks, and its event guard excludes manual/bootstrap events.

Three executable remedies were considered:

1. Generate a pre-plan bootstrap job. It checks out the exact merge source,
   builds one candidate generator, emits a manifest, uploads one immutable
   artifact, and makes plan plus all consumers depend on verified artifact
   identity. Keep the existing PR-head candidate path only when merge and
   head closures differ.
2. Move bootstrap into a reusable workflow that returns producer run ID,
   attempt, artifact ID/digest, source SHA, closure, and binary digest. Every
   consumer downloads by exact artifact/run identity and verifies the complete
   contract before execution.
3. Publish an attested immutable runtime product first, then regenerate and
   pin consumers in a follow-up. This is viable for post-merge promotion but
   cannot validate a new schema in the same PR without option 1 or 2.

The strongest design is option 1 plus the existing attested release path:
build once before planning, fan out the exact verified product, and keep
privileged policy separate from untrusted execution. The typed API should live
in the generator/runtime layer, with generated workflow changes in
`crates/velnor-workflow/src/s2/primitives/ir.rs`, runtime product assembly in
`crates/velnor-workflow/src/s2/runtime.rs` and `src/runtime.rs`, and the
generated workflow producer/consumer templates in the generator templates.
Do not hand-patch generated YAML.

The manifest contract must bind all of these before execution: repository,
workflow, producer run and attempt, successful conclusion, source SHA, build
SHA, canonical generator closure, project configuration/schema digest,
platform, profile/features, artifact ID and digest, binary SHA, and the
binary's self-reported revision and closure. Artifact names or latest-run
selection are insufficient. Fork candidates must stay outside privileged
policy; same-repository candidates still require exact identity and successful
producer evidence.

## Decision

Status: **accepted as transport evidence; bootstrap experiment rejected as
unproven**. PR967's directory transport was actually exercised and no quota
failure occurred in the successful run, but there are fewer than five
controlled cohorts, cold runner MBX evidence, one failed runner test, and no
candidate product handoff. Next work: independently review the bootstrap API,
implement the pre-plan product producer, then run real PR and policy paths with
exact artifact/run verification.

## Integrated candidate, first CI observation

Source f0fb1c012adc2b7e604eaab332785eb5bf780caa, run
[35484350008](https://github.com/tailrocks/velnor/actions/runs/35484350008).
The completed bench job
[106008631268](https://github.com/tailrocks/velnor/actions/runs/35484350008/job/106008631268)
ran action 867fc530 and MBX 1.12.0; raw duration 368 seconds. It explicitly
reported no MBX cache found. Rustup, mise, mold and Cargo archives restored,
so this is MBX-cold, not an entirely cold runner. Test compilation reported
3m01s and Clippy 2m15s. Compiler statistics remain poor: test 3 hits / 0 misses /
1840 not looked up / 131 bypasses; Clippy 3 / 3 / 1775 / 131. Both reported
zero downloaded and uploaded bytes. Bypass reasons were native C/C++-related;
these counters do not yet explain every Rust invocation.

The source-owned telemetry incorrectly classified MBX as `prefix` despite the
explicit miss. This is a measurement defect, not a warm-cache observation.
[Timestamped excerpt](../observations/velnor-35484350008-106008631268-cache-excerpt.log)
retains the conflict. Fix matched-key/outcome classification before trusting
summaries. No speedup, warm reuse, or quota plateau is established by this job.
The overall run still had active jobs and a documentation lint failure when
this observation was recorded.
