# GitHub authority-freeze/provider feasibility

Status: **read-only feasibility evidence; no repository or GitHub mutation was
performed**.

Observed UTC: `2026-09-20T02:36:32Z`.

This record answers one bounded question: which GitHub-provider mechanisms can
freeze the reviewed transition and make the main update atomic, and against
which actors? It does not approve a transition, authorize a merge, create an
App, change a ruleset, dispatch a workflow, publish, or release.

## Verdict

There is no provider-only freeze or cross-resource atomic transition in the
current authority model that can exclude a repository/org administrator. The
live ruleset grants repository-role actor `5` `bypass_mode=always`; the
authenticated caller is repository admin and organization admin. An admin can
call the documented full ruleset `PUT` or a Contents-write ref operation
directly. A coordinator, proxy, lease, or watchdog can fence only credentials
that agree to use it. It cannot fence a hostile admin who calls GitHub directly.

The documented ref update accepts a new `sha` and `force` only. `force=false`
requires a fast-forward update, but it is not an expected-old-SHA compare and
swap. The documented PR merge `sha` guards the PR head only; branch merge takes
a base branch name and a head branch/SHA, not a base SHA. The ruleset `PUT` is a
full object update and documents no version, expected-current hash, ETag, or
other CAS precondition. No undocumented header or CAS behavior is assumed.

Therefore:

* Against ordinary non-bypass writers, rulesets, required checks, required
  workflows, non-fast-forward protection, and a merge queue can enforce useful
  gates after an authorized ruleset change.
* Against the current admin threat, those controls are at most operator
  consent. An admin can change the ruleset or write the ref outside the lease.
* A merge queue integrates against the latest base plus queued PRs. It does not
  accept the caller's exact reviewed base SHA, and it is not a transaction that
  also changes a ruleset or publishes B.
* A technical guarantee against a repository admin requires authority outside
  that admin (for example a separately controlled organization-level ruleset
  on an eligible plan, or a separately owned repository/organization). That
  authority is not present in the live Free organization and requires explicit
  user/provider action.

## Live read-only snapshot

All values below came from `tailrocks/velnor`; no secret or token value was
recorded.

| Item | Observed value |
| --- | --- |
| `refs/heads/main` | `325719f1e05d3d46322c9fd3eeb9ad545e175638` |
| main parent | `e94b48406c4ed206fce2bbf39b788264e72cf39c` |
| main tree | `e9019f00578d34c3f339c7e8d55b66f7e53f8567` |
| repository | public; default branch `main`; auto-merge enabled; merge commits and squash enabled; rebase disabled |
| organization | `tailrocks`, plan `free`, default repository permission `read` |
| caller | `donbeave`; repository permission `admin`; organization membership `admin` |
| legacy branch protection | `GET /branches/main/protection` returned `404 Branch not protected` |
| active branch rulesets | `protect-main` (`19573071`), `protect-tags` (`19573007`) |
| `protect-main` target | `~DEFAULT_BRANCH`; enforcement `active` |
| `protect-main` rules | `deletion`, `non_fast_forward`, pull request (`merge`, `squash`, `rebase`; 0 approvals), required checks `{DCO, ci-required, Policy}` with no integration IDs and `strict_required_status_checks_policy=false` |
| current bypass | actor `5`, type `RepositoryRole`, `bypass_mode=always`; API also returned `current_user_can_bypass=always` |
| merge queue rule | absent from `protect-main` |
| update/restrict-updates rule | absent from `protect-main` |
| workflow/signature/deployment rule | absent from `protect-main` |

The active ruleset, not the legacy branch-protection endpoint, is therefore the
authoritative live branch control. Current protection is not an admin freeze.

## Documented provider mechanisms and limits

| Mechanism | Provider guarantee | Boundary / required authority |
| --- | --- | --- |
| `PATCH /git/refs/{ref}` with `force=false` | Rejects a non-fast-forward update; body sets target `sha`. | No expected current SHA is accepted. Read-then-update has a race. Contents-write or bypass authority is still required. |
| `non_fast_forward` ruleset rule | Blocks force pushes for users subject to the ruleset. | Does not prevent a fast-forward unwanted write and does not constrain a bypass actor. Current actor `5` bypasses. |
| `update`/restrict-updates rule | Only users with ruleset bypass permission may update matching refs. | Useful for non-bypass writers only. It is not an admin lock; an admin who can edit the ruleset can add a bypass or remove the rule. Requires an authorized ruleset mutation and a proven compatible merge actor. |
| Pull-request rule + required status checks | Requires PR-associated changes and named checks for users subject to the ruleset. A check can optionally be bound to an integration ID. | Current checks are bare contexts and there is no base-SHA CAS. Bypass remains decisive. |
| `workflows` rule | Can require a specified workflow path/ref/repository/SHA to pass. | Binds workflow provenance, not the current main SHA or ruleset version. Needs an authorized ruleset mutation and exact workflow identity. |
| `merge_queue` rule | Queues PRs and checks a synthetic merge against the latest target branch plus queued PRs before merging. | No caller-supplied base SHA. Requires `merge_group` CI. Absent from current ruleset. An admin bypass can skip it. |
| `PUT /pulls/{n}/merge` with `sha` | Refuses when the supplied PR head SHA does not match. | Guards PR head only; no base SHA. It is not a cross-resource transaction. |
| `POST /merges` | Accepts a base branch name and head branch or commit SHA. | Base is a mutable branch name; no base-SHA precondition. |
| Ruleset `PUT` | Full ruleset object can be replaced by a caller with Administration write. Bypass actors and enforcement are fields in that object. | Docs expose no version/expected-current hash/CAS. Any read/hash/write lease is a cooperating-operator protocol. |
| Organization-level ruleset | A separate organization authority can govern repositories on eligible plans. | Docs say organization rulesets require Enterprise; live organization is Free and only repository rulesets were observed. This is an explicit external-authority change, not available in this investigation. |

Official documentation: [Git references REST API](https://docs.github.com/en/rest/git/refs),
[repository rules REST API](https://docs.github.com/en/rest/repos/rules),
[branches REST API](https://docs.github.com/en/rest/branches/branches),
[pull requests REST API](https://docs.github.com/en/rest/pulls/pulls),
[merge queue](https://docs.github.com/en/enterprise-cloud@latest/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue),
[available rules for rulesets](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets),
and [ruleset authority](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/about-rulesets).

The references explicitly document ref `sha`/`force`, PR-merge head `sha`,
base branch plus head SHA for branch merges, ruleset bypass/update fields, and
merge-queue latest-base behavior. They do not document an old-ref/base CAS or a
transaction spanning ruleset, ref, checks, and publication.

## Threat-model result

### Ordinary non-bypass writer

Provider enforcement is possible in principle. An owner can require PRs,
integration-bound checks, the immutable workflow revision, non-fast-forward
updates, and optionally a merge queue. A dedicated existing App can be the only
allowed direct updater if the `update` rule is selected. This still needs a
disposable equivalent-repository test proving the exact merge actor is admitted;
this record does not assume one.

### Cooperating operator/admin

A signed lease can record the expected main SHA, PR head/base, ruleset hash,
allowed endpoints, and expiry. A proxy can reject stale or unapproved requests.
An operator can also remove actor `5` from bypass and re-read the full ruleset
before the transition. These are useful preflight and audit controls, but they
are consent, not provider-enforced safety while the same admin can call GitHub
directly.

### Hostile repository/org administrator

No current same-repository setting excludes this actor. The actor can use
Administration write to replace the ruleset or Contents write to attempt a ref
update, and the live ruleset already reports an always-bypass role. A lease or
proxy cannot observe and prevent every direct provider call. The impossibility
is structural: the threat actor owns the provider capabilities needed to alter
the guard and the guarded ref. Claims of an omnipotent-admin lock require a
separate provider root of trust and must not be made for this repository.

## Exact reviewed-tree integration

The answer is **no** for an exact `(expected base SHA, reviewed PR head)`
transition under the current ruleset and **no** for a provider-atomic base pin.
The PR merge endpoint can require the reviewed PR head, but GitHub chooses the
current base branch. The branch-merge endpoint accepts a head SHA while still
accepting only a base branch name. A pinned auxiliary ref can preserve an
immutable object for later verification, but it does not pin `main` as the
merge base.

A merge queue is a valid integration path for non-bypass writers if explicitly
configured and tested: it rechecks the PR on the latest base/queue group and
can preserve the reviewed PR's content as part of the resulting tree. It does
not preserve an exact earlier base when `main` changes, and it cannot atomically
perform the ruleset handoff, B publication, and main update. The final commit
and tree must be checked against the intended contract after the provider merge;
that check detects a race but does not make the race impossible.

If the contract requires the first main commit to have exact parent order
`(BASE_SHA, PR_HEAD_SHA)` and exact reviewed tree, the remaining choices are:

1. **Operator-quiescence path (consent only).** Obtain explicit owner/admin
   consent that all direct writers are stopped; freeze/re-read the full ruleset,
   main ref, PR head, and check identities; merge normally; immediately verify
   parents/tree/PR attribution. Any unexpected change fails closed. This is not
   safe against a hostile admin.
2. **Provider-gated non-bypass path.** Obtain explicit ruleset authority to
   remove the broad actor-5 bypass, require exact integration-bound checks and
   the appropriate queue/workflow rules, and prove the setup in a disposable
   fixture. This is enforceable for non-bypass writers but still not against the
   repository admin who retains ruleset administration.
3. **Existing dedicated updater path.** Obtain explicit authority for an
   already-existing, narrowly permissioned App and a ruleset with only that
   updater able to update the transition ref. No App creation or installation is
   authorized here. This narrows normal writers; it does not defeat a hostile
   repository/org admin.
4. **External root-of-trust path.** Move the guarded policy to a repository or
   organization whose owner is outside the threat actor, or obtain an eligible
   organization-level ruleset controlled by a separate owner. This is the only
   class that can technically exclude the current repository admin, and it
   requires explicit user/provider authority plus an independently verified
   provider configuration.

Pre-reading main and then calling merge/ref/ruleset APIs without such authority
is a TOCTOU protocol, not an atomic transition. Any plan that calls it CAS,
uses an undocumented `If-Match`/ETag, or treats a check name as a base lock is
rejected.

## Required preflight assertions before any future authority operation

These are planning assertions only; none was executed as a mutation here.

1. Read/hash the full current ruleset and main ref. If actor `5` or any
   administrator remains able to bypass or edit the guard under the declared
   threat model, classify the operation as consent-only and stop.
2. Verify exact Checks integration IDs, workflow path/ref/SHA, PR head SHA,
   expected base SHA/tree, and all required child-run attempts. Bare check names
   are insufficient.
3. If using a queue, require a `merge_group` trigger and prove the required
   checks report on the queue's synthetic SHA. The current ruleset has no queue
   rule, so this requires explicit ruleset authority.
4. If using a direct updater, identify an already-installed App and prove its
   provider permissions and the ruleset's exact bypass/update semantics in an
   isolated fixture. Do not create an App or infer provider behavior.
5. After merge, assert ref SHA, parent order, resulting tree, PR attribution,
   ruleset hash, check provider IDs, and complete run/job/check census. Treat
   this as detection and evidence, not as retroactive CAS.
6. Preserve all current-main drift and forbidden-workflow failures as hard
   blockers. A successful queue or merge cannot narrow the required Linux,
   native, package, preview, release, or Mac obligations.

## Exact read-only command transcript

Commands were run from
`/Users/donbeave/Projects/tailrocks/velnor-project/velnor3` through `rtk`:

```text
rtk gh api repos/tailrocks/velnor/git/ref/heads/main --jq '{ref:.ref,sha:.object.sha}'
=> {"ref":"refs/heads/main","sha":"325719f1e05d3d46322c9fd3eeb9ad545e175638"}

rtk gh api repos/tailrocks/velnor/commits/325719f1e05d3d46322c9fd3eeb9ad545e175638 --jq '{sha:.sha,tree:.commit.tree.sha,parents:[.parents[].sha],message:(.commit.message|split("\\n")[0]),date:.commit.author.date}'
=> sha 325719f1e05d3d46322c9fd3eeb9ad545e175638; tree e9019f00578d34c3f339c7e8d55b66f7e53f8567; parent e94b48406c4ed206fce2bbf39b788264e72cf39c; date 2026-09-20T01:19:17Z

rtk gh api repos/tailrocks/velnor/branches/main/protection
=> HTTP 404: {"message":"Branch not protected"}

rtk gh api repos/tailrocks/velnor/rulesets --jq '.[] | {id,name,target,enforcement,source_type}'
=> protect-main/19573071 branch active Repository; protect-tags/19573007 tag active Repository

rtk gh api repos/tailrocks/velnor/rulesets/19573071
=> enforcement active; target branch; include ~DEFAULT_BRANCH; rules deletion, non_fast_forward, pull_request, required_status_checks; required contexts DCO, ci-required, Policy; bypass actor_id 5, actor_type RepositoryRole, bypass_mode always; current_user_can_bypass always

rtk gh api repos/tailrocks/velnor/collaborators/donbeave/permission
=> permission admin; role_name admin

rtk gh api orgs/tailrocks/memberships/donbeave --jq '{role,state}'
=> {"role":"admin","state":"active"}

rtk gh api orgs/tailrocks --jq '{login,plan,default_repository_permission}'
=> {"login":"tailrocks","plan":{"name":"free",...},"default_repository_permission":"read"}
```

No write endpoint, dispatch, merge, App operation, publication, release, or
ruleset update was called.

