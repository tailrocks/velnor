# Migration open questions: decisions (2026-09-17)

Integration-owner answers to Q1–Q5 from `/tmp/jackin-migration-checklist.md`,
from code evidence in velnor2 worktree (base 33688938 + uncommitted slices).

## Q1: event-triggered `[[check_profile]]` (reuse: push/PR/dispatch, no cron)

ANSWER: not expressible today; EXTEND the primitive (G-followup slice).

Evidence: `config/mod.rs` `validate_check_profile_row` rejects a row without
`schedule` ("missing `schedule`; declare the cron cadence"); the renderer
(`primitives/check_profiles.rs` `render_scheduled_checks`) emits only an
`on: schedule` trigger. One scheduled file = one shared cadence.

Decision: add optional `events = ["push", "pull_request", "workflow_dispatch"]`
to `CheckProfileSection`. Exactly one of `schedule`/`events` per row; a
`scheduled-checks` declare row renders either one cron trigger (all rows share
the cadence, as today) or one event trigger set (all rows share the set).
Precedent: `docs_site` already renders event-split gates from declarative
inputs. REUSE lint stays a named task; generic code owns triggers only.

## Q2: multi-release shape (construct + preview + release + jackin-dev)

ANSWER: partially expressible; GENERALIZE the `release` family (E-followup slice).

Evidence per workflow:

- construct → `docker-image-pipeline` unit (D slice). Expressible.
- preview → `preview` declare family, per-row args. Expressible.
- release → `release` declare family, per-row contract args
  (`declared_or_configured_spec`). Expressible, one file.
- jackin-dev → NOT expressible. `primitives/mod.rs` ownership check pins
  `primitive = "release"` to `file = "release.yml"`
  ("must declare `{canonical}`, not `{file}`"), and `Release::render`
  hardcodes `render_file(ctx, "release.yml", ...)`. jackin-dev.yml is a
  second release-pipeline-shaped file (version-policy jobs + target×lane
  matrix build + publish, own path filters/triggers/concurrency), so it
  cannot fold into release.yml.

Decision: let the `release` family render to the row's declared `file`
(the `scheduled-checks` family already renders per-row files with no
canonical pin — same generalization). Per-row contract args already exist,
and `release.rs::render_file` ALREADY renders to `ctx.file` (the
`"release.yml"` argument is only the canonical name in the missing-file
error). `rows_for` pushes declared release-side rows as-is with no
same-primitive restriction, and `push_default_side_rows` already skips the
default row when any declared row names its file. So the ONLY blocker is
the canonical-file pin check in `primitives/mod.rs` ("must declare
`{canonical}`, not `{file}`"): exempt the `release` primitive from that
comparison (keep its `RELEASE_SIDE_FILES` entry for default-row rendering
and `is_release_side` detection). Duplicate-file and unknown-primitive
errors stay fail-closed. The single `[release]` config section remains the
default contract for the undeclared-args row and for `version_bump_units`
recording into project.toml — it is NOT the multi-release mechanism.

Sub-gap Q2b: version-bump validation jobs (validate-version-bump/version/
assert-version in jackin-dev.yml) have no generic home:
`workflow.version_bump_units` only records unit names into project.toml
(`runtime.rs`), enforcing nothing. The E slice's "matrix completeness"
covers build×publish, not the PR version policy ("artifact paths changed
without a version bump fails"). Decide in the E-followup: version-policy
jobs rendered from release-family args (generic policy, product-owned
version source) — do NOT reimplement as paths-filter shell in Jackin.

## Q3: `[docs]` Pages details

Defer to migration time; read from PR docs.yml then. No generator work.

## Q4: do A-prereq / C-reuse change any Jackin declaration?

REVISED after A landed (proven on /tmp/jackin-render): YES for A, no for C.
A adds `[[units]] capabilities` (+ `os`/`arch`, products, prerequisites).
Jackin's migration MUST declare:

- `swift-package-native`: `capabilities = ["xcframework"]` (declared unit,
  scan-excluded manifest; without this it renders ubuntu, regressing slice 1).
- `swift-package-native-design-prototypes-unifiedagentusage` (scanned):
  `capabilities = ["xcode"]` — AppKit prototype, genuinely Apple-bound, but
  scan cannot prove it (platforms-lists have false positives on portable
  libs). Row-over-scan merge confirmed (`apply_unit_row` merges
  `platform_requirement` over the scanned base).
Without these declarations the Apple split renders but both Swift units
take the default executor. Worse, undeclared Swift also flips runtime
provisioning: `render_unit_runtime` keys on `requires_apple()`, so an
undeclared Apple-bound unit downloads the Linux plan artifact instead of
bootstrapping via the setup action (proven by pristine-vs-current render
diff). The declarations are therefore load-bearing for correctness, not
just placement, and a contract note now names any Swift unit on the
default executor. C adds no config keys (verified: no
`[[units]]`/workflow keys in the C diff).

## Q5: `codebook-contract.sh` / `construct-result-contract.sh` refs

RESOLVED: zero refs in Jackin main (`0be3fcf9` `git grep` empty for both
names). All three contract scripts (`codebook-contract.sh`,
`construct-result-contract.sh`, `docs-lychee-contract.sh`) exist in
main's `scripts/ci/` unreferenced → delete all three in the migration
commit once the B/F replacements render. No generator work.

## Challenge verdicts (2026-09-17; `/tmp/followup-design-challenge.md`)

An independent challenger REJECTED all three proposals as specified. The
challenge is accepted with one correction (C8). Revised designs below;
implementers must build these, not the originals.

### Q1 revised: file-level `events` (G-followup)

Per-row `events` with XOR is rejected: it puts file-level data on the wrong
row, XORs what the `docs_site` precedent composes (fixed events + optional
schedule, `docs_site.rs:209-219`), and is stricter than the platform
requires (mixed event sets don't over-execute; mixed crons do). Also every
scheduled-checks file already renders `workflow_dispatch` — only push/PR
triggers and cron-lessness are missing.

Revised: `events` becomes a `scheduled-checks` DECLARE-row arg (next to
`profiles`/`name`). `schedule` stays per-profile; the renderer emits the
shared-cron trigger plus the declared event set. Cron-lessness falls out by
allowing schedule-less profiles only in files whose row sets `events`,
validated in `select_profiles` where file context exists. Rule: "one file,
one trigger set". The followup must also decide: evented-file concurrency
(PR-only-cancel like both PR consumer files and `docs_site.rs:607`, not
silent always-cancel), lanes (single declared lane vs matrix), and must
write the "why not a CI unit" justification (whole-repo compliance check
needing its own required status context + main-branch runs independent of
affected selection — or reuse becomes a unit and Q1 evaporates).

### Q2 revised: shape first, pin stays until mapped (E-followup)

Pin removal alone is rejected: with per-row files but hardcoded content,
two `release` rows render duplicate `name: Release`, identical tag
triggers, identical concurrency — fail-closed becomes fail-open-bytes.
The pin may be a guardrail (one tag-triggered stable publisher per repo is
plausibly correct-by-construction), and jackin-dev (rolling main-branch
binary publisher, no tags) is closer to the `preview` family (push-main
triggers + rolling publish + producer/mode bindings) than to `release`.

Revised: name the second SHAPE first — jackin-dev as preview-kind vs new
rolling-publisher kind, with a trigger/job-graph diff against both
renderers — then unpin the file for the family that owns it, generalizing
`name`/triggers/concurrency per row in the SAME slice (duplicate identity
across files = usage error). CORRECTION to challenge C8: default-row
interaction is already safe — `push_default_side_rows` skips the default
row when ANY declared row names its file — so declaring `release.yml`
suppresses the default; lock that with a test rather than specifying anew.

C6 first pass (integration owner; implementer must confirm against
renderers): jackin-dev is a VERSIONED BINARY-TOOL publisher, not rolling:
PR version-gate + version/assert jobs, target×lane matrix build, publish
creates immutable `jackin-dev-v${VERSION}` release (tarballs + sha256 +
bundles + SBOMs) serialized on a tap-publish group, guarded by a
published-reuse check. vs `preview`: rolling commit-identity prerelease,
push+bare-dispatch, always-cancel, build+publish only — publication
semantics differ (rolling vs immutable-versioned). vs `release`:
tag-triggered stable publisher — publication semantics closest, but
triggers (tags vs push-main+PR+dispatch), version source (tag vs
manifest+gate), and matrix jobs differ. Provisional: NEITHER family fits
as-is; E-followup likely adds a versioned main-branch-driven publisher
shape (new kind or new family) WITH per-row name/triggers/concurrency in
the same slice. The `release` pin stays until that shape lands.

### Q2b revised: generic gate, product task (E-followup)

The generic version-policy renderer is rejected: the policy body (product
path sets, cargo-tree closure compare, manifest sed extraction, Homebrew
formula check) parameterized is "a product workflow with extra steps",
violating the genericity law the same doc cites for Q1 ("named tasks own
every product assertion"). Moving the body into generic code relocates
product logic to the party that cannot verify it.

Revised: generic renders a PR-only `validate-version` GATE job (placement,
PR-only `if`, `fetch-depth: 0`, failure propagation) running a DECLARED
NAMED TASK (check_profile-`tasks` shape); Jackin owns the task body
(paths, closure check, version compare, formula check). Gate placement
reuses `resolve-mode`'s PR→validate mapping. The followup must
supersede-or-reuse `version_bump_matches` (one classifier, not two
cargo-specific copies) and account for `assert-version` separately
(published-result reuse → generic admission machinery or explicit drop;
formula check stays product-owned). Ledger L8 corrected: selection
(version_bump_units narrowing) is not enforcement (a failing gate).

## Q6: dispatch `lanes` override on preview/docs/scheduled/maintenance (OPEN)

Every #994 side file carries a `workflow_dispatch` input `lanes:
velnor|github|both` (default velnor) threaded into `runs-on` conditionals
(preview), matrix lane configs + artifact patterns (preview), and job
placement (docs, scheduled, maintenance copies). Generic coverage today:
renovate KEPT the override (H8a: `lanes` + dispatch override, precedent
for preserving it); stable release has the equivalent `runner` input;
`versioned-tool` (E-2) renders `lanes`. Preview, docs-site,
scheduled-checks, and maintenance render BARE `workflow_dispatch:` with no
override — a human cannot manually dispatch those families onto the other
lane (the fleet-down recovery path).

Decision (challenged, `/tmp/q6-full.md` — implement after the Q7/Q8/M-5/M-6
batch lands): conditional runs-on everywhere, `both` dropped from options
in all four families. Key evidence: `both` is already dead in every
conditional copy (runs-ons test only `== 'github'`, so `both` ≡ velnor by
reading the expressions); reuse-compliance's matrix `writer` flag has no
consumer; preview's `both` leg uploads artifacts that are never downloaded.
Per-family: preview + docs-site + maintenance take the github-default input
(`github (default) | velnor`, options `[github, velnor]` — preview's
inverted polarity vs other copies matches its generic default, precedent
not drift); scheduled-checks takes per-file default lane with a homogeneity
rule (all non-macos profiles share one lane or usage error) and macos
profiles always static (desktop-cadence precedent: lanes input present but
unthreaded into macos-26 jobs). Preview stays lane-neutral (no lane-suffixed
artifact names — answers the open artifact question). Surface: uniform
`lanes_input = true` (default false = today's bytes exactly), new
`Args::flag`, shared `lanes_dispatch_input`/`lanes_runs_on` helpers,
renovate consolidation only if byte-identical, `lanes_input` with
`runners != Both` is a usage error (renovate's no-escalation law).
Maintenance renders lanes first above `pull_request_number`; preview lanes
before mode inputs (implementer confirms). NOT dropped: H8a precedent +
universal PR-copy presence make the recovery lane live behavior.

## Q7: release tag trigger pattern is hardcoded `v*` (OPEN, small)

All five stable-release trigger renders hardcode `tags: ["v*"]`
(`release.rs`:3038/3126/3250/3655/3774); no config field exists. Jackin
release triggers on `v[0-9]*` (ab5b0c4 release.yml). With `v*`, pushing a
non-conforming tag (e.g. `vbeta`) triggers a run that then fails closed in
`verify-tag` — correct but noisy (spurious red checks); no wrong publish
is possible. Decision: add optional `tag_pattern` (declare-arg +/or
`[release]` key, default `v*`, validated as a GitHub tag-filter glob —
no path separators, non-empty, must start with `v`), rendered at all five
sites. Post-E-2 (release.rs owned). Jackin declares `v[0-9]*`.

## Q8: image lane logs into GHCR only; construct publishes to Docker Hub (OPEN)

Every image-lane login step hardcodes `registry: ghcr.io` with
`GITHUB_TOKEN` (`release.rs` image-admission/image-platform/image/publish
jobs). Jackin construct pushes `projectjackin/construct` to Docker Hub
with `secrets.DOCKERHUB_USERNAME`/`secrets.DOCKERHUB_TOKEN`, login gated
on publish-runs + non-empty creds (ab5b0c4 construct.yml:268-274,
370-374). The image NAME already flows from config; only auth is
GHCR-bound. Decision: add optional registry auth to the docker contract
(`registry` host default `ghcr.io` + credential-secret refs, e.g.
`registry_username_secret`/`registry_password_secret` defaulting to the
github.actor/GITHUB_TOKEN pair; Docker Hub = empty/default host with
explicit secrets). Validation: secret refs match `^[A-Z][A-Z0-9_]*$`
(existing secret-name predicate); login step renders only on publish
paths (compare jobs never log in with write creds). Post-E-2
(release.rs owned). Jackin construct declares Docker Hub + its two
secret names; GHCR consumers render byte-identical output.

## Sequencing impact

- G-followup (`check_profile` events) and E-followup (release family file
  generalization + version policy) are NEW slices. Both touch
  `config/mod.rs` + their primitive file; spawn only after the current
  H-remainder agent (which owns those files) delivers, to avoid triple
  divergent edits.
- Migration commit 1 (pin + FFI edge) is unaffected and stays first.
- `[release]`-family declares for Jackin wait for the E-followup pin.
