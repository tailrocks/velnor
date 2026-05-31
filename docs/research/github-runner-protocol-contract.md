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

The next call registers a `TaskAgent` in the selected pool. The minimum payload shape Velnor needs:

```json
{
  "name": "velnor-runner",
  "version": "2.326.0",
  "maxParallelism": 1,
  "ephemeral": false,
  "disableUpdate": true,
  "labels": [
    { "name": "velnor" },
    { "name": "hetzner-sentry-ci" }
  ]
}
```

OAuth key exchange adds `authorization.publicKey`. If GitHub returns V2 runner authorization, Velnor must store `ServerUrlV2` and use broker flow.

## Message Loop Calls

Classic flow from generated `TaskAgentHttpClientBase`:

```text
POST create session
locationId: 134e239e-2df3-4794-a6f6-24f1f19ec8dc
api version: 5.1-preview.1

GET message
locationId: c3a054f6-7a8a-49c0-944e-3a8e5d7adfd7
api version: 6.0-preview.1
query: sessionId, lastMessageId, status, runnerVersion, os, architecture, disableUpdate
```

Job lock:

```text
RenewAgentRequestAsync(poolId, requestId, lockToken, orchestrationId)
FinishAgentRequestAsync(poolId, requestId, lockToken, finishTime, result)
```

## Velnor First Implementation Rule

Implement and test in this order:

1. URL derivation and payload serialization.
2. Tenant credential exchange.
3. Agent add/replace/remove.
4. Session create/delete.
5. Message long-poll.
6. Job lock renew/finish.

This avoids building Docker execution before Velnor can receive a real job.
