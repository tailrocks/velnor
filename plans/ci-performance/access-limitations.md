# Observed access limits

These limits affect specific validation paths, not the entire campaign.
Implementation, local signed-off commits, connector PR creation, public run/job/log
reads, and local checks continue. SSH access must be assessed per attempt. No blocked requirement is marked complete.

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

CLI authentication rechecked 2026-09-20 02:57 UTC: same invalid account token;
`gh auth status` exited 1. No second permission question or token request sent.

A feasible dispatch alternative remains unimplemented: a generated, trusted
CI controller can use its own explicitly scoped `actions: write` run token to
dispatch an existing validation workflow at the exact candidate ref and await
its real outcome. This needs independent event/trust/recursion review and
safe non-publishing inputs. It is not proof that local CLI authorization works,
and no controller dispatch has been performed.
GitHub documents `workflow_dispatch` as an exception to token-triggered
workflow suppression; dispatch targets must exist on the default branch. See
[the token event contract](https://docs.github.com/en/enterprise-cloud@latest/actions/concepts/security/github_token)
and [workflow dispatch syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#onworkflow_dispatch).

At approximately 04:40 UTC, SSH fetches for Velnor PR heads 966/963 and Jackin
1004/1006/1005 failed with exit 128. Exact operations used
`git -c fetch.prune=false fetch origin refs/pull/<N>/head:refs/remotes/origin/pr-<N>`.
Velnor reported `sign_and_send_pubkey: signing failed for ED25519 "donbeave SSH"
from agent: communication with agent failed`; Jackin reported `agent refused
operation`. Both then reported `Permission denied (publickey)`.

The same fetches through the public HTTPS repository URLs succeeded, preserving
read-only PR inspection. Remote URLs were not changed. The bounded retry `GIT_SSH_COMMAND='ssh -o BatchMode=yes -o ConnectTimeout=15'
git push origin codex/ci-performance-campaign` for local commit
`64cef5ef93a90660983b3f2ec9dc42cfebda73c3` also exited 128 with the same SSH
agent communication/signing failure. Exact-revision remote CI for that commit
is blocked until a working push path is restored; local implementation and
verification continue. Earlier successful pushes do not prove current access. No credential was read or requested in chat.

## Verified API publication fallback

Independent review accepted publishing the identical verified tree through
GitHub's Git-object API, with a non-force ref update and race checks. The nine
uploaded blob IDs matched local Git; the created tree exactly matched
`971c60e49840504b5948542dd232d9e2294b61ab`. API commit
`057ed827ab487a8b7b818ab95a4b3c24dff4fe69` has parent
`b6b4f2e14ab035def118612596df28e1f10d148b` and the original message, signoff, and
coauthor trailers. Author/committer remained the same account; timestamp and
commit ID changed. The existing branch still pointed to the expected parent
immediately before its non-force update.

HTTPS fetch confirmed the new remote commit. Local unpublished `64cef5e` is
preserved at `refs/codex/unpublished/locked-64cef5e`; `reset --soft` aligned the
working branch to the identical API tree. Before/after hashes confirmed all
43 modified/untracked paths and the index unchanged. This restores publication
through an authenticated alternative; it does not repair SSH or CLI tokens.

The same exact-tree procedure published cancellation guards (`35a07a59`),
the strict-lock fixture repair (`bb94bac9`), and Rust validation ordering
(`95432856`). Each retains its original local commit under
`refs/codex/unpublished/`, verifies the remote tree/parent/message, and preserves
dirty file and index bytes during local alignment. One Rust-order blob upload
failed with an HTTP transport error; a bounded retry uploaded the same blob
SHA successfully. Ref updates were never forced. DCO signoff is retained;
these API commits do not claim cryptographic commit signatures.

A noninteractive HTTPS Git push was also attempted after alignment to
`df9fb272`: `GIT_TERMINAL_PROMPT=0 git -c credential.interactive=false -c
core.askPass= push https://github.com/tailrocks/velnor.git
HEAD:refs/heads/codex/ci-performance-campaign`. It exited 128 with
`fatal: unable to get password from user`. This confirms the ordinary HTTPS
push path also lacks usable configured credentials; the authenticated
connector publication path remains working. No password or token was read.

## 2026-09-20 09:40 UTC: SSH read access restored

A bounded noninteractive retry succeeded:

```text
GIT_SSH_COMMAND='ssh -o BatchMode=yes -o ConnectTimeout=10' git ls-remote origin HEAD
exit 0; main 9307861d2668424d27012c3c736d48ab47f9012e
```

This supersedes the earlier SSH read failure for the current session. It does
not alone prove branch-write or Actions-dispatch access; those operations
need their own observed outcomes. No credentials were changed or read.

At 09:42 UTC, the same noninteractive SSH transport pushed commit
`112e6acc` successfully to the existing campaign branch (fast-forward from
`2447cc8d`). Ordinary SSH publication is available again. `gh auth status`
still exits 1 with an invalid-token report; Actions mutation capability remains
separate from Git transport and uses the connector where supported.

## 2026-09-20 10:09 UTC: SSH signing failure recurred

The normal noninteractive push of local commit `7397441a` failed with exit 128:
`sign_and_send_pubkey: signing failed for ED25519 "donbeave SSH" from agent:
communication with agent failed`, followed by `Permission denied (publickey)`.
No credential change was attempted. The existing Git-object connector fallback
published the identical tree as `ebc05ab0`; parent, message, DCO/coauthor
trailers and tree hash were verified before a non-forced ref update. The local
commit remains under `refs/codex/unpublished/checkpoint-7397441`; soft alignment
preserved the pending index and worktree. Remote publication remains available
through that fallback despite intermittent SSH signing access.
