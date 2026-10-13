# GitHub Runner Protocol Contract

Reference source: `actions/runner` commit `c6a124e` from `main`, inspected on 2026-05-31.
Live GitHub smoke data was collected against hosted GitHub on 2026-06-01.

This is the first concrete wire contract Velnor should emulate.

GitHub does not document this protocol as a stable public API. Velnor still intentionally depends on it in Phase 0: the project goal is drop-in self-hosted runner compatibility, and that requires speaking the same registration, broker, run-service, and job-completion contracts as the official runner. Protocol drift should be handled with live compatibility tests and narrow contract fixtures.

## Scope URLs

For hosted GitHub, the runner maps configured URLs to API URLs like this:

```text
https://github.com/OWNER/REPO
  -> https://api.github.com/repos/OWNER/REPO/actions/runners/registration-token
  -> https://api.github.com/repos/OWNER/REPO/actions/runners/remove-token

https://github.com/ORG
  -> https://api.github.com/orgs/ORG/actions/runners/registration-token
  -> https://api.github.com/orgs/ORG/actions/runners/remove-token

https://github.com/enterprises/NAME
  -> https://api.github.com/enterprises/NAME/actions/runners/registration-token
  -> https://api.github.com/enterprises/NAME/actions/runners/remove-token
```

For GitHub Enterprise Server, the runner uses `/api/v3`:

```text
https://github.example.com/OWNER/REPO
  -> https://github.example.com/api/v3/repos/OWNER/REPO/actions/runners/registration-token
```

## Tenant Credential

If the user provides a GitHub PAT instead of a runner registration token, the official runner first requests a short-lived runner token:

```text
POST https://api.github.com/repos/OWNER/REPO/actions/runners/registration-token
Authorization: basic base64("github:<PAT>")
Accept: application/vnd.github.v3+json
```

Response:

```json
{
  "token": "...",
  "expires_at": "2026-05-31T20:00:00Z"
}
```

Registration token exchange:

```text
POST https://api.github.com/actions/runner-registration
Authorization: RemoteAuth <runner-registration-token>
Content-Type: application/json

{
  "url": "https://github.com/OWNER/REPO",
  "runner_event": "register"
}
```

Removal uses the same endpoint with:

```json
{
  "runner_event": "remove"
}
```

The response provides a server URL, token schema/token data, and sometimes `use_v2_flow`.

```json
{
  "url": "https://pipelines.actions.githubusercontent.com/...",
  "token_schema": "OAuthAccessToken",
  "token": "...",
  "use_v2_flow": true
}
```

Velnor stores these as runner credentials and then talks to the distributed task APIs.

## Agent Registration Payload

The runner lists self-hosted pools, chooses the default internal pool unless a runner group is requested, then checks existing agents by name:

```text
GET <server_url>/_apis/distributedtask/pools?api-version=5.1-preview.1
GET <server_url>/_apis/distributedtask/pools/{poolId}/agents?agentName={name}&api-version=6.0-preview.2
```

The next call registers or replaces a `TaskAgent` in the selected pool:

```text
POST <server_url>/_apis/distributedtask/pools/{poolId}/agents?api-version=6.0-preview.2
PUT  <server_url>/_apis/distributedtask/pools/{poolId}/agents/{agentId}?api-version=6.0-preview.2
```

The minimum payload shape Velnor needs:

```json
{
  "name": "velnor-runner",
  "version": "2.334.0",
  "osDescription": "linux",
  "maxParallelism": 1,
  "ephemeral": false,
  "disableUpdate": true,
  "authorization": {
    "publicKey": {
      "exponent": "...",
      "modulus": "..."
    }
  },
  "labels": [
    { "name": "self-hosted", "type": "System" },
    { "name": "linux", "type": "System" },
    { "name": "x86_64", "type": "System" },
    { "name": "velnor", "type": "User" },
    { "name": "hetzner-sentry-ci", "type": "User" }
  ]
}
```

GitHub returns agent id, OAuth authorization data, and sometimes agent properties that enforce V2. Velnor stores `clientId`, `authorizationUrl`, and the generated private key so it can mint OAuth JWT credentials for session/message APIs later. It also preserves `ServerUrlV2` and `UseV2Flow` from returned agent properties. The runner loop now uses broker session/message APIs and run-service `acquirejob`, `renewjob`, and `completejob` when V2 is enabled.

## OAuth Credential Exchange

Normal registered runners do not keep using the temporary registration token. They store OAuth app credentials and sign a JWT client assertion.

Token request:

```text
POST <authorizationUrl>
Accept: application/json
Content-Type: application/x-www-form-urlencoded

grant_type=client_credentials
client_assertion_type=urn:ietf:params:oauth:client-assertion-type:jwt-bearer
client_assertion=<RS256 JWT>
```

JWT assertion claims:

```json
{
  "iss": "<clientId>",
  "sub": "<clientId>",
  "aud": "<authorizationUrl>",
  "jti": "<uuid>",
  "nbf": 1710000000,
  "exp": 1710000300
}
```

The access token from this exchange is used as the bearer token for distributed task session/message APIs.
Hosted GitHub rejects longer runner client assertion lifetimes; Velnor now keeps the JWT lifetime at 5 minutes.

## Message Loop Calls

Classic flow from generated `TaskAgentHttpClientBase`:

```text
POST create session
locationId: 134e239e-2df3-4794-a6f6-24f1f19ec8dc
api version: 5.1-preview.1
route: <server_url>/_apis/distributedtask/pools/{poolId}/sessions

GET message
locationId: c3a054f6-7a8a-49c0-944e-3a8e5d7adfd7
api version: 6.0-preview.1
query: sessionId, lastMessageId, status, runnerVersion, os, architecture, disableUpdate
route: <server_url>/_apis/distributedtask/pools/{poolId}/messages

DELETE message
locationId: c3a054f6-7a8a-49c0-944e-3a8e5d7adfd7
api version: 5.1-preview.1
route: <server_url>/_apis/distributedtask/pools/{poolId}/messages/{messageId}?sessionId={sessionId}

DELETE session
locationId: 134e239e-2df3-4794-a6f6-24f1f19ec8dc
api version: 5.1-preview.1
route: <server_url>/_apis/distributedtask/pools/{poolId}/sessions/{sessionId}
```

V2 broker/run-service flow:

```text
POST <server_url_v2>/_apis/broker/session
GET  <server_url_v2>/_apis/broker/message
POST <server_url_v2>/_apis/broker/acknowledge

POST <run_service_url>/_apis/runtime/runs/{planId}/jobs/{jobId}/acquire
POST <run_service_url>/_apis/runtime/runs/{planId}/jobs/{jobId}/renew
POST <run_service_url>/_apis/runtime/runs/{planId}/jobs/{jobId}/complete
```

Live hosted GitHub sends a classic `BrokerMigration` message first for current self-hosted runners. That message may omit `messageId`, so Velnor must not require it. After migration, broker `session` responses may omit the nested `agent` object. Run-service acquired job payloads can omit repository resources that classic `actions/checkout` planning normally uses, so native checkout should derive the self repository from `github.repository`, `github.sha`, `github.ref`, and `github.server_url` when needed. Run-service acquired job payloads also encode step inputs as typed expression values, including `inputs = { type, map = [{ Key = { lit }, Value = { lit } }] }`; script and checkout adapters must normalize that shape before execution.

The broker can also send control messages while a job is running. Velnor handles `JobCancellation` by killing the active Docker job container and completing the job as canceled. It handles `BrokerMigration` by replacing the broker base URL for later polls.

Run-service `acquirejob` uses the runner OAuth token. After the full job is acquired, renew and complete calls should use the job-scoped `SystemVssConnection` access token from the acquired job payload; using the runner OAuth token can return `401 Not authorized for this job`.

## Runner Removal

Remote unregister mirrors registration:

```text
POST https://api.github.com/repos/OWNER/REPO/actions/runners/remove-token
Authorization: basic base64("github:<PAT>")

POST https://api.github.com/actions/runner-registration
Authorization: RemoteAuth <runner-remove-token>

DELETE <server_url>/_apis/distributedtask/pools/{poolId}/agents/{agentId}?api-version=6.0-preview.2
```

Velnor uses local config to know `poolId`, `agentId`, and GitHub scope URL, then removes the local config file.

Job lock:

```text
RenewAgentRequestAsync(poolId, requestId, lockToken, orchestrationId)
FinishAgentRequestAsync(poolId, requestId, lockToken, finishTime, result)
```

## Velnor First Implementation Rule

Implement and test in this order:

1. URL derivation and payload serialization.
2. Tenant credential exchange.
3. Agent pool lookup and agent add/replace.
4. Session create/delete.
5. Message long-poll.
6. Job lock renew/finish.

This avoids building Docker execution before Velnor can receive a real job.

## Current Implementation Status

The `velnor-runner run` path can exchange stored OAuth credentials and call either the classic session/message APIs or the V2 broker/run-service APIs. Classic jobs finish through `JobCompleted` plus agent request finish; V2 jobs acquire, renew, and complete through run-service.

Live proof on 2026-06-01:

- runner registration against hosted GitHub succeeded
- classic session creation succeeded
- classic `BrokerMigration` to hosted broker succeeded
- broker session/message poll succeeded
- run-service `RunnerJobRequest` acquisition succeeded
- run-service step input normalization succeeded for real hosted payload shape
- `--complete-noop` completed a real GitHub job as `success`

Remaining proof gap: normal Docker execution still needs a host/daemon setup where the Docker daemon can see Velnor's bind-mounted work directories. In the current development environment, Docker accepted the bind mount but exposed an empty directory inside the container, causing script paths under `/__t` to be missing. Velnor now has a bind-mount visibility preflight so this fails before user steps with an actionable operator error.
