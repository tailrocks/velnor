# Observed access limits

These limits affect specific validation paths, not the entire campaign.
Implementation, signed SSH pushes, connector PR creation, public run/job/log
reads, and local checks continue. No blocked requirement is marked complete.

| Observed operation | Result | Affected scope | Feasible continuation |
| --- | --- | --- | --- |
| `gh workflow run ci-policy.yml --repo tailrocks/parallax --ref codex/ci-performance-campaign` | HTTP 401, POST `/repos/tailrocks/parallax/actions/workflows/358687592/dispatches`, Requires authentication | Exact candidate manual policy; controlled dispatch cohorts and scheduled child validation | Local CLI authentication refresh requested; meanwhile PR workflows run from pushed branches |
| Connector GET `/repos/{repository}/actions/runners?per_page=100&page=1` for all three repositories | HTTP 400: endpoint is not an allowed public repository/search endpoint | Direct runner online/busy/capability inventory | Inspect available job runner metadata and repository declarations; authenticated CLI required for management API reads |
| `gh auth status`, rechecked 2026-09-20 01:52 UTC | Active account `donbeave`; token invalid | Authenticated CLI API writes/private management reads | User can authenticate locally; no credential requested in chat |

No connector runner-list or workflow-dispatch tool was discovered. Existing
workflow/job rerun tools may replay an existing candidate run, but a rerun is
not proof that a newly changed workflow revision executes. Use only after
checking revision semantics and preserving attempts.

Velnor default branch advanced during work. SSH `ls-remote` and fetch proved
`325719f1e05d3d46322c9fd3eeb9ad545e175638`; an earlier repeated connector PR
response still showed e94. A refreshed request agreed with SSH. Refresh heads
at integration/acceptance and record retrieval times; do not assume an earlier
API response is a current revision. Main is now integrated without force-push.
