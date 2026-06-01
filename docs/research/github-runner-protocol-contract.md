# GitHub Runner Protocol Contract

Reference source: `actions/runner` commit `c6a124e` from `main`, inspected on 2026-05-31.

This is the first concrete wire contract Velnor should emulate.

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
  "version": "2.326.0",
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

GitHub returns agent id, OAuth authorization data, and sometimes agent properties that enforce V2. Velnor stores `clientId`, `authorizationUrl`, and the generated private key so it can mint OAuth JWT credentials for session/message APIs later. It also preserves `ServerUrlV2` and `UseV2Flow` from returned agent properties. The runner loop now uses broker session/message APIs and run-service `acquirejob` when V2 is enabled; remaining V2 work is replacing classic lock renewal/completion with run-service `renewjob`/`completejob`.

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
  "exp": 1710000600
}
```

The access token from this exchange is used as the bearer token for distributed task session/message APIs.

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

## Current Implementation Gap

The `velnor-runner run --once` path can now exchange stored OAuth credentials and call classic session/message APIs. This still needs a live GitHub registration test, and V2 broker/session support remains separate work if GitHub forces `use_v2_flow`.
