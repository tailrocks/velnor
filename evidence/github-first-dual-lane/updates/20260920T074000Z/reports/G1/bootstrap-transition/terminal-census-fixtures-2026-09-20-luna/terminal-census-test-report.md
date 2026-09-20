# Independent terminal-census fixture/test contract

Synthetic external evidence only; no source, authority, GitHub, release, workflow, dispatch, runner, or credential mutation. Not readiness approval.

Positive: strict_schema=pass; semantic_contract=pass; required consumers=15.
Negatives: 24; schema rejected=11; semantic rejected=24; schema gaps=13.

Each row binds exact consumer workflow/job/check identity, source Contents provenance, run event/ref/attempt/head, attempt-scoped job/steps, Check Runs identity/app/status, and verify-B producer/API/source-record lineage. Exact identity-set equality is semantic; minItems is insufficient.

## Cases

- `omitted-consumer` — schema=reject; semantic=reject; required row omitted
- `duplicate-consumer` — schema=accept; semantic=reject; duplicate identity with changed provenance
- `substitute-consumer` — schema=accept; semantic=reject; substituted required identity
- `extra-success-row` — schema=reject; semantic=reject; extra row
- `extra-failed-row` — schema=reject; semantic=reject; extra failed child row
- `failed-job` — schema=reject; semantic=reject; failed child
- `skipped-job` — schema=reject; semantic=reject; skipped child
- `neutral-check` — schema=reject; semantic=reject; neutral check
- `wrong-app` — schema=accept; semantic=reject; wrong app
- `wrong-source` — schema=accept; semantic=reject; wrong source
- `wrong-source-ref` — schema=accept; semantic=reject; wrong source ref
- `wrong-event` — schema=accept; semantic=reject; wrong event
- `wrong-ref` — schema=accept; semantic=reject; wrong ref
- `wrong-run-attempt` — schema=accept; semantic=reject; wrong attempt
- `wrong-job-parent` — schema=accept; semantic=reject; job parent mismatch
- `wrong-check-parent` — schema=accept; semantic=reject; check parent mismatch
- `wrong-check-external-id` — schema=accept; semantic=reject; check identity mismatch
- `wrong-producer` — schema=reject; semantic=reject; producer mismatch
- `child-job-absent` — schema=reject; semantic=reject; child job absent
- `child-check-absent` — schema=reject; semantic=reject; child check absent
- `child-step-absent` — schema=reject; semantic=reject; child step absent
- `rows-digest-mismatch` — schema=accept; semantic=reject; digest mismatch
- `excluded-identity-included` — schema=accept; semantic=reject; excluded identity included
- `source-blob-absent` — schema=reject; semantic=reject; source blob absent

## Integration

Add maxItems=15, uniqueItems=true, strict source/run/job/check/producer objects and completed/success constants to verify-B. Then run the registry/equality verifier in the recommendations JSON. JSON Schema cannot enforce the exact required set or cross-object lineage alone.

Repair model SHA: `7996c852e749c32eafb338a38f0b7e865ee7ea22e0325de7d504db07c67b9aea` (12-repair-3)
V12 freeze SHA: `0693377186d05348585749f199450cb4f3e69c9641118157ad10a862f7887b69`; root digest `e2b30dc21732f957ee9b69b45f270b2d72a995b624d4b19a448eb86985e404f8`

No synthetic result is live evidence or an authority claim.
