Investigation complete. Working tree untouched; simulation worktree removed. Findings below.

## 1. Mechanism — why #1063 fails and which binary should satisfy `prove_candidate`

**Publisher** (`ci-unit-rust.yml:612-679`, `candidate_publish: true` on the `rust-velnor-workflow` unit): builds the candidate from `CANDIDATE_PR_HEAD_SHA` (reusing the unit's merge-tree binary only if merge closure == head closure), writes `candidate-manifest.json` with `revision=PR_HEAD, closure=head_candidate_closure`, publishes artifact `velnor-workflow-candidate-<closure16>-<OS>-<ARCH>`. It **skips** publishing when `head_closure == base_pin_closure`, where `base_pin` = pin declared at `BASE` (`inputs.base_sha`).

**Consumer** (base branch's `ci-policy.yml` runs under `pull_request_target`): three literals all rendered from the **base** tree's `[generator] revision` — `rev:` (setup action installs the Stage-0 validator product), `BASE_PIN:` (closure comparison), `VELNOR_WORKFLOW_POLICY_REVISION:` (validator rev; the Enforce step passes no `--base-revision` flag, so `run_cli` reads it from env per `s2/policy.rs:281-289`). Acquire step: if `closure(pin) == closure(BASE_PIN)` (release profile) → the running validator IS the pin's renderer → try `--check`; match = early exit, differ = fall through to candidate download. Else full fall-through: download head candidate, verify manifest closure == locally computed head candidate, digest-bind, export `VELNOR_WORKFLOW_PINNED_BINARY` + `VELNOR_WORKFLOW_CANDIDATE_MANIFEST`.

**Validator** (`s2/policy.rs`): `generated-tree` = `regenerate_and_compare` → `resolve_pinned_binary` tries (1) running binary if its `SOURCE_CLOSURE ∈ expected_closures(pin)` = pin's [release, debug, candidate] closures, else (2) `PINNED_BINARY` with **hard error** if it isn't the pin, else PATH/build (build forbidden in CI). Only if a pin renderer is obtained and its render differs does `render_with_candidate` run (sole `load_candidate_manifest` caller, `s2/policy.rs:1540`): manifest closure must equal locally computed head candidate closure, digest before exec, `--closure` as tripwire, render must match → `Candidate` PASS on PRs.

**The crux**: the env-slot head candidate can satisfy the *pin* leg only when head's closure inputs equal the pin's. #1063: validator `eed474c4`, pin `6737cdb3`, `closure-rel(6737cdb3)=0e62c07a… ≠ closure-rel(eed474c4)=ec8123d5…` → full fall-through → `PINNED_BINARY` = head candidate reporting `0a64505f…` (I recomputed all four digests locally via `git ls-tree`+footer+sha256 — all match the reviewer/CI exactly). Running binary can't prove pin 6737cdb3, `PINNED_BINARY` isn't the pin either → hard error **before the candidate exception is ever reached**. A stale pin + changed closure inputs is unpassable by design; the pin leg must be satisfiable by the base validator itself (pin == validator, early-exit path).

## 2. Ground truth — merged generator PRs all PASSED (pre-#1060: pin == validator == 6737cdb3)

| PR | head | Policy | validator (`POLICY_REVISION`) | declared pin | acquire path | generated-tree |
|----|------|--------|-------------------------------|--------------|--------------|----------------|
| 1059 | `f7bebb42` | ✅ success 21 Sep 20:45Z | `6737cdb3` | `6737cdb3` | "shares the base closure but the tree differs… falling through to candidate path" | PASS candidate `34ebf792…` |
| 1061 | `84877bf5` | ✅ success 21 Sep 20:54Z | `6737cdb3` | `6737cdb3` | same fall-through | PASS candidate `15883497…` |
| 1053 | `59f35323` | ✅ success 21 Sep 20:45Z | `6737cdb3` | `6737cdb3` | same fall-through | PASS candidate `30a78e30…` |
| 1063 | `5349ec32` | ❌ failure 21 Sep 21:38Z | `eed474c4` | `6737cdb3` | straight to candidate download (no "shares the base closure" line) | FAIL "could not regenerate… `PINNED_BINARY=… is not the declared pin: reports closure 0a64505f…`" |

Their `pin-monotonic` reads "the declared pin **is** the base validator" — the early-exit precondition #1063 lacks. (Logs: `/tmp/p105{3,8,9},p1061,p1063-policy.log`.)

## 3. Blast radius — NOT repo-wide. Reviewer's §4 "any PR fails identically" is REFUTED

Open PRs with Policy runs **after** #1060 merged all pass: 1058 ✅21:50Z, 1056 ✅21:42Z, 1055 ✅21:46Z, 1054 ✅21:39Z, 1050 ✅21:39Z, 1044 ✅21:51Z (1057 in-progress; 1052/973 last ran pre-#1060). Cause: they **rebased onto new main** (merge-base `45ef1ebe`) and declare pin `eed474c4` = validator. Only #1063 still sits on base `eed474c4` with stale pin `6737cdb3`. Oracle: PR 1058's log shows the post-fix path — `pin eed474c4… shares the base closure and renders the tree` → `PASS generated-tree every generated file is byte-identical to the render of velnor-workflow at eed474c4…` → `policy: 11 rules, 0 failed`.

## 4. #1060 suspect — CLEARED. Main is self-consistent; the premise misreads the commit message

#1060's title says "bump self-pin 6737cdb3 to 80bc420d" but the **actual tree diff** is `revision = 6737cdb3 → eed474c4`, and all `ci-policy.yml` literals are `eed474c4`. Verified: every pin literal on `45ef1ebe` (toml + both workflows) is `eed474c4`. Validator rev **should be the base branch's declared pin** — `ci-policy.yml` is base-owned and its literals render from base's `[generator] revision` (`s2/policy.rs:3-7` trust invariant) — and main satisfies exactly that. (Side note: #1062 `45ef1ebe` is an **empty commit** — `git diff c674f5bb 45ef1ebe` is empty; its "bump to eed474c4" was already done by #1060. Also, reviewer's "rebase does not fix: closure(80bc420d)≠validator" compares the wrong revision — actual pin is `eed474c4`, trivially equal to the validator.) No harness repair needed.

## 5. Fix recipe for #1063 (branch owner; no rule weakened, no harness change)

1. `git fetch origin && git rebase origin/main` (onto `45ef1ebe`). Inherits declared pin `eed474c4` = current base validator. I simulated this exactly (detached worktree + cherry-picks `228fc58f` `5349ec32`, since removed): **applies clean**, and rebased head keeps candidate closure `0a64505f…` (main drift touches no closure paths).
2. Verify locally (no new commit expected): build, run `velnor-workflow generate . --check --pin-build` → must print "Generated files are current", exit 0. **Proven in simulation**: candidate render is byte-identical to the rebased tree (23/23 files `cmp`-clean), and `--pin-build` confirms the pin renderer reproduces it. The port changes zero rendered bytes, so no re-render commit is needed. (If it ever differs: run `generate .`, commit — `.github` is outside closure paths, closure-safe.)
3. Force-push the rebased branch. **Do NOT bump the pin on this branch** — pin discipline keeps Stage-0 on the published base product until merge; the pin-forward to the merge commit happens in a later pin-bump PR. (Rejected alternative: pin-to-head would pass this PR's check but poison post-merge `pull_request_target`, whose setup `rev:` would point at an unpublished product and break every future Policy run.)
4. Post-merge follow-up (separate, as usual): pin-forward commit advancing `[generator] revision` to the merge commit + re-render, mirroring #1060.

**Expected post-fix Policy log** (mirrors proven oracle PR 1058): `pin eed474c4… shares the base closure and renders the tree; the Stage-0 validator renders` → `PASS pin-monotonic the declared pin is the base validator eed474c4…` → `PASS generated-tree every generated file is byte-identical to the render of velnor-workflow at eed474c4…` → `policy: 11 rules, 0 failed`.
