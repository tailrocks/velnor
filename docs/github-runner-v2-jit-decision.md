# GitHub Runner V2 JIT Decision

Status: accepted.

Velnor uses GitHub runner V2 only. Velnor does not use classic runner
registration-token setup as a product path, and does not implement classic
distributed-task polling as fallback behavior.

## Decision

Velnor runner setup must use GitHub's just-in-time runner configuration API:

```text
POST /repos/{owner}/{repo}/actions/runners/generate-jitconfig
POST /orgs/{org}/actions/runners/generate-jitconfig
POST /enterprises/{enterprise}/actions/runners/generate-jitconfig
```

The response contains `encoded_jit_config`. Velnor decodes that config and
requires:

- `UseV2Flow=true`
- `ServerUrlV2`
- runner OAuth credential data
- runner id, runner name, pool id, GitHub URL, and work folder

Each Velnor daemon slot owns one JIT runner identity and one V2 broker session.
JIT runners are ephemeral, so long-running daemon mode must recycle a slot by
creating a new JIT runner config after a job completes or the slot becomes
unusable.

## Why

Hosted GitHub can return classic-only settings through the old
registration-token exchange. That path does not give Velnor the broker
settings it needs (`UseV2Flow` and `ServerUrlV2`) in normal repository testing.

The JIT API does return the V2 settings on hosted GitHub. That makes it the
correct setup path for Velnor's design:

- one daemon controls many internal runner slots
- each slot maps cleanly to one ephemeral GitHub runner identity
- jobs are acquired through broker/run-service V2
- each job runs in its own Docker container
- no classic distributed-task listener is needed

## Not Supported

These are not Velnor product paths:

- `actions/runner-registration` as the normal setup path
- repository runner registration tokens as the normal setup path
- classic distributed-task message polling
- fallback from V2 setup failure to classic polling
- broad support for classic GitHub Enterprise Server runner behavior unless it
  also supplies V2 JIT settings

If setup does not provide `UseV2Flow=true` and `ServerUrlV2`, Velnor must fail
with a clear error.

## API v3 Clarification

`/api/v3` is the GitHub Enterprise Server REST API base path. It is not runner
V2.

Runner V2 means the broker/run-service runner protocol selected by
`UseV2Flow=true` and `ServerUrlV2`.

## References

- GitHub REST docs: create JIT config for repository, organization, and
  enterprise self-hosted runners:
  <https://docs.github.com/en/rest/actions/self-hosted-runners>
- `actions/runner` V2 listener switch:
  <https://github.com/actions/runner/blob/v2.334.0/src/Runner.Listener/Runner.cs#L393-L404>
- V2 broker introduction:
  <https://github.com/actions/runner/pull/2500>
- V2 runner registration work:
  <https://github.com/actions/runner/pull/2505>
- Runner release that introduced vNext/V2 service changes:
  <https://github.com/actions/runner/releases/tag/v2.304.0>
