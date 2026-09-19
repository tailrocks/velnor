# Checker adversarial fixtures — bounded independent review

Date: 2026-09-20

## Scope and exact revision

Reviewed the exact clean detached checker worktree at
`2ba66b116dd5511f0b4f2a6856cfbed6bd290152`:

```text
/private/tmp/g1-checker-adversarial-final
```

`rtk git status --short --branch` returned `## HEAD (no branch)`. No checker
source was edited. The only writes are this external evidence directory and
generated fixture/result files below.

The source permalink for this revision is:

<https://github.com/tailrocks/velnor/blob/2ba66b116dd5511f0b4f2a6856cfbed6bd290152/crates/velnor-tools/src/evidence_check.rs>

The strict schema says every object uses `deny_unknown_fields`, aliases and
legacy fallbacks are rejected, and parse failure is failure
(`docs/ci/github-first-dual-lane/evidence-schema.md:22-25`). It also requires
the fixed exact 32-repository scope (`:29-80`), independently reconciled
source/run/job/check/child identities (`:99-164`), and says missing/failed
work fails (`:116-121`). The harness tests those claims against the actual
entrypoint.

## Harness and verification commands

Fixture generator:

```text
rtk proxy /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-adversarial-fixtures/generate_fixtures.sh \
  /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-adversarial-fixtures/fixtures
```

One positive G0 inventory and one positive G1 hosted execution matrix are
generated from the canonical 32 names. The G1 control has one completed
successful job, required check, source/run identity, and HTTPS log URL per
repository. G4/G5 fixtures deliberately use a Velnor-only eligibility policy
to isolate host-capability semantics; their host is synthetic.

Full entrypoint harness:

```text
rtk proxy /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-adversarial-fixtures/run_cases.sh \
  /private/tmp/g1-checker-adversarial-final/target/debug/velnor-tools \
  /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-adversarial-fixtures/fixtures \
  /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-adversarial-fixtures/out
```

Machine-readable results: `out/results.ndjson`.

Exact source validation:

```text
rtk cargo test -p velnor-tools evidence_check
cargo test: 9 passed, 207 filtered out (1 suite, 0.00s)

rtk cargo clippy -p velnor-tools --all-targets -- -D warnings
cargo clippy: No issues found
```

These prove the exact tip compiles and its existing tests pass; they do not
prove the semantic gaps below are acceptable.

## Results

### Positive controls

| Case | Result |
| --- | --- |
| `positive-g0` | exit 0, `pass`, 0 findings |
| `positive-g1` | exit 0, `pass`, 0 findings |

The positive controls are genuine checker passes. They are fixture claims only,
not live GitHub/G7 acceptance.

### Correct fail-closed cases

| Mutation | Exit/status | Finding or parse result |
| --- | --- | --- |
| G0 record uses provider `third_party` | 1 / fail | `g0-record-role` |
| 32-row manifest substitutes one repository name | 1 / fail | `manifest-scope`, `missing-repository`, `missing-snapshot-repository`, `snapshot-out-of-scope`, `unknown-repository` |
| G1 empty logs | 1 / fail | `missing-log` |
| G1 malformed log URL | 1 / fail | `missing-log` |
| G1 expected job name rebinding | 1 / fail | `job-inventory-mismatch`, `missing-job` |
| G1 extra actual job ID | 1 / fail | `job-inventory-mismatch` |
| G1 duplicate actual job ID | 1 / fail | `duplicate-job` |
| Required `workflow_run` child omitted | 1 / fail | `child-run-inventory` |
| Manual-dispatch main execution, even same source SHA | 1 / fail | `event-source`, `missing-main-evidence` |
| GitHub execution claims Velnor runner kind/labels | 1 / fail | `host-trust` |
| Unknown root field in manifest/snapshot/evidence | 1 / parse error | strict JSON parse failure |
| Old flat fleet rows as repository strings | 1 / parse error | strict manifest parse failure |
| Uppercase `PR` applicability or `GitHub` eligibility enum | 1 / parse error | strict manifest parse failure |
| Flat release/install alias fields, equal or conflicting value | 1 / parse error | strict evidence parse failure |

Relevant implementation: strict deserialization uses `deny_unknown_fields`
throughout the typed structs (`evidence_check.rs:214-320`, `:325-483`,
`:489-595`, `:601-803`) and `read_json` reports strict parse failure
(`:932-936`). Expected job and actual-ID set/binding checks are in
`:2691-2827`; child count is checked in `:2915-3004`; manual main event is
rejected in `:2521-2541`.

### False passes / semantic gaps

| Mutation | Observed result | Why it passes |
| --- | --- | --- |
| G0 inventory record has `blocker` and `next_action` while `gate_status=inventory` | exit 0, `pass`, 0 findings | G0 returns immediately after `check_g0_record`; blocker/next-action checks are below that return (`:2242-2244`, general unfinished check `:2312-2331`) |
| Manifest adds an eligible `third_party` provider key and expected job; all other G0 inputs remain valid | exit 0, `pass`, 0 findings | `check_eligibility` requires github/Velnor but rejects no extra keys (`:3881-3897`); expected-job validation accepts any key present in the map (`:3752-3763`) |
| G1 run URL changed coherently to `https://evil.example/wrong/repo/run/999` | exit 0, `pass`, 0 findings | run URL is only required nonempty and compared for equality between record/snapshot (`:1778-1789`, `:2391-2457`); no GitHub host/repository/run-ID binding |
| G1 log URL changed coherently to `https://evil.example/unbound/log` | exit 0, `pass`, 0 findings | `check_execution_record` checks only nonempty HTTPS-looking URL, not repository/run/job identity (`:2484-2492`) |
| Required check source URL changed coherently to `https://evil.example/unbound/check` | exit 0, `pass`, 0 findings | required-check validation checks context/app, run/job/status/conclusion/event and nonempty URL, but no independent API check identity or URL binding (`:2829-2912`) |
| Child graph present with correct repository/path/event/source but provider `evil-third-party` and evil URL | exit 0, `pass`, 0 findings | child validation checks count/spec/source SHA and link equality, but no child provider, URL host/run binding, or branch field (`:2915-3004`) |
| G4 synthetic Velnor host: `runner_kind=velnor-managed`, nonempty `host_id=nonempty-host`, required label `velnor-host`, no Mac/OrbStack/Docker evidence | exit 0, `pass`, 0 findings | host binding validates only kind, nonempty identity, labels, and forbidden labels (`:2619-2688`); execution schema has no Mac, OrbStack, Docker endpoint, engine, image, or container proof |
| Same synthetic host fixture at G5 with typed non-applicable release/install reason | exit 0, `pass`, 0 findings | same host-only validation; no actual host capability proof |
| `check-evidence`, `evidence-verify`, and `verify-evidence` commands | each exit 0, `pass` | CLI retains legacy aliases at `crates/velnor-tools/src/main.rs:78-81`, contradicting schema no-alias/no-legacy contract |

The G0 third-provider *record* itself correctly fails because G0 rows must be
inventory rows. The gap is the unrecognized third-provider policy key being
accepted in an otherwise internally consistent manifest/record, rather than
the record-role guard.

## Structural findings

1. G0 is not fully fail-closed. `check_record` performs identity/pin/workload
   checks, then returns from the G0 branch before generic `gate_status`,
   blocker, and `next_action` handling. A blocked inventory can therefore
   report `pass` to the top-level report. G0 needs an explicit blocked state or
   must reject unresolved blocker/next-action fields before returning.

2. Provider policy is open-ended. The source only requires `github` and
   `velnor` keys; it does not reject a third key. The reviewed schema says
   provider eligibility is a closed three-value policy and the goal excludes a
   third provider. Use a closed provider-key/type validation and test an
   internally consistent third-provider row.

3. Provenance URLs are opaque strings. Run, log, check, and child URLs can all
   be evil/unbound URLs while every internal repeated field agrees. URL syntax
   and equality are not independent GitHub evidence. Bind each URL to the
   canonical GitHub API/web host, repository, run/job/check/child ID, and
   expected event/provider; live collection must independently obtain those
   identities.

4. Required checks are self-attested facts. A record can provide a successful
   status/conclusion/app/context/job/run tuple and an arbitrary URL; no check
   run ID/app identity is reconciled to a separately collected check object.
   Add typed live check identities and reject self-declared status-only rows.

5. Child graph is under-specified. `ChildWorkflowSpec` has repository, path,
   and event only; no branch/ref/provider contract. The checker accepts an
   evil child provider and URL when the expected repository/path/event/source
   fields match. Add provider, source/ref/branch, run URL/ID binding, and
   producer identity to the reviewed child contract.

6. G4/G5 host acceptance is identity-only. A nonempty host ID and correct
   `velnor-managed` label satisfy the checker without proving the authorized
   Mac, OrbStack Docker endpoint, engine architecture, image digest, or
   container execution. The synthetic G4 and G5 passes are therefore semantic
   false positives, not pilot evidence.

7. Legacy CLI aliases remain despite strict schema policy. Remove
   `check-evidence`, `evidence-verify`, and `verify-evidence`; one canonical
   `evidence-check` command should be accepted. Nested release/install and
   unknown JSON fields already fail correctly; the CLI alias path is the
   remaining direct compatibility surface tested here.

## Disposition and owner handoff

This is an adversarial fixture/review report, not a gate approval. The exact
checker tip compiles and passes its existing unit suite, but the false-pass
cases above must be closed before relying on it for G0/G1/G3/G4/G5 claims.
The owner should retain the fixture harness and make each marked false-pass
case exit nonzero or produce an explicit non-authoritative/blocked status.

No source edits were made by this review. No G2 distribution implementation
or fixture ownership was duplicated.
