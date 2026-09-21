# Required-gate mapping review

Reviewed commit `7e21a551` and the generated `ci-main.yml` / `ci-pr.yml` gate
against `observations/gate-baseline-f0fb1c01.json` and
`observations/gate-candidate-local.json`.

## Verdict

The mapping fix is correct for its claimed bounded scope: malformed, unknown,
duplicate, empty, and unmapped selected unit/provider obligations now fail
before result evaluation. The fix does not establish that every selected
obligation executed; provider-admission semantics remain a separate blocker.

`render_nodes_required` derives one `RequiredCaller` list, then uses that same
list for aggregate `needs`, `EXPECTED_CALLERS`, and per-caller verdicts. The
shell gate validates the generated contract and requires every selected pair to
map to it. It also rejects malformed `NEEDS_JSON` and treats missing results as
failure in both selected and unexpected branches.

The preserved raw replay is decisive for the targeted regression:

| case | baseline `f0fb1c01` | candidate |
| --- | ---: | ---: |
| empty selection | exit 0 | exit 0 |
| unknown unit | exit 0 | exit 1 |
| unknown provider | exit 0 | exit 1 |
| empty providers | exit 0 | exit 1 |

The candidate test matrix also covers malformed JSON, duplicate units,
duplicate providers, and non-string providers. Generated workflow output
contains the same validation and `EXPECTED_CALLERS` contract.

## Remaining gate gap

The selected-but-unadmitted branch still accepts `skipped`:

```bash
if [[ "$PROVIDER_ADMITTED_*" == true ]]; then
  # selected caller must succeed
else
  # selected caller may be skipped
fi
```

I replayed the exact generated `ci-main` gate with
`SELECTED_UNITS=[{"unit_id":"bun-velnor","providers":["github-hosted"]}]`,
all caller results skipped, and `PROVIDER_ADMITTED_GITHUB_HOSTED=false`.
It returned exit 0. This is a real selected-obligation false-green path, not a
mapping failure. Resolve it in the provider-admission policy review, with
explicit semantics for unavailable providers and trusted/fork events, before
claiming the required gate is complete.

No gate source or generated workflow files were changed during this review.
