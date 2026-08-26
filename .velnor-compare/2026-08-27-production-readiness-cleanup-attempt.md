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
  one Velnor, two ChainArgos, and five Jackin registrations. A second runner
  deletion query returned no remaining `next-*` registrations.
- The stale release run was included in the broad cancellation request; no new
  workflow was dispatched.

Authoritative follow-up required: query each listed run individually until
`status=completed` and `conclusion=cancelled`, or record the GitHub-side
external blocker. Do not dispatch verification while any prior run remains
non-completed.
