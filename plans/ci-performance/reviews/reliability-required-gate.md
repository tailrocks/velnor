# Independent review: required gate cancellation

Reviewed shared-worktree diff on main 97bac4c458. Four source files changed:
source-1 and source-2 IR renderers, plus their generator revisions and existing
render assertions. Reviewer did not edit repository files or run shared builds.

## Verdict

APPROVE the source-1 and source-2 cancellation repair, contingent on parent test and
regeneration results. Required gate and display mirror now use job-level
`always()`. Expensive caller cancellation behavior remains unchanged. Removing
the cancellation argument from required rendering prevents future callers from
accidentally recreating the skipped-required-check bug.

New tests inspect parsed generated YAML, verify exact dependency sets and both
gate guards, and demonstrate that removed edges or `!cancelled()` mutations
fail validation. Executed emitted-shell tests include successful selected work,
explicit empty-selection skips, rejected undeclared success, and failed,
cancelled, skipped, or absent plan/caller results. Existing policy tests cover
those same policy outcomes and malformed or unmapped selections. These are
semantic checks rather than snapshot-only evidence.

Independent replay script `replay_gate.py` uses the checked-in generated main
shell, whose verdict logic is unchanged by this patch. It checks control and
unit failures plus admitted/nonadmitted and nonapplicable outcomes. Raw results
are in `gate-replay-results.json` once the process completes. This replay proves
verdict logic, not GitHub scheduler behavior; real PR and main runs remain
required integration evidence.

## Remaining scope limits

The source-1 admission skip finding is resolved in the updated diff: both
nested and direct required verdicts now run unconditionally on the hosted
control plane. No repository checkout or action executes in these verdicts.
Parsed regression coverage spans GitHub/Velnor/Both runner modes, PR/main
events, and direct/nested rendering, asserting always() and hosted runners.
This preserves provider trust while ensuring the final verdict exists.

The new graph mutation test covers source-2 PR rendering and expected dependency
sets computed from the existing caller model. It does not by itself prove all
main/nightly/provider configurations or that discovery enumerated every
obligation. Existing coverage and further integration work must supply those
claims. No merge protection or stale-candidate enforcement claim follows from
this patch alone.
