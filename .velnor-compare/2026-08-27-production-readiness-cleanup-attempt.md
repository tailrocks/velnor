# Verification cleanup attempt

Captured 2026-08-27. This records the required pre-verification cleanup
attempt; the checklist item remains unchecked because GitHub has not reached a
clean state.

- Velnor run `33010337075` was already `cancelled`; later old pending/queued
  Velnor runs received cancellation requests.
- ChainArgos runs `33010149626` and `33010150644` received cancellation
  requests but remained `in_progress`/`queued` at the first check. Older queued
  runs `32985134450`, `32984965998`, and `32984867843` return HTTP 409 from
  `POST /repos/ChainArgos/java-monorepo/actions/runs/<id>/cancel`:
  `Cannot cancel a workflow run that has not been queued yet.`
- Jackin runs `32962658148` and `32940470688` received cancellation requests
  but remained queued at the first check.
- Eight offline `next-*` validation-owned runner registrations were removed:
  `tailrocks/velnor` runner `3492`; `ChainArgos/java-monorepo` runners `14671`
  and `14719`; and `jackin-project/jackin` runners `4578`, `4571`, `4569`,
  `4566`, and `4581`. The pre-delete query showed each `status=offline` and
  `busy=false`; the post-delete query returned no remaining `next-*`
  registrations.
- Exact cancellation targets were: Velnor `33011217619`, `32989529327`,
  `32987778460`, `32987670118`; ChainArgos `33010149626`, `33010150644`,
  `32985134450`, `32984965998`, `32984867843`; and Jackin `32962658148`,
  `32940470688`. The broad request also targeted stale Velnor release run
  `33009178017`; it was not a verification dispatch. No new workflow was
  dispatched by the campaign.

Authoritative follow-up required: query each listed run individually until
`status=completed` and `conclusion=cancelled`, or record the GitHub-side
external blocker. Do not dispatch verification while any prior run remains
non-completed.

## Force-cancel follow-up

The current GitHub REST workflow-runs documentation supports
`POST .../force-cancel` for runs that do not respond to normal cancellation.
Force-cancel returned `202 Accepted` for the old Velnor and Jackin runs; they
then disappeared from the non-completed listing. The three ChainArgos runs
still returned HTTP 409 with `Cannot cancel a workflow run that has not been
queued yet.`

Authoritative API inspection of ChainArgos runs `32985134450`, `32984965998`,
and `32984867843` shows workflow status `queued`, `latest_check_runs_count=0`,
no jobs, and check suites `89353010038`, `89352428140`, and `89352110318`,
respectively. Each check suite also remains `status=queued` with zero check
runs and unchanged `updated_at` since creation. This is an observed GitHub-side
queue-object blocker; normal and force cancellation cannot target it. The
evidence does not establish the deeper backend cause.

## Subsequent independent recheck

Fresh read-only investigator rechecked the three runs and suites after this
record was pushed: all remain `queued`, with zero jobs, zero check runs, and
no conclusion. Repository-wide ChainArgos state has zero in-progress runs but
these three nonterminal queue objects remain. The cleanup checkbox therefore
cannot be checked, and verification remains prohibited until GitHub-side
remediation changes those objects to terminal state or removes them.
