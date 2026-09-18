# BOOTSTRAP DISCLOSURE — one-time authorized merge of PR #918

**Authorization.** Repo operator authorized on 2026-09-17 a ONE-TIME bootstrap
merge of the rendezvous fix ONLY (this PR #918). Scope: merge with
Policy-red-only + all-else-green, immediate green pin-bump PR after to heal
main (~30 min window), full disclosure in both PRs. Recorded in the campaign
ledger IMPOSSIBILITY RESULT entry. Every later main landing goes green via the
fixed rendezvous.

**Why a red merge is unavoidable (impossibility evidence).** Three independent
code-level proofs (`/tmp/pr916-flow.md`, `/tmp/trigger-fix.md`,
`/tmp/candidate-binding.md`) show the candidate path cannot rendezvous today:

- Publisher names the artifact by the PR-HEAD candidate closure (`ir.rs`
  `head_closure`, rendered `ci-unit-rust.yml`).
- Policy Acquire polls by the AUDITED-PIN candidate closure and gates
  manifest == pin (`lib.rs` Acquire template).
- The validator gates manifest == HEAD (`policy.rs` `wanted`).

Jointly satisfiable only when pin..head is closure-clean, so NO
render-changing generator PR can ever land green — and any rendezvous fix is
itself render-changing (infinite regress). A binary-side trigger is infeasible
(Enforce has no token; every token path is a render change). Hence: one
authorized red-merge of the class fix (poll-by-head + head gating), then all
green forever.

**What this PR does.** Generator `lib.rs` Acquire template + regen only;
`policy.rs` untouched. Poll name + manifest gate derive from
`velnor-workflow closure --rev="$HEAD_SHA" --candidate` (the identity publisher
and validator already use); same-closure early exit additionally requires
tree==pin-render via `--plain --check`. Independent review CERTIFIED content
(`/tmp/v-rendezvous.md`); all gates reproduced in scratch.

**Conflict resolution (mechanical, verified).** `origin/main` moved twice under
this PR (#917, then pin-bump `dead5ecb`); merge commit `933239b2` merges
current `origin/main` (`dead5ecb`) into this branch. The only conflict was the
generator state-digest file, resolved per standing rule (`git add -A` +
rebuilt-generator `--plain --force` regen): exactly the 2 ci-main/ci-policy
digest lines vs main, ZERO YAML drift. Gates at the merge: `cargo test -p
velnor-workflow` all pass (754 lib, 0 failed), clippy/fmt/actionlint clean,
`--plain --dry-run` 0 files, `--plain --check --pin-build` exit 0 with
candidate notice, Acquire `bash -n` + `shellcheck` clean.

**Predicted CI shape for head `933239b2`.** Planning green; ALL GitHub units
green; Policy RED with the verbatim trap signature only (base-owned old YAML
still polls by pin — the circularity this PR breaks); Velnor-lane admission
noise identical to the #916/#917 precedent set (pre-existing infra, fails
closed pre-execution). Anything else red blocks the merge. Post-merge chain:
pin-bump to the merge commit goes green via the fixed rendezvous, then #916
rebased goes green via the NORMAL candidate path.
