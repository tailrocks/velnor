# D1 canary PERMISSIONS probe — result (read-only, no repos/sets created)

Date (UTC): 2026-09-17. Method: `gh api` GET only. No secret values printed or stored.

Input needs (from `/tmp/d1-canary-prep.md` §1–§2): new repo `tailrocks/velnor-d1-canary`,
runner group `velnor-d1-canary-group` (selected-repos = canary repo only), org-level
scale set `velnor-d1-canary`, GitHub App credentials (Administration R/W + Actions R).

## 1. Can the current token create repos in `tailrocks`?

YES (in principle; creation NOT attempted — read-only probe).

- Token scopes (from `X-Oauth-Scopes` header + `gh auth status`): `admin:org`,
  `delete_repo`, `gist`, `repo`, `workflow` (classic PAT — OAuth scope form, not
  fine-grained).
- Org membership (`/user/memberships/orgs/tailrocks`): state `active`, role `admin`.
- Org repo-creation policy fields (`members_can_create_repos`, `default_repo_permission`)
  returned null via this token — value unknown, but irrelevant: org **admin** role can
  create repos regardless of the member policy. `repo` scope authorizes repo creation;
  `admin:org` authorizes org-level administration (runner groups); `workflow` scope
  authorizes pushing canary workflow files.
- Canary repo check (`GET /repos/tailrocks/velnor-d1-canary`): **404 Not Found** —
  expected; repo does not exist yet, name is free. No repo was created by this probe.

## 2. Scale sets / runner groups visible (names only)

- Runner groups (`/orgs/tailrocks/actions/runner-groups`, total 2):
  - `Default` (visibility: all)
  - `velnor-trusted` (visibility: selected)
  - `velnor-d1-canary-group` does NOT exist yet — expected; to be created in a later step.
- Org self-hosted runners (total 5, names only): `velnor-tailrocks-slot-1`,
  `velnor-tailrocks-slot-1-next-3657379-9`, `velnor-tailrocks-slot-3`,
  `velnor-tailrocks-slot-6`, `velnor-tailrocks-slot-7`.
- Scale sets: **not listable via classic REST with this token**. `GET` on both
  `/orgs/tailrocks/actions/runners/scale-sets` and `/orgs/tailrocks/actions/runner-scale-sets`
  (plus the repo-level variant) returns **404**; `/orgs/tailrocks/actions/hosted-runners`
  returns 404 "GitHub hosted runners are not supported for this organization".
  This is an endpoint-availability fact (self-hosted scale sets are managed via the
  runtime `_apis/runtime/runnerscalesets` API / UI, not classic REST), NOT a permission
  denial — so existing scale sets can be neither confirmed nor inventoried by this probe.
- Org Actions permissions: `allowed_actions: all` (workflows can run).

## 3. Usable GitHub App? (installations by name, no secrets)

NO. `GET /orgs/tailrocks/installations` lists 5 installed apps (all unsuspended);
none has **Administration:write**, which the canary admin plane requires
(prep §2.1: Administration R/W + Actions R):

| App slug | Administration | Actions | Verdict |
|---|---|---|---|
| `renovate` | read | — | NO (needs write) |
| `dco-2` | — | — | NO |
| `tailrocks-package-updater` | — | — | NO |
| `chatgpt-codex-connector` | — | write | NO (no Administration) |
| `jackin-daemon` | — | — | NO |

(Note: `GET /user/installations` returns 403 with a PAT — that endpoint needs an
App user-to-server token. The org-level list above succeeded via `admin:org` and is
the authoritative inventory. No installation IDs, client IDs, or keys are recorded here.)

## Verdict: NO-GO for the live canary — EXTERNAL BLOCKER

- **GO (current token sufficient):** create repo `tailrocks/velnor-d1-canary`, create
  runner group `velnor-d1-canary-group` locked to it, push canary workflows.
  Basis: org role `admin` + scopes `repo`, `admin:org`, `workflow`.
- **EXACT BLOCKER (external, operator manual step per prep §2.2.1):** no GitHub App
  installed on org `tailrocks` grants **Administration:write** (closest is `renovate`
  with Administration:read). Required: create/install App `velnor-d1-canary`
  (org-owned, permissions Administration R/W + Actions R, installed on `tailrocks`)
  and provision its private key + App/installation IDs into the bastion credential
  provider. This cannot be done via PAT REST (App creation is a UI/manifest flow)
  and was out of scope for this read-only probe.
- **Consequence:** prep §2.1 mandates App auth for the graded canary and FAILS CLOSED
  on PAT auth, so the live canary cannot proceed until the App exists — even though
  the current PAT could perform the repo/group bringup steps.
- **Not a blocker:** scale-set REST 404s (expected endpoint absence, not a denial);
  canary repo/group/set names all currently free (no collisions).
